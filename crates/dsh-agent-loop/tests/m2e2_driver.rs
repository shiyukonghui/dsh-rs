//! M2e-2：ReactLoopAgent 同步驱动测试（turn/step 状态机、pre-step 决策、
//! send/followup/steer、取消、max-tokens 粘性、request-error 重试、status 事件）。

#![allow(clippy::type_complexity)] // mock 闭包（Rc<dyn Fn>）与驱动泥合 seam 一致，显式类型
#![allow(clippy::result_large_err)] // mock stream 的 Result Err 携带 LlmError（设计）

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use dsh_agent::{Agent, AgentBus, AgentRegistry, AgentStatus, InboxTarget, NextFn};
use dsh_agent_loop::{
    LoopDeps, PendingCall, ReactLoopAgent, ToolExecCtx, ToolExecOutcome,
};
use dsh_llm::call_config::CallConfigAdapterDefaults;
use dsh_llm::retry::{ResolvedAlwaysRetryPolicy, ResolvedRetryBackoff, ResolvedRetryPolicy};
use dsh_llm::{
    CallConfig, CallId, ContentBlock, FinishReason, GenerateOptions, LlmError, LlmFailure, Message,
    MessageId, PreparedLlmCall, StreamChunk, ToolCallBlock,
};
use dsh_session::{
    store::SessionStore, AgentCancelCause, CreateSessionMeta, CreateSessionOptions, EventKind,
    Session, SessionId,
};
use dsh_system_prompt::{AssembleContext, AssembledSection, PromptAssembly};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn store() -> Arc<SessionStore> {
    Arc::new(SessionStore::new())
}

fn session(store: &Arc<SessionStore>, id: &str) -> Arc<Session> {
    store
        .create(
            Some(SessionId(id.to_string())),
            &CreateSessionOptions {
                seed: None,
                meta: Some(CreateSessionMeta {
                    seed_length: Some(0),
                    ..Default::default()
                }),
            },
        )
        .unwrap()
}

struct TestWorld {
    reg: Arc<AgentRegistry>,
    bus: AgentBus,
}

impl TestWorld {
    fn new() -> Self {
        let bus = AgentBus::new();
        let reg = Arc::new(AgentRegistry::new(bus.clone()));
        TestWorld { reg, bus }
    }
    fn agent(&self, id: &str) -> Arc<Agent> {
        let s = session(&store(), id);
        Arc::new(
            Agent::new(
                SessionId(id.to_string()),
                s,
                dsh_agent::AgentOptions {
                    provider: Some("litellm".into()),
                    model: Some("deepseek-r1".into()),
                    max_tokens: None,
                },
                self.reg.bus().clone(),
                dsh_scope::ScopeKey::new(),
            )
            .unwrap(),
        )
    }
}

fn listen_status(bus: &AgentBus) -> Arc<Mutex<Vec<String>>> {
    let log = Arc::new(Mutex::new(Vec::new()));
    let l = log.clone();
    bus.on(
        "agent/status",
        true,
        None,
        Arc::new(move |_n, p| {
            let s = p
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string();
            l.lock().unwrap().push(s);
        }),
    );
    log
}

fn count_of(s: &Arc<Session>, kind: EventKind) -> usize {
    s.events().into_iter().filter(|e| e.kind == kind).count()
}

fn turn_end_reason(s: &Arc<Session>) -> Value {
    s.events()
        .into_iter()
        .find(|e| e.kind == EventKind::TurnEnd)
        .map(|e| e.data.get("reason").cloned().unwrap_or(Value::Null))
        .unwrap_or(Value::Null)
}

// ---- mock deps ----

fn mock_assemble(system: &str) -> Arc<dyn Fn(&AssembleContext) -> Result<PromptAssembly, String> + Send + Sync> {
    let system = system.to_string();
    Arc::new(move |_ctx: &AssembleContext| {
        Ok(PromptAssembly {
            sections: vec![AssembledSection {
                name: "mock".into(),
                text: system.clone(),
            }],
            contexts: vec![],
            tools: vec![],
            variables: vec![],
        })
    })
}

fn mock_prepare(provider: &str, model: &str) -> Arc<dyn Fn(CallConfig) -> Result<PreparedLlmCall, LlmError> + Send + Sync> {
    let provider = provider.to_string();
    let model = model.to_string();
    Arc::new(move |_c: CallConfig| {
        Ok(PreparedLlmCall {
            config: CallConfig {
                provider: provider.clone(),
                model: model.clone(),
                ..Default::default()
            },
            retry_policy: ResolvedRetryPolicy::Always(ResolvedAlwaysRetryPolicy {
                backoff: ResolvedRetryBackoff {
                    initial_delay_ms: 1,
                    max_delay_ms: 1,
                    jitter_ratio: 0.0,
                },
            }),
            adapter_defaults: CallConfigAdapterDefaults::default(),
            context: None,
            stream: None,
        })
    })
}

fn mock_stream(
    script: Arc<Mutex<VecDeque<Vec<StreamChunk>>>>,
    calls: Arc<AtomicU32>,
) -> Arc<dyn Fn(&GenerateOptions) -> Result<Vec<StreamChunk>, LlmError> + Send + Sync> {
    Arc::new(move |_req: &GenerateOptions| -> Result<Vec<StreamChunk>, LlmError> {
        calls.store(calls.load(Ordering::SeqCst) + 1, Ordering::SeqCst);
        let next = script.lock().unwrap().pop_front().unwrap_or_default();
        Ok(next)
    })
}

fn mock_tool(
    concluded_seq: &[bool],
    contexts: Arc<Mutex<VecDeque<Vec<Message>>>>,
) -> Arc<dyn Fn(&ToolExecCtx) -> ToolExecOutcome + Send + Sync> {
    let concluded_seq: Vec<bool> = concluded_seq.to_vec();
    let idx = Arc::new(AtomicUsize::new(0usize));
    Arc::new(move |_ctx: &ToolExecCtx| {
        let i = idx
            .load(Ordering::SeqCst)
            .min(concluded_seq.len().saturating_sub(1));
        idx.store(idx.load(Ordering::SeqCst) + 1, Ordering::SeqCst);
        let context = contexts.lock().unwrap().pop_front().unwrap_or_default();
        ToolExecOutcome {
            concluded: concluded_seq[i],
            context,
            pending: Vec::new(),
        }
    })
}

fn mock_tool_never() -> Arc<dyn Fn(&ToolExecCtx) -> ToolExecOutcome + Send + Sync> {
    Arc::new(|_ctx: &ToolExecCtx| ToolExecOutcome {
        concluded: true,
        context: vec![],
        pending: Vec::new(),
    })
}

fn deps(
    assemble: Arc<dyn Fn(&AssembleContext) -> Result<PromptAssembly, String> + Send + Sync>,
    stream: Arc<dyn Fn(&GenerateOptions) -> Result<Vec<StreamChunk>, LlmError> + Send + Sync>,
    tool: Arc<dyn Fn(&ToolExecCtx) -> ToolExecOutcome + Send + Sync>,
) -> LoopDeps {
    LoopDeps {
        assemble,
        prepare_call: mock_prepare("litellm", "deepseek-r1"),
        stream,
        project_context: Arc::new(|_a: &PromptAssembly| None),
        tool_exec: tool,
    }
}

// ---- chunk builders ----

fn text_chunks(text: &str, finish: FinishReason) -> Vec<StreamChunk> {
    vec![
        StreamChunk::BlockStart {
            index: 0,
            block_type: "text".parse().unwrap(),
        },
        StreamChunk::TextDelta {
            index: 0,
            text: text.into(),
        },
        StreamChunk::BlockEnd {
            index: 0,
            block: ContentBlock::text(text),
        },
        StreamChunk::Finish {
            reason: finish,
            replay_state: None,
        },
    ]
}

fn tool_call_chunks(id: &str) -> Vec<StreamChunk> {
    vec![
        StreamChunk::ToolCallDelta {
            index: 0,
            id: CallId::from_raw(id),
            name: Some("demo".into()),
            arguments_delta: "{}".into(),
        },
        StreamChunk::ToolCallDelta {
            index: 0,
            id: CallId::from_raw(id),
            name: None,
            arguments_delta: "{}".into(),
        },
        StreamChunk::BlockEnd {
            index: 0,
            block: ContentBlock::ToolCall(ToolCallBlock {
                id: CallId::from_raw(id),
                name: "demo".into(),
                arguments: "{}".into(),
            }),
        },
        StreamChunk::Finish {
            reason: FinishReason::ToolCalls,
            replay_state: None,
        },
    ]
}

fn error_chunks(message: &str, code: &str) -> Vec<StreamChunk> {
    vec![StreamChunk::Finish {
        reason: FinishReason::Error {
            failure: LlmFailure {
                message: message.into(),
                code: code.into(),
                status: None,
                provider_retry_after_ms: None,
                request_id: None,
            },
        },
        replay_state: None,
    }]
}

fn user_msg(id: &str, text: &str) -> Message {
    Message::user(MessageId(id.to_string()), vec![ContentBlock::text(text)])
}

fn msg_text(m: &Message) -> String {
    match &m.content[0] {
        ContentBlock::Text(t) => t.text().to_string(),
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[test]
fn send_runs_one_turn_completed() {
    let w = TestWorld::new();
    let a = w.agent("a");
    let status = listen_status(&w.bus);
    let script = Arc::new(Mutex::new(VecDeque::from(vec![text_chunks(
        "hello there",
        FinishReason::Stop,
    )])));
    let calls = Arc::new(AtomicU32::new(0));
    let driver = ReactLoopAgent::new(
        a.clone(),
        w.reg.clone(),
        deps(mock_assemble("be helpful"), mock_stream(script, calls.clone()), mock_tool_never()),
    );

    driver.followup(user_msg("m1", "hi")).unwrap();

    assert_eq!(status.lock().unwrap().as_slice(), &["running".to_string(), "idle".to_string()]);
    assert_eq!(count_of(&a.session, EventKind::TurnStart), 1);
    assert_eq!(count_of(&a.session, EventKind::StepStart), 1);
    assert_eq!(count_of(&a.session, EventKind::TurnEnd), 1);
    assert_eq!(turn_end_reason(&a.session)["kind"], "completed");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(driver.status(), AgentStatus::Idle);
    // derive：user + assistant（模型可见 ⟺ 已登录）
    let msgs = a.session.derive_messages().unwrap();
    assert_eq!(msgs.len(), 2);
    assert!(msgs[0].is_user());
    assert!(msgs[1].is_assistant());
    assert_eq!(msg_text(&msgs[1]), "hello there");
}

#[test]
fn stable_prompt_logs_single_initial_header() {
    let w = TestWorld::new();
    let a = w.agent("a");
    let script = Arc::new(Mutex::new(VecDeque::from(vec![
        text_chunks("one", FinishReason::Stop),
        text_chunks("two", FinishReason::Stop),
    ])));
    let calls = Arc::new(AtomicU32::new(0));
    let driver = ReactLoopAgent::new(
        a.clone(),
        w.reg.clone(),
        deps(mock_assemble("same system"), mock_stream(script, calls.clone()), mock_tool_never()),
    );
    driver.followup(user_msg("m1", "first")).unwrap();
    driver.followup(user_msg("m2", "second")).unwrap();

    let headers = a
        .session
        .events()
        .into_iter()
        .filter(|e| e.kind == EventKind::RequestHeader)
        .collect::<Vec<_>>();
    assert_eq!(headers.len(), 1);
    assert_eq!(headers[0].data["reason"], "initial");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn steer_continues_same_turn_until_concluded() {
    let w = TestWorld::new();
    let a = w.agent("a");
    let step1_chunks = tool_call_chunks("c1");
    let step2_chunks = text_chunks("done", FinishReason::Stop);
    let script = Arc::new(Mutex::new(VecDeque::from(vec![step1_chunks, step2_chunks])));
    let calls = Arc::new(AtomicU32::new(0));
    let contexts = Arc::new(Mutex::new(VecDeque::from(vec![vec![user_msg(
        "steer",
        "use the tool result",
    )]])));
    let driver = ReactLoopAgent::new(
        a.clone(),
        w.reg.clone(),
        deps(
            mock_assemble("be helpful"),
            mock_stream(script, calls.clone()),
            mock_tool(&[false, true], contexts),
        ),
    );
    driver.followup(user_msg("m1", "go")).unwrap();

    assert_eq!(count_of(&a.session, EventKind::TurnStart), 1);
    assert_eq!(count_of(&a.session, EventKind::StepStart), 2);
    assert_eq!(count_of(&a.session, EventKind::TurnEnd), 1);
    assert_eq!(turn_end_reason(&a.session)["kind"], "completed");
    let msgs = a.session.derive_messages().unwrap();
    assert!(msgs.iter().any(|m| m.id.raw() == "steer"));
}

#[test]
fn followup_runs_second_turn() {
    let w = TestWorld::new();
    let a = w.agent("a");
    let script = Arc::new(Mutex::new(VecDeque::from(vec![
        text_chunks("one", FinishReason::Stop),
        text_chunks("two", FinishReason::Stop),
    ])));
    let calls = Arc::new(AtomicU32::new(0));
    let driver = ReactLoopAgent::new(
        a.clone(),
        w.reg.clone(),
        deps(mock_assemble("sys"), mock_stream(script, calls.clone()), mock_tool_never()),
    );
    driver.followup(user_msg("m1", "first")).unwrap();
    assert_eq!(count_of(&a.session, EventKind::TurnEnd), 1);
    driver.followup(user_msg("m2", "second")).unwrap();
    assert_eq!(count_of(&a.session, EventKind::TurnEnd), 2);
    let turns: Vec<u64> = a
        .session
        .events()
        .into_iter()
        .filter(|e| e.kind == EventKind::TurnStart)
        .map(|e| e.data["turn"].as_u64().unwrap())
        .collect();
    assert_eq!(turns, vec![1, 2]);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn reject_closes_turn_blocked() {
    let w = TestWorld::new();
    let a = w.agent("a");
    w.bus.on_chain(
        "agent/pre-step",
        true,
        None,
        Arc::new(|_payload: Value, _next: NextFn| json!({ "kind": "reject" })),
    );
    let script = Arc::new(Mutex::new(VecDeque::new()));
    let calls = Arc::new(AtomicU32::new(0));
    let driver = ReactLoopAgent::new(
        a.clone(),
        w.reg.clone(),
        deps(mock_assemble("sys"), mock_stream(script, calls.clone()), mock_tool_never()),
    );
    driver.followup(user_msg("m1", "hi")).unwrap();
    assert_eq!(turn_end_reason(&a.session)["kind"], "blocked");
    assert_eq!(count_of(&a.session, EventKind::StepStart), 0);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn empty_prestep_completes_without_step() {
    let w = TestWorld::new();
    let a = w.agent("a");
    w.bus.on_chain(
        "agent/pre-step",
        true,
        None,
        Arc::new(|_payload: Value, _next: NextFn| json!({ "kind": "enter", "messages": [] })),
    );
    let script = Arc::new(Mutex::new(VecDeque::new()));
    let calls = Arc::new(AtomicU32::new(0));
    let driver = ReactLoopAgent::new(
        a.clone(),
        w.reg.clone(),
        deps(mock_assemble("sys"), mock_stream(script, calls.clone()), mock_tool_never()),
    );
    driver.followup(user_msg("m1", "hi")).unwrap();
    assert_eq!(turn_end_reason(&a.session)["kind"], "completed");
    assert_eq!(count_of(&a.session, EventKind::StepStart), 0);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn max_tokens_sticky_keeps_turn_reason() {
    let w = TestWorld::new();
    let a = w.agent("a");
    let script = Arc::new(Mutex::new(VecDeque::from(vec![
        text_chunks("cut", FinishReason::MaxTokens),
        text_chunks("done", FinishReason::Stop),
    ])));
    let calls = Arc::new(AtomicU32::new(0));
    let driver = ReactLoopAgent::new(
        a.clone(),
        w.reg.clone(),
        deps(mock_assemble("sys"), mock_stream(script, calls), mock_tool_never()),
    );
    // pre-step 监听器在第一步注入 steer → 同 turn 续到第二步
    // （D-115：监听器须 Send+Sync → 经 Send 的 agent.inbox 直注同效应；
    //  与 steer（splice+wake）对当前正在排空的 turn 等价——主循环按 next_step 续跑）
    let steer_agent = a.clone();
    let injected = Arc::new(AtomicBool::new(false));
    let inj = injected.clone();
    w.bus.on_chain(
        "agent/pre-step",
        true,
        None,
        Arc::new(move |payload: Value, next: NextFn| {
            let decision = next(payload);
            if !inj.swap(true, Ordering::SeqCst) {
                let _ = steer_agent
                    .inbox
                    .append_msg(InboxTarget::NextStep, user_msg("s1", "keep going"));
            }
            decision
        }),
    );
    driver.followup(user_msg("m1", "go")).unwrap();

    assert_eq!(count_of(&a.session, EventKind::TurnStart), 1);
    assert_eq!(count_of(&a.session, EventKind::StepStart), 2);
    assert_eq!(count_of(&a.session, EventKind::TurnEnd), 1);
    assert_eq!(turn_end_reason(&a.session)["kind"], "max-tokens");
}

#[test]
fn cancel_before_turn_start_leaves_no_records() {
    let w = TestWorld::new();
    let a = w.agent("a");
    let status = listen_status(&w.bus);
    let driver = ReactLoopAgent::new(
        a.clone(),
        w.reg.clone(),
        deps(
            mock_assemble("sys"),
            mock_stream(Arc::new(Mutex::new(VecDeque::new())), Arc::new(AtomicU32::new(0))),
            mock_tool_never(),
        ),
    );
    let cancel = driver.cancel_token();
    w.bus.on(
        "agent/status",
        true,
        None,
        Arc::new(move |_n, p| {
            if p.get("status").and_then(Value::as_str) == Some("running") {
                *cancel.lock().unwrap() = Some(AgentCancelCause::User);
            }
        }),
    );
    driver.followup(user_msg("m1", "hi")).unwrap();
    assert_eq!(count_of(&a.session, EventKind::TurnStart), 0);
    assert_eq!(count_of(&a.session, EventKind::TurnEnd), 0);
    assert_eq!(status.lock().unwrap().as_slice(), &["running".to_string(), "idle".to_string()]);
    assert_eq!(driver.status(), AgentStatus::Idle);
}

#[test]
fn cancel_mid_turn_aborts_without_error_event() {
    let w = TestWorld::new();
    let a = w.agent("a");
    let errors = Arc::new(AtomicUsize::new(0));
    let err_log = errors.clone();
    w.bus.on("agent/error", true, None, Arc::new(move |_n, _p| {
        err_log.fetch_add(1, Ordering::SeqCst);
    }));
    let script = Arc::new(Mutex::new(VecDeque::from(vec![text_chunks(
        "partial",
        FinishReason::Stop,
    )])));
    let driver = ReactLoopAgent::new(
        a.clone(),
        w.reg.clone(),
        deps(
            mock_assemble("sys"),
            mock_stream(script, Arc::new(AtomicU32::new(0))),
            mock_tool_never(),
        ),
    );
    let cancel = driver.cancel_token();
    let cancelled = Arc::new(AtomicBool::new(false));
    let c = cancelled.clone();
    w.bus.on_chain(
        "agent/pre-step",
        true,
        None,
        Arc::new(move |payload: Value, next: NextFn| {
            if !c.swap(true, Ordering::SeqCst) {
                *cancel.lock().unwrap() = Some(AgentCancelCause::User);
            }
            next(payload)
        }),
    );
    driver.followup(user_msg("m1", "hi")).unwrap();

    assert_eq!(count_of(&a.session, EventKind::TurnStart), 1);
    let reason = turn_end_reason(&a.session);
    assert_eq!(reason["kind"], "aborted");
    assert_eq!(reason["reason"]["kind"], "user");
    // 无 agent/error（abort 路径静默）
    assert_eq!(errors.load(Ordering::SeqCst), 0);
}

#[test]
fn request_error_retry_once_then_succeeds() {
    let w = TestWorld::new();
    let a = w.agent("a");
    let script = Arc::new(Mutex::new(VecDeque::from(vec![
        error_chunks("server boom", "SERVER"),
        text_chunks("recovered", FinishReason::Stop),
    ])));
    let calls = Arc::new(AtomicU32::new(0));
    let driver = ReactLoopAgent::new(
        a.clone(),
        w.reg.clone(),
        deps(mock_assemble("sys"), mock_stream(script, calls.clone()), mock_tool_never()),
    );
    let attempts = Arc::new(AtomicUsize::new(0));
    let at = attempts.clone();
    w.bus.on_chain(
        "agent/request-error",
        true,
        None,
        Arc::new(move |_payload: Value, _next: NextFn| {
            let i = at.fetch_add(1, Ordering::SeqCst);
            if i == 0 {
                json!({ "kind": "retry" })
            } else {
                Value::Null
            }
        }),
    );
    driver.followup(user_msg("m1", "go")).unwrap();

    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert_eq!(turn_end_reason(&a.session)["kind"], "completed");
    // 失败 attempt 的 chunk 已登录（无 assistant/message 关闭），成功 attempt 的消息在：
    // error_chunks 产 1 个 Finish chunk；text_chunks 产 4 个 → 共 5 个 assistant/chunk
    assert_eq!(count_of(&a.session, EventKind::AssistantChunk), 5);
    assert_eq!(count_of(&a.session, EventKind::AssistantMessage), 1);
    let msgs = a.session.derive_messages().unwrap();
    assert_eq!(msgs.len(), 2);
    assert_eq!(msg_text(&msgs[1]), "recovered");
}

#[test]
fn turn_stopping_serial_dispatched_on_close() {
    let w = TestWorld::new();
    let a = w.agent("a");
    let stopped = Arc::new(Mutex::new(Vec::new()));
    let log = stopped.clone();
    w.bus.on_chain(
        "agent/turn-stopping",
        true,
        None,
        Arc::new(move |payload: Value, next: NextFn| {
            log.lock()
                .unwrap()
                .push(payload.get("turn").and_then(Value::as_u64).unwrap_or(0));
            next(payload)
        }),
    );
    let script = Arc::new(Mutex::new(VecDeque::from(vec![text_chunks(
        "bye",
        FinishReason::Stop,
    )])));
    let calls = Arc::new(AtomicU32::new(0));
    let driver = ReactLoopAgent::new(
        a.clone(),
        w.reg.clone(),
        deps(mock_assemble("sys"), mock_stream(script, calls), mock_tool_never()),
    );
    driver.followup(user_msg("m1", "hi")).unwrap();
    assert_eq!(stopped.lock().unwrap().as_slice(), &[1u64]);
}

#[test]
fn inbox_live_events_emitted() {
    let w = TestWorld::new();
    let a = w.agent("a");
    let inserted = Arc::new(Mutex::new(0usize));
    let log = inserted.clone();
    w.bus.on("agent/inbox/inserted", true, None, Arc::new(move |_n, _p| *log.lock().unwrap() += 1));
    let script = Arc::new(Mutex::new(VecDeque::new()));
    let calls = Arc::new(AtomicU32::new(0));
    let driver = ReactLoopAgent::new(
        a.clone(),
        w.reg.clone(),
        deps(
            mock_assemble("sys"),
            mock_stream(script, calls),
            mock_tool_never(),
        ),
    );
    driver.followup(user_msg("m1", "hi")).unwrap();
    assert_eq!(*inserted.lock().unwrap(), 1);
}

#[test]
fn pre_step_payload_shows_claimed_messages_and_agent_fusion() {
    let w = TestWorld::new();
    let a = w.agent("a");
    let payloads = Arc::new(Mutex::new(Vec::new()));
    let log = payloads.clone();
    w.bus.on_chain(
        "agent/pre-step",
        true,
        None,
        Arc::new(move |payload: Value, next: NextFn| {
            log.lock().unwrap().push(payload.clone());
            next(payload)
        }),
    );
    let script = Arc::new(Mutex::new(VecDeque::from(vec![text_chunks(
        "hi",
        FinishReason::Stop,
    )])));
    let calls = Arc::new(AtomicU32::new(0));
    let driver = ReactLoopAgent::new(
        a.clone(),
        w.reg.clone(),
        deps(mock_assemble("sys"), mock_stream(script, calls), mock_tool_never()),
    );
    driver.followup(user_msg("m1", "hello")).unwrap();
    let log = payloads.lock().unwrap();
    let log: &Vec<Value> = &log;
    assert_eq!(log.len(), 1);
    let p = &log[0];
    assert_eq!(p["turn"], 1u64);
    assert_eq!(p["step"], 1u64);
    // agent 融合注入
    assert_eq!(p["agent"]["id"], "a");
    let msgs = p["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["id"], "m1");
}

// ---------------------------------------------------------------------------
// D-106 段 A：审批暂停 / 恢复（pending 工具调用机制）
// ---------------------------------------------------------------------------

fn demo_block(id: &str) -> ToolCallBlock {
    ToolCallBlock {
        id: CallId::from_raw(id),
        name: "demo".into(),
        arguments: "{}".into(),
    }
}

#[test]
fn approval_pending_pauses_turn_with_approval_pending_reason() {
    let w = TestWorld::new();
    let a = w.agent("a");
    let script = Arc::new(Mutex::new(VecDeque::from(vec![tool_call_chunks("c1")])));
    let calls = Arc::new(AtomicU32::new(0));
    let block = demo_block("c1");
    let tool: Arc<dyn Fn(&ToolExecCtx) -> ToolExecOutcome + Send + Sync> =
        Arc::new(move |_ctx: &ToolExecCtx| ToolExecOutcome {
            concluded: false,
            context: vec![],
            pending: vec![PendingCall {
                block: block.clone(),
                call_seq: 7,
            }],
        });
    let driver = ReactLoopAgent::new(
        a.clone(),
        w.reg.clone(),
        deps(mock_assemble("sys"), mock_stream(script, calls.clone()), tool),
    );
    driver.followup(user_msg("m1", "go")).unwrap();

    // 暂停：turn/end = approval-pending；不续发 LLM；Idle 停车；pending 留驻；无 result。
    assert_eq!(turn_end_reason(&a.session)["kind"], "approval-pending");
    assert_eq!(count_of(&a.session, EventKind::TurnEnd), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(driver.status(), AgentStatus::Idle);
    assert_eq!(driver.pending_calls().len(), 1);
    assert_eq!(driver.pending_calls()[0].block.id, CallId::from_raw("c1"));
    assert_eq!(count_of(&a.session, EventKind::ToolResult), 0);
}

#[test]
fn kick_resume_reruns_pending_then_continues() {
    let w = TestWorld::new();
    let a = w.agent("a");
    // 第 1 次 LLM → tool call c1；恢复后第 2 次 LLM → 纯文本收尾。
    let script = Arc::new(Mutex::new(VecDeque::from(vec![
        tool_call_chunks("c1"),
        text_chunks("done", FinishReason::Stop),
    ])));
    let calls = Arc::new(AtomicU32::new(0));
    let block = demo_block("c1");
    let invocations = Arc::new(AtomicU32::new(0));
    let resume_seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let inv = invocations.clone();
    let seen = resume_seen.clone();
    let tool: Arc<dyn Fn(&ToolExecCtx) -> ToolExecOutcome + Send + Sync> =
        Arc::new(move |ctx: &ToolExecCtx| {
        let i = inv.load(Ordering::SeqCst);
        inv.store(i + 1, Ordering::SeqCst);
        if i == 0 {
            ToolExecOutcome {
                concluded: false,
                context: vec![],
                pending: vec![PendingCall {
                    block: block.clone(),
                    call_seq: 7,
                }],
            }
        } else {
            // 恢复：必须收到 resume 集（绝不在正常路径断言的直观信号）。
            for p in &ctx.resume {
                seen.lock().unwrap().push(p.block.id.raw().to_string());
            }
            ToolExecOutcome {
                concluded: false,
                context: vec![],
                pending: Vec::new(),
            }
        }
    });
    let driver = ReactLoopAgent::new(
        a.clone(),
        w.reg.clone(),
        deps(mock_assemble("sys"), mock_stream(script, calls.clone()), tool),
    );
    driver.followup(user_msg("m1", "go")).unwrap();
    assert_eq!(turn_end_reason(&a.session)["kind"], "approval-pending");
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // fail-loud：无操作触发不了恢复（空踢不臆造入口）——先断言正常恢复路径。
    driver.kick_resume().unwrap();

    assert_eq!(invocations.load(Ordering::SeqCst), 2, "恢复必须重跑 tool_exec");
    assert_eq!(
        resume_seen.lock().unwrap().as_slice(),
        &["c1".to_string()]
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2, "恢复后模型续发请求");
    let ends: Vec<String> = a
        .session
        .events()
        .into_iter()
        .filter(|e| e.kind == EventKind::TurnEnd)
        .map(|e| e.data["reason"]["kind"].as_str().unwrap_or("?").to_string())
        .collect();
    assert_eq!(ends, vec!["approval-pending".to_string(), "completed".to_string()]);
    assert!(driver.pending_calls().is_empty(), "恢复后 pending 清空");
    assert_eq!(count_of(&a.session, EventKind::TurnStart), 2, "恢复是新 turn");
    assert_eq!(driver.status(), AgentStatus::Idle);
}

#[test]
fn kick_resume_fails_loud_without_pending() {
    let w = TestWorld::new();
    let a = w.agent("a");
    let driver = ReactLoopAgent::new(
        a.clone(),
        w.reg.clone(),
        deps(
            mock_assemble("sys"),
            mock_stream(Arc::new(Mutex::new(VecDeque::new())), Arc::new(AtomicU32::new(0))),
            mock_tool_never(),
        ),
    );
    assert!(driver.kick_resume().is_err(), "无待决审批 → fail loud");
}

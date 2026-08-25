//! M2f：审批接线闭环集成——真实 SystemPrompt + 注册表 + MockAdapter 里，
//! 硬守卫（ask）在无审批通道时把 tool/result 物化为逐字拒绝错误（工具体不执行）；
//! 审批通道 allowed-once 时工具实际运行。复用 m2e3_service 的 world/MockAdapter 模式。

#![allow(clippy::type_complexity)]
#![allow(clippy::result_large_err)]

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;

use dsh_agent::{Agent, AgentBus, AgentRegistry, AgentOptions, AgentStatus};
use dsh_agent_loop::create_loop_agent;
use dsh_llm::{
    CallId, ContentBlock, FinishReason, GenerateOptions, LlmAdapter, LlmFailure, LlmRuntime,
    MessageId, StreamChunk, ToolCallBlock,
};
use dsh_session::{
    store::SessionStore, CreateSessionMeta, CreateSessionOptions, EventKind, Session, SessionId,
};
use dsh_system_prompt::{Config, SystemPrompt};
use dsh_tools::{define_tool, ApprovalOutcome, DefineToolOptions, PreToolDecision, ToolExecution, ToolRegistry};
use serde_json::json;

// ---------------------------------------------------------------------------
// world（复制 m2e3_service 的最小可测子集）
// ---------------------------------------------------------------------------

fn user_msg(id: &str, text: &str) -> dsh_llm::Message {
    dsh_llm::Message::user(MessageId::from_raw(id), vec![ContentBlock::text(text)])
}

fn turn_end_reason(s: &Arc<Session>) -> serde_json::Value {
    s.events()
        .into_iter()
        .find(|e| e.kind == EventKind::TurnEnd)
        .map(|e| e.data.get("reason").cloned().unwrap_or(serde_json::Value::Null))
        .unwrap_or(serde_json::Value::Null)
}

fn count_of(s: &Arc<Session>, kind: EventKind) -> usize {
    s.events().into_iter().filter(|e| e.kind == kind).count()
}

struct MockAdapter {
    script: Rc<RefCell<VecDeque<Vec<StreamChunk>>>>,
    calls: Rc<Cell<u32>>,
}

impl MockAdapter {
    fn new(script: Rc<RefCell<VecDeque<Vec<StreamChunk>>>>, calls: Rc<Cell<u32>>) -> Self {
        MockAdapter { script, calls }
    }
}

impl LlmAdapter for MockAdapter {
    fn stream(&self, _options: GenerateOptions) -> Box<dyn Iterator<Item = StreamChunk>> {
        self.calls.set(self.calls.get() + 1);
        let next = self.script.borrow_mut().pop_front().unwrap_or_else(|| {
            vec![StreamChunk::Finish {
                reason: FinishReason::Error {
                    failure: LlmFailure {
                        message: "mock adapter script exhausted".to_string(),
                        code: "SCRIPT_EXHAUSTED".to_string(),
                        status: None,
                        provider_retry_after_ms: None,
                        request_id: None,
                    },
                },
                replay_state: None,
            }]
        });
        Box::new(next.into_iter())
    }
}

fn text_chunks(text: &str) -> Vec<StreamChunk> {
    vec![
        StreamChunk::BlockStart { index: 0, block_type: "text".parse().unwrap() },
        StreamChunk::TextDelta { index: 0, text: text.into() },
        StreamChunk::BlockEnd { index: 0, block: ContentBlock::text(text) },
        StreamChunk::Finish { reason: FinishReason::Stop, replay_state: None },
    ]
}

fn tool_call_chunks(arguments: &str) -> Vec<StreamChunk> {
    vec![
        StreamChunk::ToolCallDelta {
            index: 0,
            id: CallId::from_raw("c1"),
            name: Some("echo".into()),
            arguments_delta: arguments.into(),
        },
        StreamChunk::BlockEnd {
            index: 0,
            block: ContentBlock::ToolCall(ToolCallBlock {
                id: CallId::from_raw("c1"),
                name: "echo".into(),
                arguments: arguments.into(),
            }),
        },
        StreamChunk::Finish { reason: FinishReason::ToolCalls, replay_state: None },
    ]
}

fn stream(script: &[Vec<StreamChunk>]) -> (Rc<RefCell<VecDeque<Vec<StreamChunk>>>>, Rc<Cell<u32>>) {
    let q = Rc::new(RefCell::new(VecDeque::from_iter(script.iter().cloned())));
    let calls = Rc::new(Cell::new(0u32));
    (q, calls)
}

struct World {
    a: Rc<Agent>,
    driver: Rc<dsh_agent_loop::ReactLoopAgent>,
    ran: Rc<Cell<bool>>,
}

fn build(
    script: &[Vec<StreamChunk>],
    ask: bool,
    outcome: Option<ApprovalOutcome>,
) -> World {
    let (q, calls) = stream(script);
    let llm = Rc::new(LlmRuntime::new());
    llm.register_adapter(&["mock"], Rc::new(MockAdapter::new(q, calls))).unwrap();

    let tools = Rc::new(ToolRegistry::new(dsh_tools::ToolExecutionMode::Native));
    let ran = Rc::new(Cell::new(false));
    let ran2 = ran.clone();
    tools
        .register_global(Rc::new(
            define_tool(DefineToolOptions {
                name: "echo".into(),
                description: "echo the given text".into(),
                parameters: json!({ "text": { "type": "string", "required": true } }),
                output_schema: json!({ "type": "json" }),
                render: Rc::new(|_, v| vec![ContentBlock::text(serde_json::to_string(v).unwrap())]),
                execute: Rc::new(move |args, _| {
                    ran2.set(true);
                    Ok(args["text"].clone())
                }),
                is_concurrency_safe: Some(Rc::new(|_| true)),
                ..Default::default()
            })
            .unwrap(),
        ))
        .unwrap();
    if ask {
        tools
            .add_pre_decision(
                Rc::new(|_e: &ToolExecution| Some(PreToolDecision::Ask { reason: None })),
                None,
            )
            .unwrap();
    }
    if let Some(outcome) = outcome {
        tools.set_approval_provider(Some(Rc::new(move |_e: &ToolExecution, _r: Option<&str>| outcome)));
    }

    let prompt = Rc::new(SystemPrompt::new(&Config::default(), Rc::new(|| {})).unwrap());
    let bus = AgentBus::new();
    let reg = Rc::new(AgentRegistry::new(bus.clone()));
    let store = Arc::new(SessionStore::new());
    let s = store
        .create(
            Some(SessionId("a".into())),
            &CreateSessionOptions {
                seed: None,
                meta: Some(CreateSessionMeta { seed_length: Some(0), ..Default::default() }),
            },
        )
        .unwrap();
    let a = Rc::new(
        Agent::new(
            SessionId("a".into()),
            s,
            AgentOptions {
                provider: Some("mock".into()),
                model: Some("mock-model".into()),
                max_tokens: None,
            },
            bus.clone(),
            dsh_scope::ScopeKey::new(),
        )
        .unwrap(),
    );
    let driver = create_loop_agent(a.clone(), reg, prompt, llm, tools, 4);
    World { a, driver, ran }
}

fn tool_result_of(s: &Arc<Session>) -> serde_json::Value {
    s.events()
        .into_iter()
        .find(|e| e.kind == EventKind::ToolResult)
        .map(|e| e.data.clone())
        .unwrap_or(serde_json::Value::Null)
}

/// 从 tool/result 事件解析出 tool-result block 的 isError 与其首段文本。
struct ToolResultView {
    is_error: Option<bool>,
    text: String,
}

fn denial_of(data: &serde_json::Value) -> ToolResultView {
    let msg: dsh_llm::Message = serde_json::from_value(data["message"].clone()).unwrap();
    let dsh_llm::ContentBlock::ToolResult(block) = &msg.content[0] else {
        panic!("tool-result block expected; got {:?}", msg.content[0]);
    };
    let dsh_llm::ContentBlock::Text(t) = &block.content[0] else {
        panic!("text block expected inside tool-result");
    };
    ToolResultView { is_error: block.is_error, text: t.text().to_string() }
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[test]
fn hard_guard_denial_lands_in_tool_result_and_body_does_not_run() {
    let w = build(
        &[tool_call_chunks(r#"{"text":"hi"}"#), text_chunks("denied reply")],
        /* ask */ true,
        /* no approval channel */ None,
    );
    w.driver
        .followup(user_msg("m1", "Use the echo tool on 'hi' then answer."))
        .expect("loop must run");
    assert_eq!(count_of(&w.a.session, EventKind::TurnEnd), 1);
    assert_eq!(turn_end_reason(&w.a.session)["kind"], "completed");
    assert!(!w.ran.get(), "echo body must not run when approval is denied");

    // tool/result 携带逐字拒绝内容与 isError（守卫拒绝 = 普通 error 结果，无 failure
    // info——对齐 TS serviceAsk 的 `error: { message }` 且 error.info 缺省）
    let tr = tool_result_of(&w.a.session);
    assert!(tr["error"].is_null(), "guard denial carries no failure info block");
    let did_deny = denial_of(&tr);
    assert!(did_deny.is_error == Some(true));
    assert_eq!(did_deny.text, "Error: tool \"echo\" requires approval (not yet supported)");

    // 模型第二段能读到拒绝文本（answer 流触发）
    let assistants: Vec<_> = w
        .a
        .session
        .events()
        .into_iter()
        .filter(|e| e.kind == EventKind::AssistantMessage)
        .collect();
    assert_eq!(assistants.len(), 2);
    assert_eq!(
        assistants[1].data["message"]["content"][0]["text"].as_str().unwrap(),
        "denied reply"
    );
    assert_eq!(w.driver.status(), AgentStatus::Idle);
}

#[test]
fn approved_once_lets_tool_body_run() {
    let w = build(
        &[tool_call_chunks(r#"{"text":"hi"}"#), text_chunks("approved reply")],
        /* ask */ true,
        /* approval outcome */ Some(ApprovalOutcome::AllowedOnce),
    );
    w.driver
        .followup(user_msg("m1", "Use the echo tool on 'hi' then answer."))
        .expect("loop must run");
    assert_eq!(count_of(&w.a.session, EventKind::TurnEnd), 1);
    assert_eq!(turn_end_reason(&w.a.session)["kind"], "completed");
    assert!(w.ran.get(), "echo body must run after an allowed-once grant");

    // tool/result 非错误、携带 echo 渲染值，模型看到的是结果
    let tr = tool_result_of(&w.a.session);
    let did_run = denial_of(&tr);
    assert!(did_run.is_error == Some(false));
    assert_eq!(did_run.text, "\"hi\"");
    assert_eq!(w.driver.status(), AgentStatus::Idle);
}

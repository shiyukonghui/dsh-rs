//! M2g：AgentLoopHost 宿主装配测试——组合配置校验（逐字消息）、settings、
//! 配置身份 key、按身份装配 agent 驱动真实闭环、生命周期 teardown。

#![allow(clippy::type_complexity)]
#![allow(clippy::result_large_err)]

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use dsh_agent_loop::{
    AgentLoopConfig, AgentLoopHost, ConfiguredAgent, CONFIGURED_AGENT_IDENTITIES_KEY,
    validate_configured_agents,
};
use dsh_llm::{
    CallId, ContentBlock, FinishReason, GenerateOptions, LlmAdapter, LlmFailure, LlmRuntime,
    Message, MessageId, StreamChunk, ToolCallBlock,
};
use dsh_session::types::EventKind;
use dsh_tools::{define_tool, DefineToolOptions, ToolRegistry};
use serde_json::json;

// ---------------------------------------------------------------------------
// 最小 world（MockAdapter + echo 工具）
// ---------------------------------------------------------------------------

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

fn tool_call_chunks() -> Vec<StreamChunk> {
    vec![
        StreamChunk::ToolCallDelta {
            index: 0,
            id: CallId::from_raw("c1"),
            name: Some("echo".into()),
            arguments_delta: r#"{"text":"hi"}"#.into(),
        },
        StreamChunk::BlockEnd {
            index: 0,
            block: ContentBlock::ToolCall(ToolCallBlock {
                id: CallId::from_raw("c1"),
                name: "echo".into(),
                arguments: r#"{"text":"hi"}"#.into(),
            }),
        },
        StreamChunk::Finish { reason: FinishReason::ToolCalls, replay_state: None },
    ]
}

fn user_msg(text: &str) -> Message {
    Message::user(MessageId::from_raw("m1"), vec![ContentBlock::text(text)])
}

fn host_with(script: &[Vec<StreamChunk>]) -> (Rc<AgentLoopHost>, Rc<Cell<u32>>) {
    let llm = Rc::new(LlmRuntime::new());
    let (q, calls) = {
        let q = Rc::new(RefCell::new(VecDeque::from_iter(script.iter().cloned())));
        let calls = Rc::new(Cell::new(0u32));
        (q, calls)
    };
    llm.register_adapter(&["mock"], Rc::new(MockAdapter::new(q, calls.clone()))).unwrap();
    let tools = Rc::new(ToolRegistry::new(dsh_tools::ToolExecutionMode::Native));
    tools
        .register_global(Rc::new(
            define_tool(DefineToolOptions {
                name: "echo".into(),
                description: "echo".into(),
                parameters: json!({ "text": { "type": "string", "required": true } }),
                output_schema: json!({ "type": "json" }),
                render: Rc::new(|_, v| vec![ContentBlock::text(serde_json::to_string(v).unwrap())]),
                execute: Rc::new(|args, _| Ok(args["text"].clone())),
                is_concurrency_safe: Some(Rc::new(|_| true)),
                ..Default::default()
            })
            .unwrap(),
        ))
        .unwrap();
    let config = AgentLoopConfig {
        max_parallel_tool_calls: None,
        agents: vec![ConfiguredAgent {
            id: "a1".to_string(),
            provider: Some("mock".to_string()),
            model: Some("mock-model".to_string()),
            session_id: None,
            max_tokens: None,
            cwd: None,
            resume_session_id: None,
        }],
    };
    let host = AgentLoopHost::new(config, llm, tools).unwrap();
    (host, calls)
}

// ---------------------------------------------------------------------------
// 组合配置校验（逐字消息对齐 validateConfiguredAgents）
// ---------------------------------------------------------------------------

#[test]
fn duplicate_exact_identity_rejected_verbatim() {
    let err = validate_configured_agents(&[
        ConfiguredAgent { id: "a1".into(), session_id: Some("s1".into()), ..Default::default() },
        ConfiguredAgent { id: "a2".into(), session_id: Some("s1".into()), ..Default::default() },
    ])
    .unwrap_err();
    assert_eq!(err, r#"agents "a1" and "a2" use duplicate exact session identity "s1""#);
}

#[test]
fn session_id_and_resume_mutually_exclusive_verbatim() {
    let err = validate_configured_agents(&[ConfiguredAgent {
        id: "a1".into(),
        session_id: Some("s1".into()),
        resume_session_id: Some("r1".into()),
        ..Default::default()
    }])
    .unwrap_err();
    assert_eq!(err, r#"agent "a1": sessionId and resumeSessionId are mutually exclusive"#);
}

#[test]
fn distinct_exact_identities_pass() {
    validate_configured_agents(&[
        ConfiguredAgent { id: "a1".into(), session_id: Some("s1".into()), ..Default::default() },
        ConfiguredAgent { id: "a2".into(), session_id: Some("s2".into()), ..Default::default() },
    ])
    .unwrap();
    // resume 身份与另者 session 身份冲突也拒绝（同一精确身份不同来源）。
    let err = validate_configured_agents(&[
        ConfiguredAgent { id: "a1".into(), session_id: Some("s1".into()), ..Default::default() },
        ConfiguredAgent { id: "a2".into(), resume_session_id: Some("s1".into()), ..Default::default() },
    ])
    .unwrap_err();
    assert!(err.contains(r#"duplicate exact session identity "s1""#), "{err}");
}

#[test]
fn invalid_max_parallel_rejected_verbatim() {
    let cfg = AgentLoopConfig { max_parallel_tool_calls: Some(0), agents: vec![] };
    let err = cfg.validate().unwrap_err();
    assert_eq!(err, "maxParallelToolCalls must be a positive integer");
    // 缺省 → 常量默认（> 0）。
    assert!(AgentLoopConfig::default().validate().is_ok());
}

// ---------------------------------------------------------------------------
// 装配 + 驱动闭环
// ---------------------------------------------------------------------------

#[test]
fn host_drives_configured_agent_loop_to_completed() {
    let (host, calls) = host_with(&[tool_call_chunks(), text_chunks("Done: hi")]);
    let driver = host.ensure_agent(&host.config.agents[0]).unwrap();
    host.followup("a1", user_msg("Use the echo tool on 'hi' then answer.")).unwrap();

    // 会话（默认 id `agent-a1`）事件含完整 turn 流。
    let evs = host.events("agent-a1");
    assert!(
        evs.iter().any(|e| e.kind == EventKind::TurnEnd),
        "turn/end missing from host store"
    );
    assert!(evs.iter().any(|e| e.kind == EventKind::ToolCall));
    assert!(evs.iter().any(|e| e.kind == EventKind::ToolResult));
    let turn_end = evs.iter().find(|e| e.kind == EventKind::TurnEnd).unwrap();
    assert_eq!(turn_end.data["reason"]["kind"], "completed");

    // 两次流式请求（tool-call + 续答），驱动 idle。
    assert_eq!(calls.get(), 2);
    use dsh_agent::AgentStatus;
    assert_eq!(driver.status(), AgentStatus::Idle);
}

#[test]
fn ensure_agent_is_idempotent_and_uses_given_session_id() {
    let (host, _) = host_with(&[text_chunks("hello")]);
    let a1 = host.config.agents[0].clone();
    assert!(host.ensure_agent(&a1).is_ok());
    let first = host.ensure_agent(&a1).unwrap();
    let second = host.ensure_agent(&a1).unwrap();
    assert!(Rc::ptr_eq(&first, &second), "ensure is idempotent");
    // 显式 sessionId 生效（换一个配置 id，避免命中 a1 的幂等分支）。
    let mut a2 = a1.clone();
    a2.id = "a2".to_string();
    a2.session_id = Some("custom-s".to_string());
    assert!(host.ensure_agent(&a2).is_ok());
    let sid = dsh_session::types::SessionId::from_raw("custom-s".to_string());
    assert!(host.store.is_live(&sid), "configured session id used");
}

#[test]
fn unknown_agent_followup_fails_loud() {
    let (host, _) = host_with(&[text_chunks("x")]);
    let err = host.followup("ghost", user_msg("hi")).unwrap_err();
    assert!(err.contains("no configured agent \"ghost\""), "{err}");
}

#[test]
fn configured_identities_match_key_contract() {
    let (host, _) = host_with(&[text_chunks("x")]);
    let ids = host.configured_identities();
    assert_eq!(ids.len(), 1);
    assert_eq!(ids[0].id, "a1");
    assert_eq!(ids[0].session_id, None, "no explicit identity → deferred");
    // 显式身份呈现在 identity 上（CONFIGURED_AGENT_IDENTITIES_KEY 形态）。
    let key = CONFIGURED_AGENT_IDENTITIES_KEY;
    assert_eq!(key, "configuredAgentIdentities");
}

// ---------------------------------------------------------------------------
// 生命周期 teardown
// ---------------------------------------------------------------------------

#[test]
fn teardown_disposes_registered_disposers_and_clears_agents() {
    let (host, _) = host_with(&[text_chunks("x")]);
    host.ensure_agent(&host.config.agents[0]).unwrap();
    let disposed = Rc::new(Cell::new(false));
    let disposed2 = disposed.clone();
    host.add_disposer(Rc::new(move || disposed2.set(true)));

    assert!(host.agent("a1").is_some());
    host.teardown();
    assert!(host.agent("a1").is_none(), "teardown clears configured agents");
    assert!(disposed.get(), "teardown runs registered disposers");
    // teardown 后 followup fail loud。
    let err = host.followup("a1", user_msg("hi")).unwrap_err();
    assert!(err.contains("no configured agent \"a1\""), "{err}");
}

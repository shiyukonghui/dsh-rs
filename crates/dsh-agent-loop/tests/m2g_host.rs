//! M2g：AgentLoopHost 宿主装配测试——组合配置校验（逐字消息）、settings、
//! 配置身份 key、按身份装配 agent 驱动真实闭环、生命周期 teardown。

#![allow(clippy::type_complexity)]
#![allow(clippy::result_large_err)]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

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
    script: Arc<Mutex<VecDeque<Vec<StreamChunk>>>>,
    calls: Arc<AtomicU32>,
}

impl MockAdapter {
    fn new(script: Arc<Mutex<VecDeque<Vec<StreamChunk>>>>, calls: Arc<AtomicU32>) -> Self {
        MockAdapter { script, calls }
    }
}

impl LlmAdapter for MockAdapter {
    fn stream(&self, _options: GenerateOptions) -> Box<dyn Iterator<Item = StreamChunk>> {
        self.calls.store(self.calls.load(Ordering::SeqCst) + 1, Ordering::SeqCst);
        let next = self.script.lock().unwrap().pop_front().unwrap_or_else(|| {
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

fn host_with(script: &[Vec<StreamChunk>]) -> (Arc<AgentLoopHost>, Arc<AtomicU32>) {
    let llm = Arc::new(LlmRuntime::new());
    let (q, calls) = {
        let q = Arc::new(Mutex::new(VecDeque::from_iter(script.iter().cloned())));
        let calls = Arc::new(AtomicU32::new(0));
        (q, calls)
    };
    llm.register_adapter(&["mock"], Arc::new(MockAdapter::new(q, calls.clone()))).unwrap();
    let tools = Arc::new(ToolRegistry::new(dsh_tools::ToolExecutionMode::Native));
    tools
        .register_global(Arc::new(
            define_tool(DefineToolOptions {
                name: "echo".into(),
                description: "echo".into(),
                parameters: json!({ "text": { "type": "string", "required": true } }),
                output_schema: json!({ "type": "json" }),
                render: Arc::new(|_, v| vec![ContentBlock::text(serde_json::to_string(v).unwrap())]),
                execute: Arc::new(|args, _| Ok(args["text"].clone())),
                is_concurrency_safe: Some(Arc::new(|_| true)),
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
    assert_eq!(calls.load(Ordering::SeqCst), 2);
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
    assert!(Arc::ptr_eq(&first, &second), "ensure is idempotent");
    // 显式 sessionId 生效（换一个配置 id，避免命中 a1 的幂等分支）。
    let mut a2 = a1.clone();
    a2.id = "a2".to_string();
    a2.session_id = Some("custom-s".to_string());
    assert!(host.ensure_agent(&a2).is_ok());
    let sid = dsh_session::types::SessionId::from_raw("custom-s".to_string());
    assert!(host.store.is_live(&sid), "configured session id used");
}

// ---------------------------------------------------------------------------
// D-101：运行时 per-session agent 注册（session.create/fork 挂接真实 agent）
// ---------------------------------------------------------------------------

#[test]
fn configured_for_session_matches_static_config_and_convention() {
    let (host, _) = host_with(&[text_chunks("hello")]);
    // a1（无显式身份）按约定身份 `agent-a1` 命中。
    let via_convention = host
        .configured_for_session("agent-a1")
        .expect("convention identity resolves");
    assert_eq!(via_convention.id, "a1");
    // 无显式 sessionId 的配置 agent 不命中任意精确会话。
    assert!(host.configured_for_session("some-other").is_none());
}

#[test]
fn register_session_agent_is_routable_idempotent_and_drives_turn() {
    let (host, _) = host_with(&[text_chunks("hello")]);
    let cfg = ConfiguredAgent {
        id: "session-s9".into(),
        provider: Some("mock".into()),
        model: Some("mock-model".into()),
        session_id: Some("s9".into()),
        max_tokens: None,
        cwd: Some(r"C:\work".into()),
        resume_session_id: None,
    };
    let first = host.register_session_agent(cfg.clone()).unwrap();
    // 可被 `configured_for_session` 解析（run_rust_loop 的路由查询路径）；cwd 保留。
    let resolved = host
        .configured_for_session("s9")
        .expect("runtime agent visible to routing");
    assert_eq!(resolved.id, "session-s9");
    assert_eq!(resolved.cwd.as_deref(), Some(r"C:\work"));
    // 幂等：重复注册 → 同一装配实例，不重复登记。
    let second = host.register_session_agent(cfg).unwrap();
    assert!(Arc::ptr_eq(&first, &second), "register_session_agent is idempotent");
    // followup 经 agent id 驱动真实 turn；事件落共享 store（会话键 = sessionId）。
    host.followup("session-s9", user_msg("hi")).unwrap();
    let evs = host.events("s9");
    assert!(evs.iter().any(|e| e.kind == EventKind::TurnEnd), "turn/end under session key");
}

#[test]
fn register_session_agent_reuses_existing_agent_for_reserved_session() {
    let (host, _) = host_with(&[text_chunks("x")]);
    // `agent-a1` 是静态 a1 的约定身份 → 复用 a1，不许新配置重复登记。
    let cfg = ConfiguredAgent {
        id: "dup".into(),
        session_id: Some("agent-a1".into()),
        ..Default::default()
    };
    let agent = host.register_session_agent(cfg).unwrap();
    assert!(
        Arc::ptr_eq(&agent, &host.agent("a1").unwrap()),
        "reserved session reuses existing agent"
    );
    assert!(host.configured_for_session("agent-a1").is_some());
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
    let disposed = Arc::new(AtomicBool::new(false));
    let disposed2 = disposed.clone();
    host.add_disposer(Arc::new(move || disposed2.store(true, Ordering::SeqCst)));

    assert!(host.agent("a1").is_some());
    host.teardown();
    assert!(host.agent("a1").is_none(), "teardown clears configured agents");
    assert!(disposed.load(Ordering::SeqCst), "teardown runs registered disposers");
    // teardown 后 followup fail loud。
    let err = host.followup("a1", user_msg("hi")).unwrap_err();
    assert!(err.contains("no configured agent \"a1\""), "{err}");
}

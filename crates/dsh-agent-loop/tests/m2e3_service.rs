//! M2e-3：AgentLoop 服务装配集成测试——真实 SystemPrompt + 注册表实际工具 + MockAdapter
//! 接入 LlmRuntime，跑通「用户 → 模型 tool-call → 真实调度 → tool/result → 续答」闭环，
//! 并验证 preparedCall.stream 优先选择、invariant 守卫与 runtime-context 投影接线。

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
use dsh_tools::{define_tool, DefineToolOptions, ToolRegistry};
use serde_json::json;

// ---------------------------------------------------------------------------
// helpers
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

// ---------------------------------------------------------------------------
// MockAdapter（对齐 TS mock-adapter.ts：脚本驱动 + requests 计数）
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

// ---------------------------------------------------------------------------
// chunk builders
// ---------------------------------------------------------------------------

fn text_chunks(text: &str) -> Vec<StreamChunk> {
    vec![
        StreamChunk::BlockStart { index: 0, block_type: "text".parse().unwrap() },
        StreamChunk::TextDelta { index: 0, text: text.into() },
        StreamChunk::BlockEnd { index: 0, block: ContentBlock::text(text) },
        StreamChunk::Finish { reason: FinishReason::Stop, replay_state: None },
    ]
}

fn tool_call_chunks(id: &str, name: &str, arguments: &str) -> Vec<StreamChunk> {
    vec![
        StreamChunk::ToolCallDelta {
            index: 0,
            id: CallId::from_raw(id),
            name: Some(name.into()),
            arguments_delta: arguments.into(),
        },
        StreamChunk::BlockEnd {
            index: 0,
            block: ContentBlock::ToolCall(ToolCallBlock {
                id: CallId::from_raw(id),
                name: name.into(),
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

// ---------------------------------------------------------------------------
// world
// ---------------------------------------------------------------------------

fn store() -> Arc<SessionStore> {
    Arc::new(SessionStore::new())
}

fn agent(store: &Arc<SessionStore>, id: &str, bus: &AgentBus) -> Rc<Agent> {
    let s = store
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
        .unwrap();
    Rc::new(
        Agent::new(
            SessionId(id.to_string()),
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
    )
}

fn echo_tool() -> Rc<dsh_tools::ToolDefinition> {
    Rc::new(
        define_tool(DefineToolOptions {
            name: "echo".into(),
            description: "echo the given text".into(),
            parameters: json!({
                "text": { "type": "string", "required": true },
            }),
            output_schema: json!({ "type": "json" }),
            render: Rc::new(|_, value| vec![ContentBlock::text(serde_json::to_string(value).unwrap())]),
            execute: Rc::new(|args, _| Ok(args["text"].clone())),
            is_concurrency_safe: Some(Rc::new(|_| true)),
            ..Default::default()
        })
        .unwrap(),
    )
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[test]
fn real_loop_tool_call_then_answer_closes_turn() {
    let (q, calls) = stream(&[
        tool_call_chunks("c1", "echo", r#"{"text":"hi"}"#),
        text_chunks("Done: hi"),
    ]);
    let llm = Rc::new(LlmRuntime::new());
    llm.register_adapter(&["mock"], Rc::new(MockAdapter::new(q, calls.clone())))
        .unwrap();

    let tools = Rc::new(ToolRegistry::new(dsh_tools::ToolExecutionMode::Native));
    tools.register_global(echo_tool()).unwrap();

    let prompt = Rc::new(SystemPrompt::new(&Config::default(), Rc::new(|| {})).unwrap());

    let bus = AgentBus::new();
    let reg = Rc::new(AgentRegistry::new(bus.clone()));
    let a = agent(&store(), "a", &bus);
    let driver = create_loop_agent(a.clone(), reg, prompt, llm, tools, 8);

    driver.followup(user_msg("m1", "Use the echo tool on 'hi' then answer.")).expect("loop must run");
    assert_eq!(count_of(&a.session, EventKind::TurnEnd), 1);
    assert_eq!(turn_end_reason(&a.session)["kind"], "completed");
    assert_eq!(calls.get(), 2);

    // 会话事件链（过滤 chunk/上下文噪声事件，只保留规范事件）：turn/start → step/start →
    // [step 内 fused 认领的 user] → request/header → assistant(tool-call) → tool/call →
    // tool/result → step/end → step/start → assistant(text) → step/end → turn/end
    let kinds: Vec<EventKind> = a
        .session
        .events()
        .into_iter()
        .map(|e| e.kind)
        .filter(|k| {
            matches!(
                k,
                EventKind::UserMessage
                    | EventKind::TurnStart
                    | EventKind::TurnEnd
                    | EventKind::RequestHeader
                    | EventKind::StepStart
                    | EventKind::StepEnd
                    | EventKind::AssistantMessage
                    | EventKind::ToolCall
                    | EventKind::ToolResult
            )
        })
        .collect();
    let want: Vec<EventKind> = vec![
        EventKind::TurnStart,
        EventKind::StepStart,
        EventKind::UserMessage,
        EventKind::RequestHeader,
        EventKind::AssistantMessage,
        EventKind::ToolCall,
        EventKind::ToolResult,
        EventKind::StepEnd,
        EventKind::StepStart,
        EventKind::AssistantMessage,
        EventKind::StepEnd,
        EventKind::TurnEnd,
    ];
    assert_eq!(kinds, want);

    // 最终助手消息文本
    let assistants: Vec<_> = a
        .session
        .events()
        .into_iter()
        .filter(|e| e.kind == EventKind::AssistantMessage)
        .collect();
    assert_eq!(assistants.len(), 2);
    let second_text = assistants[1].data["message"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(second_text, "Done: hi");

    // tool/result 链接到 tool/call
    let tc = a
        .session
        .events()
        .into_iter()
        .find(|e| e.kind == EventKind::ToolCall)
        .unwrap();
    let tr = a
        .session
        .events()
        .into_iter()
        .find(|e| e.kind == EventKind::ToolResult)
        .unwrap();
    assert_eq!(tr.source_event_seqs(), Some(&vec![tc.seq]));

    // 无 runtime-context 投影（无动态上下文节 → retained never + current 空）
    for e in a.session.events().into_iter() {
        if e.kind == EventKind::UserMessage {
            let data = e.data["source"]["kind"].as_str().unwrap_or("");
            assert_ne!(data, "plugin", "unexpected runtime-context projection");
        }
    }
    // 驱动回到 idle
    assert_eq!(driver.status(), AgentStatus::Idle);
}

#[test]
fn real_loop_direct_answer_without_tools() {
    let (q, calls) = stream(&[text_chunks("hello from mock")]);
    let llm = Rc::new(LlmRuntime::new());
    llm.register_adapter(&["mock"], Rc::new(MockAdapter::new(q, calls.clone())))
        .unwrap();
    let tools = Rc::new(ToolRegistry::new(dsh_tools::ToolExecutionMode::Native));
    let prompt = Rc::new(SystemPrompt::new(&Config::default(), Rc::new(|| {})).unwrap());
    let bus = AgentBus::new();
    let reg = Rc::new(AgentRegistry::new(bus.clone()));
    let a = agent(&store(), "b", &bus);
    let driver = create_loop_agent(a.clone(), reg, prompt, llm, tools, 8);

    driver.followup(user_msg("m1", "say hello")).expect("loop must run");
    assert_eq!(count_of(&a.session, EventKind::TurnEnd), 1);
    assert_eq!(turn_end_reason(&a.session)["kind"], "completed");
    assert_eq!(calls.get(), 1);
    assert_eq!(count_of(&a.session, EventKind::ToolCall), 0, "no tool calls when the model answers directly");
}

#[test]
fn real_loop_exhausted_script_emits_turn_error() {
    // 脚本耗尽 → Finish Error → turn/end 以失败终止（不发 assistant/message）
    let (q, calls) = stream(&[]);
    let llm = Rc::new(LlmRuntime::new());
    llm.register_adapter(&["mock"], Rc::new(MockAdapter::new(q, calls.clone())))
        .unwrap();
    let tools = Rc::new(ToolRegistry::new(dsh_tools::ToolExecutionMode::Native));
    let prompt = Rc::new(SystemPrompt::new(&Config::default(), Rc::new(|| {})).unwrap());
    let bus = AgentBus::new();
    let reg = Rc::new(AgentRegistry::new(bus.clone()));
    let a = agent(&store(), "c", &bus);
    let driver = create_loop_agent(a.clone(), reg, prompt, llm, tools, 8);

    driver.followup(user_msg("m1", "boom")).expect("turn must terminate (error is a terminal outcome)");
    assert_eq!(count_of(&a.session, EventKind::TurnEnd), 1, "turn ends even on failure");
    assert_ne!(turn_end_reason(&a.session)["kind"], "completed");
    // 请求按默认 normal 重试策略有界重试；至少一次请求被发送
    assert!(calls.get() >= 1);
}

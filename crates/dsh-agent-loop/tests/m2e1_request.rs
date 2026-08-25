//! M2e-1：dsh-agent-loop 请求重建层测试
//! （requestProposal / buildRequest header-锚点 / request-reconstruction invariant /
//!  settings 严格验证）。

// LlmError 有意携带完整结构化失败（宽 Err）；default_build 镜像 8 参 buildRequest。
#![allow(clippy::result_large_err)]
#![allow(clippy::too_many_arguments)]

use std::cell::RefCell;
use std::sync::Arc;

use dsh_agent::AgentOptions;
use dsh_agent_loop::{
    build_request, check_loop_request, request_proposal, validate_max_parallel_tool_calls,
    validate_max_tokens, AgentLoopRequest, AgentLoopSettings, BuiltRequest,
};
use dsh_llm::call_config::CallConfigAdapterDefaults;
use dsh_llm::retry::{ResolvedAlwaysRetryPolicy, ResolvedRetryBackoff, ResolvedRetryPolicy};
use dsh_llm::{
    CallConfig, ContentBlock, GenerateOptions, LlmError, LlmModelContext, Message, MessageId,
    PreparedLlmCall, ReasoningEffortId, ToolSchema,
};
use dsh_session::{
    store::SessionStore, CreateSessionMeta, CreateSessionOptions, EventKind, Session, SessionId,
    SurfaceIntent, SurfaceOp,
};
use serde_json::Value;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn store() -> Arc<SessionStore> {
    Arc::new(SessionStore::new())
}

fn session_new(store: &Arc<SessionStore>, id: &str) -> Arc<Session> {
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

fn step_start(s: &Arc<Session>, turn: u64, step: u64) {
    s.append(
        EventKind::StepStart,
        serde_json::json!({ "turn": turn, "step": step }),
        None,
    )
    .unwrap();
}

fn append_user(s: &Arc<Session>, id: &str, text: &str) {
    let msg = Message::user(MessageId(id.to_string()), vec![ContentBlock::text(text)]);
    // user/message 事件 data = Message 本体（derive_event_message 直接 from_value(data)）
    s.append(
        EventKind::UserMessage,
        serde_json::to_value(&msg).unwrap(),
        Some(&SurfaceIntent {
            surface_op: SurfaceOp::Append,
            source_event_seqs: None,
        }),
    )
    .unwrap();
}

fn options(provider: &str, model: &str, max_tokens: Option<u64>) -> AgentOptions {
    AgentOptions {
        provider: Some(provider.to_string()),
        model: Some(model.to_string()),
        max_tokens,
    }
}

/// 手动裸构造 GenerateOptions（无 Default）。
fn go(
    provider: &str,
    model: &str,
    messages: Vec<Message>,
    session_id: Option<SessionId>,
) -> GenerateOptions {
    GenerateOptions {
        provider: provider.into(),
        model: model.into(),
        reasoning_effort: None,
        messages,
        system: None,
        tools: None,
        temperature: None,
        max_tokens: None,
        stop: None,
        session_id,
        purpose: None,
    }
}

fn ident_propose(c: CallConfig, _turn: u64, _step: u64) -> Result<CallConfig, String> {
    Ok(c)
}

fn no_adapter_prepare(_c: CallConfig) -> Result<PreparedLlmCall, LlmError> {
    Err(LlmError::new("no adapter", "NO_ADAPTER"))
}

fn fake_prepared(
    config: CallConfig,
    adapter_defaults: CallConfigAdapterDefaults,
    context_window: Option<u64>,
) -> PreparedLlmCall {
    PreparedLlmCall {
        config,
        retry_policy: ResolvedRetryPolicy::Always(ResolvedAlwaysRetryPolicy {
            backoff: ResolvedRetryBackoff {
                initial_delay_ms: 1,
                max_delay_ms: 1,
                jitter_ratio: 0.0,
            },
        }),
        adapter_defaults,
        context: context_window.map(|w| LlmModelContext { context_window: w }),
        stream: None,
    }
}

fn base_prepared(provider: &str, model: &str) -> impl Fn(CallConfig) -> Result<PreparedLlmCall, LlmError> {
    let provider = provider.to_string();
    let model = model.to_string();
    move |_c: CallConfig| {
        Ok(fake_prepared(
            CallConfig {
                provider: provider.clone(),
                model: model.clone(),
                ..Default::default()
            },
            CallConfigAdapterDefaults::default(),
            None,
        ))
    }
}

fn default_build(
    s: &Arc<Session>,
    opts: &AgentOptions,
    logged: bool,
    tools: &[ToolSchema],
    system: &str,
    boundary: Vec<Message>,
    propose: &dyn Fn(CallConfig, u64, u64) -> Result<CallConfig, String>,
    prepare: &dyn Fn(CallConfig) -> Result<PreparedLlmCall, LlmError>,
) -> Result<BuiltRequest, String> {
    build_request(
        s, opts, logged, tools, system, boundary, 1u64, 1u64, propose, prepare,
    )
}

fn request_headers(s: &Arc<Session>) -> Vec<(Value, String)> {
    s.events()
        .iter()
        .filter(|e| e.kind == EventKind::RequestHeader)
        .map(|e| {
            let reason = e.data["reason"]
                .as_str()
                .unwrap_or("(missing)")
                .to_string();
            (e.data["header"].clone(), reason)
        })
        .collect()
}

fn tool(name: &str) -> ToolSchema {
    ToolSchema {
        name: name.into(),
        description: String::new(),
        parameters: serde_json::json!({}),
    }
}

// ---------------------------------------------------------------------------
// settings
// ---------------------------------------------------------------------------

#[test]
fn settings_validation_messages() {
    assert_eq!(AgentLoopSettings::default().max_parallel_tool_calls, 10);
    let err = validate_max_parallel_tool_calls(0).err().unwrap();
    assert_eq!(err, "maxParallelToolCalls must be a positive integer");
    assert!(validate_max_parallel_tool_calls(1).is_ok());
    assert!(validate_max_parallel_tool_calls(10).is_ok());
    let err = validate_max_tokens(Some(0)).err().unwrap();
    assert_eq!(err, "agent maxTokens must be a positive safe integer");
    assert!(validate_max_tokens(Some(5)).is_ok());
    assert!(validate_max_tokens(None).is_ok());
}

// ---------------------------------------------------------------------------
// requestProposal
// ---------------------------------------------------------------------------

#[test]
fn request_proposal_strips_adapter_filled_dimensions_only() {
    use dsh_session::EpochHeader;
    let base = CallConfig {
        provider: "p".into(),
        model: "m".into(),
        reasoning_effort: Some(ReasoningEffortId::from_raw("high")),
        max_tokens: Some(4096),
        ..Default::default()
    };
    // 两个字段都被 adapter 填充 → 提案两者皆剥
    let h = EpochHeader {
        config: base.clone(),
        adapter_defaults: Some(CallConfigAdapterDefaults {
            reasoning_effort: Some(true),
            max_tokens: Some(true),
        }),
        system: None,
        tools: None,
    };
    let p = request_proposal(&h);
    assert_eq!(p.reasoning_effort, None);
    assert_eq!(p.max_tokens, None);
    // adapterDefaults 缺省 → 原样（显式 effort/maxTokens 保留）
    let h2 = EpochHeader {
        config: base.clone(),
        adapter_defaults: None,
        system: None,
        tools: None,
    };
    let p2 = request_proposal(&h2);
    assert!(p2.reasoning_effort.is_some());
    assert_eq!(p2.max_tokens, Some(4096));
    // 只标记一个维度 → 只剥那个
    let h3 = EpochHeader {
        config: base,
        adapter_defaults: Some(CallConfigAdapterDefaults {
            reasoning_effort: None,
            max_tokens: Some(true),
        }),
        system: None,
        tools: None,
    };
    let p3 = request_proposal(&h3);
    assert!(p3.reasoning_effort.is_some());
    assert_eq!(p3.max_tokens, None);
}

// ---------------------------------------------------------------------------
// buildRequest
// ---------------------------------------------------------------------------

#[test]
fn build_request_initial_header_and_marker() {
    let st = store();
    let s = session_new(&st, "b1");
    step_start(&s, 1, 1);
    append_user(&s, "m1", "hello");
    let boundary = s.derive_messages().unwrap();
    let tools = vec![tool("read")];
    let prepare_ctx8192 = |_c: CallConfig| -> Result<PreparedLlmCall, LlmError> {
        Ok(fake_prepared(
            CallConfig {
                provider: "litellm".into(),
                model: "deepseek-r1".into(),
                ..Default::default()
            },
            CallConfigAdapterDefaults::default(),
            Some(8192),
        ))
    };
    let built = default_build(
        &s,
        &options("litellm", "deepseek-r1", None),
        false,
        &tools,
        "You are helpful.",
        boundary.clone(),
        &ident_propose,
        &prepare_ctx8192,
    )
    .unwrap();

    // 标记 + session id + 派生消息
    assert_eq!(
        built.request.options().session_id.as_ref(),
        Some(&SessionId("b1".into()))
    );
    assert_eq!(built.request.options().messages, boundary);
    // system/tools 非空 → 请求与 header 均携带
    assert_eq!(
        built.request.options().system.as_deref(),
        Some("You are helpful.")
    );
    assert_eq!(built.request.options().tools.as_deref(), Some(tools.as_slice()));
    assert_eq!(built.header.system.as_deref(), Some("You are helpful."));
    // 首次 header：reason 'initial'
    let headers = request_headers(&s);
    assert_eq!(headers.len(), 1);
    assert_eq!(headers[0].1, "initial");
    assert!(built.request_header_logged);
    // request_context 记录（contextWindow 8192）
    let ctx = s
        .events()
        .into_iter()
        .find(|e| e.kind == EventKind::RequestContext);
    assert!(ctx.is_some());
    let ctx = ctx.unwrap();
    assert_eq!(ctx.data["contextWindow"], serde_json::json!(8192));
}

#[test]
fn build_request_stable_prompt_logs_single_header() {
    let st = store();
    let s = session_new(&st, "b2");
    step_start(&s, 1, 1);
    append_user(&s, "m1", "hi");
    let boundary = s.derive_messages().unwrap();
    let prepare = base_prepared("litellm", "m");
    // 第一次（logged=false）
    let b1 = default_build(
        &s,
        &options("litellm", "m", None),
        false,
        &[],
        "system",
        boundary.clone(),
        &ident_propose,
        &prepare,
    )
    .unwrap();
    assert!(b1.request_header_logged);
    // 第二次（logged=true），描述无变化 → 不新增 header
    default_build(
        &s,
        &options("litellm", "m", None),
        b1.request_header_logged,
        &[],
        "system",
        boundary,
        &ident_propose,
        &prepare,
    )
    .unwrap();
    assert_eq!(request_headers(&s).len(), 1);
}

#[test]
fn build_request_changed_system_logs_change_header() {
    let st = store();
    let s = session_new(&st, "b3");
    step_start(&s, 1, 1);
    append_user(&s, "m1", "hi");
    let boundary = s.derive_messages().unwrap();
    let prepare = base_prepared("litellm", "m");
    let b1 = default_build(
        &s,
        &options("litellm", "m", None),
        false,
        &[],
        "sys-A",
        boundary.clone(),
        &ident_propose,
        &prepare,
    )
    .unwrap();
    // prompt 变了 → reason 'change'
    default_build(
        &s,
        &options("litellm", "m", None),
        b1.request_header_logged,
        &[],
        "sys-B",
        boundary,
        &ident_propose,
        &prepare,
    )
    .unwrap();
    let headers = request_headers(&s);
    assert_eq!(headers.len(), 2);
    assert_eq!(headers[0].1, "initial");
    assert_eq!(headers[1].1, "change");
}

#[test]
fn build_request_empty_system_tools_omitted() {
    let st = store();
    let s = session_new(&st, "b4");
    step_start(&s, 1, 1);
    append_user(&s, "m1", "hi");
    let boundary = s.derive_messages().unwrap();
    let built = default_build(
        &s,
        &options("litellm", "m", None),
        false,
        &[],
        "",
        boundary,
        &ident_propose,
        &base_prepared("litellm", "m"),
    )
    .unwrap();
    assert!(built.request.options().system.is_none());
    assert!(built.request.options().tools.is_none());
    assert!(built.header.system.is_none());
    assert!(built.header.tools.is_none());
    // 空 system/tools → 请求字段不写（report §7.10）
    let req = serde_json::to_value(built.request.options()).unwrap();
    assert!(req.get("system").is_none());
    assert!(req.get("tools").is_none());
}

#[test]
fn build_request_proposal_strips_filled_dimensions_next_step() {
    let st = store();
    let s = session_new(&st, "b5");
    step_start(&s, 1, 1);
    append_user(&s, "m1", "hi");
    let boundary = s.derive_messages().unwrap();
    let filled_prepare = |_c: CallConfig| -> Result<PreparedLlmCall, LlmError> {
        Ok(fake_prepared(
            CallConfig {
                provider: "litellm".into(),
                model: "m".into(),
                reasoning_effort: Some(ReasoningEffortId::from_raw("high")),
                max_tokens: Some(256),
                ..Default::default()
            },
            CallConfigAdapterDefaults {
                reasoning_effort: Some(true),
                max_tokens: Some(true),
            },
            None,
        ))
    };
    // 第一次：adapter 注入 reasoningEffort + maxTokens，并标记已填充
    let b1 = default_build(
        &s,
        &options("litellm", "m", Some(300)),
        false,
        &[],
        "sys",
        boundary.clone(),
        &ident_propose,
        &filled_prepare,
    )
    .unwrap();
    // 第二次：seed = requestProposal(persistedHeader) → 两维度剥除
    let last = RefCell::new(None::<CallConfig>);
    let propose = |c: CallConfig, _t: u64, _s: u64| -> Result<CallConfig, String> {
        *last.borrow_mut() = Some(c.clone());
        Ok(c)
    };
    default_build(
        &s,
        &options("litellm", "m", Some(300)),
        b1.request_header_logged,
        &[],
        "sys",
        boundary,
        &propose,
        &base_prepared("litellm", "m"),
    )
    .unwrap();
    let seed = last.borrow().clone().unwrap();
    assert_eq!(seed.reasoning_effort, None);
    assert_eq!(seed.max_tokens, None);
    // provider/model 仍保留
    assert_eq!(seed.provider, "litellm");
    assert_eq!(seed.model, "m");
}

#[test]
fn build_request_restores_explicit_effort_on_same_route() {
    let st = store();
    let s = session_new(&st, "b6");
    step_start(&s, 1, 1);
    append_user(&s, "m1", "hi");
    let boundary = s.derive_messages().unwrap();
    let low_prepare = |_c: CallConfig| -> Result<PreparedLlmCall, LlmError> {
        Ok(fake_prepared(
            CallConfig {
                provider: "litellm".into(),
                model: "m".into(),
                reasoning_effort: Some(ReasoningEffortId::from_raw("low")),
                ..Default::default()
            },
            CallConfigAdapterDefaults::default(),
            None,
        ))
    };
    // 第一次：持久 explicit effort（options 无 maxTokens），adapter 未填充 effort
    let b1 = default_build(
        &s,
        &options("litellm", "m", None),
        false,
        &[],
        "sys",
        boundary.clone(),
        &ident_propose,
        &low_prepare,
    )
    .unwrap();
    let last = RefCell::new(None::<CallConfig>);
    let propose = |c: CallConfig, _t: u64, _s: u64| -> Result<CallConfig, String> {
        *last.borrow_mut() = Some(c.clone());
        Ok(c)
    };
    default_build(
        &s,
        &options("litellm", "m", None),
        b1.request_header_logged,
        &[],
        "sys",
        boundary,
        &propose,
        &low_prepare,
    )
    .unwrap();
    // provider+model 同、adapter 未填充 effort → 显式 effort 恢复进 seed
    let seed = last.borrow().clone().unwrap();
    assert_eq!(seed.reasoning_effort.as_ref().map(|r| r.raw()), Some("low"));
}

#[test]
fn build_request_missing_provider_model_rejects() {
    let st = store();
    let s = session_new(&st, "b7");
    step_start(&s, 1, 1);
    append_user(&s, "m1", "hi");
    let boundary = s.derive_messages().unwrap();
    // 路由为空 + 水岭不补 → 逐字错
    let err = default_build(
        &s,
        &options("", "", None),
        false,
        &[],
        "",
        boundary,
        &ident_propose,
        &no_adapter_prepare,
    )
    .err()
    .unwrap();
    assert_eq!(
        err,
        "agent \"b7\" has no provider/model: set AgentOptions.provider and AgentOptions.model or supply both via the agent/request waterfall"
    );
}

#[test]
fn build_request_no_adapter_passthrough() {
    let st = store();
    let s = session_new(&st, "b8");
    step_start(&s, 1, 1);
    append_user(&s, "m1", "hi");
    let boundary = s.derive_messages().unwrap();
    // prepare_call 抛 NO_ADAPTER → config 透传（llm/stream 插件可短路未注册路由）
    let built = default_build(
        &s,
        &options("litellm", "m", None),
        false,
        &[],
        "",
        boundary,
        &ident_propose,
        &no_adapter_prepare,
    )
    .unwrap();
    assert!(built.prepared_call.is_none());
    assert_eq!(built.request.options().provider, "litellm");
    assert_eq!(built.request.options().model, "m");
}

#[test]
fn build_request_context_logged_only_on_change() {
    let st = store();
    let s = session_new(&st, "b9");
    step_start(&s, 1, 1);
    append_user(&s, "m1", "hi");
    let boundary = s.derive_messages().unwrap();
    let prepare_8192 = |_c: CallConfig| -> Result<PreparedLlmCall, LlmError> {
        Ok(fake_prepared(
            CallConfig {
                provider: "litellm".into(),
                model: "m".into(),
                ..Default::default()
            },
            CallConfigAdapterDefaults::default(),
            Some(8192),
        ))
    };
    let prepare_none = |_c: CallConfig| -> Result<PreparedLlmCall, LlmError> {
        Ok(fake_prepared(
            CallConfig {
                provider: "litellm".into(),
                model: "m".into(),
                ..Default::default()
            },
            CallConfigAdapterDefaults::default(),
            None,
        ))
    };
    let count_ctx = || {
        s.events()
            .iter()
            .filter(|e| e.kind == EventKind::RequestContext)
            .count()
    };
    // 1: context 8192 → 记录
    let b1 = default_build(
        &s,
        &options("litellm", "m", None),
        false,
        &[],
        "sys",
        boundary.clone(),
        &ident_propose,
        &prepare_8192,
    )
    .unwrap();
    assert_eq!(count_ctx(), 1);
    // 2: context 无变化 → 不记录
    default_build(
        &s,
        &options("litellm", "m", None),
        b1.request_header_logged,
        &[],
        "sys",
        boundary.clone(),
        &ident_propose,
        &prepare_8192,
    )
    .unwrap();
    assert_eq!(count_ctx(), 1);
    // 3: context 变 → 记录
    default_build(
        &s,
        &options("litellm", "m", None),
        true,
        &[],
        "sys",
        boundary,
        &ident_propose,
        &prepare_none,
    )
    .unwrap();
    assert_eq!(count_ctx(), 2);
}

// ---------------------------------------------------------------------------
// invariant
// ---------------------------------------------------------------------------

fn reconstructed_request() -> (Arc<Session>, AgentLoopRequest) {
    let st = store();
    let s = session_new(&st, "inv");
    step_start(&s, 1, 1);
    append_user(&s, "m1", "trouble?");
    let boundary = s.derive_messages().unwrap();
    let built = default_build(
        &s,
        &options("litellm", "deepseek-r1", None),
        false,
        &[],
        "Be brief.",
        boundary,
        &ident_propose,
        &base_prepared("litellm", "deepseek-r1"),
    )
    .unwrap();
    (s, built.request)
}

#[test]
fn invariant_passes_for_reconstructed_request() {
    let (s, request) = reconstructed_request();
    let result = check_loop_request(&request, Some(&s));
    assert_eq!(result, Ok(()));
}

#[test]
fn invariant_missing_session_id_fails() {
    let (s, request) = reconstructed_request();
    let no_session = AgentLoopRequest(go(
        "litellm",
        "deepseek-r1",
        request.options().messages.clone(),
        None,
    ));
    let err = check_loop_request(&no_session, Some(&s)).err().unwrap();
    assert_eq!(err, "a loop-built request must carry a session id");
}

#[test]
fn invariant_no_live_session_fails() {
    let (s, request) = reconstructed_request();
    let _ = &s;
    let err = check_loop_request(&request, None).err().unwrap();
    assert_eq!(
        err,
        "a loop-built request must carry a live session id, got \"inv\""
    );
}

#[test]
fn invariant_no_step_start_fails() {
    let st = store();
    let s = session_new(&st, "no-step");
    append_user(&s, "m1", "hi");
    // 手动构造请求：session id + 派生消息，但日志无 step/start
    let boundary = s.derive_messages().unwrap();
    let request = AgentLoopRequest(go("p", "m", boundary, Some(SessionId("no-step".into()))));
    let err = check_loop_request(&request, Some(&s)).err().unwrap();
    assert_eq!(
        err,
        "a loop-built request with no step/start in its session log"
    );
}

#[test]
fn invariant_no_header_fails() {
    let st = store();
    let s = session_new(&st, "no-header");
    step_start(&s, 1, 1);
    append_user(&s, "m1", "hi");
    let boundary = s.derive_messages().unwrap();
    let request = AgentLoopRequest(go("p", "m", boundary, Some(SessionId("no-header".into()))));
    let err = check_loop_request(&request, Some(&s)).err().unwrap();
    assert_eq!(
        err,
        "a loop-built request with no request/header event in its session log"
    );
}

#[test]
fn invariant_messages_divergence_fails() {
    let (s, request) = reconstructed_request();
    // 在重建请求的 messages 末尾加一条 → 与派生分歧
    let mut options = request.0.clone();
    options
        .messages
        .push(Message::user(MessageId("extra".into()), vec![]));
    let bad = AgentLoopRequest(options);
    let err = check_loop_request(&bad, Some(&s)).err().unwrap();
    assert_eq!(
        err,
        "llm request for session \"inv\" diverges from the dispatch-time durable derivation (log-reconstruction desync)"
    );
}

#[test]
fn invariant_header_divergence_fails() {
    let (s, request) = reconstructed_request();
    // model 改掉 → 与折叠头分歧（messages 仍相等）
    let mut options = request.0.clone();
    options.model = "different-model".into();
    let bad = AgentLoopRequest(options);
    let err = check_loop_request(&bad, Some(&s)).err().unwrap();
    assert_eq!(
        err,
        "llm request for session \"inv\" diverges from the folded request header"
    );
}

// 编译期断言：BuildRequest 可转换回 options（供驱动在 M2e-2 消费）。
#[allow(dead_code)]
fn _unpack(b: BuiltRequest) -> GenerateOptions {
    b.request.into_options()
}

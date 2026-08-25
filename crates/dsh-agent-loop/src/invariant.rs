//! request-reconstruction invariant（`agent-loop/src/invariant.ts` 等效；THEOREM 的
//! 执行化证明：模型可见 ⟺ 已登录）。
//!
//! Rust 面：`fail` 是 `never` 返回的抛错——因此首个违例即失败（与 TS 一致）。
//! 「请求必须冻结 / messages 必须冻结」在 Rust 属于类型面（不可变值），无运行时检查
//! （D-031 声明）。`isAgentLoopRequest` 的运行时门在 Rust 变成 `AgentLoopRequest`
//! 包装类型（loop 唯一生产者；D-031 声明）。

use std::sync::Arc;

use dsh_llm::GenerateOptions;
use dsh_session::{fold_request_header, EventKind, Session};

/// loop 构建请求的类型级标记（等价 TS `markAgentLoopRequest` 的 symbol 标记）。
#[derive(Debug, Clone)]
pub struct AgentLoopRequest(pub GenerateOptions);

impl AgentLoopRequest {
    pub fn options(&self) -> &GenerateOptions {
        &self.0
    }
    pub fn into_options(self) -> GenerateOptions {
        self.0
    }
}

/// 校验一次 loop 构建的 LLM 请求能否由 session 日志逐字节重建。
///
/// - `session: None` → 「无活 session」违例（`fail('... live session id, got "<id>"')`）。
/// - `session: Some(..)` → 执行 step/start、request/header、派生消息、折叠头
///   逐字节比对；任一违例以 `Err`（首个）返回。
/// - 消息文本逐字对齐 `invariant.ts`。
pub fn check_loop_request(
    request: &AgentLoopRequest,
    session: Option<&Arc<Session>>,
) -> Result<(), String> {
    let options = &request.0;

    // (TS) Object.isFrozen(options)：Rust 不可变值，恒冻结（类型面）。
    if options.session_id.is_none() {
        return Err("a loop-built request must carry a session id".to_string());
    }
    let Some(session) = session else {
        let got = options
            .session_id
            .as_ref()
            .map(|s| s.raw().to_string())
            .unwrap_or_else(|| "undefined".to_string());
        return Err(format!(
            "a loop-built request must carry a live session id, got \"{got}\""
        ));
    };
    // (TS) Object.isFrozen(options.messages)：Rust 恒冻结（类型面）。

    let events = session.events();
    if !events.iter().any(|e| e.kind == EventKind::StepStart) {
        return Err("a loop-built request with no step/start in its session log".to_string());
    }
    let Some(header) = fold_request_header(&events, None) else {
        return Err("a loop-built request with no request/header event in its session log".to_string());
    };

    let expected = session.derive_messages().map_err(|e| e.0.clone())?;
    let actual_json = serde_json::to_string(&options.messages).unwrap_or_default();
    let expected_json = serde_json::to_string(&expected).unwrap_or_default();
    if actual_json != expected_json {
        return Err(format!(
            "llm request for session \"{}\" diverges from the dispatch-time durable derivation (log-reconstruction desync)",
            session.id()
        ));
    }

    let header_matches = options.model == header.config.model
        && options.system == header.system
        && options.temperature == header.config.temperature
        && options.max_tokens == header.config.max_tokens
        && serde_json::to_string(&options.stop).unwrap_or_default()
            == serde_json::to_string(&header.config.stop).unwrap_or_default()
        && serde_json::to_string(&options.tools.as_deref().unwrap_or(&[])).unwrap_or_default()
            == serde_json::to_string(&header.tools.as_deref().unwrap_or(&[])).unwrap_or_default();
    if !header_matches {
        return Err(format!(
            "llm request for session \"{}\" diverges from the folded request header",
            session.id()
        ));
    }

    Ok(())
}

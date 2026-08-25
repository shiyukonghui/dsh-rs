//! `dsh-agent-loop` — 宿主侧 Agent 核心运行驱动（`@deepseek-ai/dsh-agent-loop` 等效迁移）。
//!
//! 权威参考报告：`analysis/m2/agent-loop-report.md`。
//! - M2e-1：**请求重建层**（本轮）——常量/设置/strict 验证、`requestProposal` +
//!   `buildRequest` 纯核心（header 唯一锚点、context 增量、loop 标记）、
//!   request-reconstruction invariant（THEOREM 的执行化证明，fail 文本逐字）。
//! - M2e-2+：ReactLoopAgent turn/step 驱动、tool 调度、AgentLoop 服务。

pub mod agent;
pub mod build_request;
pub mod constants;
pub mod host;
pub mod invariant;
pub mod runtime_context;
pub mod service;
pub mod settings;
pub mod tool_calls;

pub use agent::{
    LoopDeps, PendingCall, PreStepDecision, ReactLoopAgent, ToolExecCtx, ToolExecOutcome,
};
pub use build_request::{build_request, request_proposal, BuiltRequest};
pub use constants::*;
pub use host::{
    AgentLoopConfig, AgentLoopHost, ConfiguredAgent, ConfiguredAgentIdentity,
    validate_configured_agents,
};
pub use invariant::{check_loop_request, AgentLoopRequest};
pub use runtime_context::{RuntimeContextProjection, CLEARED as RUNTIME_CONTEXT_CLEARED};
pub use service::{build_loop_deps, create_loop_agent};
pub use settings::{validate_max_parallel_tool_calls, validate_max_tokens, AgentLoopSettings};
pub use tool_calls::{
    append_pending_rejection, emit_pending_calls, execute_tool_calls, CODE_TOOL_REJECTED,
};

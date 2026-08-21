//! `dsh-agent-loop` — 宿主侧 Agent 核心运行驱动（`@deepseek-ai/dsh-agent-loop` 等效迁移）。
//!
//! 权威参考报告：`analysis/m2/agent-loop-report.md`。
//! - M2e-1：**请求重建层**（本轮）——常量/设置/strict 验证、`requestProposal` +
//!   `buildRequest` 纯核心（header 唯一锚点、context 增量、loop 标记）、
//!   request-reconstruction invariant（THEOREM 的执行化证明，fail 文本逐字）。
//! - M2e-2+：ReactLoopAgent turn/step 驱动、tool 调度、AgentLoop 服务。

pub mod build_request;
pub mod constants;
pub mod invariant;
pub mod settings;

pub use build_request::{build_request, request_proposal, BuiltRequest};
pub use constants::*;
pub use invariant::{check_loop_request, AgentLoopRequest};
pub use settings::{validate_max_parallel_tool_calls, validate_max_tokens, AgentLoopSettings};

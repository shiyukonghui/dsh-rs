//! `dsh-agent` — 活体 Agent 建模的 Rust 版（`@deepseek-ai/dsh-agent` 等效迁移）。
//!
//! 权威参考报告：`analysis/m2/agent-system-prompt-report.md` §A。
//! - M2d-1：**durable/记账核心** —— 类型面、Inbox（durable pending 双队列投影 +
//!   JS 兼容 splice 标准化）、foldConsumedWork（已消费工作记账），D-029。
//! - M2d-2：**活体生命周期** —— `AgentBus`（subject 作用域派发）、`AgentRegistry`
//!   （register/announce/detach/factory seam）、sync initiator、dispatch 融合、
//!   agent-invariant（status no-op 拒绝），D-030。

pub mod agent_bus;
pub mod consumed_work;
pub mod dispatch;
pub mod inbox;
pub mod invariant;
pub mod model_selection;
pub mod registry;
pub mod types;

pub use agent_bus::{AgentBus, AgentListener, ChainListener, NextFn};
pub use consumed_work::fold_consumed_work;
pub use dispatch::{assemble_context_for, emit_agent_event, fuse_agent, AgentEventDispatch};
pub use inbox::{inbox_splice, ClaimResult, Inbox, InboxNotification, InboxNotify, InboxSpliceRecord};
pub use invariant::AgentInvariant;
pub use model_selection::install_model_selection;
pub use registry::{
    agent_carrier, agent_value, agent_scope, Agent, AgentCtx, AgentFactory, AgentRegistry,
    CreateAgentOptions, InitiatorPhase, ResumeAgentOptions,
};
pub use types::*;

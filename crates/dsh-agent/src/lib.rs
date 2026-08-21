//! `dsh-agent` — 活体 Agent 建模的 Rust 版（`@deepseek-ai/dsh-agent` 等效迁移）。
//!
//! 权威参考报告：`analysis/m2/agent-system-prompt-report.md` §A。
//! M2d 首轮交付：**durable/记账核心** —— 类型面、Inbox（durable pending 双队列
//! 投影 + JS 兼容 splice 标准化）、foldConsumedWork（已消费工作记账）。注册表/
//! initiator/model-selection/invariant 在 M2d-2 交付（依赖作用域事件总线）。

pub mod consumed_work;
pub mod inbox;
pub mod types;

pub use consumed_work::fold_consumed_work;
pub use inbox::{inbox_splice, Inbox, InboxNotification, InboxNotify, InboxSpliceRecord, ClaimResult};
pub use types::*;

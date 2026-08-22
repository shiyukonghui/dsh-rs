//! `dsh-goal` — 宿主侧目标能力缝（`@deepseek-ai/dsh-goal` 等效迁移）。
//!
//! M4a 目标：纯 goal 域 —— CAS 状态机 + 事件溯源 fold + 投影 + round-driver 判定。
//! 权威参考：`deepseek-harness/packages/goal/goal/src/{types,domain,fold,runtime}.ts`。

pub mod fold;
pub mod service;
pub mod types;

pub use fold::{fold_goal_events, FoldedGoal};
pub use service::{GoalService, GoalServiceError, ServiceOptions};
pub use types::*;

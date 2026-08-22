//! `dsh-plan` — 宿主侧计划模式能力缝（`@deepseek-ai/dsh-plan-mode` 等效迁移）。
//!
//! M4c 目标：`plan/mode` 事件 last-wins 折叠 + `plan` 投影（active/pending）+ `/plan`
//! 命令判定 + `exit_plan_mode` 工具前置校验。权威参考：
//! `deepseek-harness/packages/plan/plan-mode/src/{types,index}.ts`。

pub mod exit;
pub mod fold;
pub mod projection;
pub mod types;

pub use exit::{exit_plan_mode_check, ExitCheck, EXIT_PLAN_MODE};
pub use fold::{fold_plan_mode, fold_plan_mode_prefix, has_open_turn};
pub use projection::{
    plan_projection_from_events, plan_projection_view, plan_unit_apply, PlanUnitState,
    RunningCommand,
};
pub use types::PlanProjection;

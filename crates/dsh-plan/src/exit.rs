//! `dsh-plan` exit_plan_mode 前置校验 —— 对齐 `packages/plan/plan-mode/src/index.ts`
//! `exit_plan_mode` 工具的 execute 前置：调用 agent 存在 → plan mode active →
//! plan 以 `# 标题` 开头 → 评审通道可用。

use dsh_session::types::SessionEvent;

use crate::fold::fold_plan_mode;

/// 工具名（对齐 `EXIT_PLAN_MODE` 常量）。
pub const EXIT_PLAN_MODE: &str = "exit_plan_mode";

/// 前置校验失败分类（逐字对齐 TS 错误文案要点；message 由 M4h 装配时渲染）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExitCheck {
    /// 非 plan mode。
    NotInPlanMode,
    /// plan 不带 `# 标题` 开头。
    NeedsHeading,
    /// 无 user-questions 评审通道。
    NoReviewChannel,
    /// 通过（可发起评审）。
    Ok,
}

/// 校验 exit_plan_mode 前置条件。
///
/// - `plan` 必须 `^#\s+\S`（非空标题）。
/// - `review_channel` 表示宿主是否装配了 user-questions 通道。
pub fn exit_plan_mode_check(
    events: &[SessionEvent],
    plan: &str,
    review_channel: bool,
) -> Result<(), ExitCheck> {
    if !fold_plan_mode(events) {
        return Err(ExitCheck::NotInPlanMode);
    }
    let trimmed = plan.trim_start();
    let has_heading = trimmed.len() >= 2
        && trimmed.starts_with('#')
        && trimmed[1..].starts_with(char::is_whitespace)
        && !trimmed[1..].trim().is_empty();
    if !has_heading {
        return Err(ExitCheck::NeedsHeading);
    }
    if !review_channel {
        return Err(ExitCheck::NoReviewChannel);
    }
    Ok(())
}

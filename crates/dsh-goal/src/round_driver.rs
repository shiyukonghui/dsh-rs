//! `dsh-goal` 自动续跑驱动 —— 对齐 `packages/goal/goal-round-driver/src/index.ts`。
//!
//! 纯驱动谓词 + 一次准入尝试：通过 `StatusPort` 抽象宿主（agent-loop 的 status/inbox/
//! followup），driver 本身不持有 agent-loop。宿主每轮结束调用 `round_driver_outcome`（
//! 只读判定）或 `drive_once`（判定 + 若续跑则凑齐一轮 user 提示并 followup）。
//!
//! 判定：`phase==active ∧ activation==armed ∧ roundsStarted < maxGoalRounds
//! ∧ status_idle ∧ !has_pending_inbox` → `Continue`。
//! 下一轮号 = roundsStarted + 1（≤ maxGoalRounds）。

use crate::service::GoalService;
use crate::types::{GoalActivation, GoalId, GoalPhase};

/// 宿主状态端口（由 web.rs / agent-loop 装配实现）。
pub trait StatusPort {
    /// agent 是否空闲（无 driver 活跃）。
    fn status_idle(&self) -> bool;
    /// 有无竞争 inbox（有则本驱动不得插入）。
    fn has_pending_inbox(&self) -> bool;
    /// 把一条 user 消息投进 agent 收件箱并发起唤醒。
    fn followup(&mut self, id: &GoalId, message: &str) -> Result<(), String>;
}

/// 驱动判定结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundOutcome {
    /// 续跑（已/将发起 followup）。
    Continue,
    /// 不应续跑（无目标 / 非 active / disarmed / 已到 cap / 忙）。
    Noop,
}

/// 只读判定：当前是否应发起下一轮。
pub fn round_driver_outcome(
    service: &GoalService,
    id: &GoalId,
    port: &dyn StatusPort,
) -> Option<RoundOutcome> {
    // id 与当前目标一致性（get 对不匹配返回 NotFound）。
    if service.get(id).is_err() {
        return None;
    }
    if service.phase() != Some(GoalPhase::Active) {
        return None;
    }
    if service.activation() != GoalActivation::Armed {
        return None;
    }
    let started = service.rounds_started();
    if started >= service.max_goal_rounds() || service.max_goal_rounds() == 0 {
        return None;
    }
    if !port.status_idle() || port.has_pending_inbox() {
        return None;
    }
    Some(RoundOutcome::Continue)
}

/// 驱动一次：判定 + 若续跑则凑齐一轮 user 提示（含 Round: N/M）并发起 followup。
pub fn drive_once(
    service: &mut GoalService,
    port: &mut dyn StatusPort,
    id: &GoalId,
) -> Result<RoundOutcome, String> {
    if round_driver_outcome(service, id, port).is_none() {
        return Ok(RoundOutcome::Noop);
    }
    let next = service.rounds_started() + 1;
    let total = service.max_goal_rounds();
    let objective = service.objective().unwrap_or("").to_string();
    // 准入本轮（递增 roundsStarted 到 next；驱动只推进本轮）。
    service.admit_round(id, next).map_err(|e| format!("{e}"))?;
    let text = render_round_prompt(&objective, next, total);
    port.followup(id, &text)?;
    Ok(RoundOutcome::Continue)
}

/// 轮次提示渲染（对齐 goal-round-driver prompt.ts）。
pub fn render_round_prompt(objective: &str, round: u64, total: u64) -> String {
    format!(
        "<goal_round>Objective: {objective}\nRound: {round}/{total}\nContinue working toward the objective; before reporting completion, read the current goal and mark it complete. If work remains, leave the goal active and continue.</goal_round>"
    )
}

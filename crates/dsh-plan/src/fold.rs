//! `dsh-plan` 折叠：`foldPlanMode`（最后 `plan/mode` 胜出）+ `hasOpenTurn` 前缀折叠。
//!
//! 对齐 `packages/plan/plan-mode/src/index.ts`。状态永远可从 session log 纯重放恢复
//! （无 live mirror）。

use dsh_session::types::{EventKind, SessionEvent};

/// 折叠 `events[0, end)`，返回 plan mode 是否 active；无任何 `plan/mode` → inactive。
pub fn fold_plan_mode(events: &[SessionEvent]) -> bool {
    fold_plan_mode_prefix(events, events.len())
}

/// 折叠前缀 `events[0, end)`。
pub fn fold_plan_mode_prefix(events: &[SessionEvent], end: usize) -> bool {
    let mut active = false;
    for event in events.iter().take(end) {
        if event.kind == EventKind::PlanMode {
            active = event.data.get("active").and_then(|v| v.as_bool()).unwrap_or(false);
        }
    }
    active
}

/// 日志是否持有「已开未关」的 turn（turn/start … turn/end 配对）。
pub fn has_open_turn(events: &[SessionEvent]) -> bool {
    let mut open = false;
    for event in events {
        if event.kind == EventKind::TurnStart {
            open = true;
        } else if event.kind == EventKind::TurnEnd {
            open = false;
        }
    }
    open
}

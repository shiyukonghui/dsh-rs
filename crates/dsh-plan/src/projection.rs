//! `dsh-plan` 投影单元：fold `command/run[name=plan]` / `command/done` / `plan/mode`
//! 出 `PlanUnitState`，`view` 出 `{active, pending}` 投影。
//!
//! 对齐 `packages/plan/plan-mode/src/index.ts` 的 PlanUnitState + projection unit：
//! - `command/run` name=='plan' → `running={commandId, wanted: args.trim()!="off"}`
//!   （args 缺省则本轮不动）。
//! - 配对 `command/done`（仅 success 且 wanted≠active）→ 落 `wanted`；
//! - `plan/mode` → active 落定 + wanted 清。
//! - `view`：`pending = (running?.wanted ?? wanted) !== null && !== active`。

use dsh_session::types::{EventKind, SessionEvent};
use serde_json::{json, Value};

/// 投影单元状态（plain JSON 可持久化缓存）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlanUnitState {
    pub active: bool,
    /// 未落定的选择目标模式；无待定时 null。
    pub wanted: Option<bool>,
    /// 最晚一条待结算的 plan 命令。
    pub running: Option<RunningCommand>,
}

/// 一条待结算的 plan 命令。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunningCommand {
    pub command_id: String,
    pub wanted: bool,
}

impl PlanUnitState {
    pub fn init() -> Self {
        PlanUnitState { active: false, wanted: None, running: None }
    }
}

/// 折叠一条事件。
pub fn plan_unit_apply(state: &mut PlanUnitState, event: &SessionEvent) {
    if event.kind == EventKind::CommandRun {
        let name = event.data.get("name").and_then(|v| v.as_str()).unwrap_or("");
        if name != "plan" {
            return;
        }
        let Some(args) = event.data.get("args") else {
            return; // args 缺省（undefined）不动
        };
        let args = args.as_str().unwrap_or("");
        let command_id = event.data.get("commandId").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let wanted = args.trim() != "off";
        state.running = Some(RunningCommand { command_id, wanted });
        return;
    }
    if event.kind == EventKind::CommandDone {
        if let Some(running) = &state.running {
            let command_id = event.data.get("commandId").and_then(|v| v.as_str()).unwrap_or("");
            if command_id == running.command_id {
                let kind = event.data.get("kind").and_then(|v| v.as_str()).unwrap_or("");
                let wanted = if kind == "success" && running.wanted != state.active {
                    Some(running.wanted)
                } else {
                    None
                };
                state.wanted = wanted;
                state.running = None;
            }
        }
        return;
    }
    if event.kind == EventKind::PlanMode {
        let active = event.data.get("active").and_then(|v| v.as_bool()).unwrap_or(false);
        state.active = active;
        state.wanted = None;
    }
}

/// 投影视图 `{active, pending}`。
pub fn plan_projection_view(state: &PlanUnitState) -> Value {
    let wanted = state.running.as_ref().map(|r| r.wanted).or(state.wanted);
    json!({
        "active": state.active,
        "pending": wanted.is_some_and(|w| w != state.active),
    })
}

/// 从整段事件序列折出投影（M4h 注册 ProjectionUnit 时可用 apply+view 组合）。
pub fn plan_projection_from_events(events: &[SessionEvent]) -> Value {
    let mut state = PlanUnitState::init();
    for e in events {
        plan_unit_apply(&mut state, e);
    }
    plan_projection_view(&state)
}

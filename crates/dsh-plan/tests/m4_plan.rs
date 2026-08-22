//! M4c dsh-plan 折叠 + 投影 + exit 前置测试（TDD 红-绿）。
//!
//! 对齐 `packages/plan/plan-mode/src/index.ts`：`foldPlanMode`（最后 plan/mode 胜出）、
//! PlanUnitState 投影（command/run[name=plan] → running；command/done → wanted；
//! plan/mode → active 落定 + wanted 清）、`view={active,pending}`、
//! `hasOpenTurn`、`exit_plan_mode` 前置校验（仅 plan mode + `^#\s+\S` 标题 + 评审通道）。

use dsh_plan::exit::{exit_plan_mode_check, ExitCheck};
use dsh_plan::fold::{fold_plan_mode, has_open_turn};
use dsh_plan::projection::{plan_projection_from_events, plan_unit_apply, PlanUnitState, plan_projection_view};
use dsh_session::types::{EventKind, SessionEvent};
use serde_json::json;

fn ev(seq: u64, kind: EventKind, data: serde_json::Value) -> SessionEvent {
    SessionEvent::new(seq, 1000 + seq as i64, kind, data)
}

fn plan_mode(seq: u64, active: bool) -> SessionEvent {
    ev(seq, EventKind::PlanMode, json!({ "active": active }))
}

fn command_run(seq: u64, command_id: &str, name: &str, args: &str) -> SessionEvent {
    ev(
        seq,
        EventKind::CommandRun,
        json!({ "commandId": command_id, "name": name, "args": args }),
    )
}

fn command_done(seq: u64, command_id: &str, kind: &str) -> SessionEvent {
    ev(seq, EventKind::CommandDone, json!({ "commandId": command_id, "kind": kind }))
}

fn turn_start(seq: u64) -> SessionEvent {
    ev(seq, EventKind::TurnStart, json!({}))
}

fn turn_end(seq: u64) -> SessionEvent {
    ev(seq, EventKind::TurnEnd, json!({}))
}

/// 无 plan/mode → inactive。
#[test]
fn fold_no_events_inactive() {
    assert!(!fold_plan_mode(&[]));
}

/// plan/mode 最后一条胜出。
#[test]
fn fold_last_wins() {
    let events = vec![plan_mode(0, true), plan_mode(1, false), plan_mode(2, true)];
    assert!(fold_plan_mode(&events));
    let events2 = vec![plan_mode(0, true), plan_mode(1, false)];
    assert!(!fold_plan_mode(&events2));
}

/// fold 支持前缀 end（TS foldPlanMode 第二参数）。
#[test]
fn fold_prefix_end() {
    let events = vec![plan_mode(0, true), plan_mode(1, false)];
    assert!(fold_plan_mode_prefix(&events, 1), "仅见第一条 active");
    assert!(!fold_plan_mode_prefix(&events, 2));
}

/// has_open_turn：turn/start 未配错 → true。
#[test]
fn open_turn_detected() {
    let events = vec![turn_start(0), turn_end(1), turn_start(2)];
    assert!(has_open_turn(&events));
    let events2 = vec![turn_start(0), turn_end(1)];
    assert!(!has_open_turn(&events2));
}

/// 投影单元初始态。
#[test]
fn plan_unit_init() {
    let state = PlanUnitState::init();
    assert!(!state.active);
    assert!(state.wanted.is_none());
    assert!(state.running.is_none());
}

/// command/run[name=plan] → running 记录；args 缺省（undefined）时不断言。
#[test]
fn plan_unit_command_run_sets_running() {
    let events = vec![command_run(0, "cmd-1", "plan", "ship M4")];
    let mut state = PlanUnitState::init();
    for e in &events {
        plan_unit_apply(&mut state, e);
    }
    let running = state.running.as_ref().expect("running set");
    assert_eq!(running.command_id, "cmd-1");
    assert!(running.wanted, "非 off → wanted true");
}

/// command/run[name=plan, args=off] → wanted false。
#[test]
fn plan_unit_off_command_wants_false() {
    let events = vec![command_run(0, "cmd-2", "plan", " off ")];
    let mut state = PlanUnitState::init();
    for e in &events {
        plan_unit_apply(&mut state, e);
    }
    assert!(!state.running.as_ref().expect("r").wanted);
}

/// command/done + plan/mode 落定：wanted 清、active 落定、pending=false。
#[test]
fn plan_unit_commit_clears_pending() {
    let events = vec![
        command_run(0, "cmd-1", "plan", "ship"),
        turn_start(1),
        command_done(2, "cmd-1", "success"),
        plan_mode(3, true),
    ];
    let mut state = PlanUnitState::init();
    for e in &events {
        plan_unit_apply(&mut state, e);
    }
    assert!(state.active);
    assert!(state.wanted.is_none());
    assert!(state.running.is_none());
    let view = plan_projection_view(&state);
    assert_eq!(view, json!({ "active": true, "pending": false }));
}

/// command/done 非 success 不落 wanted（kind != success 时 wanted 清）。
#[test]
fn plan_unit_failed_done_keeps_pending_clear() {
    let events = vec![command_run(0, "cmd-1", "plan", "ship"), command_done(1, "cmd-1", "error")];
    let mut state = PlanUnitState::init();
    for e in &events {
        plan_unit_apply(&mut state, e);
    }
    assert!(!state.active);
    assert!(state.wanted.is_none(), "失败 done 不落 wanted");

    let events2 = vec![command_run(0, "cmd-1", "plan", "ship"), command_done(1, "cmd-1", "success")];
    let mut state2 = PlanUnitState::init();
    for e in &events2 {
        plan_unit_apply(&mut state2, e);
    }
    assert_eq!(state2.wanted, Some(true), "成功 done 落 wanted=true");
}

/// 投影 pending：wanted(true) != active(false) → pending=true；落定后 false。
#[test]
fn plan_projection_pending() {
    let events = vec![command_run(0, "cmd-1", "plan", "ship"), turn_start(1)];
    let mut state = PlanUnitState::init();
    for e in &events {
        plan_unit_apply(&mut state, e);
    }
    // active=false, running.wanted=true → pending true
    assert_eq!(plan_projection_view(&state), json!({ "active": false, "pending": true }));

    let events2 = vec![command_run(0, "cmd-1", "plan", "ship"), plan_mode(1, true)];
    let mut state2 = PlanUnitState::init();
    for e in &events2 {
        plan_unit_apply(&mut state2, e);
    }
    assert_eq!(plan_projection_view(&state2), json!({ "active": true, "pending": false }));
}

/// 从事件序列整体折出 plan 投影（M4h 注册 ProjectionUnit 用 apply/view 二件套）。
#[test]
fn plan_projection_from_full_log() {
    let events = vec![plan_mode(0, true)];
    let view = plan_projection_from_events(&events);
    assert_eq!(view, json!({ "active": true, "pending": false }));
}

/// exit_plan_mode 前置校验：非 plan mode → Err；无标题 → Err；两者符合 → 通过。
#[test]
fn exit_check_gates() {
    // 非 plan mode
    let evs = vec![plan_mode(0, false)];
    let r1 = exit_plan_mode_check(&evs, "# Plan", true);
    assert!(matches!(r1, Err(ExitCheck::NotInPlanMode)));
    // 无标题
    let evs2 = vec![plan_mode(0, true)];
    let r2 = exit_plan_mode_check(&evs2, "no heading plan", true);
    assert!(matches!(r2, Err(ExitCheck::NeedsHeading)));
    // 无评审通道（seam 缺失）
    let r3 = exit_plan_mode_check(&evs2, "# Plan", false);
    assert!(matches!(r3, Err(ExitCheck::NoReviewChannel)));
    // 全部符合
    let r4 = exit_plan_mode_check(&evs2, "# Plan title", true);
    assert!(matches!(r4, Ok(())), "plan mode + heading + channel → 通过");
}

fn fold_plan_mode_prefix(events: &[SessionEvent], end: usize) -> bool {
    fold_plan_mode_prefix_impl(events, end)
}

// 注意：dsh-plan::fold 提供 fold_plan_mode(events)，prefix 支持需显式提供。
fn fold_plan_mode_prefix_impl(events: &[SessionEvent], end: usize) -> bool {
    use dsh_plan::fold::fold_plan_mode_prefix;
    fold_plan_mode_prefix(events, end)
}

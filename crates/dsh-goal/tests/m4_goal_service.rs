//! M4a dsh-goal 状态机（service）测试：CAS 转换 + 错误码 + arm/disarm + 轮次准入。
//!
//! 对齐 `packages/goal/goal/src/runtime.ts`（GoalService）：create/edit/pause/resume/
//! complete/clear + GOAL_* 错误码逐字；resume 前 revision 校验（CAS）；轮次准入
//! round <= maxGoalRounds。

use dsh_goal::service::{
    GoalService, GoalServiceError, ServiceOptions,
};
use dsh_goal::types::{GoalActivation, GoalPhase};

fn svc() -> GoalService {
    GoalService::new(ServiceOptions { max_consecutive_blocked_rounds: 3 })
}

/// create → active + armed + revision 1 + maxGoalRounds 缺省用配置（默认 256）。
#[test]
fn create_arms_active() {
    let mut s = svc();
    let refr = s.create("ship M4", None).expect("create ok");
    assert_eq!(refr.revision, 1);
    let view = s.get(&refr.id).expect("get ok");
    assert_eq!(view.phase, GoalPhase::Active);
    assert_eq!(view.activation, GoalActivation::Armed);
    assert_eq!(view.max_goal_rounds, 256);
    assert_eq!(view.rounds_started, 0);
}

/// create 带显式 maxGoalRounds。
#[test]
fn create_with_max_rounds() {
    let mut s = svc();
    let refr = s.create("task", Some(10)).expect("create ok");
    let view = s.get(&refr.id).expect("get ok");
    assert_eq!(view.max_goal_rounds, 10);
}

/// 已存在 active goal 时重复 create → GOAL_ALREADY_EXISTS。
#[test]
fn create_when_exists_conflicts() {
    let mut s = svc();
    s.create("first", None).expect("ok");
    let err = s.create("second", None).expect_err("应冲突");
    assert_eq!(err.code(), "GOAL_ALREADY_EXISTS");
}

/// 空 objective → GOAL_INVALID_OBJECTIVE。
#[test]
fn create_empty_objective_rejected() {
    let mut s = svc();
    let err = s.create("", None).expect_err("空目标应拒");
    assert_eq!(err.code(), "GOAL_INVALID_OBJECTIVE");
}

/// maxGoalRounds=0 → GOAL_INVALID_MAX_ROUNDS。
#[test]
fn create_zero_rounds_rejected() {
    let mut s = svc();
    let err = s.create("task", Some(0)).expect_err("0 轮应拒");
    assert_eq!(err.code(), "GOAL_INVALID_MAX_ROUNDS");
}

/// edit objective + revision +1，phase 不变。
#[test]
fn edit_objective() {
    let mut s = svc();
    let r1 = s.create("a", None).expect("create");
    let r2 = s.edit(&r1, Some("b"), None).expect("edit");
    assert_eq!(r2.revision, r1.revision + 1);
    let view = s.get(&r1.id).expect("get");
    assert_eq!(view.objective, "b");
    assert_eq!(view.phase, GoalPhase::Active);
}

/// edit 至少要给一个字段 → GOAL_INVALID_EDIT。
#[test]
fn edit_both_none_rejected() {
    let mut s = svc();
    let r1 = s.create("a", None).expect("create");
    let err = s.edit(&r1, None, None).expect_err("都应缺省");
    assert_eq!(err.code(), "GOAL_INVALID_EDIT");
}

/// edit 用错 revision（stale）→ GOAL_STALE_REVISION。
#[test]
fn edit_stale_revision_conflicts() {
    let mut s = svc();
    let r1 = s.create("a", None).expect("create");
    s.edit(&r1, Some("b"), None).expect("edit -> rev2");
    let stale = r1.clone(); // revision 1，已过期
    let err = s.edit(&stale, Some("c"), None).expect_err("stale 应冲突");
    assert_eq!(err.code(), "GOAL_STALE_REVISION");
}

/// pause active → paused + disarmed。
#[test]
fn pause_disarms() {
    let mut s = svc();
    let r1 = s.create("a", None).expect("create");
    let r2 = s.pause(&r1).expect("pause");
    assert_eq!(r2.revision, 2);
    let view = s.get(&r1.id).expect("get");
    assert_eq!(view.phase, GoalPhase::Paused);
    assert_eq!(view.activation, GoalActivation::Disarmed);
}

/// resume paused → active + armed；且 CAS 校验。
#[test]
fn resume_rearms() {
    let mut s = svc();
    let r1 = s.create("a", None).expect("create");
    let r2 = s.pause(&r1).expect("pause");
    let r3 = s.resume(&r2).expect("resume");
    assert_eq!(r3.revision, 3);
    let view = s.get(&r1.id).expect("get");
    assert_eq!(view.phase, GoalPhase::Active);
    assert_eq!(view.activation, GoalActivation::Armed);
}

/// complete → complete + disarmed。
#[test]
fn complete_disarms() {
    let mut s = svc();
    let r1 = s.create("a", None).expect("create");
    let r2 = s.complete(&r1).expect("complete");
    assert_eq!(r2.revision, 2);
    let view = s.get(&r1.id).expect("get");
    assert_eq!(view.phase, GoalPhase::Complete);
    assert_eq!(view.activation, GoalActivation::Disarmed);
}

/// clear → goal 无（墓碑）+ 新 revision。
#[test]
fn clear_removes() {
    let mut s = svc();
    let r1 = s.create("a", None).expect("create");
    let cleared = s.clear(&r1).expect("clear");
    assert_eq!(cleared.revision, r1.revision + 1);
    assert!(s.get(&r1.id).is_err(), "clear 后 get 不应有 goal");
}

/// complete 后再次 create → 允许（新目标）。
#[test]
fn create_after_complete() {
    let mut s = svc();
    let r1 = s.create("a", None).expect("create");
    s.complete(&r1).expect("complete");
    let r2 = s.create("b", None).expect("再次 create 允许");
    assert_eq!(r2.revision, 1);
    assert_ne!(r1.id, r2.id);
}

/// pause/complete/resume 在错误 phase 的转换 → GOAL_INVALID_TRANSITION 或
/// GOAL_NOT_FOUND（对不存在的目标）。
#[test]
fn transition_on_missing_goal() {
    let mut s = svc();
    let err = s.pause(&dsh_goal::types::GoalRef::new("ghost", 1)).expect_err("不存在");
    assert_eq!(err.code(), "GOAL_NOT_FOUND");
}

/// 轮次准入：round 从 1..maxGoalRounds 可进入，超出拒。
#[test]
fn round_admission_boundary() {
    let mut s = svc();
    let r = s.create("task", Some(2)).expect("create");
    assert!(s.admit_round(&r.id, 1).is_ok());
    assert!(s.admit_round(&r.id, 2).is_ok());
    assert!(s.admit_round(&r.id, 3).is_err(), "round 3 超 cap 2");
}

/// 轮次准入后 roundsStarted 回读递增（供 goal-round-driver 判定）。
#[test]
fn admitted_rounds_increment_view() {
    let mut s = svc();
    let r = s.create("task", Some(5)).expect("create");
    s.admit_round(&r.id, 1).expect("admit 1");
    s.admit_round(&r.id, 2).expect("admit 2");
    assert_eq!(s.get(&r.id).expect("get").rounds_started, 2);
}

/// 错误码逐字（对照参考源码）。
#[test]
fn error_codes_exact() {
    // 全部错误码枚举值（后续 mutate 时逐字断言）
    let codes = GoalServiceError::ALL;
    assert!(codes.contains(&"GOAL_AGENT_NOT_LIVE"));
    assert!(codes.contains(&"GOAL_NOT_FOUND"));
    assert!(codes.contains(&"GOAL_ALREADY_EXISTS"));
    assert!(codes.contains(&"GOAL_STALE_REVISION"));
    assert!(codes.contains(&"GOAL_INVALID_OBJECTIVE"));
    assert!(codes.contains(&"GOAL_INVALID_MAX_ROUNDS"));
    assert!(codes.contains(&"GOAL_INVALID_BLOCK_REASON"));
    assert!(codes.contains(&"GOAL_INVALID_EDIT"));
    assert!(codes.contains(&"GOAL_INVALID_TRANSITION"));
}

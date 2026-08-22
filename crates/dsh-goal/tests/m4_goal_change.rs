//! M4h 补实：GoalService 产 `goal/change` 事件 meta（D-045：caller 经 take_last_change
//! 取最近一次变更并落会话）。
//!
//! 对齐 reference `GoalService` 每次非 clear 变更写完整 snapshot（last-wins），clear 写
//! 墓碑。web.rs 在每次成功 mutation 后 take_last_change() → 构造 `goal/change` 事件
//! 载荷 append 进 session（验收 #2「goal/change 事件落会话」）。

use dsh_goal::service::{GoalService, ServiceOptions};
use dsh_goal::types::{GoalChangeMeta, GoalOperation, GoalPhase, GOAL_CHANGE_VERSION};

fn svc() -> GoalService {
    GoalService::new(ServiceOptions { max_consecutive_blocked_rounds: 3 })
}

fn change_of(s: &mut GoalService) -> GoalChangeMeta {
    s.take_last_change().expect("服务应记录最近一次变更 meta")
}

/// create → snapshot meta：version=1、operation=create、goal 匹配、roundsStarted=0。
#[test]
fn create_emits_snapshot_meta() {
    let mut s = svc();
    let refr = s.create("ship M4", None).expect("create ok");
    let meta = change_of(&mut s);
    let GoalChangeMeta::Snapshot(snap) = meta else {
        panic!("create 应产生 snapshot change");
    };
    assert_eq!(snap.version, GOAL_CHANGE_VERSION);
    assert_eq!(snap.operation, GoalOperation::Create);
    assert_eq!(snap.goal.id, refr.id);
    assert_eq!(snap.goal.revision, 1);
    assert_eq!(snap.goal.phase, GoalPhase::Active);
    assert_eq!(snap.rounds_started, 0);
    assert!(snap.created_at > 0);
    assert_eq!(snap.updated_at, snap.created_at);
}

/// edit → snapshot meta：operation=edit、revision 递增、objective 更新。
#[test]
fn edit_emits_snapshot_meta() {
    let mut s = svc();
    let refr = s.create("ship M4", None).expect("create ok");
    let edited = s.edit(&refr, Some("ship M4 fully"), None).expect("edit ok");
    let meta = change_of(&mut s);
    let GoalChangeMeta::Snapshot(snap) = meta else {
        panic!("edit 应产生 snapshot change");
    };
    assert_eq!(snap.operation, GoalOperation::Edit);
    assert_eq!(snap.goal.revision, edited.revision);
    assert_eq!(snap.goal.objective, "ship M4 fully");
}

/// pause → snapshot meta：operation=pause、phase=paused、revision+1。
#[test]
fn pause_emits_snapshot_meta() {
    let mut s = svc();
    let refr = s.create("ship M4", None).expect("create ok");
    let paused = s.pause(&refr).expect("pause ok");
    let meta = change_of(&mut s);
    let GoalChangeMeta::Snapshot(snap) = meta else {
        panic!("pause 应产生 snapshot change");
    };
    assert_eq!(snap.operation, GoalOperation::Pause);
    assert_eq!(snap.goal.phase, GoalPhase::Paused);
    assert_eq!(snap.goal.revision, paused.revision);
}

/// clear → clear 墓碑 meta：operation=clear、cleared ref 匹配、clearedAt>0。
#[test]
fn clear_emits_tombstone_meta() {
    let mut s = svc();
    let refr = s.create("ship M4", None).expect("create ok");
    let cleared = s.clear(&refr).expect("clear ok");
    let meta = change_of(&mut s);
    let GoalChangeMeta::Clear(clear) = meta else {
        panic!("clear 应产生墓碑 change");
    };
    assert_eq!(clear.operation, GoalOperation::Clear);
    assert_eq!(clear.cleared.id, cleared.id);
    assert_eq!(clear.cleared.revision, cleared.revision);
    assert!(clear.cleared_at > 0);
}

/// 只读操作（get）不产生新 meta；连续 take 第二次得 None。
#[test]
fn read_ops_do_not_overwrite_change() {
    let mut s = svc();
    let refr = s.create("ship M4", None).expect("create ok");
    let create_meta = change_of(&mut s);
    // get 是只读——不产生新 meta
    let _ = s.get(&refr.id).expect("get ok");
    assert_eq!(s.take_last_change(), None, "get 不应产生变更 meta");
    // 但已取走的 create meta 是完整可折叠的（last-wins snapshot）
    let GoalChangeMeta::Snapshot(snap) = create_meta else {
        panic!("create 应产生 snapshot change");
    };
    assert_eq!(snap.goal.id, refr.id);
}

/// 连续 mutation：每次只反映最近一次（last-wins 语义由 fold 承担，服务侧只存最近）。
#[test]
fn succession_tracks_only_latest() {
    let mut s = svc();
    let refr = s.create("ship M4", None).expect("create ok");
    let _ = s.pause(&refr).expect("pause ok");
    let meta = change_of(&mut s);
    let GoalChangeMeta::Snapshot(snap) = meta else {
        panic!("应产 snapshot");
    };
    assert_eq!(snap.operation, GoalOperation::Pause);
    assert_eq!(s.take_last_change(), None, "最后一次已取走");
}

//! M4a dsh-goal 回放 fold 测试（TDD 红-绿）。
//!
//! 对齐 `packages/goal/goal/src/fold.ts`：`decodeGoalChange`（非 goal 事件 → 不相干；
//! malformed → fail loud）+ `applyGoalChange`（revision 精确 +1、计数/时间戳守恒、
//! create 要求 revision=1/active/0 轮/前 goal 仅 complete、seenGoalIds 不重复、
//! clear 时间戳不回拨）。

use dsh_goal::fold::{decode_goal_change, fold_goal_events, FoldedGoal};
use dsh_goal::types::GoalOperation;
use serde_json::json;

/// 构造一条 goal/change snapshot 事件载荷。
#[allow(clippy::too_many_arguments)]
fn snap_meta(
    op: GoalOperation,
    id: &str,
    revision: u64,
    objective: &str,
    phase: &str,
    max_rounds: u64,
    rounds_started: u64,
    created_at: i64,
    updated_at: i64,
) -> serde_json::Value {
    json!({
        "kind": "goal/change",
        "version": 1,
        "operation": op.as_str(),
        "goal": {
            "id": id,
            "revision": revision,
            "objective": objective,
            "phase": phase,
            "maxGoalRounds": max_rounds,
        },
        "roundsStarted": rounds_started,
        "createdAt": created_at,
        "updatedAt": updated_at,
    })
}

fn clear_meta(id: &str, revision: u64, cleared_at: i64) -> serde_json::Value {
    json!({
        "kind": "goal/change",
        "version": 1,
        "operation": "clear",
        "cleared": { "id": id, "revision": revision },
        "clearedAt": cleared_at,
    })
}

/// 折一条 goal/change snapshot：状态正确、计数/时间戳守恒。
#[test]
fn fold_goal_change_snapshot() {
    let folded = fold_goal_events(&[snap_meta(
        GoalOperation::Create,
        "goal-1",
        1,
        "ship M4",
        "active",
        256,
        0,
        1000,
        1000,
    )]);
    let goal = folded.goal.expect("create 后有 goal");
    assert_eq!(goal.id.0, "goal-1");
    assert_eq!(goal.revision, 1);
    assert_eq!(goal.objective, "ship M4");
    assert_eq!(goal.phase.as_str(), "active");
    assert_eq!(goal.max_goal_rounds, 256);
    assert_eq!(folded.rounds_started, 0);
    assert_eq!(folded.created_at, Some(1000));
    assert_eq!(folded.updated_at, Some(1000));
    assert!(folded.last_ref.is_some());
}

/// 空事件 → 无 goal。
#[test]
fn fold_empty_no_goal() {
    let folded = fold_goal_events(&[]);
    assert!(folded.goal.is_none());
    assert!(folded.last_ref.is_none());
}

/// 非 goal 事件不相干（`decode_goal_change` 对该 payload 返回 None，fold 跳过）。
#[test]
fn fold_ignores_unrelated_event() {
    let unrelated = json!({"some": "other"});
    let decoded = decode_goal_change(&unrelated);
    assert!(decoded.is_none(), "非 goal 载荷应不相干");
}

/// malformed goal/change（缺 goal 字段）→ fail loud（Err）。
#[test]
fn fold_malformed_fails_loud() {
    let bad = json!({ "kind": "goal/change", "version": 1 });
    let decoded = decode_goal_change(&bad);
    assert!(decoded.is_some());
    assert!(decoded.unwrap().is_err(), "缺字段应为解析错误");
}

/// 多条变更 → last-wins 全量快照；revision 精确 +1；计数/时间戳守恒。
#[test]
fn fold_sequential_mutations() {
    let events = vec![
        snap_meta(GoalOperation::Create, "goal-1", 1, "a", "active", 256, 0, 1000, 1000),
        snap_meta(GoalOperation::Edit, "goal-1", 2, "b", "active", 128, 0, 1000, 2000),
        snap_meta(GoalOperation::Pause, "goal-1", 3, "b", "paused", 128, 0, 1000, 3000),
    ];
    let folded = fold_goal_events(&events);
    let goal = folded.goal.expect("有 goal");
    assert_eq!(goal.revision, 3);
    assert_eq!(goal.objective, "b");
    assert_eq!(goal.phase.as_str(), "paused");
    assert_eq!(goal.max_goal_rounds, 128);
    assert_eq!(folded.updated_at, Some(3000));
    assert_eq!(folded.created_at, Some(1000));
    assert_eq!(folded.last_ref.unwrap().revision, 3);
}

/// clear 墓碑 → goal=None、last_ref 记为墓碑 ref。
#[test]
fn fold_clear_tombstone() {
    let events = vec![
        snap_meta(GoalOperation::Create, "goal-1", 1, "a", "active", 256, 0, 1000, 1000),
        clear_meta("goal-1", 2, 2000),
    ];
    let folded = fold_goal_events(&events);
    assert!(folded.goal.is_none(), "clear 后无 goal");
    assert_eq!(folded.last_ref.unwrap().revision, 2);
    assert_eq!(folded.updated_at, Some(2000));
}

/// clear 之后再次 create → 新 goal（revision 从 1 重新起）。
#[test]
fn fold_create_after_clear() {
    let events = vec![
        snap_meta(GoalOperation::Create, "goal-1", 1, "a", "active", 256, 0, 1000, 1000),
        clear_meta("goal-1", 2, 2000),
        snap_meta(GoalOperation::Create, "goal-2", 1, "b", "active", 10, 0, 3000, 3000),
    ];
    let folded = fold_goal_events(&events);
    let goal = folded.goal.expect("clear 后 create 有 goal");
    assert_eq!(goal.id.0, "goal-2");
    assert_eq!(goal.revision, 1);
    assert_eq!(folded.created_at, Some(3000));
}

/// 严格 fold：revision 往回跳（2 → 1）→ fail loud。
#[test]
fn fold_revision_regression_fails() {
    let events = vec![
        snap_meta(GoalOperation::Create, "goal-1", 1, "a", "active", 256, 0, 1000, 1000),
        snap_meta(GoalOperation::Edit, "goal-1", 1, "a", "active", 256, 0, 1000, 2000),
    ];
    let result = fold_goal_events_fallible(&events);
    assert!(result.is_err(), "revision 未递增应为错误");
}

fn fold_goal_events_fallible(events: &[serde_json::Value]) -> Result<FoldedGoal, String> {
    // fold.rs 内部提供 fallible 视图（测试沿用）
    use dsh_goal::fold::fold_goal_events_strict;
    fold_goal_events_strict(events)
}

/// 严格 fold：第一条非 create（前 goal 无或 complete）→ 拒。
#[test]
fn fold_first_must_be_create() {
    let events = vec![snap_meta(
        GoalOperation::Edit,
        "goal-1",
        2,
        "a",
        "active",
        256,
        0,
        1000,
        1000,
    )];
    let result = fold_goal_events_fallible(&events);
    assert!(result.is_err(), "首条非 create 应被拒");
}

/// seenGoalIds 不重复：同一目标 clear 后又 create 用旧 id → 拒。
#[test]
fn fold_reuses_cleared_goal_id_rejected() {
    let events = vec![
        snap_meta(GoalOperation::Create, "goal-1", 1, "a", "active", 256, 0, 1000, 1000),
        clear_meta("goal-1", 2, 2000),
        snap_meta(GoalOperation::Create, "goal-1", 1, "again", "active", 256, 0, 3000, 3000),
    ];
    let result = fold_goal_events_fallible(&events);
    assert!(result.is_err(), "clear 后重用 goal id 应被拒");
}

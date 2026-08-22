//! M4h 补齐：goal / plan / subagent 三个投影单元测试（TDD 红-绿）。
//!
//! 对齐 TS 参考：
//! - `goal`：`packages/goal/goal/src/index.ts`（key 'goal'，view = 整值
//!   `{goal: null|snapshot, roundsStarted, createdAt, updatedAt}`，goal 键恒在、
//!   无目标/clear 时 null——本 Rust 以「目标对象（键在）而非缺键」对齐 TS wire）；
//! - `plan`：`packages/plan/plan-mode/src/index.ts`（key 'plan'，view
//!   `{active, pending}`，plan_unit_apply 语义）；
//! - `subagent`：`packages/subagent/subagent/src/projection.ts`（key 'subagent'，
//!   view `{mode, label?, seq} | null`，last-wins 身份）。
//!
//! 经 `ProjectionRegistry`/`ProjectionSession` 公共 API 驱动（与 projection.rs 内测
//! 同款），每条 unit 的 state_version 见各 `into_unit()`。

use dsh_goal::types::GoalOperation;
use dsh_session::types::{EventKind, SessionEvent};
use dsh_session_query::projection::{ProjectionRegistry, ProjectionSession};
use dsh_session_query::{m4_projection_units, goal_projection_unit, plan_projection_unit, subagent_projection_unit};
use serde_json::{json, Value};

fn ev(seq: u64, kind: EventKind, data: Value) -> SessionEvent {
    SessionEvent::new(seq, 1000 + seq as i64, kind, data)
}

fn goal_ev(seq: u64, data: Value) -> SessionEvent {
    ev(seq, EventKind::GoalChange, data)
}

/// 构造一条 goal/change snapshot 事件载荷（m4_goal_fold.rs 同款 helper，故带同款 allow）。
#[allow(clippy::too_many_arguments)]
fn goal_snapshot_data(
    id: &str,
    revision: u64,
    objective: &str,
    phase: &str,
    max_rounds: u64,
    rounds_started: u64,
    created_at: i64,
    updated_at: i64,
) -> Value {
    json!({
        "kind": "goal/change",
        "version": 1,
        "operation": GoalOperation::Create.as_str(),
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

fn goal_clear_data(id: &str, revision: u64) -> Value {
    json!({
        "kind": "goal/change",
        "version": 1,
        "operation": "clear",
        "cleared": { "id": id, "revision": revision },
        "clearedAt": 2000,
    })
}

fn plan_mode(seq: u64, active: bool) -> SessionEvent {
    ev(seq, EventKind::PlanMode, json!({ "active": active }))
}

fn subagent_descriptor(seq: u64, data: Value) -> SessionEvent {
    ev(seq, EventKind::SubagentDescriptor, data)
}

/// 三 unit 注册进一个注册表 → 键唯一、无冲突。
#[test]
fn three_units_register_together() {
    let mut reg = ProjectionRegistry::new();
    for unit in m4_projection_units() {
        reg.register(unit).expect("unique keys register ok");
    }
    // 幂等重注册（同 stateVersion）不冲突
    for unit in m4_projection_units() {
        reg.register(unit).expect("re-register same key+version is no-op");
    }
    assert!(reg.get("goal").is_some());
    assert!(reg.get("plan").is_some());
    assert!(reg.get("subagent").is_some());
    assert!(reg.get("todos").is_none());
}

/// goal：空 → view.goal == null（键在）；喂 create snapshot → view.goal 是含
/// id/revision/phase 的对象；再喂 clear 墓碑 → goal 回到 null。
#[test]
fn goal_projection_empty_create_clear() {
    let mut reg = ProjectionRegistry::new();
    reg.register(goal_projection_unit().into_unit()).unwrap();
    let mut session = ProjectionSession::new(&reg);

    let snap = session.snapshot();
    let v = &snap.values["goal"];
    assert_eq!(v["goal"], json!(null), "空日志 view.goal 应为 null 而非缺键");
    assert!(v.get("roundsStarted").is_some());
    assert!(v.get("createdAt").is_some());
    assert!(v.get("updatedAt").is_some());

    let e = goal_ev(0, goal_snapshot_data("goal-1", 1, "ship M4", "active", 256, 0, 1000, 1000));
    session.observe(&e);
    let snap = session.snapshot();
    let v = &snap.values["goal"];
    let goal = v["goal"].as_object().expect("create 后 goal 为对象");
    assert_eq!(goal["id"], json!("goal-1"));
    assert_eq!(goal["revision"], json!(1));
    assert_eq!(goal["objective"], json!("ship M4"));
    assert_eq!(goal["phase"], json!("active"));
    assert_eq!(goal["maxGoalRounds"], json!(256));
    assert_eq!(v["roundsStarted"], json!(0));

    let clear = goal_ev(1, goal_clear_data("goal-1", 1));
    session.observe(&clear);
    let snap = session.snapshot();
    assert_eq!(snap.values["goal"]["goal"], json!(null), "clear 后 goal 回 null");
}

/// goal：无关事件不影响投影；malformed goal/change 保持原值（不崩）。
#[test]
fn goal_ignores_unrelated_and_malformed() {
    let mut reg = ProjectionRegistry::new();
    reg.register(goal_projection_unit().into_unit()).unwrap();
    let mut session = ProjectionSession::new(&reg);

    let e = goal_ev(0, goal_snapshot_data("goal-1", 1, "ship", "active", 16, 0, 1, 1));
    session.observe(&e);
    let after_goal = session.snapshot().values["goal"].clone();

    // 无关事件
    session.observe(&ev(1, EventKind::TurnStart, json!({})));
    assert_eq!(session.snapshot().values["goal"], after_goal);

    // malformed goal/change（缺 goal 字段）→ decode Err → 保持
    session.observe(&goal_ev(2, json!({ "kind": "goal/change", "version": 1, "operation": "create" })));
    assert_eq!(session.snapshot().values["goal"], after_goal);
}

/// plan：空 → {active:false, pending:false}；plan/mode active=true → active 判对。
#[test]
fn plan_projection_empty_and_mode() {
    let mut reg = ProjectionRegistry::new();
    reg.register(plan_projection_unit().into_unit()).unwrap();
    let mut session = ProjectionSession::new(&reg);

    let snap = session.snapshot();
    assert_eq!(snap.values["plan"], json!({ "active": false, "pending": false }));

    session.observe(&plan_mode(0, true));
    assert_eq!(session.snapshot().values["plan"], json!({ "active": true, "pending": false }));

    session.observe(&plan_mode(1, false));
    assert_eq!(session.snapshot().values["plan"], json!({ "active": false, "pending": false }));
}

/// plan：command/run(name=plan, args≠off) → pending=true；配对 command/done
/// success → 保持 wanted；plan/mode 落定 → pending=false。
#[test]
fn plan_projection_command_pending_path() {
    let mut reg = ProjectionRegistry::new();
    reg.register(plan_projection_unit().into_unit()).unwrap();
    let mut session = ProjectionSession::new(&reg);

    session.observe(&ev(
        0,
        EventKind::CommandRun,
        json!({ "name": "plan", "args": "build the crates", "commandId": "cmd-1" }),
    ));
    assert_eq!(
        session.snapshot().values["plan"],
        json!({ "active": false, "pending": true }),
        "wanted=true 未落定 → pending"
    );

    session.observe(&ev(1, EventKind::CommandDone, json!({ "commandId": "cmd-1", "kind": "success" })));
    assert_eq!(
        session.snapshot().values["plan"],
        json!({ "active": false, "pending": true }),
        "success done → wanted 保留 → 仍 pending"
    );

    session.observe(&plan_mode(2, true));
    assert_eq!(
        session.snapshot().values["plan"],
        json!({ "active": true, "pending": false }),
        "plan/mode 落定 → active + 清 wanted"
    );
}

/// subagent：无事件 → view null；喂 one-shot descriptor → {mode,label?,seq}；
/// 无关事件不影响。
#[test]
fn subagent_projection_descriptor() {
    let mut reg = ProjectionRegistry::new();
    reg.register(subagent_projection_unit().into_unit()).unwrap();
    let mut session = ProjectionSession::new(&reg);

    let snap = session.snapshot();
    assert_eq!(snap.values["subagent"], json!(null), "无 descriptor → 空(null)");

    let e = subagent_descriptor(
        0,
        json!({ "version": 2, "mode": "one-shot", "provider": "mock", "label": "L1" }),
    );
    session.observe(&e);
    let v = &session.snapshot().values["subagent"];
    assert_eq!(v["mode"], json!("one-shot"));
    assert_eq!(v["label"], json!("L1"));
    assert_eq!(v["seq"], json!(0));

    // 无关事件不改变
    session.observe(&ev(1, EventKind::TurnStart, json!({})));
    let v = &session.snapshot().values["subagent"];
    assert_eq!(v["mode"], json!("one-shot"));

    // 无 label 的 one-shot：label 键缺省（对齐 TS optional label）
    let mut s2 = ProjectionSession::new(&reg);
    let e2 = subagent_descriptor(3, json!({ "version": 2, "mode": "one-shot", "provider": "mock" }));
    s2.observe(&e2);
    let v2 = &s2.snapshot().values["subagent"];
    assert_eq!(v2["mode"], json!("one-shot"));
    assert!(v2.get("label").is_none(), "one-shot 无 label 时键缺省");
    assert_eq!(v2["seq"], json!(3));
}

/// subagent：版本不符/当前版本结构坏 → 复位 null（TS：不可信 payload → 无值）。
#[test]
fn subagent_untrusted_resets_to_null() {
    let mut reg = ProjectionRegistry::new();
    reg.register(subagent_projection_unit().into_unit()).unwrap();
    let mut session = ProjectionSession::new(&reg);

    // 先建立可信身份
    session.observe(&subagent_descriptor(
        0,
        json!({ "version": 2, "mode": "continuable", "provider": "mock", "label": "A" }),
    ));
    assert_eq!(session.snapshot().values["subagent"]["mode"], json!("continuable"));

    // 版本不符 → 复位 null
    session.observe(&subagent_descriptor(
        1,
        json!({ "version": 99, "mode": "one-shot", "provider": "mock" }),
    ));
    assert_eq!(session.snapshot().values["subagent"], json!(null));

    // 当前版本但结构坏（mode 非法）→ 复位 null
    session.observe(&subagent_descriptor(
        2,
        json!({ "version": 2, "mode": "banana", "provider": "mock" }),
    ));
    assert_eq!(session.snapshot().values["subagent"], json!(null));
}

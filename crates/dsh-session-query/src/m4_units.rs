//! M4h 补齐：`goal` / `plan` / `subagent` 三个投影单元构造器（供 `ProjectionRegistry`
//! 注册，web.rs/Boot 装配）。
//!
//! 对齐 TS 参考（每一键的 stateVersion 沿用「本 Rust 移植镜像 TS 参考值」的项目惯例，
//! 与已落地的 `todos` unit 一致——其 stateVersion 亦等于 TS tool-todo 参考值）：
//! - `goal`：`packages/goal/goal/src/index.ts`（key 'goal'，apply 只响应 `goal/change`，
//!   view = 整值 `{goal: null|snapshot, roundsStarted, createdAt, updatedAt}`；
//!   与 TS 的「整体 null」不同，本 Rust 以 `GoalProjection` wire 形状输出——`goal` 键恒在、
//!   无目标/clear 时为 `null`，其余计数在无目标时归零，见 [`goal init`] 说明）；
//! - `plan`：`packages/plan/plan-mode/src/index.ts`（key 'plan'，view
//!   `{active, pending}`；折叠复用 `dsh_plan::plan_unit_apply`，状态以 JSON 镜像承载，
//!   不重复实现 fold 语义）；
//! - `subagent`：`packages/subagent/subagent/src/projection.ts`
//!   `subagentIdentityProjectionDefinition`（key 'subagent'，view
//!   `{mode, label?, seq} | null`，last-wins 身份；不可信 payload 复位 null）。
//!
//! 每个单元保持与 `todo::TodosProjection` 同构：`{init, apply, view}` 三枚纯函数 +
//! `into_unit()` 产出 `ProjectionUnit`（闭包均为非捕获 fn，天然 `'static`）。

use dsh_session::types::{EventKind, SessionEvent};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// goal 投影单元
// ---------------------------------------------------------------------------

/// `goal` 投影单元的处理函数集（init/apply/view 都是纯函数，够注册进 Registry）。
///
/// stateVersion = 4（镜像 TS goal 参考值；TS 的 stateVersion 是投影缓存失效版本，
/// 与 `GOAL_CHANGE_VERSION` 载荷版本无关——本移植沿用「镜像 TS 参考」惯例，见模块头）。
pub struct GoalProjection {
    /// 初始态：无目标（`goal: null`，四个键恒在——键值而非缺键）。
    pub init: fn() -> Value,
    /// 折叠：`goal/change` snapshot → 目标对象 + 计数；`goal/change` clear → 目标
    /// 回 null（计数归零）；malformed/其它事件保持。
    pub apply: fn(&mut Value, &SessionEvent),
    /// 视图：整值即投影（state == wire）。
    pub view: fn(&Value) -> Value,
}

impl GoalProjection {
    /// 注册进 `ProjectionRegistry` 的 unit 形状。
    pub fn into_unit(self) -> crate::projection::ProjectionUnit {
        crate::projection::ProjectionUnit::new("goal", 4, self.init, self.apply, self.view)
    }
}

fn goal_view(state: &Value) -> Value {
    state.clone()
}

fn goal_apply(state: &mut Value, event: &SessionEvent) {
    if event.kind != EventKind::GoalChange {
        return;
    }
    match dsh_goal::fold::decode_goal_change(&event.data) {
        Some(Ok(dsh_goal::types::GoalChangeMeta::Clear(_))) => {
            // clear 墓碑 → 无目标；计数归零（中性哨兵——无目标即无可派生计数）
            *state = goal_init();
        }
        Some(Ok(dsh_goal::types::GoalChangeMeta::Snapshot(meta))) => {
            *state = json!({
                "goal": meta.goal,
                "roundsStarted": meta.rounds_started,
                "createdAt": meta.created_at,
                "updatedAt": meta.updated_at,
            });
        }
        _ => {
            // malformed / 非目标载荷 → 保持（TS applyGoalProjection catch 后返回同态）
        }
    }
}

fn goal_init() -> Value {
    json!({ "goal": null, "roundsStarted": 0, "createdAt": 0, "updatedAt": 0 })
}

/// `goal` 投影单元（stand-alone 供测试/宿主注册）。
pub fn goal_projection_unit() -> GoalProjection {
    GoalProjection {
        init: goal_init,
        apply: goal_apply,
        view: goal_view,
    }
}

// ---------------------------------------------------------------------------
// plan 投影单元
// ---------------------------------------------------------------------------

/// `plan` 投影单元的处理函数集。
///
/// stateVersion = 2（镜像 TS plan 参考值）。fold 语义不在本模块重实现：内部状态以
/// JSON 镜像 `dsh_plan::PlanUnitState`，`apply`/`view` 都经镜像换算复用
/// `dsh_plan::plan_unit_apply` 与 `dsh_plan::plan_projection_view`（单一事实源）。
pub struct PlanProjection {
    /// 初始态：inactive、无待定选择、无在途命令。
    pub init: fn() -> Value,
    /// 折叠：`command/run`/`command/done`/`plan/mode`（复用 dsh_plan fold）。
    pub apply: fn(&mut Value, &SessionEvent),
    /// 视图：`{active, pending}`（复用 dsh_plan 投影）。
    pub view: fn(&Value) -> Value,
}

impl PlanProjection {
    /// 注册进 `ProjectionRegistry` 的 unit 形状。
    pub fn into_unit(self) -> crate::projection::ProjectionUnit {
        crate::projection::ProjectionUnit::new("plan", 2, self.init, self.apply, self.view)
    }
}

/// 内部状态 JSON → `dsh_plan::PlanUnitState`（镜像换算，非重复逻辑）。
fn plan_state_from_value(v: &Value) -> dsh_plan::PlanUnitState {
    dsh_plan::PlanUnitState {
        active: v.get("active").and_then(Value::as_bool).unwrap_or(false),
        wanted: v.get("wanted").and_then(Value::as_bool),
        running: v
            .get("running")
            .and_then(|r| if r.is_null() { None } else { Some(r) })
            .map(|r| dsh_plan::RunningCommand {
                command_id: r
                    .get("commandId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                wanted: r.get("wanted").and_then(Value::as_bool).unwrap_or(false),
            }),
    }
}

/// `dsh_plan::PlanUnitState` → 内部状态 JSON（镜像，键名对齐 TS `PlanUnitState`）。
fn plan_state_to_value(ps: &dsh_plan::PlanUnitState) -> Value {
    json!({
        "active": ps.active,
        "wanted": ps.wanted,
        "running": ps.running.as_ref().map(|r| json!({
            "commandId": r.command_id,
            "wanted": r.wanted,
        })),
    })
}

fn plan_init() -> Value {
    json!({ "active": false, "wanted": null, "running": null })
}

fn plan_apply(state: &mut Value, event: &SessionEvent) {
    let mut ps = plan_state_from_value(state);
    dsh_plan::plan_unit_apply(&mut ps, event);
    *state = plan_state_to_value(&ps);
}

fn plan_view(state: &Value) -> Value {
    let ps = plan_state_from_value(state);
    dsh_plan::plan_projection_view(&ps)
}

/// `plan` 投影单元（stand-alone 供测试/宿主注册）。
pub fn plan_projection_unit() -> PlanProjection {
    PlanProjection {
        init: plan_init,
        apply: plan_apply,
        view: plan_view,
    }
}

// ---------------------------------------------------------------------------
// subagent 投影单元
// ---------------------------------------------------------------------------

/// `subagent` 投影单元的处理函数集。
///
/// stateVersion = 2（镜像 TS `subagentIdentityProjectionDefinition` 参考值）。
/// 语义：本会话（per-session/subagent log）last-wins 的身份——`subagent/descriptor`
/// 事件上经 `dsh_subagent::fold_descriptor_from_events` 解析；版本不符或当前版本
/// 结构坏（不可信 payload）→ 复位为无值（TS：`descriptorIdentity` undefined）。
pub struct SubagentProjection {
    /// 初始态：无身份（state = `{}`，view 出 `null`）。
    pub init: fn() -> Value,
    /// 折叠：`subagent/descriptor` → `{identity: {mode, label?, seq}}`；其它事件保持。
    pub apply: fn(&mut Value, &SessionEvent),
    /// 视图：`{mode, label?, seq} | null`（identity 或缺省）。
    pub view: fn(&Value) -> Value,
}

impl SubagentProjection {
    /// 注册进 `ProjectionRegistry` 的 unit 形状。
    pub fn into_unit(self) -> crate::projection::ProjectionUnit {
        crate::projection::ProjectionUnit::new("subagent", 2, self.init, self.apply, self.view)
    }
}

fn subagent_init() -> Value {
    json!({})
}

/// 把 `Descriptor` 折叠为 TS 形状的身份 `{mode, label?, seq}`。
fn subagent_identity(desc: &dsh_subagent::Descriptor, seq: u64) -> Value {
    match desc {
        dsh_subagent::Descriptor::OneShot { label, .. } => {
            let mut obj = serde_json::Map::new();
            obj.insert("mode".into(), Value::String("one-shot".into()));
            if let Some(label) = label {
                obj.insert("label".into(), Value::String(label.clone()));
            }
            obj.insert("seq".into(), Value::from(seq));
            Value::Object(obj)
        }
        dsh_subagent::Descriptor::Continuable { label, .. } => json!({
            "mode": "continuable",
            "label": label,
            "seq": seq,
        }),
    }
}

fn subagent_apply(state: &mut Value, event: &SessionEvent) {
    if event.kind != EventKind::SubagentDescriptor {
        return;
    }
    // 以 dsh-subagent 的 fold 做权威解析：单条事件 JSON 帧（type + data）即可。
    let frame = json!({ "type": event.kind.as_str(), "data": event.data });
    match dsh_subagent::fold_descriptor_from_events(&[frame]) {
        Ok(Some(desc)) => {
            *state = json!({ "identity": subagent_identity(&desc, event.seq) });
        }
        _ => {
            // 不可信 payload（版本不符 / 当前版本结构坏）→ 复位无值
            *state = subagent_init();
        }
    }
}

fn subagent_view(state: &Value) -> Value {
    state.get("identity").cloned().unwrap_or(Value::Null)
}

/// `subagent` 投影单元（stand-alone 供测试/宿主注册）。
pub fn subagent_projection_unit() -> SubagentProjection {
    SubagentProjection {
        init: subagent_init,
        apply: subagent_apply,
        view: subagent_view,
    }
}

// ---------------------------------------------------------------------------
// 聚合构造（M4h 注册批量）
// ---------------------------------------------------------------------------

/// 一次注册 M4h 三键（goal/plan/subagent）的构造器列表。
pub fn m4_projection_units() -> Vec<crate::projection::ProjectionUnit> {
    vec![
        goal_projection_unit().into_unit(),
        plan_projection_unit().into_unit(),
        subagent_projection_unit().into_unit(),
    ]
}

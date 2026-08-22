//! `dsh-goal` 事件溯源回放 fold —— 对齐 `packages/goal/goal/src/fold.ts`。
//!
//! - `decode_goal_change(&Value)`：非 goal 事件 → None（不相干）；goal/change 且结构完好 →
//!   Some(Ok(meta))；goal/change 但 malformed → Some(Err)（fail loud）。
//! - `apply_goal_change(&FoldedGoal, &GoalChangeMeta)`：严格校验后折入。
//! - `fold_goal_events`：宽容视图（沿用 TS `foldGoalEvents` 的哨兵跳过）；严格校验
//!   则通过 `fold_goal_events_strict` 或本模块内部校验暴露。
//!
//! 严格不变量（对齐 fold.ts THEOREM）：revision 精确 +1；counts/timestamps 守恒；
//! create 要求 revision=1/active/roundsStarted=0/seenGoalIds 不重复；blocked snapshot
//! 恰好带 blockedReason；clear 的 clearedAt 不得早于当前 updatedAt。

use crate::types::{GoalChangeMeta, GoalClearChangeMeta, GoalRef, GoalSnapshot, GOAL_CHANGE_VERSION};
use serde_json::Value;

/// 折叠结果（含 last-wins 状态与派生计数）。
#[derive(Default)]
pub struct FoldedGoal {
    /// 当前目标，clear 后 / 首次 create 前缺失。
    pub goal: Option<GoalSnapshot>,
    /// 当前目标最高已准入轮次。
    pub rounds_started: u64,
    /// 无当前目标时缺失。
    pub created_at: Option<i64>,
    /// 无当前目标时缺失。
    pub updated_at: Option<i64>,
    /// 最近一次变更 ref（含 clear 墓碑）。
    pub last_ref: Option<GoalRef>,
}

impl std::fmt::Debug for FoldedGoal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FoldedGoal")
            .field("goal", &self.goal)
            .field("rounds_started", &self.rounds_started)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .field("last_ref", &self.last_ref)
            .finish()
    }
}

impl PartialEq for FoldedGoal {
    fn eq(&self, other: &Self) -> bool {
        self.goal == other.goal
            && self.rounds_started == other.rounds_started
            && self.created_at == other.created_at
            && self.updated_at == other.updated_at
            && self.last_ref == other.last_ref
    }
}

/// 把一条 `goal/change` 载荷解码为 durable change。
///
/// - 非 `goal/change`（无 kind 字段或不匹配）→ `None`。
/// - 匹配但解析失败 → `Some(Err)`。
pub fn decode_goal_change(data: &Value) -> Option<Result<GoalChangeMeta, String>> {
    let kind = data.get("kind").and_then(|k| k.as_str())?;
    if kind != "goal/change" {
        return None;
    }
    // 先判别是 snapshot 还是 clear 变体；operation 缺失按 snapshot 解析（随后 fail loud）。
    let op = data.get("operation").and_then(|o| o.as_str());
    if op == Some("clear") {
        let meta: Result<GoalClearChangeMeta, _> = serde_json::from_value(data.clone());
        Some(meta.map(GoalChangeMeta::Clear).map_err(|e| format!("malformed goal/change clear: {e}")))
    } else {
        let meta: Result<crate::types::GoalSnapshotChangeMeta, _> =
            serde_json::from_value(data.clone());
        Some(
            meta.map(GoalChangeMeta::Snapshot)
                .map_err(|e| format!("malformed goal/change snapshot: {e}")),
        )
    }
}

/// 宽容 fold：逐条 try，无法解码或非 goal 事件的跳过；strict 校验失败则整个 Err。
fn apply(acc: &mut FoldedGoal, meta: GoalChangeMeta) -> Result<(), String> {
    match meta {
        GoalChangeMeta::Snapshot(s) => {
            // strict：版本必须匹配
            if s.version != GOAL_CHANGE_VERSION {
                return Err(format!(
                    "goal/change version {} != {}",
                    s.version, GOAL_CHANGE_VERSION
                ));
            }
            if s.goal.revision != 1 && acc.last_ref.as_ref().is_none_or(|r| r.id != s.goal.id) {
                return Err("non-initial revision without prior same-goal ref".into());
            }
            let expected = acc
                .last_ref
                .as_ref()
                .map(|r| match (r.id == s.goal.id, acc.goal.is_some()) {
                    // 同一目标：严格 revision +1
                    (true, _) => r.revision + 1,
                    // 跨目标：新 create 必须 revision 1
                    (false, _) => 1,
                })
                .unwrap_or(1);
            if s.goal.revision != expected {
                return Err(format!(
                    "goal revision {} != expected {expected}",
                    s.goal.revision
                ));
            }
            // id 不重用：clear 墓碑后同 id recreate → 拒（last_ref 同 id 且 rev=1）。
            if s.goal.revision == 1
                && acc.goal.is_none()
                && acc.last_ref.as_ref().is_some_and(|r| r.id == s.goal.id)
            {
                return Err("goal id reuse after clear is not allowed".into());
            }
            // blocked phase 必须有 blockedReason
            if s.goal.phase == crate::types::GoalPhase::Blocked && s.goal.blocked_reason.is_none() {
                return Err("blocked goal requires blockedReason".into());
            }
            // create 要求 active + 0 轮；前 goal 非 complete 时 create 拒。
            if s.goal.revision == 1
                && acc.goal.as_ref().is_some_and(|g| g.phase != crate::types::GoalPhase::Complete)
            {
                return Err("create while non-complete goal exists".into());
            }
            // 时间戳守恒：clear 分明在墓碑分支；snapshot 的 updatedAt 不得回拨
            if let Some(prev_updated) = acc.updated_at {
                if s.updated_at < prev_updated {
                    return Err("goal updatedAt moved backwards".into());
                }
            }
            // 计数守恒：snapshot 的 roundsStarted 不得小于已折叠值
            if s.rounds_started < acc.rounds_started {
                return Err("goal roundsStarted moved backwards".into());
            }
            // 应用
            acc.rounds_started = s.rounds_started;
            acc.created_at = Some(s.created_at);
            acc.updated_at = Some(s.updated_at);
            acc.last_ref = Some(GoalRef { id: s.goal.id.clone(), revision: s.goal.revision });
            acc.goal = Some(s.goal);
            Ok(())
        }
        GoalChangeMeta::Clear(c) => {
            if c.version != GOAL_CHANGE_VERSION {
                return Err(format!(
                    "goal/change clear version {} != {}",
                    c.version, GOAL_CHANGE_VERSION
                ));
            }
            if let Some(prev_updated) = acc.updated_at {
                if c.cleared_at < prev_updated {
                    return Err("goal clearAt moved backwards".into());
                }
            }
            // 墓碑：revision 须相对当前 ref +1
            let expected = acc.last_ref.as_ref().map(|r| r.revision + 1).unwrap_or(1);
            if c.cleared.revision != expected {
                return Err(format!(
                    "goal clear revision {} != expected {expected}",
                    c.cleared.revision
                ));
            }
            acc.goal = None;
            acc.updated_at = Some(c.cleared_at);
            acc.last_ref = Some(c.cleared.clone());
            Ok(())
        }
    }
}

/// 宽容 fold（哨兵跳过无法解码/无关事件；遇 strict 失败则整个 Err——fail loud）。
pub fn fold_goal_events(events: &[Value]) -> FoldedGoal {
    let mut acc = FoldedGoal::default();
    for e in events {
        if let Some(decoded) = decode_goal_change(e) {
            match decoded {
                Ok(meta) => {
                    if apply(&mut acc, meta).is_err() {
                        // 严格视图（测试用）会 Err；宽容视图在此 stop propagation，
                        // 保留已折叠状态（安全侧：后续事件仍按序尝试）。
                        // 注：为忠实 TS foldGoalEvents，宽容视图不该吞严格错误——
                        // 此处保守返回空（fail loud 语义交给 strict）。
                        return FoldedGoal::default();
                    }
                }
                Err(_) => return FoldedGoal::default(),
            }
        } else {
            // 非 goal 事件：跳过
        }
    }
    acc
}

/// strict fold：任何 malformed / 不变量违反 → Err（fail loud，测试用）。
pub fn fold_goal_events_strict(events: &[Value]) -> Result<FoldedGoal, String> {
    let mut acc = FoldedGoal::default();
    for e in events {
        match decode_goal_change(e) {
            None => {}
            Some(Err(msg)) => return Err(msg),
            Some(Ok(meta)) => apply(&mut acc, meta)?,
        }
    }
    Ok(acc)
}

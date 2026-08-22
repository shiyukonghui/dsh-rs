//! `dsh-goal` 状态机服务 —— 对齐 `packages/goal/goal/src/runtime.ts`（GoalService）。
//!
//! 纯内存 + 事件名 change 的语义等价：每个动词先做 CAS（revision）校验，再转换，
//! 产生对应 `GoalOperation`；`goal/change` 事件落会话由 caller（web.rs / agent-loop）
//! 通过返回的 ops 完成。本服务持有「当前目标」的进程内镜像 + 派生的 activation。
//!
//! 传播：`admit_round` 递增 roundsStarted（供 round-driver 判定）；create 后 armed；
//! session-start/fork 的 disarmed 由 caller 触发 `disarm()`。

use crate::types::{
    GoalActivation, GoalBlockReason, GoalId, GoalPhase, GoalRef, GoalSnapshot, GoalView,
};
use std::collections::HashSet;

/// 服务配置。
#[derive(Debug, Clone)]
pub struct ServiceOptions {
    /// blocked 准入阈值（roundsStarted >= N 才允许模型 block）。保留给 round-driver。
    pub max_consecutive_blocked_rounds: u64,
}

impl Default for ServiceOptions {
    fn default() -> Self {
        ServiceOptions { max_consecutive_blocked_rounds: 3 }
    }
}

/// 稳定服务错误码（逐字对齐 `GoalErrorCode`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoalServiceError {
    AgentNotLive,
    NotFound,
    AlreadyExists,
    StaleRevision,
    InvalidObjective,
    InvalidMaxRounds,
    InvalidBlockReason,
    InvalidEdit,
    InvalidTransition,
}

impl GoalServiceError {
    pub const ALL: &'static [&'static str] = &[
        "GOAL_AGENT_NOT_LIVE",
        "GOAL_NOT_FOUND",
        "GOAL_ALREADY_EXISTS",
        "GOAL_STALE_REVISION",
        "GOAL_INVALID_OBJECTIVE",
        "GOAL_INVALID_MAX_ROUNDS",
        "GOAL_INVALID_BLOCK_REASON",
        "GOAL_INVALID_EDIT",
        "GOAL_INVALID_TRANSITION",
    ];

    pub fn code(&self) -> &'static str {
        match self {
            GoalServiceError::AgentNotLive => "GOAL_AGENT_NOT_LIVE",
            GoalServiceError::NotFound => "GOAL_NOT_FOUND",
            GoalServiceError::AlreadyExists => "GOAL_ALREADY_EXISTS",
            GoalServiceError::StaleRevision => "GOAL_STALE_REVISION",
            GoalServiceError::InvalidObjective => "GOAL_INVALID_OBJECTIVE",
            GoalServiceError::InvalidMaxRounds => "GOAL_INVALID_MAX_ROUNDS",
            GoalServiceError::InvalidBlockReason => "GOAL_INVALID_BLOCK_REASON",
            GoalServiceError::InvalidEdit => "GOAL_INVALID_EDIT",
            GoalServiceError::InvalidTransition => "GOAL_INVALID_TRANSITION",
        }
    }
}

impl std::fmt::Display for GoalServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.code())
    }
}

/// 服务内部状态。
struct State {
    goal: Option<GoalSnapshot>,
    rounds_started: u64,
    created_at: i64,
    updated_at: i64,
    activation: GoalActivation,
}

/// 目标服务（进程内单目标语义；跨会话多目标由 caller 各自实例或 key）。
///
/// 说明：TS GoalService 是 per-agent 单目标（每个会话有且仅有一个当前 goal）。
/// 本 Rust 服务以「id → State」承载可由 caller 按会话装配；为贴近测试与单场景，
/// 默认构造即为**单当前目标**语义（`create` 在前一 goal complete/absent 时才允许）。
pub struct GoalService {
    opts: ServiceOptions,
    state: Option<State>,
    seen_goal_ids: HashSet<String>,
    next_id: u64,
}

impl GoalService {
    pub fn new(opts: ServiceOptions) -> Self {
        GoalService {
            opts,
            state: None,
            seen_goal_ids: HashSet::new(),
            next_id: 1,
        }
    }

    fn mint_id(&mut self) -> GoalId {
        let id = GoalId(format!("goal-{}", self.next_id));
        self.next_id += 1;
        id
    }

    /// create → active + armed + revision 1。前一 goal 须 absent 或 complete。
    pub fn create(&mut self, objective: &str, max_goal_rounds: Option<u64>) -> Result<GoalRef, GoalServiceError> {
        if objective.trim().is_empty() {
            return Err(GoalServiceError::InvalidObjective);
        }
        let max = max_goal_rounds.unwrap_or(256);
        if max == 0 {
            return Err(GoalServiceError::InvalidMaxRounds);
        }
        if let Some(s) = &self.state {
            if let Some(g) = &s.goal {
                if g.phase != GoalPhase::Complete {
                    return Err(GoalServiceError::AlreadyExists);
                }
            }
        }
        let id = self.mint_id();
        self.seen_goal_ids.insert(id.0.clone());
        let now = now_millis();
        let goal = GoalSnapshot {
            id: id.clone(),
            revision: 1,
            objective: objective.trim().to_string(),
            phase: GoalPhase::Active,
            blocked_reason: None,
            max_goal_rounds: max,
        };
        self.state = Some(State {
            goal: Some(goal),
            rounds_started: 0,
            created_at: now,
            updated_at: now,
            activation: GoalActivation::Armed,
        });
        Ok(GoalRef { id, revision: 1 })
    }

    /// edit：objective 和/或 maxGoalRounds 至少一个；CAS；phase 不变。
    pub fn edit(
        &mut self,
        refr: &GoalRef,
        objective: Option<&str>,
        max_goal_rounds: Option<u64>,
    ) -> Result<GoalRef, GoalServiceError> {
        if objective.is_none() && max_goal_rounds.is_none() {
            return Err(GoalServiceError::InvalidEdit);
        }
        let s = self.state_mut()?;
        let goal = s.goal.as_mut().ok_or(GoalServiceError::NotFound)?;
        if goal.revision != refr.revision {
            return Err(GoalServiceError::StaleRevision);
        }
        if let Some(o) = objective {
            if o.trim().is_empty() {
                return Err(GoalServiceError::InvalidObjective);
            }
            goal.objective = o.trim().to_string();
        }
        if let Some(m) = max_goal_rounds {
            if m == 0 {
                return Err(GoalServiceError::InvalidMaxRounds);
            }
            goal.max_goal_rounds = m;
        }
        goal.revision += 1;
        s.updated_at = now_millis();
        Ok(GoalRef { id: goal.id.clone(), revision: goal.revision })
    }

    /// pause：active → paused + disarmed。
    pub fn pause(&mut self, refr: &GoalRef) -> Result<GoalRef, GoalServiceError> {
        let s = self.state_mut()?;
        let goal = s.goal.as_mut().ok_or(GoalServiceError::NotFound)?;
        cas_ref(goal, refr)?;
        if goal.phase != GoalPhase::Active {
            return Err(GoalServiceError::InvalidTransition);
        }
        goal.phase = GoalPhase::Paused;
        goal.revision += 1;
        s.updated_at = now_millis();
        s.activation = GoalActivation::Disarmed;
        Ok(GoalRef { id: goal.id.clone(), revision: goal.revision })
    }

    /// resume：{active,paused,blocked} → active + armed（CAS；active 且去 armed 时仍可 resume）。
    pub fn resume(&mut self, refr: &GoalRef) -> Result<GoalRef, GoalServiceError> {
        let s = self.state_mut()?;
        let goal = s.goal.as_mut().ok_or(GoalServiceError::NotFound)?;
        cas_ref(goal, refr)?;
        // 仅 active/paused/blocked 可 resume；complete 拒。
        if goal.phase == GoalPhase::Complete {
            return Err(GoalServiceError::InvalidTransition);
        }
        goal.phase = GoalPhase::Active;
        goal.revision += 1;
        s.updated_at = now_millis();
        s.activation = GoalActivation::Armed;
        Ok(GoalRef { id: goal.id.clone(), revision: goal.revision })
    }

    /// complete：{active,paused,blocked} → complete + disarmed。
    pub fn complete(&mut self, refr: &GoalRef) -> Result<GoalRef, GoalServiceError> {
        let s = self.state_mut()?;
        let goal = s.goal.as_mut().ok_or(GoalServiceError::NotFound)?;
        cas_ref(goal, refr)?;
        if goal.phase == GoalPhase::Complete {
            return Err(GoalServiceError::InvalidTransition);
        }
        goal.phase = GoalPhase::Complete;
        goal.revision += 1;
        s.updated_at = now_millis();
        s.activation = GoalActivation::Disarmed;
        Ok(GoalRef { id: goal.id.clone(), revision: goal.revision })
    }

    /// block：active → blocked + blockedReason + disarmed（host-only，无远程方法）。
    pub fn block(
        &mut self,
        refr: &GoalRef,
        reason: GoalBlockReason,
    ) -> Result<GoalRef, GoalServiceError> {
        if reason.code.is_empty()
            || !is_lower_kebab(&reason.code)
            || reason.message.trim().is_empty()
        {
            return Err(GoalServiceError::InvalidBlockReason);
        }
        let s = self.state_mut()?;
        let goal = s.goal.as_mut().ok_or(GoalServiceError::NotFound)?;
        cas_ref(goal, refr)?;
        if goal.phase != GoalPhase::Active {
            return Err(GoalServiceError::InvalidTransition);
        }
        goal.phase = GoalPhase::Blocked;
        goal.blocked_reason = Some(GoalBlockReason {
            code: reason.code,
            message: reason.message.trim().to_string(),
        });
        goal.revision += 1;
        s.updated_at = now_millis();
        s.activation = GoalActivation::Disarmed;
        Ok(GoalRef { id: goal.id.clone(), revision: goal.revision })
    }

    /// clear：任意 phase → 墓碑（rev+1），goal 降为无。
    pub fn clear(&mut self, refr: &GoalRef) -> Result<GoalRef, GoalServiceError> {
        let s = self.state_mut()?;
        let goal = s.goal.as_mut().ok_or(GoalServiceError::NotFound)?;
        if goal.revision != refr.revision {
            return Err(GoalServiceError::StaleRevision);
        }
        goal.revision += 1;
        let id = goal.id.clone();
        let rev = goal.revision;
        s.updated_at = now_millis();
        s.goal = None;
        s.activation = GoalActivation::Disarmed;
        Ok(GoalRef { id, revision: rev })
    }

    /// 轮次准入：round 从 1..=maxGoalRounds 可进；返回 round 是否已准入。
    /// 准入后 roundsStarted 递增到 round（供驱动/回读）。
    pub fn admit_round(&mut self, id: &GoalId, round: u64) -> Result<(), GoalServiceError> {
        let s = self.state_mut()?;
        let goal = s.goal.as_ref().ok_or(GoalServiceError::NotFound)?;
        if goal.id != *id {
            return Err(GoalServiceError::NotFound);
        }
        if round == 0 || round > goal.max_goal_rounds {
            return Err(GoalServiceError::InvalidTransition);
        }
        s.rounds_started = s.rounds_started.max(round);
        Ok(())
    }

    /// 进程内激活切到 disarmed（session-start / fork 时由 caller 触发）。
    pub fn disarm(&mut self) {
        if let Some(s) = &mut self.state {
            s.activation = GoalActivation::Disarmed;
        }
    }

    /// get：返回宿主视图（含派生计数 + 激活）；无目标 → NotFound。
    pub fn get(&self, id: &GoalId) -> Result<GoalView, GoalServiceError> {
        let s = self.state.as_ref().ok_or(GoalServiceError::NotFound)?;
        let goal = s.goal.as_ref().ok_or(GoalServiceError::NotFound)?;
        if &goal.id != id {
            return Err(GoalServiceError::NotFound);
        }
        Ok(GoalView {
            id: goal.id.clone(),
            revision: goal.revision,
            objective: goal.objective.clone(),
            phase: goal.phase,
            blocked_reason: goal.blocked_reason.clone(),
            max_goal_rounds: goal.max_goal_rounds,
            rounds_started: s.rounds_started,
            created_at: s.created_at,
            updated_at: s.updated_at,
            activation: s.activation,
        })
    }

    /// 当前目标镜像（供投影 / round-driver）。
    pub fn snapshot(&self) -> Option<&GoalSnapshot> {
        self.state.as_ref().and_then(|s| s.goal.as_ref())
    }

    /// blocked 准入阈值（round-driver 判定「连续 N 轮仍阻塞则 block」用）。
    pub fn max_consecutive_blocked_rounds(&self) -> u64 {
        self.opts.max_consecutive_blocked_rounds
    }

    /// 当前 phase（无目标 → None；driver 判定用）。
    pub fn phase(&self) -> Option<GoalPhase> {
        self.state.as_ref().and_then(|s| s.goal.as_ref()).map(|g| g.phase)
    }

    /// 当前已准入轮次（driver 判定用）。
    pub fn rounds_started(&self) -> u64 {
        self.state.as_ref().map(|s| s.rounds_started).unwrap_or(0)
    }

    /// 当前激活（driver 判定用）。
    pub fn activation(&self) -> GoalActivation {
        self.state.as_ref().map(|s| s.activation).unwrap_or(GoalActivation::Disarmed)
    }

    /// 当前 maxGoalRounds（driver 判定用）。
    pub fn max_goal_rounds(&self) -> u64 {
        self.state
            .as_ref()
            .and_then(|s| s.goal.as_ref())
            .map(|g| g.max_goal_rounds)
            .unwrap_or(0)
    }

    /// 当前目标 objective（driver 提示渲染用）。
    pub fn objective(&self) -> Option<&str> {
        self.state.as_ref().and_then(|s| s.goal.as_ref()).map(|g| g.objective.as_str())
    }

    fn state_mut(&mut self) -> Result<&mut State, GoalServiceError> {
        self.state.as_mut().ok_or(GoalServiceError::NotFound)
    }
}

/// CAS 前置校验：goal.revision 必须等于 refr.revision。
fn cas_ref(goal: &GoalSnapshot, refr: &GoalRef) -> Result<(), GoalServiceError> {
    if goal.revision != refr.revision {
        return Err(GoalServiceError::StaleRevision);
    }
    Ok(())
}


fn is_lower_kebab(s: &str) -> bool {
    // ^[a-z][a-z0-9]*(?:-[a-z0-9]+)*$
    let bytes = s.as_bytes();
    if bytes.is_empty() || !(bytes[0].is_ascii_lowercase()) {
        return false;
    }
    let mut prev_dash = false;
    for &b in &bytes[1..] {
        if b == b'-' {
            if prev_dash {
                return false;
            }
            prev_dash = true;
        } else {
            if !(b.is_ascii_lowercase() || b.is_ascii_digit()) {
                return false;
            }
            prev_dash = false;
        }
    }
    !prev_dash
}

#[cfg(test)]
fn now_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(not(test))]
fn now_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

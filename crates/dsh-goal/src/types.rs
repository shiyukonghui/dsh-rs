//! `dsh-goal` 纯类型与事件溯源 payload —— 对齐 `@deepseek-ai/dsh-goal` types.ts/domain.ts 逐字。
//!
//! - `GoalRef` 是 CAS 标识（id + 递增 revision）。
//! - `GoalSnapshot` 是每次非 clear 变更写下的完整 durable 状态（last-wins）。
//! - `GoalChangeMeta` 是 `goal/change` 会话事件载荷（snapshot 或 clear 墓碑两个变体）。
//! - `GoalProjection` 是 wire 投影（不含进程内 activation）。

use serde::{Deserialize, Serialize};

/// 目标标识（运行时要求非空字符串；惯例 `goal-<id>`）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GoalId(pub String);

/// Compare-and-set 标识：一个目标的一个确切 revision。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalRef {
    pub id: GoalId,
    /// 正整数；每次 durable 变更递增。
    pub revision: u64,
}

impl GoalRef {
    pub fn new(id: impl Into<String>, revision: u64) -> Self {
        GoalRef { id: GoalId(id.into()), revision }
    }
}

/// 持久化生命周期阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GoalPhase {
    Active,
    Paused,
    Blocked,
    Complete,
}

impl GoalPhase {
    pub fn as_str(&self) -> &'static str {
        match self {
            GoalPhase::Active => "active",
            GoalPhase::Paused => "paused",
            GoalPhase::Blocked => "blocked",
            GoalPhase::Complete => "complete",
        }
    }
}

/// 机器可路由 + 人类可读的阻塞原因。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalBlockReason {
    /// 稳定 lower-kebab-case 分类。
    pub code: String,
    /// 非空的人类/模型可读说明。
    pub message: String,
}

/// 每次非 clear 目标变更写下的完整 durable 状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalSnapshot {
    pub id: GoalId,
    pub revision: u64,
    /// 人类请求的完成目标。
    pub objective: String,
    pub phase: GoalPhase,
    /// 恰好 phase==blocked 时出现。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<GoalBlockReason>,
    /// 准入轮次总上限。
    #[serde(rename = "maxGoalRounds")]
    pub max_goal_rounds: u64,
}

/// 进程内是否可自动续跑当前 active goal（永不持久化）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GoalActivation {
    Armed,
    Disarmed,
}

/// 当前目标宿主视图（含派生计数与进程内激活）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalView {
    pub id: GoalId,
    pub revision: u64,
    pub objective: String,
    pub phase: GoalPhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<GoalBlockReason>,
    pub max_goal_rounds: u64,
    /// 最高已准入轮次号。
    pub rounds_started: u64,
    /// create 变更 epoch 毫秒。
    pub created_at: i64,
    /// 最近变更 epoch 毫秒。
    pub updated_at: i64,
    pub activation: GoalActivation,
}

/// wire 投影：`goal` 缺省 / clear 后为 None。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalProjection {
    pub goal: Option<GoalSnapshot>,
    pub rounds_started: u64,
    pub created_at: i64,
    pub updated_at: i64,
}

/// 目标状态变更动词（持久化 source change 记录）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GoalOperation {
    Create,
    Edit,
    Pause,
    Resume,
    Complete,
    Block,
    Clear,
}

impl GoalOperation {
    pub fn as_str(&self) -> &'static str {
        match self {
            GoalOperation::Create => "create",
            GoalOperation::Edit => "edit",
            GoalOperation::Pause => "pause",
            GoalOperation::Resume => "resume",
            GoalOperation::Complete => "complete",
            GoalOperation::Block => "block",
            GoalOperation::Clear => "clear",
        }
    }
}

/// `goal/change` 事件载荷版本。
pub const GOAL_CHANGE_VERSION: u64 = 1;

/// snapshot 变体载荷字段（TS `GoalSnapshotChangeMeta`）：
/// `{kind, version, operation, goal, roundsStarted, createdAt, updatedAt}`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalSnapshotChangeMeta {
    #[serde(rename = "kind")]
    pub kind: String,
    pub version: u64,
    pub operation: GoalOperation,
    pub goal: GoalSnapshot,
    #[serde(rename = "roundsStarted")]
    pub rounds_started: u64,
    #[serde(rename = "createdAt")]
    pub created_at: i64,
    #[serde(rename = "updatedAt")]
    pub updated_at: i64,
}

/// clear 墓碑载荷字段（TS `GoalClearChangeMeta`）：
/// `{kind, version, operation:'clear', cleared:{id,revision}, clearedAt}`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalClearChangeMeta {
    #[serde(rename = "kind")]
    pub kind: String,
    pub version: u64,
    pub operation: GoalOperation,
    pub cleared: GoalRef,
    #[serde(rename = "clearedAt")]
    pub cleared_at: i64,
}

/// durable change 联合（snapshot | clear）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum GoalChangeMeta {
    Snapshot(GoalSnapshotChangeMeta),
    Clear(GoalClearChangeMeta),
}

//! `dsh-plan` 纯类型。

use serde::{Deserialize, Serialize};

/// plan 投影的 wire 值。`active` 是生效的已记录状态（最后一条 `plan/mode` 之前
/// inactive）；`pending` 在选择目标异于 active、尚未由其配对 `command/done` 失败、
/// 且无更晚的 `plan/mode` 记录该状态时为 true。能力缺失（plan-mode 未组合）= 键缺失，
/// 而非值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanProjection {
    pub active: bool,
    pub pending: bool,
}

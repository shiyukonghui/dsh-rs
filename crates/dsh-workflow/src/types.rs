//! `dsh-workflow` 纯类型（M4g：WorkflowMeta wire / WorkflowRunInfo / AgentInfo / Result）。

use serde::{Deserialize, Serialize};

/// WorkflowMeta wire（name/description 必填，whenToUse/phases 可选）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct WorkflowMeta {
    pub name: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub when_to_use: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phases: Option<Vec<WorkflowPhase>>,
}

/// WorkflowMeta 的 wire 键名（camelCase）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowPhase {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// WorkflowResult wire（value 仅 completed 有意义；非 completed 带 error）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowResult {
    pub value: serde_json::Value,
    pub stop_reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub agents_started: u64,
}

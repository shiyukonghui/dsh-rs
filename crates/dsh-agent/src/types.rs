//! dsh-agent 类型面（对齐报告 §A.2 导出类型全集）。

use serde::{Deserialize, Serialize};

use dsh_brand::ReasoningEffortId;
use dsh_session::SessionEvent;

/// `AgentCancelCause` re-export（从 dsh-session：closed union）。
pub use dsh_session::AgentCancelCause;

// ---------------------------------------------------------------------------
// 命名空间导出（Wire 形状）
// ---------------------------------------------------------------------------

/// `InboxTarget`：入队边界（'next-turn' | 'next-step'）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InboxTarget {
    NextTurn,
    NextStep,
}

/// `agent/inbox/spliced` 的 `outcome`（当前仅有 'canceled' 字面量）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InboxSpliceOutcome {
    #[serde(rename = "canceled")]
    Canceled,
}

/// `AgentStatus`：'idle' 无 driver 活跃；'running' 有 driver 活跃。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Idle,
    Running,
}

impl AgentStatus {
    /// wire 字面量（'idle' / 'running'）。
    pub fn wire_str(&self) -> &'static str {
        match self {
            AgentStatus::Idle => "idle",
            AgentStatus::Running => "running",
        }
    }
}

/// `SessionStartSource`：标识「初始化已关闭的无输入 turn」的来源（水岭仅发一次）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionStartSource {
    Startup,
    Resume,
    Clear,
    Compact,
}

/// `AgentOptions`（deployment-specific；merge-extensible）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentOptions {
    /// provider 路由（调用时必须有已注册 adapter）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// 由所选 provider adapter 解释的 model id。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// 每个对话 model 请求的最大输出 token。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
}

/// `CancelOptions`：`keepInbox` 为 true 时保留 queued/steering，中止活跃 turn，
/// 不记 canceled splice。
#[derive(Debug, Clone, Default)]
pub struct CancelOptions {
    pub keep_inbox: Option<bool>,
}

/// `ModelSelection`：下一步进入 prompt 组装时选定的模型。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSelection {
    pub provider: String,
    pub model: String,
    /// adapter 自有 reasoning effort；缺省 = provider/default 行为。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffortId>,
}

/// `ModelSelectionRef`：current（选择侧）与 assembled（组装捕获快照侧）双槽。
#[derive(Debug, Default, Clone)]
pub struct ModelSelectionRef {
    pub current: Option<ModelSelection>,
    pub assembled: Option<ModelSelection>,
}

/// `ConsumedWork`：从 log 折叠出的「已消费工作」记账。
#[derive(Debug, Clone, PartialEq)]
pub struct ConsumedWork {
    /// 最近一个「为已消费工作记账」的已关闭 turn；无则缺省。
    pub end: Option<SessionEvent>,
    /// 接受了但在该 turn 之后未经运行被 cancel 出 inbox。
    pub dropped_unrun: bool,
}

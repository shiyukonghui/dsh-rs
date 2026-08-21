//! 压缩词汇：结果类型与 `compaction/*` 会话事件载荷（log-only，不入 surface）。
//!
//! 权威参考：`deepseek-harness/packages/compaction/compaction/src/types.ts`。
//!
//! 注：事件 wire 形状的构造统一在 `absorb`（payload 构造函数），本模块只保留
//! 共享数据词汇（CompactionTrigger/CompactionResult/ManualCompactionError/ShadowedRange）。

use serde::{Deserialize, Serialize};

use crate::CompactionId;

/// 自动策略要求后端考虑压缩的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CompactionTrigger {
    Pressure,
    ContextOverflow,
}

/// 显式空闲会话压缩请求的预期失败类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManualCompactionErrorCode {
    Busy,
    Cancelled,
    Changed,
    Summary,
    Commit,
    Persistence,
}

impl ManualCompactionErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ManualCompactionErrorCode::Busy => "busy",
            ManualCompactionErrorCode::Cancelled => "cancelled",
            ManualCompactionErrorCode::Changed => "changed",
            ManualCompactionErrorCode::Summary => "summary",
            ManualCompactionErrorCode::Commit => "commit",
            ManualCompactionErrorCode::Persistence => "persistence",
        }
    }
}

/// 分类的压缩失败。
#[derive(Debug, Clone, PartialEq)]
pub struct ManualCompactionError {
    pub code: ManualCompactionErrorCode,
    pub message: String,
}

impl std::fmt::Display for ManualCompactionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for ManualCompactionError {}

/// 一次成功压缩操作的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionResult {
    /// 本次压缩完整持久生命周期共享的稳定标识。
    pub compaction_id: CompactionId,
    /// 发起本次压缩的人工命令（手动时）。
    pub source_command_id: Option<String>,
    /// 追加的 `compaction/start` 事件 seq。
    pub start_seq: u64,
    /// 追加的 `compaction/summary` 事件 seq。
    pub summary_seq: u64,
    /// 追加的 `compaction/end` 事件 seq。
    pub end_seq: u64,
    /// 后端产生的摘要内容块。
    pub summary: Vec<dsh_llm::types::ContentBlock>,
    /// 被遮蔽的边界对：被替换范围的第一个（`start`）与最后一个（`end`）
    /// surface 节点的 seq。这是 surface-位置跨度，不是数值 seq 区间——先前的
    /// replace 落地一个高 seq 摘要节点到更早范围的位置后，`start` 可能大于 `end`。
    pub shadowed_range: ShadowedRange,
    /// 被遮蔽节点 seq（surface 顺序），权威集合。
    pub shadowed_seqs: Vec<u64>,
    /// 被遮蔽内容的估算 token 数。
    pub shadowed_token_count: u64,
}

/// 被替换 surface 范围的边界对。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowedRange {
    pub start: u64,
    pub end: u64,
}

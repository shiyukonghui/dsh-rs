//! 压缩检查点来源（对齐 `deepseek-harness/packages/compaction/compaction/src/checkpoint.ts`）。
//!
//! TS 侧 `compactCheckpointSource` 返回 `{kind:'plugin', plugin:'compact', compactionId,
//! sourceCommandId?}`——开放 plugin 对象经 `PluginMessageSource` 的 `extra` 字段无损承载；
//! `isCompactCheckpointSource` 只看 `kind==='plugin' && plugin==='compact'`。

use dsh_llm::types::{MessageSource, PluginMessageSource};

use crate::CompactionId;

const COMPACT_CHECKPOINT_PLUGIN: &str = "compact";

/// 具体压缩检查点携带的消息来源（plugin='compact' + compactionId + 可选 sourceCommandId）。
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionCheckpointSource {
    pub compaction_id: CompactionId,
    pub source_command_id: Option<String>,
}

/// 创建与一次压缩事务相关的检查点来源。
pub fn compact_checkpoint_source(
    compaction_id: CompactionId,
    source_command_id: Option<String>,
) -> CompactionCheckpointSource {
    CompactionCheckpointSource { compaction_id, source_command_id }
}

/// 把检查点来源转成消息来源（`kind='plugin'`, plugin='compact', 携带 compactionId）。
pub fn checkpoint_message_source(source: &CompactionCheckpointSource) -> MessageSource {
    let mut plugin = PluginMessageSource::new(COMPACT_CHECKPOINT_PLUGIN)
        .with_extra("compactionId", serde_json::json!(source.compaction_id.raw()));
    if let Some(cid) = &source.source_command_id {
        plugin = plugin.with_extra("sourceCommandId", serde_json::json!(cid));
    }
    MessageSource::Plugin(plugin)
}

/// 测试持久化消息来源是否标识压缩检查点。
pub fn is_compact_checkpoint_source(source: &MessageSource) -> bool {
    matches!(source, MessageSource::Plugin(p) if p.plugin() == COMPACT_CHECKPOINT_PLUGIN)
}

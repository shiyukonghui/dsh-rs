//! 压缩事件载荷构造 + 吸收进 session 的共享记录语义（对齐
//! `deepseek-harness/packages/compaction/compaction-basic/src/region.ts` 的
//! `commitCompactionBody` 与 `compaction-tool-result-pruner` 的 shadow-price 协议）。
//!
//! 本模块只做**事件的 wire 形状与落盘顺序**：`compaction/start` 先落（锁）、
//! 摘要后 `compaction/summary`、替换 user 消息（`user/message` Replace，source 为
//! `compactCheckpointSource`）、`compaction/end` 释放锁；`compaction/prune` 紧跟它
//! 遮蔽节点的替换事件（shadow-price 协议：纯消费者减去被遮蔽节点的价格而不保留逐
//! 节点状态）。

use dsh_llm::types::{ContentBlock, Message, MessageId, TokenUsage};
use dsh_session::runtime::Session;
use dsh_session::types::{EventKind, SurfaceIntent, SurfaceOp, SessionEvent};
use serde_json::{json, Value};

use crate::checkpoint::{checkpoint_message_source, CompactionCheckpointSource};
use crate::types::ShadowedRange;
use crate::CompactionId;

fn compaction_id_value(id: &CompactionId) -> Value {
    json!(id.raw())
}

/// `compaction/start` 载荷：compactionId + 可选 sourceCommandId + 属主 turn（可空）。
pub fn compaction_start_payload(
    compaction_id: &CompactionId,
    source_command_id: Option<&str>,
    turn: Option<u64>,
) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("compactionId".into(), compaction_id_value(compaction_id));
    if let Some(cid) = source_command_id {
        obj.insert("sourceCommandId".into(), json!(cid));
    }
    obj.insert(
        "turn".into(),
        match turn {
            Some(t) => json!(t),
            None => Value::Null,
        },
    );
    Value::Object(obj)
}

/// `compaction/end` 载荷：与 start 相同的生命周期字段；失败时附一层 error chain 文本。
pub fn compaction_end_payload(
    compaction_id: &CompactionId,
    source_command_id: Option<&str>,
    turn: Option<u64>,
    error: Option<&str>,
) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("compactionId".into(), compaction_id_value(compaction_id));
    if let Some(cid) = source_command_id {
        obj.insert("sourceCommandId".into(), json!(cid));
    }
    obj.insert(
        "turn".into(),
        match turn {
            Some(t) => json!(t),
            None => Value::Null,
        },
    );
    if let Some(e) = error {
        obj.insert("error".into(), json!(e));
    }
    Value::Object(obj)
}

/// `compaction/summary` 载荷：完整摘要记录（wire 形状对齐 region.ts commit 段）。
#[allow(clippy::too_many_arguments)]
pub fn compaction_summary_payload(
    compaction_id: &CompactionId,
    source_command_id: Option<&str>,
    summary: &[ContentBlock],
    raw_output: Option<&[ContentBlock]>,
    llm_stream_call: bool,
    shadowed_range: ShadowedRange,
    shadowed_seqs: &[u64],
    shadowed_token_count: u64,
    provider: &str,
    model: &str,
    max_tokens: Option<u64>,
    usage: Option<&TokenUsage>,
) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("compactionId".into(), compaction_id_value(compaction_id));
    if let Some(cid) = source_command_id {
        obj.insert("sourceCommandId".into(), json!(cid));
    }
    obj.insert(
        "summary".into(),
        serde_json::to_value(summary).unwrap_or(Value::Null),
    );
    if let Some(raw) = raw_output {
        obj.insert("rawOutput".into(), serde_json::to_value(raw).unwrap_or(Value::Null));
    }
    obj.insert("llmStreamCall".into(), json!(llm_stream_call));
    obj.insert(
        "shadowedRange".into(),
        json!({ "start": shadowed_range.start, "end": shadowed_range.end }),
    );
    obj.insert("shadowedSeqs".into(), serde_json::to_value(shadowed_seqs).unwrap_or(Value::Null));
    obj.insert("shadowedTokenCount".into(), json!(shadowed_token_count));
    obj.insert("provider".into(), json!(provider));
    obj.insert("model".into(), json!(model));
    if let Some(m) = max_tokens {
        obj.insert("maxTokens".into(), json!(m));
    }
    if let Some(u) = usage {
        obj.insert("usage".into(), serde_json::to_value(u).unwrap_or(Value::Null));
    }
    Value::Object(obj)
}

/// `compaction/prune` 载荷（shadow-price 事件）：遮蔽单节点范围 + 其 token 价格。
pub fn compaction_prune_payload(
    shadowed_range: ShadowedRange,
    shadowed_seqs: &[u64],
    shadowed_token_count: u64,
) -> Value {
    json!({
        "shadowedRange": { "start": shadowed_range.start, "end": shadowed_range.end },
        "shadowedSeqs": shadowed_seqs,
        "shadowedTokenCount": shadowed_token_count,
    })
}

/// 把生成的 checkpoint source 用于替换 user 消息的 `Message`（role=user）。
pub fn checkpoint_user_message(
    id: MessageId,
    content: Vec<ContentBlock>,
    checkpoint: &CompactionCheckpointSource,
) -> Message {
    Message {
        id,
        role: dsh_llm::types::Role::User,
        content,
        source: checkpoint_message_source(checkpoint),
    }
}

/// 完整压缩身体（start 已落）：summary 事件 + 替换 user 消息（surface Replace）。
///
/// 返回追加的 `compaction/summary` 事件与替换 `user/message` 事件。
#[allow(clippy::too_many_arguments)]
pub fn commit_compaction_body(
    session: &Session,
    start_seq: u64,
    compaction_id: &CompactionId,
    source_command_id: Option<&str>,
    summary: &[ContentBlock],
    checkpoint_content: Vec<ContentBlock>,
    raw_output: Option<&[ContentBlock]>,
    llm_stream_call: bool,
    shadowed_range: ShadowedRange,
    shadowed_seqs: &[u64],
    shadowed_token_count: u64,
    provider: &str,
    model: &str,
    max_tokens: Option<u64>,
    usage: Option<&TokenUsage>,
    checkpoint_message_id: MessageId,
) -> Result<(SessionEvent, SessionEvent), dsh_session::SessionError> {
    let summary_event = session.append(
        EventKind::CompactionSummary,
        compaction_summary_payload(
            compaction_id,
            source_command_id,
            summary,
            raw_output,
            llm_stream_call,
            shadowed_range,
            shadowed_seqs,
            shadowed_token_count,
            provider,
            model,
            max_tokens,
            usage,
        ),
        None,
    )?;
    let checkpoint_source = CompactionCheckpointSource {
        compaction_id: compaction_id.clone(),
        source_command_id: source_command_id.map(str::to_string),
    };
    let checkpoint_message = checkpoint_user_message(
        checkpoint_message_id,
        checkpoint_content,
        &checkpoint_source,
    );
    let replacement = session.append(
        EventKind::UserMessage,
        serde_json::to_value(&checkpoint_message).unwrap_or(Value::Null),
        Some(&SurfaceIntent {
            surface_op: SurfaceOp::Replace { start: shadowed_range.start, end: shadowed_range.end },
            source_event_seqs: Some(
                std::iter::once(start_seq)
                    .chain(std::iter::once(summary_event.seq))
                    .chain(shadowed_seqs.iter().copied())
                    .collect(),
            ),
        }),
    )?;
    Ok((summary_event, replacement))
}

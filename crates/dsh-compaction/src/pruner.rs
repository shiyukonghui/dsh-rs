//! ToolResultPruner：模型无关的工具结果裁剪（对齐
//! `deepseek-harness/packages/compaction/compaction-tool-result-pruner`）。
//!
//! 确定性 head/middle/tail Unicode 码点裁剪（保留 rich-block 顺序），替换每个超
//! 预算的当前 surface tool/result 节点；每次替换前追加 `compaction/prune`
//! shadow-price 事件定价被遮蔽节点（经调用方的 token 估算缝）。

use dsh_llm::types::ContentBlock;
use dsh_session::runtime::Session;
use dsh_session::types::{EventKind, SurfaceIntent, SurfaceOp};
use serde_json::Value;

use crate::absorb::compaction_prune_payload;
use crate::types::ShadowedRange;

/// 固定替换标记（每个移除的中间跨度）。
pub const PRUNE_MARKER: &str = "\n\n[... tool result middle pruned ...]\n\n";

/// 低摩擦默认（面向 coding-agent 工具输出）。
pub const DEFAULT_THRESHOLD_CHARS: u64 = 8192;
pub const DEFAULT_HEAD_CHARS: u64 = 4096;
pub const DEFAULT_TAIL_CHARS: u64 = 1024;

/// 已解析且不可变的字符预算。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrunerConfig {
    pub threshold_chars: u64,
    pub head_chars: u64,
    pub tail_chars: u64,
}

/// 原始（可能不合法的）prune 配置。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolResultPruneConfig {
    pub threshold_chars: Option<u64>,
    pub head_chars: Option<u64>,
    pub tail_chars: Option<u64>,
}

/// 逐 Unicode 码点计数（不拆 surrogate pair）。
pub fn code_point_length(text: &str) -> u64 {
    text.chars().count() as u64
}

/// 解析并校验裁剪预算。
pub fn resolve_prune_config(config: &ToolResultPruneConfig) -> Result<PrunerConfig, String> {
    let resolved = PrunerConfig {
        threshold_chars: config.threshold_chars.unwrap_or(DEFAULT_THRESHOLD_CHARS),
        head_chars: config.head_chars.unwrap_or(DEFAULT_HEAD_CHARS),
        tail_chars: config.tail_chars.unwrap_or(DEFAULT_TAIL_CHARS),
    };
    if resolved.threshold_chars == 0 {
        return Err(format!(
            "ToolResultPruneConfig: thresholdChars ({}) must be a positive integer",
            resolved.threshold_chars
        ));
    }
    let emitted_chars = resolved
        .head_chars
        .checked_add(code_point_length(PRUNE_MARKER))
        .and_then(|v| v.checked_add(resolved.tail_chars))
        .ok_or_else(|| "ToolResultPruneConfig: headChars + marker + tailChars overflow".to_string())?;
    if emitted_chars > resolved.threshold_chars {
        return Err(format!(
            "ToolResultPruneConfig: headChars + marker + tailChars ({emitted_chars}) must be at most thresholdChars ({})",
            resolved.threshold_chars
        ));
    }
    Ok(resolved)
}

/// 一次稳定的 current-surface 快照后的裁剪结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrunedEntry {
    pub original_seq: u64,
    pub replacement_seq: u64,
    pub call_id: String,
    pub chars_before: u64,
    pub chars_after: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PruneResult {
    pub pruned: Vec<PrunedEntry>,
    pub chars_removed: u64,
}

/// token 估算缝（对遮蔽节点定价；M1e 服务层接 token-meter）。
pub type EstimateFn = Box<dyn Fn(&dsh_llm::types::Message) -> u64>;

/// 确定性 head/middle/tail 裁剪器（无状态；`prune_session` 需要 session 引用）。
pub struct ToolResultPruner {
    pub config: PrunerConfig,
}

impl ToolResultPruner {
    pub fn new(config: &ToolResultPruneConfig) -> Result<Self, String> {
        Ok(ToolResultPruner { config: resolve_prune_config(config)? })
    }

    /// 测量文本内容（Unicode 码点）；非文本 block 计零。
    pub fn measure_content(&self, blocks: &[ContentBlock]) -> u64 {
        let mut chars = 0u64;
        for block in blocks {
            if let ContentBlock::Text(t) = block {
                chars += code_point_length(&t.text);
            }
        }
        chars
    }

    /// 替换超预算的文本中间，保留 rich-block 顺序。
    /// @returns 裁剪后的内容，或 `null`（文本在预算内）。
    pub fn prune_content(&self, blocks: &[ContentBlock]) -> Result<Option<Vec<ContentBlock>>, String> {
        let total_chars = self.measure_content(blocks);
        if total_chars <= self.config.threshold_chars {
            return Ok(None);
        }
        let removed_start = self.config.head_chars;
        let removed_end = total_chars - self.config.tail_chars; // 保守非负
        let mut pruned: Vec<ContentBlock> = Vec::new();
        let mut consumed = 0u64;
        let mut marker_inserted = false;

        for block in blocks {
            match block {
                ContentBlock::Text(t) => {
                    let points: Vec<char> = t.text.chars().collect();
                    let block_start = consumed;
                    let block_end = block_start + points.len() as u64;
                    let head_end = (points.len() as u64)
                        .min(removed_start.saturating_sub(block_start));
                    let tail_start = (points.len() as u64)
                        .min(removed_end.saturating_sub(block_start));
                    let intersects_removed = block_start < removed_end && block_end > removed_start;
                    let marker = if intersects_removed && !marker_inserted {
                        PRUNE_MARKER
                    } else {
                        ""
                    };
                    if !marker.is_empty() {
                        marker_inserted = true;
                    }
                    let mut text = String::new();
                    text.extend(points.iter().take(head_end as usize));
                    text.push_str(marker);
                    text.extend(points.iter().skip(tail_start as usize));
                    if !text.is_empty() {
                        let mut new_block = t.clone();
                        new_block.text = text;
                        pruned.push(ContentBlock::Text(new_block));
                    }
                    consumed = block_end;
                }
                other => pruned.push(other.clone()),
            }
        }
        if !marker_inserted {
            return Err("tool-result prune: failed to locate the removed text span".into());
        }
        let chars_after = self.measure_content(&pruned);
        if chars_after > self.config.threshold_chars || chars_after >= total_chars {
            return Err("tool-result prune: replacement must be smaller and within threshold".into());
        }
        Ok(Some(pruned))
    }

    /// 裁剪一个稳定 current-surface 快照里每个超预算 tool/result 节点。
    ///
    /// 每个替换保留除 `content` 外的完整事件 data，cite 被遮蔽节点，并紧跟一个
    /// `compaction/prune` shadow-price 事件（`estimate_message` 对遮蔽节点定价）。
    /// @throws 当 session 拒绝替换；本趟更早提交的替换保持 durable。
    pub fn prune_session(
        &self,
        session: &Session,
        estimate_message: &EstimateFn,
    ) -> Result<PruneResult, dsh_session::SessionError> {
        let events = session.events();
        let nodes = session.surface_nodes()?;
        let mut candidates: Vec<(u64, dsh_session::types::SessionEvent)> = Vec::new();
        for seq in &nodes {
            if let Some(event) = events.get(*seq as usize) {
                if event.kind == EventKind::ToolResult && event.seq == *seq {
                    candidates.push((*seq, event.clone()));
                }
            }
        }

        let mut pruned: Vec<PrunedEntry> = Vec::new();
        let mut chars_removed = 0u64;
        for (seq, event) in candidates {
            let result = event
                .data
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array())
                .and_then(|a| a.first().cloned())
                .ok_or_else(|| dsh_session::SessionError("tool-result prune: missing content block".into()))?;
            let content_blocks: Vec<ContentBlock> =
                serde_json::from_value(result.get("content").cloned().unwrap_or(Value::Null))
                    .map_err(|e| dsh_session::SessionError(format!("tool-result prune: malformed content: {e}")))?;
            let pruned_content = self
                .prune_content(&content_blocks)
                .map_err(dsh_session::SessionError)?;
            let pruned_content = match pruned_content {
                None => continue,
                Some(p) => p,
            };
            let chars_before = self.measure_content(&content_blocks);
            let chars_after = self.measure_content(&pruned_content);

            let message_value = event.data.get("message").cloned().unwrap_or(Value::Null);
            let mut message_obj = match message_value {
                Value::Object(m) => m,
                _ => {
                    return Err(dsh_session::SessionError("tool-result prune: message must be an object".into()));
                }
            };
            // content: [{...result, content: pruned}]
            let mut new_result = result.clone();
            if let Value::Object(map) = &mut new_result {
                map.insert(
                    "content".into(),
                    serde_json::to_value(&pruned_content).unwrap_or(Value::Null),
                );
            }
            message_obj.insert(
                "content".into(),
                serde_json::Value::Array(vec![new_result]),
            );
            let call_id = message_obj
                .get("source")
                .and_then(|s| s.get("callId"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            // 保留完整事件 data（turn/step 等外层字段），只替换 message.content。
            let mut replacement_data = event.data.clone();
            if let Value::Object(root) = &mut replacement_data {
                root.insert("message".into(), Value::Object(message_obj.clone()));
            }

            // 一次性读取 session 状态后逐次 append；事件数 = 快照就绪数。
            // Shadow-price 协议：meting 事件与替换事件同步相邻追加。
            let billable_message: Value = Value::Object(message_obj);
            session.append(
                EventKind::CompactionPrune,
                compaction_prune_payload(
                    ShadowedRange { start: seq, end: seq },
                    &[seq],
                    estimate_message(
                        &serde_json::from_value(billable_message).map_err(|e| {
                            dsh_session::SessionError(format!("tool-result prune: malformed message: {e}"))
                        })?,
                    ),
                ),
                None,
            )?;
            let replacement = session.append(
                EventKind::ToolResult,
                replacement_data,
                Some(&SurfaceIntent {
                    surface_op: SurfaceOp::Replace { start: seq, end: seq },
                    source_event_seqs: Some(vec![seq]),
                }),
            )?;
            pruned.push(PrunedEntry {
                original_seq: seq,
                replacement_seq: replacement.seq,
                call_id,
                chars_before,
                chars_after,
            });
            chars_removed += chars_before - chars_after;
        }
        Ok(PruneResult { pruned, chars_removed })
    }
}

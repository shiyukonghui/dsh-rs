//! 增量 chunk → 消息组装器（对齐
//! `deepseek-harness/packages/llm/llm/src/assembler.ts` 的 `BlockAssembler`）。
//!
//! 唯一规范的组装算法：agent loop 把原始 chunk 流喂进来（同时记录原始 chunk 供重放），
//! 流结束后读取 blocks/message/usage/finish，或在取消截断时读 `interrupted_blocks`。
//!
//! 容错 delta-only 协议（无 block-start/end）；对已由 block-end 关闭的 index 的后续
//! delta 忽略（畸形流），避免损坏已完成的 block。

use std::collections::BTreeMap;

use dsh_brand::CallId;

use crate::types::{
    ContentBlock, FinishReason, Message, MessageId, MessageSource, PluginMessageSource,
    ReplayEnvelope, StreamChunk, TokenUsage,
};

#[derive(Debug, Clone, PartialEq)]
struct PartialBlock {
    block_type: String,
    text: String,
    tool_call_id: Option<CallId>,
    tool_call_name: Option<String>,
    tool_call_arguments: String,
    /// 由 `block-end` 设置——权威，冻结 partial。
    block: Option<ContentBlock>,
}

/// 增量组装原始 `StreamChunk` 为完整 `ContentBlock` 与最终 assistant `Message`。
#[derive(Debug, Clone, Default)]
pub struct BlockAssembler {
    partials: BTreeMap<u64, PartialBlock>,
    order: Vec<u64>,
    usage: Option<TokenUsage>,
    finish: Option<FinishReason>,
    replay_state: Option<ReplayEnvelope>,
}

impl BlockAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// 按流顺序喂入一个原始 chunk。
    pub fn push(&mut self, chunk: StreamChunk) {
        match chunk {
            StreamChunk::BlockStart { index, block_type } => {
                if !self.partials.contains_key(&index) {
                    self.order.push(index);
                    self.partials.insert(
                        index,
                        PartialBlock {
                            block_type: block_type.as_str().to_string(),
                            text: String::new(),
                            tool_call_id: None,
                            tool_call_name: None,
                            tool_call_arguments: String::new(),
                            block: None,
                        },
                    );
                }
            }
            StreamChunk::TextDelta { index, text } => {
                let partial = self.ensure(index, "text".to_string());
                if partial.block.is_some() {
                    return; // 已由 block-end 关闭；忽略掉队者
                }
                partial.text.push_str(&text);
            }
            StreamChunk::ReasoningDelta { index, text } => {
                let partial = self.ensure(index, "reasoning".to_string());
                if partial.block.is_some() {
                    return;
                }
                partial.text.push_str(&text);
            }
            StreamChunk::BlockEnd { index, block } => {
                let block_type = block.type_();
                let partial = self.ensure(index, block_type.to_string());
                // 首个关闭胜出；忽略重复关闭（保持流出与实际组装一致）
                if partial.block.is_some() {
                    return;
                }
                partial.block = Some(block);
            }
            StreamChunk::ToolCallDelta { index, id, name, arguments_delta } => {
                let partial = self.ensure(index, "tool-call".to_string());
                if partial.block.is_some() {
                    return;
                }
                partial.tool_call_id = Some(id);
                if let Some(n) = name {
                    partial.tool_call_name = Some(n);
                }
                partial.tool_call_arguments.push_str(&arguments_delta);
            }
            StreamChunk::Usage { usage } => {
                self.usage = Some(usage);
            }
            StreamChunk::Finish { reason, replay_state } => {
                self.finish = Some(reason);
                self.replay_state = replay_state;
            }
            StreamChunk::Unknown { .. } => { /* 未知 chunk 忽略（容错） */ }
        }
    }

    fn ensure(&mut self, index: u64, block_type: String) -> &mut PartialBlock {
        if !self.partials.contains_key(&index) {
            self.order.push(index);
            self.partials.insert(
                index,
                PartialBlock {
                    block_type,
                    text: String::new(),
                    tool_call_id: None,
                    tool_call_name: None,
                    tool_call_arguments: String::new(),
                    block: None,
                },
            );
        }
        self.partials.get_mut(&index).expect("partial present after ensure")
    }

    fn assemble(&self, partial: &PartialBlock, index: u64) -> ContentBlock {
        if let Some(block) = &partial.block {
            return block.clone();
        }
        match partial.block_type.as_str() {
            "text" => ContentBlock::Text(crate::types::TextBlock { text: partial.text.clone() }),
            "reasoning" => {
                ContentBlock::Reasoning(crate::types::ReasoningBlock { text: partial.text.clone() })
            }
            "tool-call" => ContentBlock::ToolCall(crate::types::ToolCallBlock {
                id: partial
                    .tool_call_id
                    .clone()
                    .unwrap_or_else(|| CallId::from_raw(format!("call-{index}"))),
                name: partial.tool_call_name.clone().unwrap_or_default(),
                arguments: partial.tool_call_arguments.clone(),
            }),
            other => panic!("cannot assemble incomplete block of type \"{other}\""),
        }
    }

    fn must_get(&self, index: u64) -> &PartialBlock {
        self.partials
            .get(&index)
            .unwrap_or_else(|| panic!("BlockAssembler invariant violated: no partial for index {index}"))
    }

    /// 所有已见 block 的共享 keep/drop 决策：max-tokens 截断丢弃无法安全执行的
    /// tool call。发出的 blocks 与重放元数据都由此结果派生，不会彼此不一致。
    fn assembled(&self) -> (Vec<ContentBlock>, Option<ReplayEnvelope>) {
        let all: Vec<ContentBlock> =
            self.order.iter().map(|&index| self.assemble(self.must_get(index), index)).collect();
        let kept: Option<Vec<bool>> = match self.finish() {
            FinishReason::MaxTokens => Some(all.iter().map(|b| b.type_() != "tool-call").collect()),
            _ => None,
        };
        let blocks = match &kept {
            Some(kept) => all
                .iter()
                .zip(kept.iter())
                .filter(|(_, k)| **k)
                .map(|(b, _)| b.clone())
                .collect(),
            None => all.clone(),
        };
        let envelope = self.replay_state.clone();
        match envelope {
            None => (blocks, None),
            Some(env) => {
                let Some(env_blocks) = env.blocks.as_ref() else {
                    return (blocks, Some(env));
                };
                if env_blocks.len() != all.len() {
                    return (blocks, None);
                }
                let replay = match &kept {
                    Some(kept) if blocks.len() != all.len() => ReplayEnvelope {
                        response: env.response,
                        blocks: Some(
                            env_blocks
                                .iter()
                                .zip(kept.iter())
                                .filter(|(_, k)| **k)
                                .map(|(b, _)| b.clone())
                                .collect(),
                        ),
                    },
                    _ => env,
                };
                (blocks, Some(replay))
            }
        }
    }

    /// 按流顺序组装所有已见 blocks。max-token 截断丢弃 tool call；未关闭的 open
    /// block 由累积 delta 组装（从未被 block-end 关闭的未知 block 类型 panic）。
    pub fn blocks(&self) -> Vec<ContentBlock> {
        self.assembled().0
    }

    /// 中断流可安全终化的前缀：已关闭与 open text/reasoning blocks 中非空白内容的
    /// 部分，按流顺序；tool call 被省略（中断先于调度），open 未知 block 亦省略。
    pub fn interrupted_blocks(&self) -> Vec<ContentBlock> {
        self.order
            .iter()
            .filter_map(|&index| {
                let partial = self.must_get(index);
                let block_type = partial.block.as_ref().map(|b| b.type_().to_string()).unwrap_or_else(|| partial.block_type.clone());
                if block_type != "text" && block_type != "reasoning" {
                    return None;
                }
                let block = self.assemble(partial, index);
                let is_nonempty = match &block {
                    ContentBlock::Text(t) => !t.text.trim().is_empty(),
                    ContentBlock::Reasoning(r) => !r.text.trim().is_empty(),
                    _ => false,
                };
                if is_nonempty {
                    Some(block)
                } else {
                    None
                }
            })
            .collect()
    }

    /// 来自 `usage` chunk 的 token 记账；到达前为 None。
    pub fn usage(&self) -> Option<&TokenUsage> {
        self.usage.as_ref()
    }

    /// 来自 `finish` chunk 的结束原因；流无结束 chunk 时回落 `{kind:"stop"}`。
    pub fn finish(&self) -> FinishReason {
        self.finish.clone().unwrap_or(FinishReason::Stop)
    }

    /// 终末 finish chunk 的重放元数据，逐块条目随 `blocks` 同步修剪。
    pub fn replay_state(&self) -> Option<ReplayEnvelope> {
        let (_, replay) = self.assembled();
        replay
    }

    /// 组装好的 assistant 消息（role=assistant，内容=blocks()）。
    pub fn message(&self, id: MessageId) -> Message {
        let source = MessageSource::Plugin(PluginMessageSource::new("dsh-llm/assembler"));
        Message {
            id,
            role: crate::types::Role::Assistant,
            content: self.blocks(),
            source,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn assembles_text_and_reasoning_delta_only_protocol() {
        let mut a = BlockAssembler::new();
        a.push(StreamChunk::TextDelta { index: 0, text: "hello".into() });
        a.push(StreamChunk::ReasoningDelta { index: 1, text: "think".into() });
        a.push(StreamChunk::TextDelta { index: 0, text: " world".into() });
        let blocks = a.blocks();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].type_(), "text");
        assert_eq!(blocks[1].type_(), "reasoning");
        assert_eq!(
            serde_json::to_value(&blocks[0]).unwrap(),
            json!({"type": "text", "text": "hello world"})
        );
    }

    #[test]
    fn block_end_freeze_wins_over_deltas() {
        let mut a = BlockAssembler::new();
        a.push(StreamChunk::BlockStart { index: 0, block_type: "text".parse().unwrap() });
        a.push(StreamChunk::TextDelta { index: 0, text: "hi".into() });
        a.push(StreamChunk::BlockEnd {
            index: 0,
            block: ContentBlock::Text(crate::types::TextBlock { text: "final".into() }),
        });
        // 关闭后的掉队 delta 被忽略
        a.push(StreamChunk::TextDelta { index: 0, text: "STRAGGLER".into() });
        let blocks = a.blocks();
        assert_eq!(blocks.len(), 1);
        assert_eq!(serde_json::to_value(&blocks[0]).unwrap(), json!({"type": "text", "text": "final"}));
    }

    #[test]
    fn tool_call_delta_assembles_with_fallback_id() {
        let mut a = BlockAssembler::new();
        a.push(StreamChunk::ToolCallDelta {
            index: 0,
            id: CallId::from_raw("c1"),
            name: Some("demo".into()),
            arguments_delta: "{\"a\":".into(),
        });
        a.push(StreamChunk::ToolCallDelta {
            index: 0,
            id: CallId::from_raw("c1"),
            name: None,
            arguments_delta: "1}".into(),
        });
        let blocks = a.blocks();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].type_(), "tool-call");
        match &blocks[0] {
            ContentBlock::ToolCall(t) => {
                assert_eq!(t.id, CallId::from_raw("c1"));
                assert_eq!(t.name, "demo");
                assert_eq!(t.arguments, "{\"a\":1}");
            }
            _ => panic!("expected tool-call"),
        }
    }

    #[test]
    fn usage_and_finish_are_recorded() {
        let usage = TokenUsage { input_tokens: 10, output_tokens: 5, cache_read_tokens: None, cache_write_tokens: None, reasoning_tokens: None };
        let mut a = BlockAssembler::new();
        assert_eq!(a.usage(), None);
        assert_eq!(a.finish(), FinishReason::Stop);
        a.push(StreamChunk::Usage { usage: usage.clone() });
        a.push(StreamChunk::Finish { reason: FinishReason::ToolCalls, replay_state: None });
        assert_eq!(a.usage(), Some(&usage));
        assert_eq!(a.finish(), FinishReason::ToolCalls);
    }

    #[test]
    fn max_tokens_truncation_drops_tool_calls() {
        let mut a = BlockAssembler::new();
        a.push(StreamChunk::BlockStart { index: 0, block_type: "text".parse().unwrap() });
        a.push(StreamChunk::TextDelta { index: 0, text: "ok".into() });
        a.push(StreamChunk::ToolCallDelta {
            index: 1,
            id: CallId::from_raw("c1"),
            name: Some("demo".into()),
            arguments_delta: "{}".into(),
        });
        a.push(StreamChunk::Finish { reason: FinishReason::MaxTokens, replay_state: None });
        let blocks = a.blocks();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].type_(), "text");
    }

    #[test]
    fn interrupted_blocks_keep_only_nonempty_text_reasoning() {
        let mut a = BlockAssembler::new();
        a.push(StreamChunk::TextDelta { index: 0, text: "kept".into() });
        a.push(StreamChunk::TextDelta { index: 1, text: "   ".into() });
        a.push(StreamChunk::ToolCallDelta {
            index: 2,
            id: CallId::from_raw("c1"),
            name: Some("demo".into()),
            arguments_delta: "{}".into(),
        });
        let blocks = a.interrupted_blocks();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].type_(), "text");
        assert_eq!(
            serde_json::to_value(&blocks[0]).unwrap(),
            json!({"type": "text", "text": "kept"})
        );
    }

    #[test]
    fn replay_state_blocks_pruned_with_max_tokens() {
        let mut a = BlockAssembler::new();
        a.push(StreamChunk::BlockStart { index: 0, block_type: "text".parse().unwrap() });
        a.push(StreamChunk::TextDelta { index: 0, text: "ok".into() });
        a.push(StreamChunk::ToolCallDelta {
            index: 1,
            id: CallId::from_raw("c1"),
            name: Some("demo".into()),
            arguments_delta: "{}".into(),
        });
        let env = ReplayEnvelope {
            response: json!({"id": "r1"}),
            blocks: Some(vec![json!({"i": 0}), json!({"i": 1})]),
        };
        a.push(StreamChunk::Finish { reason: FinishReason::MaxTokens, replay_state: Some(env) });
        let replay = a.replay_state().unwrap();
        assert_eq!(replay.blocks.as_ref().unwrap().len(), 1);
        assert_eq!(replay.blocks.as_ref().unwrap()[0], json!({"i": 0}));
    }
}

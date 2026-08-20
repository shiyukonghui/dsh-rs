//! 把 DeepSeek SSE payload 翻译为 harness `StreamChunk` 协议（对齐
//! `deepseek-harness/packages/llm/llm-deepseek/src/translate.ts`）。
//!
//! 每个 content/reasoning/tool-call 索引一个状态化 harness block；空的初始
//! reasoning delta 不开 block。finish reason 与最新 usage 延迟到 `[DONE]`，
//! 覆盖 finish-attached 与 trailing usage-only 两种形态。

use dsh_llm::types::{
    CallId, ContentBlock, FinishReason, LlmFailure, ReasoningBlock, StreamChunk, TextBlock,
    TokenUsage, ToolCallBlock,
};
use dsh_llm::EMPTY_RESPONSE_CODE;

use crate::sse::DONE;
use crate::types::{WireChunk, WireUsage};

/// 映射 wire finish_reason 词表到 harness FinishReason。
pub fn map_finish_reason(reason: &str) -> FinishReason {
    match reason {
        "stop" => FinishReason::Stop,
        "tool_calls" => FinishReason::ToolCalls,
        "length" => FinishReason::MaxTokens,
        other => FinishReason::Error {
            failure: LlmFailure {
                message: format!("model stopped: {other}"),
                code: other.to_uppercase(),
                status: None,
                provider_retry_after_ms: None,
                request_id: None,
            },
        },
    }
}

/// 映射 wire usage 字段为层约定（DISJOINT）计数。
/// DeepSeek 的 `prompt_tokens` 包含缓存命中，需减出 `input_tokens`。
pub fn map_usage(usage: &WireUsage) -> TokenUsage {
    let cache_read = usage
        .prompt_tokens_details
        .as_ref()
        .and_then(|d| d.cached_tokens)
        .or(usage.prompt_cache_hit_tokens);
    let reasoning = usage.completion_tokens_details.as_ref().and_then(|d| d.reasoning_tokens);
    TokenUsage {
        input_tokens: usage.prompt_tokens.saturating_sub(cache_read.unwrap_or(0)),
        output_tokens: usage.completion_tokens,
        cache_read_tokens: cache_read,
        cache_write_tokens: None,
        reasoning_tokens: reasoning,
    }
}

/// 一个处于组装中的 open block。
#[derive(Debug, Clone)]
struct OpenBlock {
    index: u64,
    kind: &'static str, // "text" | "reasoning" | "tool-call"
    text: String,
    call_id: Option<CallId>,
    name: Option<String>,
}

/// 组装一个 open block 的最终 ContentBlock。
fn close_block(block: &OpenBlock) -> ContentBlock {
    match block.kind {
        "text" => ContentBlock::Text(TextBlock { text: block.text.clone() }),
        "reasoning" => ContentBlock::Reasoning(ReasoningBlock { text: block.text.clone() }),
        _ => ContentBlock::ToolCall(ToolCallBlock {
            id: block.call_id.clone().unwrap_or_else(|| CallId::from_raw("")),
            name: block.name.clone().unwrap_or_default(),
            arguments: block.text.clone(),
        }),
    }
}

/// 翻译错误（对齐 `LlmError` code `MALFORMED_RESPONSE`/`STREAM_CLOSED`）。
#[derive(Debug, Clone, PartialEq)]
pub struct TranslateError {
    pub message: String,
    pub code: &'static str,
}

impl std::fmt::Display for TranslateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

/// 消费 SSE data 载荷（`[DONE]` 终结）并产出 StreamChunks。
///
/// 偏差与 TS `translate` 的同步收集等价：deltas 随到随出，
/// block-end/usage/finish 全部延迟到 `[DONE]`。`[DONE]` 到来前无任何
/// 已开 block 的 `stop`（或缺省 finish）是退化完成，映射为 `EMPTY_RESPONSE`
/// 错误 finish。
pub fn translate(
    payloads: impl IntoIterator<Item = String>,
) -> Result<Vec<StreamChunk>, TranslateError> {
    // `order` 是唯一 live block 存储：open 后按 position 原地累积，避免 clone 分叉。
    let mut order: Vec<OpenBlock> = Vec::new();
    // text_block / reasoning_block 记录其在 `order` 中的 position。
    let mut text_pos: Option<usize> = None;
    let mut reasoning_pos: Option<usize> = None;
    // tool call wire index → order position。
    let mut tool_pos: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
    let mut pending_finish: Option<FinishReason> = None;
    let mut pending_usage: Option<TokenUsage> = None;
    let mut out: Vec<StreamChunk> = Vec::new();

    for payload in payloads {
        if payload == DONE {
            for block in &order {
                out.push(StreamChunk::BlockEnd { index: block.index, block: close_block(block) });
            }
            if let Some(usage) = pending_usage {
                out.push(StreamChunk::Usage { usage });
            }
            let reason = pending_finish.unwrap_or(FinishReason::Stop);
            let reason = match reason {
                FinishReason::Stop if order.is_empty() => FinishReason::Error {
                    failure: LlmFailure {
                        message: "model returned a completed response with no content".into(),
                        code: EMPTY_RESPONSE_CODE.to_string(),
                        status: None,
                        provider_retry_after_ms: None,
                        request_id: None,
                    },
                },
                other => other,
            };
            out.push(StreamChunk::Finish { reason, replay_state: None });
            return Ok(out);
        }

        let chunk: WireChunk = match serde_json::from_str(&payload) {
            Ok(c) => c,
            Err(_) => {
                let truncated: String = payload.chars().take(120).collect();
                return Err(TranslateError {
                    message: format!("malformed SSE payload: {truncated}"),
                    code: "MALFORMED_RESPONSE",
                });
            }
        };

        if let Some(choices) = &chunk.choices {
            for choice in choices {
                let Some(delta) = &choice.delta else { continue };

                // reasoning 先行：thinking 模式在文本前交错；首个空串 chunk 不开 block。
                if let Some(Some(reasoning)) = &delta.reasoning_content {
                    if !reasoning.is_empty() {
                        if reasoning_pos.is_none() {
                            let pos = order.len();
                            order.push(OpenBlock {
                                index: order.len() as u64,
                                kind: "reasoning",
                                text: String::new(),
                                call_id: None,
                                name: None,
                            });
                            out.push(StreamChunk::BlockStart {
                                index: pos as u64,
                                block_type: "reasoning".parse().unwrap(),
                            });
                            reasoning_pos = Some(pos);
                        }
                        let pos = reasoning_pos.unwrap();
                        order[pos].text.push_str(reasoning);
                        out.push(StreamChunk::ReasoningDelta { index: pos as u64, text: reasoning.clone() });
                    }
                }

                if let Some(Some(content)) = &delta.content {
                    if !content.is_empty() {
                        if text_pos.is_none() {
                            let pos = order.len();
                            order.push(OpenBlock {
                                index: pos as u64,
                                kind: "text",
                                text: String::new(),
                                call_id: None,
                                name: None,
                            });
                            out.push(StreamChunk::BlockStart {
                                index: pos as u64,
                                block_type: "text".parse().unwrap(),
                            });
                            text_pos = Some(pos);
                        }
                        let pos = text_pos.unwrap();
                        order[pos].text.push_str(content);
                        out.push(StreamChunk::TextDelta { index: pos as u64, text: content.clone() });
                    }
                }

                if let Some(calls) = &delta.tool_calls {
                    for call in calls {
                        let pos = *tool_pos.entry(call.index).or_insert_with(|| {
                            let pos = order.len();
                            order.push(OpenBlock {
                                index: pos as u64,
                                kind: "tool-call",
                                text: String::new(),
                                call_id: None,
                                name: None,
                            });
                            out.push(StreamChunk::BlockStart {
                                index: pos as u64,
                                block_type: "tool-call".parse().unwrap(),
                            });
                            pos
                        });
                        if let Some(id) = &call.id {
                            order[pos].call_id = Some(CallId::from_raw(id.clone()));
                        }
                        if let Some(name) = call.function.as_ref().and_then(|f| f.name.as_ref()) {
                            order[pos].name = Some(name.clone());
                        }
                        let fragment = call.function.as_ref().and_then(|f| f.arguments.clone()).unwrap_or_default();
                        order[pos].text.push_str(&fragment);
                        out.push(StreamChunk::ToolCallDelta {
                            index: pos as u64,
                            id: order[pos].call_id.clone().unwrap_or_else(|| CallId::from_raw("")),
                            name: order[pos].name.clone(),
                            arguments_delta: fragment,
                        });
                    }
                }

                if let Some(Some(reason)) = &choice.finish_reason {
                    pending_finish = Some(map_finish_reason(reason));
                }
            }
        }

        // usage 可能在 finish chunk 附件或 trailing usage-only chunk；保留最新。
        if let Some(Some(usage)) = &chunk.usage {
            pending_usage = Some(map_usage(usage));
        }
    }

    // 到达此处说明载荷序列没有 [DONE]——违反 parseSse 约定。
    Err(TranslateError {
        message: "SSE payload stream ended without [DONE]".into(),
        code: "STREAM_CLOSED",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_finish_reason_vocabulary() {
        assert_eq!(map_finish_reason("stop"), FinishReason::Stop);
        assert_eq!(map_finish_reason("tool_calls"), FinishReason::ToolCalls);
        assert_eq!(map_finish_reason("length"), FinishReason::MaxTokens);
        match map_finish_reason("content_filter") {
            FinishReason::Error { failure } => assert_eq!(failure.code, "CONTENT_FILTER"),
            _ => panic!("expected error finish"),
        }
    }

    #[test]
    fn maps_usage_disjoint_counts() {
        let usage = WireUsage {
            prompt_tokens: 120,
            completion_tokens: 30,
            prompt_cache_hit_tokens: Some(20),
            prompt_cache_miss_tokens: Some(100),
            prompt_tokens_details: Some(crate::types::WirePromptTokensDetails { cached_tokens: Some(20) }),
            completion_tokens_details: Some(crate::types::WireCompletionTokensDetails { reasoning_tokens: Some(5) }),
        };
        let mapped = map_usage(&usage);
        assert_eq!(mapped.input_tokens, 100);
        assert_eq!(mapped.output_tokens, 30);
        assert_eq!(mapped.cache_read_tokens, Some(20));
        assert_eq!(mapped.reasoning_tokens, Some(5));
    }

    #[test]
    fn text_stream_defers_blocks_until_done() {
        let chunks = translate(vec![
            json!({"choices":[{"delta":{"content":"Hel"}}]}).to_string(),
            json!({"choices":[{"delta":{"content":"lo"}}]}).to_string(),
            json!({"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":2}}).to_string(),
            "[DONE]".to_string(),
        ])
        .unwrap();
        assert_eq!(chunks[0], StreamChunk::BlockStart { index: 0, block_type: "text".parse().unwrap() });
        assert_eq!(chunks[1], StreamChunk::TextDelta { index: 0, text: "Hel".into() });
        assert_eq!(chunks[2], StreamChunk::TextDelta { index: 0, text: "lo".into() });
        assert_eq!(chunks[3], StreamChunk::BlockEnd { index: 0, block: ContentBlock::Text(TextBlock { text: "Hello".into() }) });
        assert!(matches!(chunks[4], StreamChunk::Usage { .. }));
        assert_eq!(chunks[5], StreamChunk::Finish { reason: FinishReason::Stop, replay_state: None });
    }

    #[test]
    fn reasoning_empty_first_chunk_does_not_open_block() {
        let chunks = translate(vec![
            json!({"choices":[{"delta":{"reasoning_content":""}}]}).to_string(),
            json!({"choices":[{"delta":{"content":"hi"}}]}).to_string(),
            "[DONE]".to_string(),
        ])
        .unwrap();
        // 不应有 reasoning block-start；只有 text
        assert_eq!(chunks.iter().filter(|c| matches!(c, StreamChunk::BlockStart { block_type, .. } if block_type.as_str() == "reasoning")).count(), 0);
        assert!(chunks.iter().any(|c| matches!(c, StreamChunk::BlockStart { block_type, .. } if block_type.as_str() == "text")));
    }

    #[test]
    fn tool_call_deltas_concatenate_by_index() {
        let chunks = translate(vec![
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","type":"function","function":{"name":"demo","arguments":"{\"a\":"}}]}}]}"#.to_string(),
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"1}"}}]}}]}"#.to_string(),
            json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}).to_string(),
            "[DONE]".to_string(),
        ])
        .unwrap();
        match &chunks[0] {
            StreamChunk::BlockStart { index, block_type } => {
                assert_eq!(*index, 0);
                assert_eq!(block_type.as_str(), "tool-call");
            }
            other => panic!("expected tool-call block-start, got {other:?}"),
        }
        match chunks.iter().find(|c| matches!(c, StreamChunk::ToolCallDelta { arguments_delta, .. } if arguments_delta == "{\"a\":")) {
            Some(_) => {}
            None => panic!("expected first args delta"),
        }
        let block_end = chunks.iter().find(|c| matches!(c, StreamChunk::BlockEnd { .. })).unwrap();
        match block_end {
            StreamChunk::BlockEnd { block: ContentBlock::ToolCall(t), .. } => {
                assert_eq!(t.id, CallId::from_raw("c1"));
                assert_eq!(t.name, "demo");
                assert_eq!(t.arguments, "{\"a\":1}");
            }
            other => panic!("expected assembled tool-call block, got {other:?}"),
        }
        assert!(matches!(chunks.last(), Some(StreamChunk::Finish { reason: FinishReason::ToolCalls, .. })));
    }

    #[test]
    fn malformed_payload_aborts() {
        let err = translate(vec!["not-json".to_string(), "[DONE]".to_string()]).unwrap_err();
        assert_eq!(err.code, "MALFORMED_RESPONSE");
    }

    #[test]
    fn empty_stop_response_maps_to_empty_response_error() {
        let chunks = translate(vec![
            json!({"choices":[{"delta":{},"finish_reason":"stop"}]}).to_string(),
            "[DONE]".to_string(),
        ])
        .unwrap();
        match chunks.last() {
            Some(StreamChunk::Finish { reason: FinishReason::Error { failure }, .. }) => {
                assert_eq!(failure.code, EMPTY_RESPONSE_CODE);
            }
            other => panic!("expected EMPTY_RESPONSE error finish, got {other:?}"),
        }
    }

    #[test]
    fn missing_done_is_stream_closed() {
        let err = translate(vec![json!({"choices":[{"delta":{"content":"hi"}}]}).to_string()]).unwrap_err();
        assert_eq!(err.code, "STREAM_CLOSED");
    }
}

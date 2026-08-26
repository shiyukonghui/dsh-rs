//! 把 harness 消息序列化为 DeepSeek chat completions wire（对齐
//! `deepseek-harness/packages/llm/llm-deepseek/src/serialize.ts`）。
//!
//! 文本化约定（M1 无 image 路径，M6 前 reject）：`assert_text_only` 在任一平铺文本前
//! 拒绝 core image content（`UNSUPPORTED_CONTENT`）。thinking/effort 解析进 wire
//! 顶层 `thinking`/`reasoning_effort`；pure tool-call 回合回放 `content: ""`（永不 null）。

use dsh_llm::types::{
    ContentBlock, GenerateOptions, LlmFailure, Message, Purpose, Role,
};
use dsh_llm::LlmError;

use crate::types::{
    WireAssistantContent, WireMessage, WireRequest, WireStreamOptions, WireThinking, WireTool,
    WireToolCall, WireToolCallFunction, WireToolFunction, WireUserContent,
};

/// 适配器级请求默认值（来自插件配置）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RequestDefaults {
    pub thinking: Option<Thinking>,
    pub reasoning_effort: Option<Effort>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Thinking {
    Enabled,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effort {
    Off,
    Low,
    High,
    Max,
}

impl Effort {
    fn wire(&self) -> &'static str {
        match self {
            Effort::Off => "off",
            Effort::Low => "low",
            Effort::High => "high",
            Effort::Max => "max",
        }
    }
    fn from_str(s: &str) -> Option<Effort> {
        match s {
            "off" => Some(Effort::Off),
            "low" => Some(Effort::Low),
            "high" => Some(Effort::High),
            "max" => Some(Effort::Max),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct ResolvedThinking {
    thinking: Option<Thinking>,
    reasoning_effort: Option<Effort>,
}

/// 校验适配器拥有的 effort，再解析为 DeepSeek wire 字段。
fn reasoning_effort(effort: &str) -> Result<Effort, LlmError> {
    Effort::from_str(effort)
        .ok_or_else(|| {
            LlmError::new(format!("DeepSeek does not support reasoning effort \"{effort}\""), "UNSUPPORTED_REASONING_EFFORT")
        })
}

/// 解析一组合法 thinking/effort 对。
fn resolve_thinking(options: &GenerateOptions, defaults: &RequestDefaults) -> Result<ResolvedThinking, LlmError> {
    if options.purpose == Some(Purpose::SessionTitle) {
        return Ok(ResolvedThinking { thinking: Some(Thinking::Disabled), reasoning_effort: None });
    }
    let effort = match &options.reasoning_effort {
        Some(e) => Some(reasoning_effort(e.raw())?),
        None => defaults.reasoning_effort,
    };
    if defaults.thinking == Some(Thinking::Disabled) {
        match effort {
            None | Some(Effort::Off) => {}
            Some(e) => {
                return Err(LlmError::new(
                    format!(
                        "DeepSeek deployment does not support reasoning effort \"{}\"",
                        e.wire()
                    ),
                    "UNSUPPORTED_REASONING_EFFORT",
                ))
            }
        }
    }
    match effort {
        Some(Effort::Off) => Ok(ResolvedThinking { thinking: Some(Thinking::Disabled), reasoning_effort: None }),
        Some(e @ (Effort::Low | Effort::High | Effort::Max)) => {
            Ok(ResolvedThinking { thinking: Some(Thinking::Enabled), reasoning_effort: Some(e) })
        }
        None => Ok(ResolvedThinking { thinking: defaults.thinking, reasoning_effort: None }),
    }
}

/// 拼接一条消息的 text blocks。
fn flatten_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|b| b.as_text())
        .map(|t| t.text.clone())
        .collect::<Vec<_>>()
        .join("")
}

/// 任一文本平铺路径前的图片拒绝（M1 文本化；M6 attachment 迁移后走 image 路径）。
fn assert_text_only(blocks: &[ContentBlock]) -> Result<(), LlmError> {
    if blocks.iter().any(|b| matches!(b, ContentBlock::Image(_))) {
        return Err(LlmError::new(
            "The DeepSeek chat-completions adapter does not support image content.",
            "UNSUPPORTED_CONTENT",
        ));
    }
    Ok(())
}

/// 序列化一条 assistant 消息（text + reasoning + tool calls）。
fn serialize_assistant(message: &Message) -> WireMessage {
    let text = flatten_text(&message.content);
    let reasoning = message.content
        .iter()
        .filter_map(|b| b.as_reasoning())
        .map(|r| r.text.clone())
        .collect::<Vec<_>>()
        .join("");
    let tool_calls: Vec<WireToolCall> = message.content
        .iter()
        .filter_map(|b| b.as_tool_call())
        .map(|t| WireToolCall {
            id: t.id.raw().to_string(),
            type_: "function".into(),
            function: WireToolCallFunction { name: t.name.clone(), arguments: t.arguments.clone() },
        })
        .collect();
    let mut assistant = WireMessage::Assistant {
        content: Some(WireAssistantContent::Text(text)),
        reasoning_content: (!reasoning.is_empty()).then_some(reasoning),
        tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
    };
    // 保持 assistant content 永不 null：Text("") 已覆盖 pure tool-call 回合。
    let _ = &mut assistant;
    assistant
}

/// 序列化会话消息。tool-result blocks 成为独立 `role: 'tool'` 消息。
pub fn serialize_messages(messages: &[Message]) -> Result<Vec<WireMessage>, LlmError> {
    let mut wire: Vec<WireMessage> = Vec::new();
    for message in messages {
        assert_text_only(&message.content)?;
        match message.role {
            Role::System => {
                wire.push(WireMessage::System { content: flatten_text(&message.content) });
            }
            Role::Assistant => {
                wire.push(serialize_assistant(message));
            }
            Role::User => {
                let tool_results: Vec<_> = message.content.iter().filter_map(|b| b.as_tool_result()).collect();
                let text = flatten_text(&message.content);
                if !text.is_empty() || tool_results.is_empty() {
                    wire.push(WireMessage::User { content: WireUserContent::Text(text) });
                }
                for result in tool_results {
                    let content_text = flatten_text(&result.content);
                    let content = if content_text.is_empty() {
                        "(no output)".to_string()
                    } else {
                        content_text
                    };
                    wire.push(WireMessage::Tool {
                        tool_call_id: result.tool_call_id.raw().to_string(),
                        content,
                    });
                }
            }
        }
    }
    Ok(wire)
}

/// 组装共享请求字段（text-only 与 image-capable 共用）。
fn request_with_messages(
    options: &GenerateOptions,
    messages: Vec<WireMessage>,
    defaults: &RequestDefaults,
) -> Result<WireRequest, LlmError> {
    let tools: Option<Vec<WireTool>> = options.tools.as_ref().map(|tools| {
        tools
            .iter()
            .map(|tool| WireTool {
                type_: "function".into(),
                function: WireToolFunction {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    parameters: tool.parameters.clone(),
                },
            })
            .collect()
    });
    let resolved = resolve_thinking(options, defaults)?;
    Ok(WireRequest {
        model: options.model.clone(),
        messages,
        stream: true,
        stream_options: WireStreamOptions { include_usage: true },
        thinking: resolved.thinking.map(|t| WireThinking {
            type_: if t == Thinking::Enabled { "enabled" } else { "disabled" }.into(),
        }),
        reasoning_effort: resolved.reasoning_effort.map(|e| e.wire().to_string()),
        tools: tools.filter(|t| !t.is_empty()),
        temperature: options.temperature,
        max_tokens: options.max_tokens,
        stop: options.stop.clone(),
    })
}

/// 构造完整 wire 请求。总是流式（`stream: true` + usage 上报）；可选字段省略而非 null。
pub fn serialize_request(options: &GenerateOptions, defaults: &RequestDefaults) -> Result<WireRequest, LlmError> {
    let mut messages: Vec<WireMessage> = Vec::new();
    if let Some(system) = &options.system {
        messages.push(WireMessage::System { content: system.clone() });
    }
    messages.extend(serialize_messages(&options.messages)?);
    request_with_messages(options, messages, defaults)
}

/// 便捷：把 `LlmError` 的失败事实拆出（适配器边界转换用）。
pub(crate) fn failure_of(err: &LlmError) -> LlmFailure {
    LlmFailure {
        message: err.message.clone(),
        code: err.code.clone(),
        status: None,
        provider_retry_after_ms: None,
        request_id: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_llm::types::{CallId, MessageId};
    use serde_json::json;

    fn user_msg(text: &str) -> Message {
        Message::user(MessageId::from_raw("u"), vec![ContentBlock::Text(dsh_llm::types::TextBlock { text: text.into() })])
    }

    fn assistant_msg(text: &str) -> Message {
        Message::assistant(MessageId::from_raw("a"), "deepseek", "deepseek-chat", vec![ContentBlock::Text(dsh_llm::types::TextBlock { text: text.into() })])
    }

    fn tool_result_msg(call_id: &str, content: &str) -> Message {
        // harness 词表：tool result 是 user 角色消息里的 tool-result block
        let block = ContentBlock::ToolResult(dsh_llm::types::ToolResultBlock {
            tool_call_id: CallId::from_raw(call_id),
            content: vec![ContentBlock::Text(dsh_llm::types::TextBlock { text: content.into() })],
            is_error: None,
        });
        Message::tool_result(
            MessageId::from_raw("t"),
            CallId::from_raw(call_id),
            vec![block],
        )
    }

    fn options(provider_model: (&str, &str)) -> GenerateOptions {
        GenerateOptions {
            provider: provider_model.0.into(),
            model: provider_model.1.into(),
            reasoning_effort: None,
            messages: vec![],
            system: None,
            tools: None,
            temperature: None,
            max_tokens: None,
            stop: None,
            session_id: None,
            purpose: None,
            signal: None,
        }
    }

    #[test]
    fn system_and_messages_serialize_in_order() {
        let mut ops = options(("deepseek", "deepseek-chat"));
        ops.system = Some("you are helpful".into());
        ops.messages = vec![user_msg("hi"), assistant_msg("hello")];
        let req = serialize_request(&ops, &RequestDefaults::default()).unwrap();
        assert_eq!(req.messages.len(), 3);
        assert_eq!(req.messages[0], WireMessage::System { content: "you are helpful".into() });
        assert_eq!(req.messages[1], WireMessage::User { content: WireUserContent::Text("hi".into()) });
        assert_eq!(req.messages[2], WireMessage::Assistant {
            content: Some(WireAssistantContent::Text("hello".into())),
            reasoning_content: None,
            tool_calls: None,
        });
        assert_eq!(req.model, "deepseek-chat");
        assert!(req.stream);
        assert!(req.stream_options.include_usage);    }

    #[test]
    fn tool_result_expands_to_tool_message() {
        let mut ops = options(("deepseek", "deepseek-chat"));
        ops.messages = vec![user_msg("call tool c1"), tool_result_msg("c1", "42")];
        let req = serialize_request(&ops, &RequestDefaults::default()).unwrap();
        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.messages[0], WireMessage::User { content: WireUserContent::Text("call tool c1".into()) });
        assert_eq!(req.messages[1], WireMessage::Tool { tool_call_id: "c1".into(), content: "42".into() });
    }

    #[test]
    fn empty_tool_output_gets_no_output_placeholder() {
        let mut ops = options(("deepseek", "deepseek-chat"));
        ops.messages = vec![tool_result_msg("c1", "")];
        let req = serialize_request(&ops, &RequestDefaults::default()).unwrap();
        assert_eq!(req.messages[0], WireMessage::Tool { tool_call_id: "c1".into(), content: "(no output)".into() });
    }

    #[test]
    fn assistant_tool_call_turn_replays_empty_content_never_null() {
        let call = ContentBlock::ToolCall(dsh_llm::types::ToolCallBlock {
            id: CallId::from_raw("c1"),
            name: "demo".into(),
            arguments: "{}".into(),
        });
        let msg = Message {
            id: MessageId::from_raw("a"),
            role: Role::Assistant,
            content: vec![call],
            source: dsh_llm::types::MessageSource::User, // 仅序列化 role/content
        };
        let mut ops = options(("deepseek", "deepseek-chat"));
        ops.messages = vec![msg];
        let req = serialize_request(&ops, &RequestDefaults::default()).unwrap();
        match &req.messages[0] {
            WireMessage::Assistant { content, tool_calls, .. } => {
                assert!(tool_calls.is_some());
                assert!(matches!(content, Some(WireAssistantContent::Text(s)) if s.is_empty()));
            }
            other => panic!("expected assistant, got {other:?}"),
        }
    }

    #[test]
    fn image_content_rejected_text_only() {
        let image = ContentBlock::Image(dsh_llm::types::ImageBlock {
            attachment: dsh_llm::types::ImageAttachmentRef {
                attachment_id: dsh_brand::AttachmentIdType::from_raw("att-1"),
                media_type: "image/png".into(),
                bytes: 10,
                width: 1,
                height: 1,
                name: None,
            },
        });
        let mut ops = options(("deepseek", "deepseek-chat"));
        ops.messages = vec![Message {
            id: MessageId::from_raw("u"),
            role: Role::User,
            content: vec![image],
            source: dsh_llm::types::MessageSource::User,
        }];
        let err = serialize_request(&ops, &RequestDefaults::default()).unwrap_err();
        assert!(err.message.contains("does not support image content."));
    }

    #[test]
    fn reasoning_passback_included_when_present() {
        let mut msg = assistant_msg("ok");
        msg.content.push(ContentBlock::Reasoning(dsh_llm::types::ReasoningBlock { text: "think".into() }));
        let mut ops = options(("deepseek", "deepseek-chat"));
        ops.messages = vec![msg];
        let req = serialize_request(&ops, &RequestDefaults::default()).unwrap();
        match &req.messages[0] {
            WireMessage::Assistant { reasoning_content, .. } => {
                assert_eq!(reasoning_content.as_deref(), Some("think"));
            }
            other => panic!("expected assistant, got {other:?}"),
        }
    }

    #[test]
    fn thinking_defaults_emit_wire_fields() {
        let defaults = RequestDefaults {
            thinking: Some(Thinking::Enabled),
            reasoning_effort: Some(Effort::High),
        };
        let ops = options(("deepseek", "deepseek-chat"));
        let req = serialize_request(&ops, &defaults).unwrap();
        assert_eq!(req.thinking, Some(WireThinking { type_: "enabled".into() }));
        assert_eq!(req.reasoning_effort.as_deref(), Some("high"));
        // 无 effort 请求不带 reasoning_effort 字段
        let defaults = RequestDefaults::default();
        let req = serialize_request(&ops, &defaults).unwrap();
        assert_eq!(req.reasoning_effort, None);
        assert_eq!(req.thinking, None);
    }

    #[test]
    fn session_title_disables_thinking() {
        let defaults = RequestDefaults { thinking: Some(Thinking::Enabled), reasoning_effort: None };
        let mut ops = options(("deepseek", "deepseek-chat"));
        ops.purpose = Some(Purpose::SessionTitle);
        let req = serialize_request(&ops, &defaults).unwrap();
        assert_eq!(req.thinking, Some(WireThinking { type_: "disabled".into() }));
    }

    #[test]
    fn unsupported_effort_rejected() {
        let mut ops = options(("deepseek", "deepseek-chat"));
        ops.reasoning_effort = Some(dsh_llm::ReasoningEffortId::from_raw("extreme"));
        let err = serialize_request(&ops, &RequestDefaults::default()).unwrap_err();
        assert!(err.message.contains("does not support reasoning effort"));
    }

    #[test]
    fn tools_serialize_function_schema() {
        let mut ops = options(("deepseek", "deepseek-chat"));
        ops.tools = Some(vec![dsh_llm::types::ToolSchema {
            name: "demo".into(),
            description: "do stuff".into(),
            parameters: json!({"type": "object", "properties": {}}),
        }]);
        let req = serialize_request(&ops, &RequestDefaults::default()).unwrap();
        let tools = req.tools.unwrap();
        assert_eq!(tools[0].function.name, "demo");
        assert_eq!(tools[0].function.parameters, json!({"type": "object", "properties": {}}));
    }

    #[test]
    fn golden_request_json_anchors_wire_parity() {
        // 与 TS `serializeRequest` 的字节级差异锚：字段名 snake_case、顺序与
        // `types.tsWireRequest` 一致、缺省字段省略、tool 结果独立成 role:'tool'。
        let mut ops = options(("deepseek", "deepseek-chat"));
        ops.system = Some("You are a helpful assistant.".into());
        ops.messages = vec![user_msg("What is the capital of France?"), tool_result_msg("c1", "(no output)")];
        let req = serialize_request(&ops, &RequestDefaults::default()).unwrap();
        let wire = serde_json::to_string(&req).unwrap();
        assert_eq!(
            wire,
            concat!(
                r#"{"model":"deepseek-chat","messages":["#,
                r#"{"role":"system","content":"You are a helpful assistant."},"#,
                r#"{"role":"user","content":"What is the capital of France?"},"#,
                r#"{"role":"tool","tool_call_id":"c1","content":"(no output)"}"#,
                r#"],"stream":true,"stream_options":{"include_usage":true}}"#
            )
        );
    }
}

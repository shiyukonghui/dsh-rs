//! DeepSeek chat-completions wire 格式（OpenAI 兼容）。仅类型。
//!
//! 权威参考：`deepseek-harness/packages/llm/llm-deepseek/src/types.ts`。
//! **wire 字段一律 snake_case**（OpenAI 兼容协议），Rust 字段名已用 snake_case，
//! 因此不施加 `rename_all`，序列化即按字段名直出。缺省字段用
//! `skip_serializing_if = "Option::is_none"` 抑制。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `POST {baseURL}/chat/completions` 请求体。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireRequest {
    pub model: String,
    pub messages: Vec<WireMessage>,
    pub stream: bool,
    pub stream_options: WireStreamOptions,
    /// 思考模式开关（wire 顶层，非 extra_body）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<WireThinking>,
    /// 推理强度（官方档位，仅 thinking 启用时）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<WireTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireStreamOptions {
    pub include_usage: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireThinking {
    #[serde(rename = "type")]
    pub type_: String,
}

/// 一条 wire 消息（按 role 判别）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role")]
pub enum WireMessage {
    #[serde(rename = "system")]
    System { content: String },
    #[serde(rename = "user")]
    User { content: WireUserContent },
    #[serde(rename = "assistant")]
    Assistant {
        content: Option<WireAssistantContent>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reasoning_content: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_calls: Option<Vec<WireToolCall>>,
    },
    #[serde(rename = "tool")]
    Tool {
        tool_call_id: String,
        content: String,
    },
}

/// user 消息内容：纯文本字符串或有序多模态输入。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WireUserContent {
    Text(String),
    Parts(Vec<WireUserContentPart>),
}

impl WireUserContent {
    pub fn is_empty(&self) -> bool {
        match self {
            WireUserContent::Text(s) => s.is_empty(),
            WireUserContent::Parts(parts) => parts.is_empty(),
        }
    }
}

/// assistant 消息 content：null 或字符串（工具调用专回放 ""，绝不 null）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WireAssistantContent {
    Null,
    Text(String),
}

/// 多模态 user 内容的一部分。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WireUserContentPart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image_url")]
    ImageUrl { image_url: WireImageUrl },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireImageUrl {
    pub url: String,
}

/// assistant 历史消息上回放的一条完整工具调用。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub type_: String,
    #[serde(rename = "function")]
    pub function: WireToolCallFunction,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireToolCallFunction {
    pub name: String,
    pub arguments: String,
}

/// 一条 wire 工具 schema（`parameters` 是 JSON Schema object）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireTool {
    #[serde(rename = "type")]
    pub type_: String,
    #[serde(rename = "function")]
    pub function: WireToolFunction,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireToolFunction {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

/// 一条解析后的 SSE `data:` 载荷（chat.completion.chunk）。
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct WireChunk {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub choices: Option<Vec<WireChoice>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Option<WireUsage>>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct WireChoice {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta: Option<WireDelta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<Option<String>>,
}

/// 一个流式 choice 的增量内容。
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct WireDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<WireToolCallDelta>>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct WireToolCallDelta {
    pub index: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub type_: Option<String>,
    #[serde(rename = "function", skip_serializing_if = "Option::is_none")]
    pub function: Option<WireToolCallDeltaFunction>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct WireToolCallDeltaFunction {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
}

/// wire token 记账。`prompt_tokens` 含缓存命中；`mapUsage` 减掉以保持层约定。
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct WireUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_hit_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_miss_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_tokens_details: Option<WirePromptTokensDetails>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_tokens_details: Option<WireCompletionTokensDetails>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct WirePromptTokensDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct WireCompletionTokensDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
}

/// 非 2xx 错误体。
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct WireError {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<WireErrorDetail>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct WireErrorDetail {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub type_: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

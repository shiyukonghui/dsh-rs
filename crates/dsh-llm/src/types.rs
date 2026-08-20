//! LLM 能力缝的 provider-neutral 消息与流式类型面（M0: `dsh-llm:types`）。
//!
//! 权威参考：`deepseek-harness/packages/llm/llm/src/{types,message,brand,call-config}.ts`。
//!
//! 设计要点（见 M0-CONTRACT-INFRA.md §8）：
//! - TS 的合并可扩展并集（ContentBlockMap/MessageSourceMap/FinishReasonMap/StreamChunk）在
//!   Rust 用 **tagged enum + Unknown 扩展点** 建模：已知变体走 serde tag，未知类型进入
//!   `Unknown { type_, data }` 并**无损保留原始 JSON**，保证与未来/插件类型可共存。
//! - 所有 wire 字段名与 TS 一致（kebab-case tag、camelCase 字段），保证逐字节等价。

use serde::de::Error as _;
use serde_json::{Map, Value};

/// 品牌 id（dsh-brand 拥有实现；本模块按名重新导出供公开 API 引用）。
pub use dsh_brand::{
    AttachmentIdType, CallId, MessageId, ProviderRequestId, ReasoningEffortId, SessionId,
};

/// 序列化 provider 或传输的失败事实；是否可重试由策略决定（`LlmFailure`）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmFailure {
    /// 人类可读的 provider/传输失败描述。
    pub message: String,
    /// 稳定、provider-neutral 的机器路由 code。
    pub code: String,
    /// provider 返回的 HTTP status（可用时）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u32>,
    /// provider 请求的重试延迟（毫秒，可用时）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_retry_after_ms: Option<u64>,
    /// 供诊断用的不透明 provider 请求标识。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<ProviderRequestId>,
}

/// 面向用户的纯文本块。
#[derive(Debug, Clone, PartialEq)]
pub struct TextBlock {
    pub text: String,
}

impl TextBlock {
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// 推理/思考内容，与可见文本区分。
#[derive(Debug, Clone, PartialEq)]
pub struct ReasoningBlock {
    pub text: String,
}

impl ReasoningBlock {
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// 可持久化的栅格图引用（角色中立；适配器当前只产出文本，图片只在 user 侧）。
#[derive(Debug, Clone, PartialEq)]
pub struct ImageBlock {
    pub attachment: ImageAttachmentRef,
}

/// 模型请求调用一个工具。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallBlock {
    /// provider 发出的调用 id；与匹配的 tool result 关联。
    pub id: CallId,
    pub name: String,
    /// 模型产出的原始 JSON 参数字符串（未经解析）。
    pub arguments: String,
}

impl ToolCallBlock {
    pub fn id(&self) -> &CallId {
        &self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// 一次工具调用的结果，回送给模型。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolResultBlock {
    pub tool_call_id: CallId,
    pub content: Vec<ContentBlock>,
    pub is_error: Option<bool>,
}

impl ToolResultBlock {
    pub fn tool_call_id(&self) -> &CallId {
        &self.tool_call_id
    }
    pub fn is_error(&self) -> Option<bool> {
        self.is_error
    }
}

/// 合并可扩展内容块：按 `type` 判别，核心类型可扩展（插件合并）。
#[derive(Debug, Clone, PartialEq)]
pub enum ContentBlock {
    Text(TextBlock),
    Reasoning(ReasoningBlock),
    Image(ImageBlock),
    ToolCall(ToolCallBlock),
    ToolResult(ToolResultBlock),
    /// 合并扩展点：本 build 不认识但无损保留（含 type 字段的完整对象）。
    Unknown {
        /// 原始 `type` 标签。
        type_: String,
        /// 原始对象（含 `type`），回写时逐字段还原。
        data: Map<String, Value>,
    },
}

impl ContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        ContentBlock::Text(TextBlock { text: text.into() })
    }
    pub fn as_text(&self) -> Option<&TextBlock> {
        match self {
            ContentBlock::Text(t) => Some(t),
            _ => None,
        }
    }
    pub fn as_reasoning(&self) -> Option<&ReasoningBlock> {
        match self {
            ContentBlock::Reasoning(t) => Some(t),
            _ => None,
        }
    }
    pub fn as_tool_call(&self) -> Option<&ToolCallBlock> {
        match self {
            ContentBlock::ToolCall(t) => Some(t),
            _ => None,
        }
    }
    pub fn as_tool_result(&self) -> Option<&ToolResultBlock> {
        match self {
            ContentBlock::ToolResult(t) => Some(t),
            _ => None,
        }
    }
    pub fn type_(&self) -> &str {
        match self {
            ContentBlock::Text(_) => "text",
            ContentBlock::Reasoning(_) => "reasoning",
            ContentBlock::Image(_) => "image",
            ContentBlock::ToolCall(_) => "tool-call",
            ContentBlock::ToolResult(_) => "tool-result",
            ContentBlock::Unknown { type_, .. } => type_,
        }
    }
}

/// block 的 `type` 标签词汇（对齐 `ContentBlockType`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentBlockType {
    Text,
    Reasoning,
    Image,
    ToolCall,
    ToolResult,
    /// 扩展词（未知类型保留原串）。
    Unknown(String),
}

impl ContentBlockType {
    /// 已知 core 词表（对齐 TS `ContentBlockMap` 键）。
    pub const ALL: [&'static str; 5] = ["text", "reasoning", "image", "tool-call", "tool-result"];

    pub fn as_str(&self) -> &str {
        match self {
            ContentBlockType::Text => "text",
            ContentBlockType::Reasoning => "reasoning",
            ContentBlockType::Image => "image",
            ContentBlockType::ToolCall => "tool-call",
            ContentBlockType::ToolResult => "tool-result",
            ContentBlockType::Unknown(s) => s,
        }
    }
}

// 从词表字符串解析（未知词不视为错误——merge-extensible，回落 Unknown）
impl std::str::FromStr for ContentBlockType {
    type Err = std::convert::Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "text" => ContentBlockType::Text,
            "reasoning" => ContentBlockType::Reasoning,
            "image" => ContentBlockType::Image,
            "tool-call" => ContentBlockType::ToolCall,
            "tool-result" => ContentBlockType::ToolResult,
            other => ContentBlockType::Unknown(other.to_string()),
        })
    }
}

/// block 类型在 wire 上就是 tag 字符串。
impl serde::Serialize for ContentBlockType {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}
impl<'de> serde::Deserialize<'de> for ContentBlockType {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        // Infallible 解析（FromStr 语义：未知词回落 Unknown，不视为错误）
        Ok(s.parse().unwrap())
    }
}

impl serde::Serialize for ContentBlock {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let obj: Value = match self {
            ContentBlock::Text(t) => serde_json::json!({ "type": "text", "text": t.text }),
            ContentBlock::Reasoning(r) => {
                serde_json::json!({ "type": "reasoning", "text": r.text })
            }
            ContentBlock::Image(i) => {
                serde_json::json!({ "type": "image", "attachment": i.attachment })
            }
            ContentBlock::ToolCall(c) => serde_json::json!({
                "type": "tool-call", "id": c.id, "name": c.name, "arguments": c.arguments,
            }),
            ContentBlock::ToolResult(r) => {
                let mut obj = serde_json::json!({
                    "type": "tool-result",
                    "toolCallId": r.tool_call_id,
                    "content": r.content,
                });
                if let Some(e) = r.is_error {
                    obj["isError"] = serde_json::json!(e);
                }
                obj
            }
            ContentBlock::Unknown { data, .. } => Value::Object(data.clone()),
        };
        obj.serialize(s)
    }
}

impl<'de> serde::Deserialize<'de> for ContentBlock {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        let type_ = v
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| D::Error::custom("content block missing \"type\""))?;
        let obj = v
            .as_object()
            .ok_or_else(|| D::Error::custom("content block must be an object"))?;
        match type_ {
            "text" => Ok(ContentBlock::Text(TextBlock {
                text: req_str(obj, "text").map_err(D::Error::custom)?,
            })),
            "reasoning" => Ok(ContentBlock::Reasoning(ReasoningBlock {
                text: req_str(obj, "text").map_err(D::Error::custom)?,
            })),
            "image" => Ok(ContentBlock::Image(ImageBlock {
                attachment: req(obj, "attachment").map_err(D::Error::custom)?,
            })),
            "tool-call" => Ok(ContentBlock::ToolCall(ToolCallBlock {
                id: req(obj, "id").map_err(D::Error::custom)?,
                name: req_str(obj, "name").map_err(D::Error::custom)?,
                arguments: req_str(obj, "arguments").map_err(D::Error::custom)?,
            })),
            "tool-result" => Ok(ContentBlock::ToolResult(ToolResultBlock {
                tool_call_id: req(obj, "toolCallId").map_err(D::Error::custom)?,
                content: req(obj, "content").map_err(D::Error::custom)?,
                is_error: opt(obj, "isError").map_err(D::Error::custom)?,
            })),
            other => Ok(ContentBlock::Unknown {
                type_: other.to_string(),
                data: obj.clone(),
            }),
        }
    }
}

/// 附件能力缝的引用类型（M0 占位；M6 迁移 `@deepseek-ai/dsh-attachment` 时替换）。
/// wire 形状对齐 `api/sessions.schema.ts` 的 `imageAttachmentRefSchema`。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageAttachmentRef {
    pub attachment_id: AttachmentIdType,
    pub media_type: String,
    pub bytes: u64,
    pub width: u64,
    pub height: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// producer 声明的上下文形式（ContextForm）：`form` 答案「这是什么种类的信息」。
#[derive(Debug, Clone, PartialEq)]
pub enum ContextForm {
    /// 从工作区文件读出的、模型应遵循的指令。
    Instructions,
    /// 本会话可用项目目录，变化时重新发布。
    Catalog,
    /// 当前状态；同一 producer 的更新快照取代旧快照。
    Snapshot {
        /// 依组装顺序得名的贡献段。
        sections: Vec<ContextSnapshotSection>,
    },
    /// 一次性事件说明；不取代任何东西。
    Notice {
        /// 单行说明（展示时不必展开行）。
        summary: String,
    },
    /// 另一 agent 发给本 agent 的消息。
    Relay,
    /// 从其它会话日志提取的材料，可能已在路上化简。
    Recall,
}

impl ContextForm {
    /// `form` 的 wire 值（kebab-case；缺省 form 返回 `None` 语义由调用方处理）。
    pub fn form_tag(&self) -> &'static str {
        match self {
            ContextForm::Instructions => "instructions",
            ContextForm::Catalog => "catalog",
            ContextForm::Snapshot { .. } => "snapshot",
            ContextForm::Notice { .. } => "notice",
            ContextForm::Relay => "relay",
            ContextForm::Recall => "recall",
        }
    }

    fn to_json(&self) -> Value {
        match self {
            ContextForm::Instructions => serde_json::json!({ "form": "instructions" }),
            ContextForm::Catalog => serde_json::json!({ "form": "catalog" }),
            ContextForm::Snapshot { sections } => {
                serde_json::json!({ "form": "snapshot", "sections": sections })
            }
            ContextForm::Notice { summary } => {
                serde_json::json!({ "form": "notice", "summary": summary })
            }
            ContextForm::Relay => serde_json::json!({ "form": "relay" }),
            ContextForm::Recall => serde_json::json!({ "form": "recall" }),
        }
    }

    fn from_map(obj: &Map<String, Value>) -> Option<Result<ContextForm, &'static str>> {
        let form = obj.get("form").and_then(Value::as_str)?;
        Some(match form {
            "instructions" => Ok(ContextForm::Instructions),
            "catalog" => Ok(ContextForm::Catalog),
            "snapshot" => obj
                .get("sections")
                .and_then(|s| serde_json::from_value(s.clone()).ok())
                .map(|sections| ContextForm::Snapshot { sections })
                .ok_or("plugin snapshot form requires sections"),
            "notice" => obj
                .get("summary")
                .and_then(Value::as_str)
                .map(|s| ContextForm::Notice { summary: s.to_string() })
                .ok_or("plugin notice form requires summary"),
            "relay" => Ok(ContextForm::Relay),
            "recall" => Ok(ContextForm::Recall),
            _ => return Some(Err("unknown plugin form")),
        })
    }
}

/// 一个 `snapshot` 形式上下文中按名贡献的一段（组装顺序）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ContextSnapshotSection {
    pub name: String,
    pub text: String,
}

/// 模型来源（model-produced assistant message 必需的来源）。
#[derive(Debug, Clone, PartialEq)]
pub struct ModelMessageSource {
    pub provider: String,
    pub model: String,
    /// 适配器私有 lossless-JSON 重放状态（`ReplayEnvelope.response`）。
    pub replay_state: Option<Value>,
}

/// 用户角色消息携带一个 tool result 的来源。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolMessageSource {
    pub call_id: CallId,
}

impl ToolMessageSource {
    pub fn call_id(&self) -> &CallId {
        &self.call_id
    }
}

/// plugin 注入上下文来源（含可选的 ContextForm 声明）。
#[derive(Debug, Clone, PartialEq)]
pub struct PluginMessageSource {
    pub plugin: String,
    pub form: Option<ContextForm>,
}

impl PluginMessageSource {
    pub fn plugin(&self) -> &str {
        &self.plugin
    }
    pub fn form(&self) -> Option<&ContextForm> {
        self.form.as_ref()
    }
}

/// 合并可扩展消息来源：按 `kind` 判别（`MessageSourceMap`）。
#[derive(Debug, Clone, PartialEq)]
pub enum MessageSource {
    /// 直接的人类输入。
    User,
    /// 插件注入的上下文（file-change notices、skills、cron…）。
    Plugin(PluginMessageSource),
    /// 路由模型产生的助手消息。
    Model(ModelMessageSource),
    /// 携带一次工具结果的用户角色消息。
    Tool(ToolMessageSource),
    /// 合并扩展点：未知 kind 无损保留（含 kind 字段的完整对象）。
    Unknown {
        kind_: String,
        data: Map<String, Value>,
    },
}

impl MessageSource {
    pub fn as_model(&self) -> Option<&ModelMessageSource> {
        match self {
            MessageSource::Model(m) => Some(m),
            _ => None,
        }
    }
    pub fn as_tool(&self) -> Option<&ToolMessageSource> {
        match self {
            MessageSource::Tool(m) => Some(m),
            _ => None,
        }
    }
    pub fn as_plugin(&self) -> Option<&PluginMessageSource> {
        match self {
            MessageSource::Plugin(m) => Some(m),
            _ => None,
        }
    }
}

impl serde::Serialize for MessageSource {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let obj: Value = match self {
            MessageSource::User => serde_json::json!({ "kind": "user" }),
            MessageSource::Plugin(p) => {
                let mut obj = serde_json::json!({ "kind": "plugin", "plugin": p.plugin });
                if let Some(form) = &p.form {
                    match form.to_json() {
                        Value::Object(map) => {
                            for (k, v) in map {
                                obj[k] = v;
                            }
                        }
                        _ => unreachable!("context form serializes to object"),
                    }
                }
                obj
            }
            MessageSource::Model(m) => {
                let mut obj =
                    serde_json::json!({ "kind": "model", "provider": m.provider, "model": m.model });
                if let Some(rs) = &m.replay_state {
                    obj["replayState"] = rs.clone();
                }
                obj
            }
            MessageSource::Tool(t) => serde_json::json!({ "kind": "tool", "callId": t.call_id }),
            MessageSource::Unknown { data, .. } => Value::Object(data.clone()),
        };
        obj.serialize(s)
    }
}

impl<'de> serde::Deserialize<'de> for MessageSource {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        let obj = v
            .as_object()
            .ok_or_else(|| D::Error::custom("message source must be an object"))?;
        let kind = obj
            .get("kind")
            .and_then(Value::as_str)
            .ok_or_else(|| D::Error::custom("message source missing \"kind\""))?;
        match kind {
            "user" => Ok(MessageSource::User),
            "plugin" => {
                let plugin = req_str(obj, "plugin").map_err(D::Error::custom)?;
                let form = match obj.get("form") {
                    Some(_) => match ContextForm::from_map(obj) {
                        Some(Ok(f)) => Some(f),
                        Some(Err(msg)) => return Err(D::Error::custom(msg)),
                        None => None,
                    },
                    None => None,
                };
                Ok(MessageSource::Plugin(PluginMessageSource { plugin, form }))
            }
            "model" => Ok(MessageSource::Model(ModelMessageSource {
                provider: req_str(obj, "provider").map_err(D::Error::custom)?,
                model: req_str(obj, "model").map_err(D::Error::custom)?,
                replay_state: opt(obj, "replayState").map_err(D::Error::custom)?,
            })),
            "tool" => Ok(MessageSource::Tool(ToolMessageSource {
                call_id: req(obj, "callId").map_err(D::Error::custom)?,
            })),
            other => Ok(MessageSource::Unknown {
                kind_: other.to_string(),
                data: obj.clone(),
            }),
        }
    }
}

/// 消息角色（provider-neutral 会话角色）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

/// 一条不可变消息的共享表示（对齐 TS `Message` 接口的 wire 形状）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub role: Role,
    pub content: Vec<ContentBlock>,
    pub source: MessageSource,
}

impl Message {
    pub fn is_user(&self) -> bool {
        self.role == Role::User
    }
    pub fn is_assistant(&self) -> bool {
        self.role == Role::Assistant
    }
    pub fn role(&self) -> Role {
        self.role
    }
    pub fn source(&self) -> &MessageSource {
        &self.source
    }

    /// 构造一条 assistant 消息（role=assistant, source.kind=model）。
    pub fn assistant(
        id: MessageId,
        provider: impl Into<String>,
        model: impl Into<String>,
        content: Vec<ContentBlock>,
    ) -> Self {
        Message {
            id,
            role: Role::Assistant,
            content,
            source: MessageSource::Model(ModelMessageSource {
                provider: provider.into(),
                model: model.into(),
                replay_state: None,
            }),
        }
    }

    /// 构造一条 user 消息。
    pub fn user(id: MessageId, content: Vec<ContentBlock>) -> Self {
        Message {
            id,
            role: Role::User,
            content,
            source: MessageSource::User,
        }
    }

    /// 构造一条 tool-result 消息（role=user + tool 来源）。
    pub fn tool_result(id: MessageId, call_id: CallId, content: Vec<ContentBlock>) -> Self {
        Message {
            id,
            role: Role::User,
            content,
            source: MessageSource::Tool(ToolMessageSource { call_id }),
        }
    }
}

/// 为什么一次模型响应停止（`FinishReasonMap`，合并可扩展）。
#[derive(Debug, Clone, PartialEq)]
pub enum FinishReason {
    Stop,
    ToolCalls,
    MaxTokens,
    /// 被取消（携带取消时的失败）。
    Aborted { failure: LlmFailure },
    /// 出错（携带错误）。
    Error { failure: LlmFailure },
    Unknown {
        kind_: String,
        data: Map<String, Value>,
    },
}

impl serde::Serialize for FinishReason {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let obj: Value = match self {
            FinishReason::Stop => serde_json::json!({ "kind": "stop" }),
            FinishReason::ToolCalls => serde_json::json!({ "kind": "tool-calls" }),
            FinishReason::MaxTokens => serde_json::json!({ "kind": "max-tokens" }),
            FinishReason::Aborted { failure } => {
                serde_json::json!({ "kind": "aborted", "failure": failure })
            }
            FinishReason::Error { failure } => {
                serde_json::json!({ "kind": "error", "failure": failure })
            }
            FinishReason::Unknown { data, .. } => Value::Object(data.clone()),
        };
        obj.serialize(s)
    }
}

impl<'de> serde::Deserialize<'de> for FinishReason {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        let obj = v
            .as_object()
            .ok_or_else(|| D::Error::custom("finish reason must be an object"))?;
        let kind = obj
            .get("kind")
            .and_then(Value::as_str)
            .ok_or_else(|| D::Error::custom("finish reason missing \"kind\""))?;
        match kind {
            "stop" => Ok(FinishReason::Stop),
            "tool-calls" => Ok(FinishReason::ToolCalls),
            "max-tokens" => Ok(FinishReason::MaxTokens),
            "aborted" => Ok(FinishReason::Aborted {
                failure: req(obj, "failure").map_err(D::Error::custom)?,
            }),
            "error" => Ok(FinishReason::Error {
                failure: req(obj, "failure").map_err(D::Error::custom)?,
            }),
            other => Ok(FinishReason::Unknown {
                kind_: other.to_string(),
                data: obj.clone(),
            }),
        }
    }
}

/// 单次模型调用的 token 记账（缓存字段可选；计数 DISJOINT）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
}

/// 成功响应时适配器私有的 lossless-JSON 重放状态（组装时透传）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplayEnvelope {
    /// 响应级适配器私有元数据（ids、原生 stop reason）。
    pub response: Value,
    /// 按 block 首次出现顺序的逐块私有元数据。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocks: Option<Vec<Value>>,
}

/// 原始流协议块（适配器输出；`LlmRuntime.stream` 把 throw 归一为 terminal finish）。
#[derive(Debug, Clone, PartialEq)]
pub enum StreamChunk {
    BlockStart {
        index: u64,
        block_type: ContentBlockType,
    },
    TextDelta {
        index: u64,
        text: String,
    },
    ReasoningDelta {
        index: u64,
        text: String,
    },
    ToolCallDelta {
        index: u64,
        id: CallId,
        name: Option<String>,
        arguments_delta: String,
    },
    BlockEnd {
        index: u64,
        block: ContentBlock,
    },
    Usage {
        usage: TokenUsage,
    },
    Finish {
        reason: FinishReason,
        replay_state: Option<ReplayEnvelope>,
    },
    Unknown {
        type_: String,
        data: Map<String, Value>,
    },
}

impl StreamChunk {
    pub fn as_delta_text(&self) -> Option<&str> {
        match self {
            StreamChunk::TextDelta { text, .. } | StreamChunk::ReasoningDelta { text, .. } => {
                Some(text)
            }
            _ => None,
        }
    }
    pub fn as_tool_call_delta_args(&self) -> Option<&str> {
        match self {
            StreamChunk::ToolCallDelta { arguments_delta, .. } => Some(arguments_delta),
            _ => None,
        }
    }
    pub fn type_(&self) -> &str {
        match self {
            StreamChunk::BlockStart { .. } => "block-start",
            StreamChunk::TextDelta { .. } => "text-delta",
            StreamChunk::ReasoningDelta { .. } => "reasoning-delta",
            StreamChunk::ToolCallDelta { .. } => "tool-call-delta",
            StreamChunk::BlockEnd { .. } => "block-end",
            StreamChunk::Usage { .. } => "usage",
            StreamChunk::Finish { .. } => "finish",
            StreamChunk::Unknown { type_, .. } => type_,
        }
    }
}

impl serde::Serialize for StreamChunk {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let obj: Value = match self {
            StreamChunk::BlockStart { index, block_type } => serde_json::json!({
                "type": "block-start", "index": index, "blockType": block_type,
            }),
            StreamChunk::TextDelta { index, text } => {
                serde_json::json!({ "type": "text-delta", "index": index, "text": text })
            }
            StreamChunk::ReasoningDelta { index, text } => {
                serde_json::json!({ "type": "reasoning-delta", "index": index, "text": text })
            }
            StreamChunk::ToolCallDelta { index, id, name, arguments_delta } => {
                let mut obj = serde_json::json!({
                    "type": "tool-call-delta", "index": index, "id": id,
                    "argumentsDelta": arguments_delta,
                });
                if let Some(n) = name {
                    obj["name"] = serde_json::json!(n);
                }
                obj
            }
            StreamChunk::BlockEnd { index, block } => {
                serde_json::json!({ "type": "block-end", "index": index, "block": block })
            }
            StreamChunk::Usage { usage } => {
                serde_json::json!({ "type": "usage", "usage": usage })
            }
            StreamChunk::Finish { reason, replay_state } => {
                let mut obj = serde_json::json!({ "type": "finish", "reason": reason });
                if let Some(rs) = replay_state {
                    obj["replayState"] = serde_json::to_value(rs).unwrap();
                }
                obj
            }
            StreamChunk::Unknown { data, .. } => Value::Object(data.clone()),
        };
        obj.serialize(s)
    }
}

impl<'de> serde::Deserialize<'de> for StreamChunk {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        let obj = v
            .as_object()
            .ok_or_else(|| D::Error::custom("stream chunk must be an object"))?;
        let type_ = obj
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| D::Error::custom("stream chunk missing \"type\""))?;
        match type_ {
            "block-start" => Ok(StreamChunk::BlockStart {
                index: req(obj, "index").map_err(D::Error::custom)?,
                block_type: req(obj, "blockType").map_err(D::Error::custom)?,
            }),
            "text-delta" => Ok(StreamChunk::TextDelta {
                index: req(obj, "index").map_err(D::Error::custom)?,
                text: req_str(obj, "text").map_err(D::Error::custom)?,
            }),
            "reasoning-delta" => Ok(StreamChunk::ReasoningDelta {
                index: req(obj, "index").map_err(D::Error::custom)?,
                text: req_str(obj, "text").map_err(D::Error::custom)?,
            }),
            "tool-call-delta" => Ok(StreamChunk::ToolCallDelta {
                index: req(obj, "index").map_err(D::Error::custom)?,
                id: req(obj, "id").map_err(D::Error::custom)?,
                name: opt(obj, "name").map_err(D::Error::custom)?,
                arguments_delta: req_str(obj, "argumentsDelta").map_err(D::Error::custom)?,
            }),
            "block-end" => Ok(StreamChunk::BlockEnd {
                index: req(obj, "index").map_err(D::Error::custom)?,
                block: req(obj, "block").map_err(D::Error::custom)?,
            }),
            "usage" => Ok(StreamChunk::Usage {
                usage: req(obj, "usage").map_err(D::Error::custom)?,
            }),
            "finish" => Ok(StreamChunk::Finish {
                reason: req(obj, "reason").map_err(D::Error::custom)?,
                replay_state: opt(obj, "replayState").map_err(D::Error::custom)?,
            }),
            other => Ok(StreamChunk::Unknown {
                type_: other.to_string(),
                data: obj.clone(),
            }),
        }
    }
}

/// 发送给模型的工具 JSON-schema 描述（对齐 TS `ToolSchema`）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    /// 参数字段的 JSON Schema object。
    pub parameters: Value,
}

/// 一次辅助模型调用用途的分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Purpose {
    Compaction,
    SessionTitle,
}

impl Purpose {
    pub fn as_str(&self) -> &'static str {
        match self {
            Purpose::Compaction => "compaction",
            Purpose::SessionTitle => "session-title",
        }
    }
}

/// 单条完全组装好的模型请求（对齐 TS `GenerateOptions`）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateOptions {
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffortId>,
    /// 按 provider 所见顺序的会话消息（system slot 之后）。
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolSchema>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<Purpose>,
}

// ---- 路由/模型元数据（对齐 TS `llm/llm/src/types.ts` 的 LlmProviderInfo/…）----

/// 一个已注册 provider 路由的展示元数据。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmProviderInfo {
    /// `GenerateOptions.provider` 使用的路由键。
    pub id: String,
    /// 供选择器/诊断使用的人类可读 provider 名。
    pub name: String,
}

/// 合并可扩展的 provider 模型模态词表（`ModelModality`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelModality {
    Text,
    Image,
    /// 扩展词（未知模态保留原串）。
    Unknown(String),
}

impl ModelModality {
    pub fn as_str(&self) -> &str {
        match self {
            ModelModality::Text => "text",
            ModelModality::Image => "image",
            ModelModality::Unknown(s) => s,
        }
    }
}

impl serde::Serialize for ModelModality {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}
impl<'de> serde::Deserialize<'de> for ModelModality {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = String::deserialize(d)?;
        Ok(match v.as_str() {
            "text" => ModelModality::Text,
            "image" => ModelModality::Image,
            other => ModelModality::Unknown(other.to_string()),
        })
    }
}

/// 适配器发现的一条模型；目录成员是建议性的，不构成请求校验。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmModelInfo {
    /// 拥有这条模型条目的 provider 路由。
    pub provider: String,
    /// 传入 `GenerateOptions.model` 的模型 id。
    pub id: String,
    /// 供选择器使用的人类可读模型名。
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 接受的请求模态；缺席 = 未知，显式空 = 负面能力。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_modalities: Option<Vec<ModelModality>>,
}

impl LlmModelInfo {
    pub fn new(provider: impl Into<String>, id: impl Into<String>, name: impl Into<String>) -> Self {
        LlmModelInfo {
            provider: provider.into(),
            id: id.into(),
            name: name.into(),
            description: None,
            input_modalities: None,
        }
    }
}

/// provider 拥有的单条精确 provider/model 路由的上下文容量。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmModelContext {
    /// 合并的请求+响应上下文 token 上限。
    pub context_window: u64,
}

/// 适配器拥有的可选推理强度（供选择器展示）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmReasoningEffortInfo {
    pub id: ReasoningEffortId,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// 单条精确 provider/model 路由可选的推理强度。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmModelReasoningInfo {
    pub efforts: Vec<LlmReasoningEffortInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_effort: Option<ReasoningEffortId>,
}

/// 由所属适配器解析的精确路由模型元数据。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmResolvedModelInfo {
    pub provider: String,
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_modalities: Option<Vec<ModelModality>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<LlmModelContext>,
    /// 调用方省略时物化为请求的适配器配置输出上限。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_max_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<LlmModelReasoningInfo>,
}

// ---- JSON 辅助提取 ----

fn req<T: serde::de::DeserializeOwned>(obj: &Map<String, Value>, key: &str) -> Result<T, serde_json::Error> {
    serde_json::from_value(obj.get(key).cloned().unwrap_or(Value::Null))
}

fn req_str(obj: &Map<String, Value>, key: &str) -> Result<String, serde_json::Error> {
    match obj.get(key) {
        Some(Value::String(s)) => Ok(s.clone()),
        _ => Err(serde_json::Error::custom(format!("field {key:?} must be a string"))),
    }
}

fn opt<T: serde::de::DeserializeOwned>(obj: &Map<String, Value>, key: &str) -> Result<Option<T>, serde_json::Error> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => serde_json::from_value(v.clone()).map(Some),
    }
}

#[cfg(test)]
mod metadata_tests {
    use super::*;

    #[test]
    fn model_info_camel_case_roundtrip() {
        let info = LlmModelInfo {
            provider: "deepseek".into(),
            id: "deepseek-chat".into(),
            name: "DeepSeek Chat".into(),
            description: Some("desc".into()),
            input_modalities: Some(vec![ModelModality::Text]),
        };
        let v = serde_json::to_value(&info).unwrap();
        assert_eq!(v["inputModalities"][0], serde_json::json!("text"));
        assert_eq!(v["description"], serde_json::json!("desc"));
        let back: LlmModelInfo = serde_json::from_value(v).unwrap();
        assert_eq!(back, info);
    }
}

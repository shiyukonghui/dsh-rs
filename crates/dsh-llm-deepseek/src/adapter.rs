//! `DeepSeekAdapter`：对 DeepSeek（OpenAI 兼容）chat-completions 端点的
//! transport-only 适配器，产出 harness `StreamChunk`（对齐
//! `deepseek-harness/packages/llm/llm-deepseek/src/adapter.ts`）。
//!
//! 真实 HTTP/SSE IO 属于服务层线程桥（M1e）；本 crate 的适配器缝保持与
//! LlmAdapter 语义同构：`stream` 在同步核心消费一个「已获得 payloads」的
//! thunk（连接事实 + 荷载源在每次操作时解析）。wire 序列化、SSE 行解析、
//! translate 全在本 crate 单测覆盖，标称 goldens 供差分验证。

use std::sync::Arc;

use dsh_llm::types::{
    GenerateOptions, LlmModelInfo, LlmModelReasoningInfo, LlmProviderInfo,
    LlmReasoningEffortInfo, LlmResolvedModelInfo, ModelModality, ReasoningEffortId,
};
use dsh_llm::{LlmAdapter, LlmError, ResolvedRetryPolicy};

use crate::serialize::{failure_of, serialize_request, RequestDefaults};
use crate::translate::translate;
use crate::types::{WireError, WireRequest};

/// 目录中的一条可选模型条目。
#[derive(Debug, Clone)]
pub struct DeepSeekCatalogModel {
    pub id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub context_window: Option<u64>,
    pub max_tokens: Option<u64>,
    pub input_modalities: Option<Vec<ModelModality>>,
}

impl DeepSeekCatalogModel {
    pub fn new(id: impl Into<String>) -> Self {
        DeepSeekCatalogModel {
            id: id.into(),
            name: None,
            description: None,
            context_window: None,
            max_tokens: None,
            input_modalities: None,
        }
    }
}

/// 一次操作的已验证连接事实。插件层的
/// `resolve_connection` 是显式 resolve 步骤；适配器每次操作重读它——
/// 配置文件变更无需重新注册即达下一次请求。
#[derive(Debug, Clone)]
pub struct DeepSeekConnection {
    /// 端点基址；`/chat/completions` 追加。
    pub base_url: String,
    /// 应用于每次请求的默认值（thinking 模式、effort）。
    pub defaults: RequestDefaults,
    /// 显式请求值胜出时的默认输出上限。
    pub max_tokens: u64,
    /// 选定模型无精确值时的正面上下文容量。
    pub default_context_window: u64,
    /// 广告给 discovery 消费者的建议模型；请求不受限制。
    pub models: Vec<DeepSeekCatalogModel>,
    /// provider 拥有的模型请求重试策略（已解析）。
    pub retry_policy: ResolvedRetryPolicy,
}

/// transport thunk 的稳定类型：给定已解析连接事实 + wire 请求 + 原始选项，
/// 产出 SSE `data:` payloads（或失败）。
pub type PayloadsResolver =
    Arc<dyn Fn(&DeepSeekConnection, &WireRequest, &GenerateOptions) -> Result<Vec<String>, LlmError> + Send + Sync>;

/// 适配器构造选项：插件拥有的操作本地 resolve 钩子。
pub struct DeepSeekAdapterOptions {
    /// 当前验证的连接事实；每次操作调用一次。
    pub resolve_connection: Arc<dyn Fn() -> DeepSeekConnection + Send + Sync>,
    /// 解析一次操作的 SSE data payload 序列（transport thunk）。
    /// 服务层线程桥（M1e）在桥内执行真实 HTTP + SSE 字节 → payloads。
    pub resolve_payloads: PayloadsResolver,
}

/// 默认流空闲超时（真实桥使用；adapter 记录常量）。
pub const DEFAULT_STREAM_IDLE_TIMEOUT_MS: u64 = 300_000;
/// 默认合并上下文容量。
pub const DEFAULT_CONTEXT_WINDOW: u64 = 1_000_000;
/// 默认每请求输出 token 上限。
pub const DEFAULT_MAX_TOKENS: u64 = 256_000;

fn model_info(provider: &str, model: &DeepSeekCatalogModel) -> LlmModelInfo {
    LlmModelInfo {
        provider: provider.to_string(),
        id: model.id.clone(),
        name: model.name.clone().unwrap_or_else(|| model.id.clone()),
        description: model.description.clone(),
        input_modalities: Some(model.input_modalities.clone().unwrap_or_else(|| vec![ModelModality::Text])),
    }
}

/// 解析 `Retry-After` 头为毫秒延迟（对齐 TS `providerRetryAfterMs`）。
/// 当前在服务层桥（M1e）使用；同步核心单测覆盖秒值与边界。
#[allow(dead_code)]
pub fn provider_retry_after_ms(value: Option<&str>) -> Option<u64> {
    let value = value?;
    if value.bytes().all(|b| b.is_ascii_digit()) {
        let seconds: u64 = value.parse().ok()?;
        return seconds.checked_mul(1_000).filter(|d| *d > 0);
    }
    // HTTP-date → epoch 毫秒增量：M1 不做日期解析，保守 None
    let _ = value;
    None
}

/// 把 HTTP status 映射为稳定 LlmError code。
pub fn http_error_code(status: u32, wire: Option<&WireError>) -> String {
    if status == 401 || status == 403 {
        return "AUTH".into();
    }
    if status == 413 {
        return "INVALID_REQUEST".into();
    }
    let detail = match wire {
        Some(w) => {
            let detail = w.error.as_ref();
            vec![
                detail.and_then(|d| d.code.clone()),
                detail.and_then(|d| d.type_.clone()),
                detail.and_then(|d| d.message.clone()),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ")
        }
        None => String::new(),
    };
    if is_quota_exceeded(&detail) {
        return "QUOTA".into();
    }
    if status == 429 {
        return "RATE_LIMIT".into();
    }
    if status == 400 {
        if is_context_window_exceeded(&detail) {
            return "CONTEXT_WINDOW_EXCEEDED".into();
        }
        return "INVALID_REQUEST".into();
    }
    if status >= 500 {
        return "SERVER".into();
    }
    format!("HTTP_{status}")
}

/// 识别上下文窗口超限用语。
pub fn is_context_window_exceeded(detail: &str) -> bool {
    let d = detail.to_ascii_lowercase();
    d.contains("context length exceeded")
        || d.contains("context window exceeded")
        || d.contains("maximum context")
        || d.contains("too long for context")
        || d.contains("too large for context")
}

/// 识别配额/余额耗尽用语。
pub fn is_quota_exceeded(detail: &str) -> bool {
    let d = detail.to_ascii_lowercase();
    d.contains("insufficient quota")
        || d.contains("insufficient balance")
        || d.contains("insufficient credits")
        || d.contains("quota exceeded")
        || d.contains("usage limit exceeded")
        || d.contains("out of credits")
        || d.contains("out of budget")
}

fn find_reasoning(config: &DeepSeekConnection, default_effort: ReasoningEffortId) -> LlmModelReasoningInfo {
    let efforts = if config.defaults.thinking == Some(crate::serialize::Thinking::Disabled) {
        vec![
            LlmReasoningEffortInfo { id: ReasoningEffortId::from_raw("off"), name: "Off".into(), description: None },
        ]
    } else {
        vec![
            LlmReasoningEffortInfo { id: ReasoningEffortId::from_raw("off"), name: "Off".into(), description: None },
            LlmReasoningEffortInfo { id: ReasoningEffortId::from_raw("low"), name: "Low".into(), description: None },
            LlmReasoningEffortInfo { id: ReasoningEffortId::from_raw("high"), name: "High".into(), description: None },
            LlmReasoningEffortInfo { id: ReasoningEffortId::from_raw("max"), name: "Max".into(), description: None },
        ]
    };
    LlmModelReasoningInfo { efforts, default_effort: Some(default_effort) }
}

/// 第一个真实 `LlmAdapter`。一个实例服务于注册下的每个模型名
/// （harness model name 就是 wire model name）。
pub struct DeepSeekAdapter {
    config: DeepSeekAdapterOptions,
}

impl DeepSeekAdapter {
    pub fn new(config: DeepSeekAdapterOptions) -> Self {
        DeepSeekAdapter { config }
    }
}

impl LlmAdapter for DeepSeekAdapter {
    fn provider_info(&self, provider: &str) -> LlmProviderInfo {
        LlmProviderInfo { id: provider.to_string(), name: "DeepSeek".into() }
    }

    fn provider_retry_policy(&self, _provider: &str) -> Option<ResolvedRetryPolicy> {
        Some((self.config.resolve_connection)().retry_policy)
    }

    fn list_models(&self, provider: &str) -> Vec<LlmModelInfo> {
        (self.config.resolve_connection)()
            .models
            .iter()
            .map(|m| model_info(provider, m))
            .collect()
    }

    fn resolve_model(&self, provider: &str, model: &str) -> LlmResolvedModelInfo {
        let connection = (self.config.resolve_connection)();
        let configured = connection.models.iter().find(|m| m.id == model);
        let context_window = configured.and_then(|m| m.context_window).unwrap_or(connection.default_context_window);
        let default_effort = match connection.defaults.reasoning_effort {
            Some(crate::serialize::Effort::Off) => ReasoningEffortId::from_raw("off"),
            Some(crate::serialize::Effort::Low) => ReasoningEffortId::from_raw("low"),
            Some(crate::serialize::Effort::Max) => ReasoningEffortId::from_raw("max"),
            _ => ReasoningEffortId::from_raw("high"),
        };
        let base = match configured {
            // 未收录端点安全地视为 text-only：声明未验证的 image 能力会让宿主
            // 持久化端点可能在后续每回合拒绝的输入。
            None => LlmResolvedModelInfo {
                provider: provider.to_string(),
                id: model.to_string(),
                name: model.to_string(),
                description: None,
                input_modalities: Some(vec![ModelModality::Text]),
                context: Some(dsh_llm::types::LlmModelContext { context_window }),
                default_max_tokens: Some(connection.max_tokens),
                reasoning: None,
            },
            Some(m) => {
                let info = model_info(provider, m);
                LlmResolvedModelInfo {
                    provider: info.provider,
                    id: info.id,
                    name: info.name,
                    description: info.description,
                    input_modalities: info.input_modalities,
                    context: Some(dsh_llm::types::LlmModelContext { context_window }),
                    default_max_tokens: Some(m.max_tokens.unwrap_or(connection.max_tokens)),
                    reasoning: None,
                }
            }
        };
        let reasoning = find_reasoning(&connection, default_effort);
        LlmResolvedModelInfo { reasoning: Some(reasoning), ..base }
    }

    fn stream(&self, options: GenerateOptions) -> Box<dyn Iterator<Item = dsh_llm::types::StreamChunk>> {
        let connection = (self.config.resolve_connection)();
        // 图片输入在 M1 文本化适配器直接拒绝（serialize 侧 guard 已覆盖）。
        let body_result = serialize_request(&options, &connection.defaults);
        match body_result {
            Err(err) => Box::new(std::iter::once(dsh_llm::types::StreamChunk::Finish {
                reason: dsh_llm::types::FinishReason::Error { failure: failure_of(&err) },
                replay_state: None,
            })),
            Ok(body) => {
                let resolved = (self.config.resolve_payloads)(&connection, &body, &options);
                match resolved {
                    Err(err) => Box::new(std::iter::once(dsh_llm::types::StreamChunk::Finish {
                        reason: dsh_llm::types::FinishReason::Error { failure: failure_of(&err) },
                        replay_state: None,
                    })),
                    Ok(payloads) => {
                        match translate(payloads) {
                            Ok(chunks) => Box::new(chunks.into_iter()),
                            Err(err) => Box::new(std::iter::once(dsh_llm::types::StreamChunk::Finish {
                                reason: dsh_llm::types::FinishReason::Error {
                                    failure: dsh_llm::types::LlmFailure {
                                        message: err.message,
                                        code: err.code.to_string(),
                                        status: None,
                                        provider_retry_after_ms: None,
                                        request_id: None,
                                    },
                                },
                                replay_state: None,
                            })),
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_llm::types::{FinishReason, StreamChunk};
    use serde_json::json;

    fn connection() -> DeepSeekConnection {
        DeepSeekConnection {
            base_url: "https://api.deepseek.com".into(),
            defaults: RequestDefaults::default(),
            max_tokens: DEFAULT_MAX_TOKENS,
            default_context_window: DEFAULT_CONTEXT_WINDOW,
            models: vec![DeepSeekCatalogModel::new("deepseek-chat")],
            retry_policy: resolve_none_policy(),
        }
    }

    fn resolve_none_policy() -> ResolvedRetryPolicy {
        dsh_llm::retry::resolve_retry_policy(None, "deepseek").unwrap()
    }

    pub use crate::adapter::PayloadsResolver;
    fn adapter_with_payload(payloads: Vec<String>) -> DeepSeekAdapter {
        let connection: Arc<dyn Fn() -> DeepSeekConnection + Send + Sync> = Arc::new(connection);
        let resolve: PayloadsResolver = Arc::new(move |_conn, _req, _ops| Ok(payloads.clone()));
        DeepSeekAdapter::new(DeepSeekAdapterOptions {
            resolve_connection: connection,
            resolve_payloads: resolve,
        })
    }

    fn options() -> GenerateOptions {
        GenerateOptions {
            provider: "deepseek".into(),
            model: "deepseek-chat".into(),
            reasoning_effort: None,
            messages: vec![],
            system: None,
            tools: None,
            temperature: None,
            max_tokens: None,
            stop: None,
            session_id: None,
            purpose: None,
        }
    }

    #[test]
    fn provider_info_and_list_models() {
        let adapter = adapter_with_payload(vec![]);
        assert_eq!(adapter.provider_info("deepseek").name, "DeepSeek");
        let models = adapter.list_models("deepseek");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "deepseek-chat");
        assert_eq!(models[0].provider, "deepseek");
    }

    #[test]
    fn resolve_model_provides_defaults_and_reasoning() {
        let adapter = adapter_with_payload(vec![]);
        let info = adapter.resolve_model("deepseek", "deepseek-chat");
        assert_eq!(info.id, "deepseek-chat");
        assert_eq!(info.context.unwrap().context_window, DEFAULT_CONTEXT_WINDOW);
        assert_eq!(info.default_max_tokens, Some(DEFAULT_MAX_TOKENS));
        let reasoning = info.reasoning.unwrap();
        assert!(reasoning.efforts.iter().any(|e| e.id.raw() == "high"));
        // 未知模型 text-only
        let unknown = adapter.resolve_model("deepseek", "mystery");
        assert!(!unknown.input_modalities.clone().unwrap().contains(&ModelModality::Image));
    }

    #[test]
    fn stream_translates_payloads_to_chunks() {
        let adapter = adapter_with_payload(vec![
            json!({"choices":[{"delta":{"content":"hi"}}]}).to_string(),
            json!({"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":2}}).to_string(),
            "[DONE]".to_string(),
        ]);
        let chunks: Vec<StreamChunk> = adapter.stream(options()).collect();
        let mut assembler = dsh_llm::BlockAssembler::new();
        for chunk in &chunks {
            assembler.push(chunk.clone());
        }
        assert_eq!(assembler.blocks().len(), 1);
        assert_eq!(assembler.blocks()[0].type_(), "text");
        assert!(assembler.usage().is_some());
        assert_eq!(assembler.finish(), FinishReason::Stop);
    }

    #[test]
    fn stream_serialization_error_becomes_error_finish() {
        let adapter = adapter_with_payload(vec!["[DONE]".into()]);
        let mut ops = options();
        ops.reasoning_effort = Some(ReasoningEffortId::from_raw("extreme"));
        let chunks: Vec<StreamChunk> = adapter.stream(ops).collect();
        match chunks.first() {
            Some(StreamChunk::Finish { reason: FinishReason::Error { .. }, .. }) => {}
            other => panic!("expected error finish, got {other:?}"),
        }
    }

    #[test]
    fn http_error_code_maps_statuses() {
        assert_eq!(http_error_code(401, None), "AUTH");
        assert_eq!(http_error_code(429, None), "RATE_LIMIT");
        assert_eq!(http_error_code(500, None), "SERVER");
        assert_eq!(http_error_code(400, None), "INVALID_REQUEST");
        assert_eq!(http_error_code(418, None), "HTTP_418");
        let context_err = WireError {
            error: Some(crate::types::WireErrorDetail {
                message: Some("max context length exceeded".into()),
                type_: None,
                code: None,
            }),
        };
        assert_eq!(http_error_code(400, Some(&context_err)), "CONTEXT_WINDOW_EXCEEDED");
    }

    #[test]
    fn quota_and_context_classifiers() {
        assert!(is_quota_exceeded("Insufficient quota"));
        assert!(is_quota_exceeded("out of credits"));
        assert!(!is_quota_exceeded("rate limit"));
        assert!(is_context_window_exceeded("max context length exceeded"));
        assert!(is_context_window_exceeded("request too long for context"));
        assert!(!is_context_window_exceeded("bad request"));
    }

    #[test]
    fn provider_retry_after_seconds() {
        assert_eq!(provider_retry_after_ms(Some("2")), Some(2_000));
        assert_eq!(provider_retry_after_ms(Some("0")), None);
        assert_eq!(provider_retry_after_ms(None), None);
    }

    #[test]
    fn catalog_context_window_wins_over_default() {
        let connection = DeepSeekConnection {
            models: vec![DeepSeekCatalogModel {
                id: "big".into(),
                context_window: Some(64_000),
                ..DeepSeekCatalogModel::new("big")
            }],
            ..connection()
        };
        let conn: Arc<dyn Fn() -> DeepSeekConnection + Send + Sync> = Arc::new(move || connection.clone());
        let resolve: PayloadsResolver = Arc::new(|_c, _r, _o| Ok(vec![]));
        let adapter = DeepSeekAdapter::new(DeepSeekAdapterOptions { resolve_connection: conn, resolve_payloads: resolve });
        let info = adapter.resolve_model("deepseek", "big");
        assert_eq!(info.context.unwrap().context_window, 64_000);
    }
}

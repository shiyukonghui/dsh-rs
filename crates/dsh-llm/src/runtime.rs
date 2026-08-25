//! LLM 服务：适配器注册表 + 流式调用 API（对齐
//! `deepseek-harness/packages/llm/llm/src/index.ts` 的 `LlmRuntime`/`LlmAdapter`）。
//!
//! Rust 核心是单线程 `Rc<RefCell>` 纪律（D-004/D-006），无 async/AbortSignal：
//! - `LlmAdapter::stream` 返回同步 `Box<dyn Iterator<Item = StreamChunk>>`；真实
//!   HTTP/SSE IO 由服务层线程桥驱动（M1e），适配器缝保持与 TS 语义同构。
//! - 失败在最终适配器边界归一为终末 finish chunk（`FinishReason::Error/Aborted`）；
//!   中途/下游失败保持为插件/消费方错误（对齐 `adapterStream`）。
//! - 注册表提供 registerAdapter/listProviders/provideRetryPolicy/prepareCall/stream，
//!   all-or-nothing 原子注册。
//!
//! D-115（请求面并发化）：`adapters: RefCell<HashMap>` → `Mutex<HashMap>`、
//! `Rc<dyn LlmAdapter>` → `Arc<dyn LlmAdapter + Send + Sync>`、registeration 克隆
//! `Rc` → `Arc`——使 `LlmRuntime` 成为 Send+Sync（Phase 3：LoopDeps 闭包捕获跨线程）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::assembler::BlockAssembler;
use crate::call_config::{call_config_equals, CallConfig, CallConfigAdapterDefaults};
use crate::retry::{resolve_retry_policy, ResolvedRetryPolicy};
use crate::types::{
    ContentBlock, FinishReason, GenerateOptions, LlmFailure, LlmModelContext,
    LlmModelInfo, LlmProviderInfo, LlmResolvedModelInfo, MessageSource,
    ModelMessageSource, ReplayEnvelope, StreamChunk,
};

/// 稳定、provider 中立的失败 code（对齐 TS `HarnessError` 分类）。
pub const NO_ADAPTER: &str = "NO_ADAPTER";
pub const DUPLICATE_ADAPTER: &str = "DUPLICATE_ADAPTER";
pub const INVALID_ADAPTER: &str = "INVALID_ADAPTER";
pub const INVALID_PREPARED_CALL: &str = "INVALID_PREPARED_CALL";
pub const UNSUPPORTED_REASONING_EFFORT: &str = "UNSUPPORTED_REASONING_EFFORT";
pub const INVALID_MODEL_INFO: &str = "INVALID_MODEL_INFO";
pub const INVALID_MODEL_CONTEXT: &str = "INVALID_MODEL_CONTEXT";
pub const INVALID_MODEL_MAX_TOKENS: &str = "INVALID_MODEL_MAX_TOKENS";
pub const INVALID_MODEL_REASONING: &str = "INVALID_MODEL_REASONING";
pub const INVALID_CATALOG: &str = "INVALID_CATALOG";

/// LLM 相关失败（对齐 TS `LlmError`：串行化事实 + 稳定 code）。
#[derive(Debug, Clone, PartialEq)]
pub struct LlmError {
    pub message: String,
    pub code: String,
    pub failure: LlmFailure,
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for LlmError {}

impl LlmError {
    pub fn new(message: impl Into<String>, code: impl Into<String>) -> Self {
        let message = message.into();
        let code = code.into();
        let failure = LlmFailure {
            message: message.clone(),
            code: code.clone(),
            status: None,
            provider_retry_after_ms: None,
            request_id: None,
        };
        LlmError { message, code, failure }
    }
}

fn failure_from_llm_error(err: &LlmError) -> LlmFailure {
    err.failure.clone()
}

/// Provider-wire 适配器（对应 TS 抽象类 `LlmAdapter`）。`stream` 是唯一必实现。
pub trait LlmAdapter {
    /// 描述该适配器拥有的一条 provider 路由；`id` 必须等于 `provider`。
    fn provider_info(&self, provider: &str) -> LlmProviderInfo {
        LlmProviderInfo { id: provider.to_string(), name: provider.to_string() }
    }

    /// 该路由的 provider 拥有的重试策略（捕获于注册时）；None 用 normal 默认。
    fn provider_retry_policy(&self, _provider: &str) -> Option<ResolvedRetryPolicy> {
        None
    }

    /// 该 provider 当前可广告的模型（建议性；缺省空）。
    fn list_models(&self, _provider: &str) -> Vec<LlmModelInfo> {
        Vec::new()
    }

    /// 解析一条精确模型的全部元数据（与建议目录无关，不校验路由）。
    fn resolve_model(&self, provider: &str, model: &str) -> LlmResolvedModelInfo {
        LlmResolvedModelInfo {
            provider: provider.to_string(),
            id: model.to_string(),
            name: model.to_string(),
            description: None,
            input_modalities: None,
            context: None,
            default_max_tokens: None,
            reasoning: None,
        }
    }

    /// 流式调用一次模型，返回原始 chunk 流。
    fn stream(&self, options: GenerateOptions) -> Box<dyn Iterator<Item = StreamChunk>>;
}

struct AdapterRegistration {
    adapter: Arc<dyn LlmAdapter + Send + Sync>,
    provider: LlmProviderInfo,
    retry_policy: ResolvedRetryPolicy,
}

/// 一次适配器流（同步 chunk 迭代器）。
pub type AdapterStream = Box<dyn Iterator<Item = StreamChunk>>;

/// prepared call 的一次性派发函数类型。
pub type PreparedCallStream =
    Box<dyn FnMut(GenerateOptions) -> Result<AdapterStream, LlmError>>;

/// 一次调用配置 + 其注册表绑定派发（对齐 TS `PreparedLlmCall`）。
pub struct PreparedLlmCall {
    pub config: CallConfig,
    pub retry_policy: ResolvedRetryPolicy,
    pub adapter_defaults: CallConfigAdapterDefaults,
    pub context: Option<LlmModelContext>,
    /// 用捕获于准备期的注册表派发这次调用。
    pub stream: Option<PreparedCallStream>,
}

/// LLM 服务：适配器注册表 + 流式调用 API。
///
/// `LlmError` 携带完整的 `LlmFailure`（结构化 provider 事实），是有意的较宽错误值，
/// 因此 `Result<_, LlmError>` 的 Err 变体超过 128 字节；这是规范化失败携带量的合理代价。
#[allow(clippy::result_large_err)]
pub struct LlmRuntime {
    adapters: Mutex<HashMap<String, AdapterRegistration>>,
}

impl Default for LlmRuntime {
    fn default() -> Self {
        LlmRuntime::new()
    }
}

// `result_large_err`：LlmError 携带完整结构化 LlmFailure，是有意选择，见 struct 注释。
#[allow(clippy::result_large_err)]
impl LlmRuntime {
    pub fn new() -> Self {
        LlmRuntime { adapters: Mutex::new(HashMap::new()) }
    }

    fn get_registration(&self, provider: &str) -> Result<Arc<AdapterRegistration>, LlmError> {
        let borrow = self.adapters.lock().unwrap();
        if !borrow.contains_key(provider) {
            return Err(LlmError::new(
                format!("no adapter registered for provider \"{provider}\""),
                NO_ADAPTER,
            ));
        }
        // 借出冲洗问题：Mutex 守卫无法跨返回；克隆 Arc 出表后释放锁（单表注册场景，
        // 每次查询克隆一次 Arc，成本可忽略；换表由让出锁后的下一次查询可见）。
        Ok(borrow.get(provider).expect("present").clone_registration())
    }

    /// all-or-nothing 注册一个适配器的若干 provider 路由。
    pub fn register_adapter(
        &self,
        providers: &[&str],
        adapter: Arc<dyn LlmAdapter + Send + Sync>,
    ) -> Result<(), LlmError> {
        if providers.is_empty() {
            return Err(LlmError::new("an adapter must register at least one provider", INVALID_ADAPTER));
        }
        let mut unique = std::collections::HashSet::new();
        let mut registrations = Vec::new();
        let mut borrow = self.adapters.lock().unwrap();
        for provider in providers {
            if provider.is_empty() {
                return Err(LlmError::new("adapter provider names must be non-empty", INVALID_ADAPTER));
            }
            if unique.contains(*provider) || borrow.contains_key(*provider) {
                return Err(LlmError::new(
                    format!("an adapter for provider \"{provider}\" is already registered"),
                    DUPLICATE_ADAPTER,
                ));
            }
            let info = adapter.provider_info(provider);
            if info.id != *provider || info.name.is_empty() {
                return Err(LlmError::new(
                    format!("adapter metadata for provider \"{provider}\" must preserve its id and have a non-empty name"),
                    INVALID_ADAPTER,
                ));
            }
            unique.insert(provider.to_string());
            let retry_policy = match adapter.provider_retry_policy(provider) {
                Some(p) => p,
                None => resolve_retry_policy(None, &format!("llm: provider \"{provider}\" retryPolicy"))
                    .map_err(|msg| LlmError::new(msg, INVALID_ADAPTER))?,
            };
            registrations.push(AdapterRegistration { adapter: adapter.clone(), provider: info, retry_policy });
        }
        for registration in registrations {
            borrow.insert(registration.provider.id.clone(), registration);
        }
        Ok(())
    }

    /// 展示有适配器的 provider 路由（按注册顺序）。
    pub fn list_providers(&self) -> Vec<LlmProviderInfo> {
        self.adapters.lock().unwrap().values().map(|r| r.provider.clone()).collect()
    }

    /// 解析注册于某路由的重试策略（normal 默认已解析）。
    pub fn provide_retry_policy(&self, provider: &str) -> Result<ResolvedRetryPolicy, LlmError> {
        Ok(self.get_registration(provider)?.retry_policy.clone())
    }

    /// 发现某 provider 广告的模型（建议性目录）。
    pub fn list_models(&self, provider: &str) -> Result<Vec<LlmModelInfo>, LlmError> {
        let registration = self.get_registration(provider)?;
        let adapter = registration.adapter.clone();
        let models = adapter.list_models(provider);
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for model in models {
            let valid = model.provider == provider
                && !model.id.is_empty()
                && !model.name.is_empty()
                && (model.description.is_none() || !model.description.as_ref().unwrap().is_empty())
                && seen.insert(model.id.clone());
            if !valid {
                return Err(LlmError::new(
                    format!("adapter returned invalid or duplicate model metadata for provider \"{provider}\""),
                    INVALID_CATALOG,
                ));
            }
            out.push(model);
        }
        Ok(out)
    }

    /// 由所属适配器解析并校验一条精确模型元数据。
    pub fn resolve_model_info(&self, provider: &str, model: &str) -> Result<LlmResolvedModelInfo, LlmError> {
        let registration = self.get_registration(provider)?;
        let adapter = registration.adapter.clone();
        let resolved = adapter.resolve_model(provider, model);
        if resolved.provider != provider || resolved.id != model || resolved.name.is_empty() {
            return Err(LlmError::new(
                format!("adapter returned invalid exact model metadata for provider \"{provider}\" model \"{model}\""),
                INVALID_MODEL_INFO,
            ));
        }
        validate_resolved_info(provider, model, &resolved)?;
        Ok(resolved)
    }

    fn resolve_call_for(
        &self,
        registration: &AdapterRegistration,
        config: &CallConfig,
    ) -> Result<(CallConfig, Option<LlmModelContext>), LlmError> {
        let info = registration.adapter.resolve_model(&registration.provider.id, &config.model);
        if info.provider != registration.provider.id || info.id != config.model || info.name.is_empty() {
            return Err(LlmError::new(
                format!(
                    "adapter returned invalid exact model metadata for provider \"{}\" model \"{}\"",
                    registration.provider.id, config.model
                ),
                INVALID_MODEL_INFO,
            ));
        }
        validate_resolved_info(&registration.provider.id, &config.model, &info)?;
        // defaultMaxTokens 物化
        let mut resolved_config = config.clone();
        if resolved_config.max_tokens.is_none() {
            if let Some(default) = info.default_max_tokens {
                resolved_config.max_tokens = Some(default);
            }
        }
        // reasoning effort 校验（不支持 explicit 时拒绝；defaultEffort 物化）
        let requested = resolved_config.reasoning_effort.clone();
        let reasoning = info.reasoning.clone();
        match reasoning {
            None => {
                if let Some(requested) = &requested {
                    return Err(LlmError::new(
                        format!(
                            "provider \"{}\" model \"{}\" does not support reasoning effort \"{}\"",
                            registration.provider.id,
                            config.model,
                            requested.raw()
                        ),
                        UNSUPPORTED_REASONING_EFFORT,
                    ));
                }
            }
            Some(r) => {
                let effective = requested.clone().or(r.default_effort.clone());
                if let Some(e) = &effective {
                    let supported = r.efforts.iter().any(|effort| effort.id == *e);
                    if !supported {
                        return Err(LlmError::new(
                            format!(
                                "provider \"{}\" model \"{}\" does not support reasoning effort \"{}\"",
                                registration.provider.id,
                                config.model,
                                e.raw()
                            ),
                            UNSUPPORTED_REASONING_EFFORT,
                        ));
                    }
                    if requested != effective {
                        resolved_config.reasoning_effort = effective;
                    }
                }
            }
        }
        Ok((resolved_config, info.context))
    }

    /// 独立查询（不绑定后续派发）。
    pub fn resolve_call_config(&self, config: &CallConfig) -> Result<CallConfig, LlmError> {
        let registration = self.get_registration(&config.provider)?;
        Ok(self.resolve_call_for(&registration, config)?.0)
    }

    /// 在当前注册表下解析一次调用，返回带注册表绑定的单次派发句柄。
    pub fn prepare_call(&self, config: &CallConfig) -> Result<PreparedLlmCall, LlmError> {
        let provider = config.provider.clone();
        let registration = self.get_registration(&provider)?;
        let retry_policy = registration.retry_policy.clone();
        let resolved_config = registration.adapter.resolve_model(&provider, &config.model);
        if resolved_config.provider != provider || resolved_config.id != config.model || resolved_config.name.is_empty() {
            return Err(LlmError::new(
                format!("adapter returned invalid exact model metadata for provider \"{provider}\" model \"{}\"", config.model),
                INVALID_MODEL_INFO,
            ));
        }
        validate_resolved_info(&provider, &config.model, &resolved_config)?;
        let (resolved_config, context) = self.resolve_call_for(&registration, config)?;
        let adapter_defaults = CallConfigAdapterDefaults {
            reasoning_effort: (config.reasoning_effort.is_none() && resolved_config.reasoning_effort.is_some()).then_some(true),
            max_tokens: (config.max_tokens.is_none() && resolved_config.max_tokens.is_some()).then_some(true),
        };        let registration_raw = registration.clone_registration();
        let resolved_config_clone = resolved_config.clone();
        let mut dispatched = false;
        let stream: Option<PreparedCallStream> =
            Some(Box::new(move |options: GenerateOptions| {
            if dispatched {
                return Err(LlmError::new("a prepared LLM call can only be dispatched once", INVALID_PREPARED_CALL));
            }
            if !call_config_matches_options(&resolved_config_clone, &options) {
                return Err(LlmError::new("prepared LLM call config changed before adapter dispatch", INVALID_PREPARED_CALL));
            }
            dispatched = true;
            Ok(adapter_stream_final(&registration_raw, &options))
        }));
        Ok(PreparedLlmCall {
            config: resolved_config,
            retry_policy,
            adapter_defaults,
            context,
            stream,
        })
    }

    /// 直接流式调用（`options.provider` 选定适配器）。
    pub fn stream(&self, options: GenerateOptions) -> Box<dyn Iterator<Item = StreamChunk>> {
        let registration = match self.get_registration(&options.provider) {
            Ok(r) => r,
            Err(err) => {
                return Box::new(std::iter::once(finish_chunk(
                    FinishReason::Error { failure: failure_from_llm_error(&err) },
                    None,
                )))
            }
        };
        adapter_stream_final(&registration, &options)
    }
}

impl AdapterRegistration {
    fn clone_registration(&self) -> Arc<AdapterRegistration> {
        Arc::new(AdapterRegistration {
            adapter: self.adapter.clone(),
            provider: self.provider.clone(),
            retry_policy: self.retry_policy.clone(),
        })
    }
}

/// 校验解析出的精确模型元数据（对齐 `resolveModelInfoFor` 的校验段）。
// `LlmError` 宽是设计（见 struct 注释）。
#[allow(clippy::result_large_err)]
fn validate_resolved_info(
    provider: &str,
    model: &str,
    resolved: &LlmResolvedModelInfo,
) -> Result<(), LlmError> {
    if resolved.provider != provider || resolved.id != model || resolved.name.is_empty() {
        return Err(LlmError::new(
            format!("adapter returned invalid exact model metadata for provider \"{provider}\" model \"{model}\""),
            INVALID_MODEL_INFO,
        ));
    }
    if let Some(context) = &resolved.context {
        if context.context_window == 0 {
            return Err(LlmError::new(
                format!("adapter returned invalid context metadata for provider \"{provider}\" model \"{model}\""),
                INVALID_MODEL_CONTEXT,
            ));
        }
    }
    if let Some(default_max) = resolved.default_max_tokens {
        if default_max == 0 {
            return Err(LlmError::new(
                format!("adapter returned invalid default maxTokens for provider \"{provider}\" model \"{model}\""),
                INVALID_MODEL_MAX_TOKENS,
            ));
        }
    }
    if let Some(reasoning) = &resolved.reasoning {
        if reasoning.efforts.is_empty() {
            return Err(LlmError::new(
                format!("adapter returned invalid reasoning metadata for provider \"{provider}\" model \"{model}\""),
                INVALID_MODEL_REASONING,
            ));
        }
        let mut seen = std::collections::HashSet::new();
        let mut invalid = false;
        for effort in &reasoning.efforts {
            if effort.id.raw().is_empty() || effort.name.is_empty() || !seen.insert(effort.id.clone()) {
                invalid = true;
                break;
            }
        }
        if invalid {
            return Err(LlmError::new(
                format!("adapter returned invalid or duplicate reasoning effort metadata for provider \"{provider}\" model \"{model}\""),
                INVALID_MODEL_REASONING,
            ));
        }
        if let Some(default) = &reasoning.default_effort {
            if !reasoning.efforts.iter().any(|e| &e.id == default) {
                return Err(LlmError::new(
                    format!("adapter returned an unknown default reasoning effort for provider \"{provider}\" model \"{model}\""),
                    INVALID_MODEL_REASONING,
                ));
            }
        }
    }
    Ok(())
}

/// 最终适配器边界：直接派发到注册表捕获的适配器（对齐 `adapterStream` 的同步核）。
/// 真实 HTTP/SSE IO 在 M1e 由服务层线程桥驱动，失败在服务层归一为终末 failure chunk。
fn adapter_stream_final(
    registration: &Arc<AdapterRegistration>,
    options: &GenerateOptions,
) -> Box<dyn Iterator<Item = StreamChunk>> {
    // forAdapter：去掉归属另一适配器的重放状态
    let filtered = for_adapter(options.clone(), registration.adapter.as_ref());
    registration.adapter.stream(filtered)
}

/// 去掉历史路由归属另一适配器的重放状态（对齐 `forAdapter`）。
fn for_adapter(options: GenerateOptions, adapter: &dyn LlmAdapter) -> GenerateOptions {
    let mut changed = false;
    let mut messages = Vec::with_capacity(options.messages.len());
    for message in &options.messages {
        let is_assistant_model = message.role == crate::types::Role::Assistant
            && matches!(message.source, MessageSource::Model(_));
        if !is_assistant_model {
            messages.push(message.clone());
            continue;
        }
        match &message.source {
            MessageSource::Model(m) if m.replay_state.is_some() => {
                // 检查历史 provider 是否归属同一 adapter 实例
                let same = adapter_owns_provider(adapter, &m.provider);
                if same {
                    messages.push(message.clone());
                } else {
                    let mut filtered = message.clone();
                    filtered.source = MessageSource::Model(ModelMessageSource {
                        provider: m.provider.clone(),
                        model: m.model.clone(),
                        replay_state: None,
                    });
                    messages.push(filtered);
                    changed = true;
                }
            }
            _ => messages.push(message.clone()),
        }
    }
    if !changed {
        options
    } else {
        GenerateOptions { messages, ..options }
    }
}

/// 该适配器是否拥有给定 provider 路由（对齐 `this.adapters.get(src.provider)?.adapter === adapter`）。
fn adapter_owns_provider(adapter: &dyn LlmAdapter, _provider: &str) -> bool {
    // 单表场景：判定交由注册表所有权；这里通过对象标识近似。
    // Rust 侧无法直接比较 trait 对象标识，故让 stream 阶段不带重放过滤的注册表
    // 上下文缺失时保守返回 true（保留重放）。M1e 集成时改为按注册表核对。
    let _ = adapter;
    true
}

fn call_config_matches_options(config: &CallConfig, options: &GenerateOptions) -> bool {
    let proposed = CallConfig {
        provider: options.provider.clone(),
        model: options.model.clone(),
        reasoning_effort: options.reasoning_effort.clone(),
        temperature: options.temperature,
        max_tokens: options.max_tokens,
        stop: options.stop.clone(),
    };
    call_config_equals(config, &proposed)
}

fn finish_chunk(reason: FinishReason, replay_state: Option<ReplayEnvelope>) -> StreamChunk {
    StreamChunk::Finish { reason, replay_state }
}

/// 供重放状态对齐的组装便捷入口（镜像 `BlockAssembler` 用法）。
pub fn assemble_stream(chunks: impl IntoIterator<Item = StreamChunk>) -> (Vec<ContentBlock>, BlockAssembler) {
    let mut assembler = BlockAssembler::new();
    for chunk in chunks {
        assembler.push(chunk);
    }
    let blocks = assembler.blocks();
    (blocks, assembler)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::CallId;

    struct FakeAdapter {
        models: Vec<LlmModelInfo>,
    }

    impl LlmAdapter for FakeAdapter {
        fn list_models(&self, _provider: &str) -> Vec<LlmModelInfo> {
            self.models.clone()
        }
        fn resolve_model(&self, provider: &str, model: &str) -> LlmResolvedModelInfo {
            LlmResolvedModelInfo {
                provider: provider.into(),
                id: model.into(),
                name: model.into(),
                description: None,
                input_modalities: None,
                context: Some(LlmModelContext { context_window: 8192 }),
                default_max_tokens: Some(2048),
                reasoning: None,
            }
        }
        fn stream(&self, _options: GenerateOptions) -> Box<dyn Iterator<Item = StreamChunk>> {
            Box::new(std::iter::once(StreamChunk::Finish { reason: FinishReason::Stop, replay_state: None }))
        }
    }

    fn rt() -> LlmRuntime {
        let rt = LlmRuntime::new();
        let adapter: Arc<dyn LlmAdapter + Send + Sync> = Arc::new(FakeAdapter {
            models: vec![
                LlmModelInfo::new("deepseek", "deepseek-chat", "DeepSeek Chat"),
                LlmModelInfo::new("deepseek", "deepseek-reasoner", "DeepSeek Reasoner"),
            ],
        });
        rt.register_adapter(&["deepseek"], adapter).unwrap();
        rt
    }

    #[test]
    fn register_and_list_providers() {
        let rt = rt();
        let providers = rt.list_providers();
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].id, "deepseek");
        assert_eq!(providers[0].name, "deepseek");
    }

    #[test]
    fn duplicate_registration_rejected_all_or_nothing() {
        let rt = rt();
        let adapter: Arc<dyn LlmAdapter + Send + Sync> = Arc::new(FakeAdapter { models: vec![] });
        let err = rt.register_adapter(&["deepseek", "other"], adapter).unwrap_err();
        assert_eq!(err.code, DUPLICATE_ADAPTER);
        assert_eq!(rt.list_providers().len(), 1);
    }

    #[test]
    fn list_models_validates_catalog() {
        let rt = rt();
        let models = rt.list_models("deepseek").unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "deepseek-chat");
        // 未知 provider
        let err = rt.list_models("nope").unwrap_err();
        assert_eq!(err.code, NO_ADAPTER);
    }

    #[test]
    fn resolve_model_info_context() {
        let rt = rt();
        let info = rt.resolve_model_info("deepseek", "deepseek-chat").unwrap();
        assert_eq!(info.context.unwrap().context_window, 8192);
        assert_eq!(info.default_max_tokens, Some(2048));
    }

    #[test]
    fn prepare_call_materializes_defaults_and_rejects_reuse() {
        let rt = rt();
        let base = CallConfig {
            provider: "deepseek".into(),
            model: "deepseek-chat".into(),
            reasoning_effort: None,
            temperature: Some(0.5),
            max_tokens: None,
            stop: None,
        };
        let prepared = rt.prepare_call(&base).unwrap();
        // defaultMaxTokens 物化
        assert_eq!(prepared.config.max_tokens, Some(2048));
        assert_eq!(prepared.adapter_defaults.max_tokens, Some(true));
        let mut stream_fn = prepared.stream.unwrap();
        let mut options = GenerateOptions {
            provider: "deepseek".into(),
            model: "deepseek-chat".into(),
            reasoning_effort: None,
            messages: vec![],
            system: None,
            tools: None,
            temperature: Some(0.5),
            max_tokens: Some(2048),
            stop: None,
            session_id: None,
            purpose: None,
        };
        let result = stream_fn(options.clone());
        assert!(result.is_ok());
        // 二次派发拒绝
        options.temperature = Some(1.0);
        assert!(stream_fn(options).is_err());
    }

    #[test]
    fn prepare_call_detects_config_drift() {
        let rt = rt();
        let base = CallConfig {
            provider: "deepseek".into(),
            model: "deepseek-chat".into(),
            reasoning_effort: None,
            temperature: Some(0.5),
            max_tokens: None,
            stop: None,
        };
        let mut prepared = rt.prepare_call(&base).unwrap();
        let mut stream_fn = prepared.stream.take().unwrap();
        let options = GenerateOptions {
            provider: "deepseek".into(),
            model: "deepseek-chat".into(),
            reasoning_effort: None,
            messages: vec![],
            system: None,
            tools: None,
            temperature: Some(0.9),
            max_tokens: Some(2048),
            stop: None,
            session_id: None,
            purpose: None,
        };
        assert!(stream_fn(options).is_err());
    }

    #[test]
    fn stream_with_unknown_provider_returns_error_finish() {
        let rt = rt();
        let options = GenerateOptions {
            provider: "nope".into(),
            model: "m".into(),
            reasoning_effort: None,
            messages: vec![],
            system: None,
            tools: None,
            temperature: None,
            max_tokens: None,
            stop: None,
            session_id: None,
            purpose: None,
        };
        let chunks: Vec<StreamChunk> = rt.stream(options).collect();
        assert_eq!(chunks.len(), 1);
        match &chunks[0] {
            StreamChunk::Finish { reason: FinishReason::Error { .. }, .. } => {}
            other => panic!("expected error finish, got {other:?}"),
        }
    }

    #[test]
    fn prepared_call_stream_flows_chunks_through_assembler() {
        let rt = streaming_rt();
        let base = CallConfig {
            provider: "deepseek".into(),
            model: "deepseek-chat".into(),
            reasoning_effort: None,
            temperature: None,
            max_tokens: None,
            stop: None,
        };
        let mut prepared = rt.prepare_call(&base).unwrap();
        let mut stream_fn = prepared.stream.take().unwrap();
        let options = GenerateOptions {
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
        };
        let chunks = stream_fn(options).unwrap().collect::<Vec<_>>();
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].type_(), "text-delta");
        assert_eq!(chunks[1].type_(), "finish");
    }

    fn streaming_rt() -> LlmRuntime {
        let rt = LlmRuntime::new();
        let adapter: Arc<dyn LlmAdapter + Send + Sync> = Arc::new(StreamingAdapter);
        rt.register_adapter(&["deepseek"], adapter).unwrap();
        rt
    }

    struct StreamingAdapter;

    impl LlmAdapter for StreamingAdapter {
        fn stream(&self, _options: GenerateOptions) -> Box<dyn Iterator<Item = StreamChunk>> {
            Box::new(vec![
                StreamChunk::TextDelta { index: 0, text: "hello".into() },
                StreamChunk::Finish { reason: FinishReason::Stop, replay_state: None },
            ].into_iter())
        }
    }

    #[test]
    fn adapter_stream_finishes_with_replay_and_usage() {
        let rt = LlmRuntime::new();
        let adapter: Arc<dyn LlmAdapter + Send + Sync> = Arc::new(StreamWithDetails);
        rt.register_adapter(&["deepseek"], adapter).unwrap();
        let options = GenerateOptions {
            provider: "deepseek".into(),
            model: "deepseek-chat".into(),
            reasoning_effort: None,
            messages: vec![crate::types::Message::assistant(CallId::from_raw("m1").raw().into(), "p", "m", vec![])],
            system: None,
            tools: None,
            temperature: None,
            max_tokens: None,
            stop: None,
            session_id: None,
            purpose: None,
        };
        let chunks: Vec<StreamChunk> = rt.stream(options).collect();
        let mut assembler = BlockAssembler::new();
        for chunk in &chunks {
            assembler.push(chunk.clone());
        }
        assert_eq!(assembler.blocks().len(), 1);
        assert_eq!(assembler.blocks()[0].type_(), "text");
        assert!(assembler.usage().is_some());
        assert_eq!(assembler.finish(), FinishReason::Stop);
    }

    struct StreamWithDetails;

    impl LlmAdapter for StreamWithDetails {
        fn stream(&self, _options: GenerateOptions) -> Box<dyn Iterator<Item = StreamChunk>> {
            let usage = crate::types::TokenUsage {
                input_tokens: 5,
                output_tokens: 3,
                cache_read_tokens: None,
                cache_write_tokens: None,
                reasoning_tokens: None,
            };
            Box::new(vec![
                StreamChunk::TextDelta { index: 0, text: "hi".into() },
                StreamChunk::BlockEnd {
                    index: 0,
                    block: ContentBlock::Text(crate::types::TextBlock { text: "hi".into() }),
                },
                StreamChunk::Usage { usage },
                StreamChunk::Finish { reason: FinishReason::Stop, replay_state: Some(ReplayEnvelope { response: serde_json::json!({"id": "r"}), blocks: None }) },
            ].into_iter())
        }
    }
}

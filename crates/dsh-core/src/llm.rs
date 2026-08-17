//! DSH 层 llm 缝的数据承载：模型适配（消息 → 响应）。
//!
//! 第一性原理：缝的权威契约是 WIT（`dsh-loop.wit` 的 `llm` 接口）；本模块是
//! **宿主的承载**——回答「模型适配器长什么样、宿主如何生成响应」，与 WASM loop
//! 正交。WASM loop 经缝调用（`LoopHost` 桥接本类型），宿主在此注册模型适配器。
//!
//! 共享句柄用 `Arc<Mutex<>>`（同 session/tools）：满足服务仓库 Send+Sync 约束；
//! 运行时单线程，Mutex 仅用于类型约束。

use std::sync::{Arc, Mutex};

use crate::types::Value;

/// 模型生成函数：`(messages: Vec<Value>, tools: Vec<Value>) -> Value`（助手响应）。
///
/// 输入 messages 为模型历史（role/content/tool_calls 序列）；tools 为工具 schema。
/// 返回助手响应 JSON：`{ content: string, tool_calls?: [...] }`。
pub type LlmFn = dyn Fn(Vec<Value>, Vec<Value>) -> Value + Send + Sync;

/// 模型适配器注册表（内部 `Mutex` 满足服务仓库 Send+Sync 约束）。
pub struct LlmService {
    /// 默认适配器（未指定 provider 时使用）。
    default: Option<Arc<LlmFn>>,
    /// 按 provider 的适配器表。
    providers: std::collections::HashMap<String, Arc<LlmFn>>,
}

impl Default for LlmService {
    fn default() -> Self {
        Self::new()
    }
}

impl LlmService {
    pub fn new() -> Self {
        LlmService {
            default: None,
            providers: std::collections::HashMap::new(),
        }
    }

    /// 注册默认适配器（等价 DSH 的「默认模型提供者」）。
    pub fn set_default(&mut self, f: impl Fn(Vec<Value>, Vec<Value>) -> Value + Send + Sync + 'static) {
        self.default = Some(Arc::new(f));
    }

    /// 注册 provider 适配器。
    pub fn register_provider(
        &mut self,
        provider: &str,
        f: impl Fn(Vec<Value>, Vec<Value>) -> Value + Send + Sync + 'static,
    ) {
        self.providers.insert(provider.to_string(), Arc::new(f));
    }

    /// 生成响应：优先按 provider 查表，否则默认；都无 → 错误 JSON。
    pub fn generate(&self, provider: Option<&str>, messages: Vec<Value>, tools: Vec<Value>) -> Value {
        let adapter = provider
            .and_then(|p| self.providers.get(p))
            .or(self.default.as_ref());
        match adapter {
            Some(f) => f(messages, tools),
            None => serde_json::json!({
                "error": "no LLM adapter registered",
                "content": "",
            }),
        }
    }

    /// 注册 HTTP provider 适配器（M17：OpenAI 兼容 `/chat/completions`）。
    /// `base` 形如 `http://host:port[/prefix]`；`api_key` 可选（Bearer 认证）。
    pub fn register_http(&mut self, provider: &str, base: &str, api_key: Option<&str>, model: &str) {
        let base = base.to_string();
        let api_key = api_key.map(|s| s.to_string());
        let model = model.to_string();
        self.register_provider(provider, move |messages, tools| {
            crate::llm_http::chat_completions(&base, api_key.as_deref(), &model, &messages, &tools)
        });
    }

    /// 注册 HTTP 默认适配器（未指定 provider 时使用；同时作为 default）。
    pub fn register_http_default(&mut self, base: &str, api_key: Option<&str>, model: &str) {
        let base = base.to_string();
        let api_key = api_key.map(|s| s.to_string());
        let model = model.to_string();
        self.set_default(move |messages, tools| {
            crate::llm_http::chat_completions(&base, api_key.as_deref(), &model, &messages, &tools)
        });
    }
}

/// 共享 LLM 服务句柄（作为 `ctx.llm` 服务值；Send+Sync）。
pub type LlmHandle = Arc<Mutex<LlmService>>;

/// 构造共享 LLM 服务。
pub fn new_llm() -> LlmHandle {
    Arc::new(Mutex::new(LlmService::new()))
}

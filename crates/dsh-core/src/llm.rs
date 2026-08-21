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

/// 一个已注册的模型提供者条目（web `llm.providers`/`llm.models` 的目录来源）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmProviderInfo {
    /// 提供者 id（注册键）。
    pub id: String,
    /// 是否为默认适配器。
    pub is_default: bool,
    /// 已注册的模型 id（本 repo 适配器只用一个模型，模型 id = provider id）。
    pub models: Vec<String>,
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

    /// 枚举已注册提供者（含 default 标记；M1e web `llm.providers` 驱动源）。
    /// 提供者 id → 模型 id（本 repo 每 provider 注册一个模型，id 同 provider）。
    pub fn provider_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.providers.keys().cloned().collect();
        ids.sort();
        if ids.is_empty() && self.default.is_some() {
            ids.push("default".to_string());
        }
        ids
    }

    /// 提供者目录（`llm.providers`/`llm.models` 的权威来源）。
    pub fn providers(&self) -> Vec<LlmProviderInfo> {
        let mut out: Vec<LlmProviderInfo> = self
            .providers
            .keys()
            .map(|id| LlmProviderInfo {
                id: id.clone(),
                is_default: false,
                models: vec![id.clone()],
            })
            .collect();
        if self.default.is_some() {
            out.push(LlmProviderInfo {
                id: "default".to_string(),
                is_default: true,
                models: vec!["default".to_string()],
            });
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }
}

/// 共享 LLM 服务句柄（作为 `ctx.llm` 服务值；Send+Sync）。
pub type LlmHandle = Arc<Mutex<LlmService>>;

/// 构造共享 LLM 服务。
pub fn new_llm() -> LlmHandle {
    Arc::new(Mutex::new(LlmService::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_ids_empty_default_present() {
        let svc = LlmService::new();
        assert_eq!(svc.provider_ids(), Vec::<String>::new());
        assert!(svc.providers().is_empty());
    }

    #[test]
    fn providers_reflect_registered() {
        let mut svc = LlmService::new();
        svc.register_provider("deepseek", |_, _| serde_json::json!({"content": ""}));
        svc.register_provider("ollama", |_, _| serde_json::json!({"content": ""}));
        assert_eq!(svc.provider_ids(), vec!["deepseek", "ollama"]);
        let dir = svc.providers();
        assert_eq!(dir.len(), 2);
        assert_eq!(dir[0].id, "deepseek");
        assert_eq!(dir[0].models, vec!["deepseek"]);
        assert!(!dir[0].is_default);
        assert_eq!(dir[1].id, "ollama");
    }

    #[test]
    fn providers_default_marked() {
        let mut svc = LlmService::new();
        svc.set_default(|_, _| serde_json::json!({"content": ""}));
        svc.register_provider("deepseek", |_, _| serde_json::json!({"content": ""}));
        let dir = svc.providers();
        assert_eq!(dir.len(), 2);
        let default = dir.iter().find(|p| p.id == "default").expect("default in dir");
        assert!(default.is_default);
        let ds = dir.iter().find(|p| p.id == "deepseek").unwrap();
        assert!(!ds.is_default);
    }

    #[test]
    fn provider_ids_sorted() {
        let mut svc = LlmService::new();
        svc.register_provider("z", |_, _| serde_json::json!({}));
        svc.register_provider("a", |_, _| serde_json::json!({}));
        let ids = svc.provider_ids();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted);
    }
}

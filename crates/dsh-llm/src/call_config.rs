//! 会话调用配置（`LlmCallConfig`）与字段级相等。
//!
//! 权威参考：`deepseek-harness/packages/llm/llm/src/call-config.ts`。

use serde::{Deserialize, Serialize};

use crate::types::ReasoningEffortId;

/// 一次会话所有请求的 provider/model/reasoning effort/采样标量。
/// 每个字段 1:1 映射到同名 `GenerateOptions` 字段；loop 从记录的 header 构建请求，
/// 不接受逐次呼叫的这些值（影响缓存复用的 epoch-level 状态）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallConfig {
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffortId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
}

/// 精确模型适配器解析（而非调用方请求提议）提供的有效配置字段。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallConfigAdapterDefaults {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<bool>,
}

/// 字段级相等：调用方用它判断「提议的配置是否真实变化」（值得记一版 request/header）。
/// `stop` 列表逐元素比较；含未定义字段时以「双方同为未定义」相等。
pub fn call_config_equals(a: &CallConfig, b: &CallConfig) -> bool {
    if a.provider != b.provider
        || a.model != b.model
        || a.reasoning_effort != b.reasoning_effort
        || a.temperature != b.temperature
        || a.max_tokens != b.max_tokens
    {
        return false;
    }
    match (&a.stop, &b.stop) {
        (None, None) => true,
        (Some(x), Some(y)) => x.len() == y.len() && x.iter().zip(y.iter()).all(|(u, v)| u == v),
        _ => false,
    }
}

//! agent-loop 用户设置与严格验证（对齐 `AgentLoopSettings` schema + 直构校验）。

use crate::constants::DEFAULT_MAX_PARALLEL_TOOL_CALLS;

/// `AgentLoopSettings`：用户可设置的字段（仅 `maxParallelToolCalls` 一个；不含
/// `agents`——组合配置的 agents 继续服务自身消费者）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentLoopSettings {
    pub max_parallel_tool_calls: u64,
}

impl Default for AgentLoopSettings {
    fn default() -> Self {
        AgentLoopSettings {
            max_parallel_tool_calls: DEFAULT_MAX_PARALLEL_TOOL_CALLS,
        }
    }
}

/// `settings` 存储 cap 覆盖组合值时校验：必须为正 safe integer。
/// 消息逐字：`maxParallelToolCalls must be a positive integer`。
pub fn validate_max_parallel_tool_calls(value: u64) -> Result<(), String> {
    if value < 1 {
        return Err("maxParallelToolCalls must be a positive integer".to_string());
    }
    Ok(())
}

/// `AgentOptions.maxTokens` 必须为正 safe integer（`Some` 时）。
/// 消息逐字：`agent maxTokens must be a positive safe integer`。
pub fn validate_max_tokens(max_tokens: Option<u64>) -> Result<(), String> {
    if let Some(m) = max_tokens {
        if m < 1 {
            return Err("agent maxTokens must be a positive safe integer".to_string());
        }
    }
    Ok(())
}

/// 组合配置校验：`maxParallelToolCalls` 缺省取常量默认；给定值须为正。
pub fn resolve_max_parallel_tool_calls(configured: Option<u64>) -> Result<u64, String> {
    match configured {
        None => Ok(DEFAULT_MAX_PARALLEL_TOOL_CALLS),
        Some(v) => {
            validate_max_parallel_tool_calls(v)?;
            Ok(v)
        }
    }
}

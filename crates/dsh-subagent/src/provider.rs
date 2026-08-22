//! `dsh-subagent` provider 能力边界 —— 对齐 `subagent-*` 各 provider 的 capability 表。
//!
//! in-process（spawn/fork）全能力；out-of-process（acp/claude-code/codex/dsh-sdk）
//! `NO_START_CAPABILITIES`（全 false）且 `inheritsParentContext=false`。

/// 子代理提供者能力。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderCapabilities {
    pub output_schema: bool,
    pub depth_limit: bool,
    pub tool_filter: bool,
    pub persona: bool,
    /// 是否继承父上下文（fork=true、spawn=false）。
    pub inherits_parent_context: bool,
}

const NO_START: ProviderCapabilities = ProviderCapabilities {
    output_schema: false,
    depth_limit: false,
    tool_filter: false,
    persona: false,
    inherits_parent_context: false,
};

/// 一句话：返回指定 provider 名的能力表（未知 → NO_START，fail loud 由调用方校验
/// provider 是否登记）。
pub fn for_provider_name(name: &str) -> ProviderCapabilities {
    match name {
        "spawn" => ProviderCapabilities {
            output_schema: true,
            depth_limit: true,
            tool_filter: true,
            persona: true,
            inherits_parent_context: false,
        },
        "fork" => ProviderCapabilities {
            output_schema: true,
            depth_limit: true,
            tool_filter: true,
            persona: true,
            inherits_parent_context: true,
        },
        _ => NO_START,
    }
}

impl ProviderCapabilities {
    /// 登记 provider 时的能力表（不认识的 provider → NO_START 纪律）。
    pub fn for_provider(name: &str) -> ProviderCapabilities {
        for_provider_name(name)
    }
}

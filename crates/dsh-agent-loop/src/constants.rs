//! agent-loop 常量与固化默认（对齐 `agent-loop/src/constants.ts`）。

/// 并行 tool call 池的默认上限。
pub const DEFAULT_MAX_PARALLEL_TOOL_CALLS: u64 = 10;

/// settings namespace（settings section 名）。
pub const AGENT_LOOP_SETTINGS_NAMESPACE: &str = "agent-loop";

/// `ctx.provide(...)` 的 launcher 身份 context key。
pub const CONFIGURED_AGENT_IDENTITIES_KEY: &str = "configuredAgentIdentities";

/// invariant 注册用的包名。
pub const PACKAGE_NAME: &str = "@deepseek-ai/dsh-agent-loop";

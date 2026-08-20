//! dsh-llm：LLM 能力缝的消息与流式类型面（M0 契约基建，见 M0-CONTRACT-INFRA.md）。
//!
//! 权威参考：`deepseek-harness/packages/llm/llm/src/{types,message,brand,call-config}.ts`。
//! 本 crate 只承载类型/纯函数；适配器、运行时、组装器（`LlmAdapter`/`LlmRuntime`/
//! `BlockAssembler`）为 M1b 交付。

pub mod call_config;
pub mod types;

pub use call_config::{call_config_equals, CallConfig, CallConfigAdapterDefaults};
pub use types::*;

// 品牌 id 归属：dsh-llm 拥有 MessageId/CallId/ProviderRequestId/ReasoningEffortId
// （镜像 TS `llm/llm/src/brand.ts`），从 dsh-brand 重导出以保持「拥有者暴露自己的 id」语义。
pub use dsh_brand::{CallId, MessageId, ProviderRequestId, ReasoningEffortId};

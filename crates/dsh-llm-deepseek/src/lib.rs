//! dsh-llm-deepseek：DeepSeek chat-completions 适配器（M1b）。
//!
//! SSE 行流解析（`sse`）、wire 序列化（`serialize`）、wire→StreamChunk 翻译（`translate`）、
//! `DeepSeekAdapter` 实现 `LlmAdapter`（`adapter`）。
//!
//! 权威参考：`deepseek-harness/packages/llm/llm-deepseek/src/{sse,serialize,translate,adapter}.ts`。

// LlmError 宽度（约 144B > 128B）是整个 dsh-llm seam 的刻意设计：它携带完整
// failure 事实供 retry policy 透传，M2 前的伴随代价是宽 Err。
#![allow(clippy::result_large_err)]

pub mod adapter;
pub mod serialize;
pub mod sse;
pub mod translate;
pub mod types;

pub use adapter::{
    http_error_code, is_context_window_exceeded, is_quota_exceeded, DeepSeekAdapter,
    DeepSeekAdapterOptions, DeepSeekCatalogModel, DeepSeekConnection, PayloadsResolver,
    DEFAULT_CONTEXT_WINDOW, DEFAULT_MAX_TOKENS, DEFAULT_STREAM_IDLE_TIMEOUT_MS,
};
pub use sse::{parse_sse, SseError, SseParser, DONE};
pub use serialize::{serialize_messages, serialize_request, Effort, RequestDefaults, Thinking};
pub use translate::{map_finish_reason, map_usage, translate, TranslateError};

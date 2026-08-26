//! `dsh-core` — 等效迁移 Cordis 核心原语到 Rust。
//!
//! 行为规范见 `PLAN-rust-cordis-equivalent-migration.md` §1，Rust 设计见 §2。
//! M0 范围：fiber 状态机 + effect（逆序/幂等 disposer）、四模式事件
//! （emit / parallel / serial / bail / waterfall）、ServiceStore + notify
//! 依赖驱动重载、Context 门面与错误模型。
//!
//! 已知 M0 限制（见 PLAN §2.3/§3，M1 补齐）：
//! - 监听器与 effect 为同步实现；async effect / 跨 await 语义在 M1 引入。
//! - isolate / intercept 作用域在 M1 引入；当前所有服务共用根作用域。
//! - `internal/status`、`internal/plugin` 等内部事件只记录进 trace，不派发到钩子。

// 运行时是刻意单线程的（`Rc<RefCell>` 贯穿）；`Arc<dyn Plugin>`/`Arc<dyn Fn>`
// 仅用于共享所有权而非跨线程（lint 误报于非 Send/Sync 内部类型）。
#![allow(clippy::arc_with_non_send_sync)]

pub mod context;
pub mod error;
pub mod events;
pub mod fiber;
pub mod llm;
pub mod llm_http;
pub mod logger;
pub mod reflect;
pub mod registry;
pub mod runtime;
pub mod service;
pub mod session;
pub mod tools;
pub mod types;
pub mod value;

pub use context::{Cordis, IntervalTicks, TimerFn};
pub use error::{AggregateError, CordisError};
pub use events::{AsyncListener, DispatchMode, HookCallback, HookResult, Listener, NextRef};
pub use fiber::{Disposer, EffectBody, EffectMeta, EffectOutcome, FiberHandle, FiberState, GenItem, make_disposer};
pub use llm::{new_llm, LlmHandle, LlmService};
pub use logger::{format_message, hyphenate, Exporter, ExporterConfig, Logger, LoggerState, LoggerType, Message};
pub use reflect::{AccessorGet, AccessorSet, CheckFn, Property, PropertyKind};
pub use registry::{Plugin, PluginHandle};
pub use runtime::{DeferredWork, Transition};
pub use service::Service;
pub use session::{new_session, SessionEvent, SessionHandle, SessionLog, SurfaceOp};
pub use tools::{new_tool_registry, ToolRegistry, ToolRegistryHandle};
pub use types::{FiberId, HookId, ImplId, ScopeId, Value};
pub use value::json;

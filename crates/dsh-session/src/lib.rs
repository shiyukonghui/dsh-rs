//! dsh-session：会话能力缝的语义类型面（M0 契约基建，见 M0-CONTRACT-INFRA.md）。
//!
//! 权威参考：`deepseek-harness/packages/core/session/src/types.ts`。
//! M0 仅承载类型/纯函数（SessionEventMap/信封/surface/TurnEndReason/header 折叠/词表/
//! 读取闸）；`Session`/`SessionStore`/`deriveMessages` 等运行时为 M1a 交付。

pub mod invariant;
pub mod repair;
pub mod request_header;
pub mod runtime;
pub mod store;
pub mod surface;
pub mod types;

pub use repair::*;
pub use request_header::*;
pub use runtime::*;
pub use surface::*;
pub use types::*;
pub use dsh_brand::SessionId;

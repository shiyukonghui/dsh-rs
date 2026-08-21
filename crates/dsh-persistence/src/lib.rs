//! dsh-persistence：持久化能力缝的 Service Definition 层（M0 契约基建，见 M0-CONTRACT-INFRA.md）。
//!
//! 权威参考：`deepseek-harness/packages/session/session-persistence/`。
//! M0 固化 seam（trait + 类型 + 错误）；`format`/`zstd`/`jsonl`/`coordinator`/
//! `SessionWriteBehind` 为 M1d 交付。

pub mod coordinator;
pub mod format;
pub mod import;
pub mod jsonl;
pub mod seam;
pub mod write_behind;
pub mod zstd;

pub use coordinator::*;
pub use format::*;
pub use import::*;
pub use jsonl::*;
pub use seam::*;
pub use write_behind::*;
pub use zstd::*;

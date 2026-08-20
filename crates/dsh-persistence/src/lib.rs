//! dsh-persistence：持久化能力缝的 Service Definition 层（M0 契约基建，见 M0-CONTRACT-INFRA.md）。
//!
//! 权威参考：`deepseek-harness/packages/session/session-persistence/`。
//! M0 固化 seam（trait + 类型 + 错误）；`PersistenceCoordinator`/`SessionWriteBehind`/
//! JSONL 后端为 M1d 交付。

pub mod seam;

pub use seam::*;

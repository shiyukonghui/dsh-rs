//! dsh-api：Remote 契约仓库 + RPC 消息模型（M0 契约基建，见 M0-CONTRACT-INFRA.md）。
//!
//! 权威参考：RPC 契约的机械转译（`spec/README.md`）。M0 固化仓库与消息模型；
//! dispatch / lookup / codec 运行时为 M1/M3 交付。

pub mod spec;

pub use spec::*;

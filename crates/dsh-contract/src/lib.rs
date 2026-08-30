//! dsh-contract（D-216）：dsh-std 元协议三原语的 Rust 移植——身份文法、协议声明、
//! 目录协商。纪律：零业务概念（核心不认识 command/model/panel）；**不推断版本兼容**
//! （只认显式 accepts 集）；纯函数进、结构化报告出（可测、可缓存、可离线算）。
//! 文法与 dsh-std `packages/core` 逐字对齐：双实现互认即「采概念不采依赖」的实证。

pub mod catalog;
pub mod declaration;
pub mod version;

pub use catalog::{negotiate, Catalog, Definition, Issue, NegotiationReport, ReportProtocol, RequireEntry, Severity, SupportEntry};
pub use declaration::{validate_declaration_value, ApiReference, Declaration, Requirement, Support};
pub use version::{parse_api_version, ApiVersion, Stability};

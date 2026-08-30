//! canvas-shell 纯逻辑层（D-210 S1）：core.js 纯函数的 Rust 移植——宿主可测，
//! 零 DOM/零 fetch（与 core.js 同不变式：一切可证的都在这一层证）。

pub mod board;
pub mod chat;
pub mod i18n;
pub mod layout;
pub mod model;
pub mod schema;
pub mod values;

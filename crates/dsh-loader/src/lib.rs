//! `dsh-loader` —— Cordis loader 的 Rust 移植（对应 PLAN §1.8，M2 子集）。
//!
//! M2 范围：Entry / EntryGroup / EntryTree（内存形态）、update 四分支事务与回滚、
//! Loader 服务（internal/plugin 7-case 自处置检测、internal/update 写回）、
//! group 嵌套（loader 层展开，无独立 Group 插件 fiber）。
//!
//! 已知 M2 差异（记录于 PLAN §10）：
//! - 同步事务（Cordis 用 async + `Promise.allSettled`）；并行 create 降为顺序，
//!   最终状态与事件顺序一致。
//! - `!!js` disabled/config 表达式（M3 dsh-eval 引入）；M2 仅布尔。
//! - isolate / intercept entry 选项（M3）。
//! - 插件按名从 `Loader::register_plugin` 注册的仓库解析（Rust 无动态 import）。

// 同 dsh-core：单线程运行时，`Arc` 仅共享所有权（见 dsh-core lib.rs 说明）。
#![allow(clippy::arc_with_non_send_sync)]

pub mod entry;
pub mod group;
pub mod hmr;
pub mod identity;
pub mod include;
pub mod loader;

pub use entry::{Entry, EntryOptions};
pub use group::EntryGroup;
pub use hmr::Hmr;
pub use identity::{PluginIdentity, PluginRecord};
pub use include::{apply_entry_patches, apply_entry_patches_with_warn, Include, Patch};
pub use loader::{EntrySnapshot, Loader, LoaderPlugin, LoaderState, PersistSink};

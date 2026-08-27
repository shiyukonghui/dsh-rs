//! 插件身份键（A1：对齐 harness「回调为身份」）。
//!
//! cordis 以「解析后的回调函数指针」为插件身份（`registry.has(callback)`，re-import=新身份、
//! HMR/case-4 依赖此判定）。Rust 等价物 = 每注册铸一个 `Arc<()>` token（指针身份，同
//! dsh-scope `ScopeKey` 的 Arc 身份纪律）：`register_plugin` 以 Arc 指针相等判定
//! 「同名同实现 = 同身份（幂等）/ 同名新实现 = 新身份（换代）」。
//!
//! # A1 文档化偏差（DIV-A1-1，用户确认 2026-08-27）
//!
//! 注册表保持「名字 → 当前实现」的**平名单记录**形态（顺序换代），而非 (来源, name)+版本 多实现
//! 键——dsh-rs 是**宿主-owned 注册表**：同名多实现**同时共存**由宿主在 import/装配层消解（一名一
//! 当前实现）。case-4（模块消失 → self-dispose 合法、entry 不自禁用）经
//! [`Loader::remove_plugin`]（cordis `registry.delete` 同径：先删记录后 dispose 存活 fiber）触发
//! 并已由 m26 锁定。HMR 宿主入口 [`Loader::sync_plugin`]（D-162）复用本条语义：
//! Register/Replace 换代、Delete 后 entry 保留但 inert，再 Register = **全新记录/全新 lineage**
//! （generation 重置为 1、新身份 token；m27_hmr_host 锁定）。

use std::sync::Arc;

use dsh_core::Plugin;

/// 插件身份句柄：身份 = token 指针（`Arc::ptr_eq` 判定）。
#[derive(Debug, Clone)]
pub struct PluginIdentity(Arc<()>);

impl PluginIdentity {
    /// 铸造全新身份。
    pub fn new() -> Self {
        PluginIdentity(Arc::new(()))
    }
}

impl Default for PluginIdentity {
    fn default() -> Self {
        Self::new()
    }
}

impl PartialEq for PluginIdentity {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for PluginIdentity {}

impl std::hash::Hash for PluginIdentity {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::ptr::hash(Arc::as_ptr(&self.0), state);
    }
}

/// 插件仓库记录：实现 + 身份 + 换代代数。
#[derive(Clone)]
pub struct PluginRecord {
    /// 本实现的身份（同一实现重复注册保持同一）；同名新实现 = 新身份。
    pub identity: PluginIdentity,
    /// 插件实现。
    pub plugin: Arc<dyn Plugin>,
    /// 换代计数（同名新实现 = 原代数 + 1；同实现幂等不变）。
    pub generation: u64,
}

impl PluginRecord {
    pub fn new(plugin: Arc<dyn Plugin>) -> Self {
        PluginRecord {
            identity: PluginIdentity::new(),
            plugin,
            generation: 1,
        }
    }
}

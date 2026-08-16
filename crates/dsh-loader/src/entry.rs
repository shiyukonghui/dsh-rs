//! Loader entry 数据类型（Cordis `EntryOptions` / `Entry`）。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use dsh_core::{FiberId, Value};

fn default_config() -> Value {
    serde_json::json!({})
}

/// 序列化的插件入口选项（Cordis `EntryOptions`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EntryOptions {
    /// 组内稳定 id。
    pub id: String,
    /// 插件注册名（`Loader::register_plugin` 仓库键）。
    pub name: String,
    /// 传给插件的配置。
    #[serde(default = "default_config")]
    pub config: Value,
    /// 阻止本入口及子孙运行。
    #[serde(default)]
    pub disabled: bool,
    /// disabled 的 `!!js` 表达式（M3 dsh-eval；求值为 truthy 时禁用）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_expr: Option<String>,
    /// 标记为嵌套组（config 为子入口数组）。
    #[serde(default)]
    pub group: bool,
    /// 额外依赖服务（合并进插件 inject）。
    #[serde(default)]
    pub inject: Vec<String>,
    /// 服务隔离（服务名 → `true` = 入口本地 realm，或 label 字符串 = 全局 realm）。
    /// 对应 Cordis entry 选项 `isolate`。
    #[serde(default)]
    pub isolate: HashMap<String, Value>,
    /// 服务 intercept 配置（服务名 → 配置）。
    #[serde(default)]
    pub intercept: HashMap<String, Value>,
}

impl EntryOptions {
    pub fn new(id: &str, name: &str) -> Self {
        EntryOptions {
            id: id.to_string(),
            name: name.to_string(),
            config: default_config(),
            disabled: false,
            disabled_expr: None,
            group: false,
            inject: Vec::new(),
            isolate: HashMap::new(),
            intercept: HashMap::new(),
        }
    }
}

impl Default for EntryOptions {
    fn default() -> Self {
        EntryOptions::new("", "")
    }
}

/// 一个已配置的插件节点（Cordis `Entry`）。
#[derive(Debug, Clone)]
pub struct Entry {
    pub id: String,
    pub options: EntryOptions,
    /// 挂载后的 fiber（group 入口为 `None`，其「fiber」是子入口集合）。
    pub fiber: Option<FiberId>,
    /// 所属组 id。
    pub parent_group: String,
    /// group 入口的子组 id。
    pub subgroup: Option<String>,
    /// 自处置保护计数（7-case case 6）。
    pub disposing: u32,
}

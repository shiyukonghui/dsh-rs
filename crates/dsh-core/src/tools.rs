//! DSH 层 tools 缝的数据承载：工具注册表 + 执行。
//!
//! 第一性原理：缝的权威契约是 WIT（`dsh-loop.wit` 的 `tools` 接口）；本模块是
//! **宿主的承载**——回答「工具注册表长什么样、宿主如何执行工具」，与 WASM loop
//! 正交。WASM loop 经缝调用（`LoopHost` 桥接本类型），宿主在此注册/执行工具。
//!
//! 共享句柄用 `Arc<Mutex<>>`（同 session）：满足服务仓库 Send+Sync 约束；
//! 运行时单线程，Mutex 仅用于类型约束。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::types::Value;

/// 工具处理函数：`(arguments JSON) -> result JSON`。
pub type ToolFn = dyn Fn(Value) -> Value + Send + Sync;

/// 工具注册表（内部 `Mutex` 满足服务仓库 Send+Sync 约束）。
#[derive(Default)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<ToolFn>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        ToolRegistry {
            tools: HashMap::new(),
        }
    }

    /// 注册一个工具（返回旧实现 if any；等价 WIT 缝 `tools::register`）。
    pub fn register(&mut self, name: &str, f: impl Fn(Value) -> Value + Send + Sync + 'static) {
        self.tools.insert(name.to_string(), Arc::new(f));
    }

    /// 执行工具（等价 WIT 缝 `tools::execute`）；未注册 → 返回错误 JSON。
    pub fn execute(&self, name: &str, arguments: Value) -> Value {
        match self.tools.get(name) {
            Some(f) => f(arguments),
            None => serde_json::json!({"error": format!("tool \"{name}\" not registered")}),
        }
    }

    /// 已注册工具名（诊断）。
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.tools.keys().cloned().collect();
        names.sort();
        names
    }
}

/// 共享工具注册表句柄（作为 `ctx.tools` 服务值；Send+Sync）。
pub type ToolRegistryHandle = Arc<Mutex<ToolRegistry>>;

/// 构造共享工具注册表。
pub fn new_tool_registry() -> ToolRegistryHandle {
    Arc::new(Mutex::new(ToolRegistry::new()))
}

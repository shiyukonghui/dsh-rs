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
    /// 工具名 → (处理函数, schema)。schema 记录工具参数契约（供 `llm.generate`
    /// 构造工具列表 / WASM `tools::list` 枚举；无 schema 时归一为空对象 `{}`）。
    tools: HashMap<String, (Arc<ToolFn>, Value)>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        ToolRegistry {
            tools: HashMap::new(),
        }
    }

    /// 注册工具，带参数 schema（等价 WIT 缝 `tools::register` 的 schema 参数）。
    /// 返回旧实现 if any。
    pub fn register_with_schema(
        &mut self,
        name: &str,
        schema: Value,
        f: impl Fn(Value) -> Value + Send + Sync + 'static,
    ) {
        self.tools.insert(name.to_string(), (Arc::new(f), schema));
    }

    /// 注册工具（无 schema → 记为空对象 `{}`；等价 WIT 缝 `tools::register`）。
    pub fn register(&mut self, name: &str, f: impl Fn(Value) -> Value + Send + Sync + 'static) {
        self.register_with_schema(name, Value::Object(Default::default()), f);
    }

    /// 工具 schema 单查（未注册 → None）。
    pub fn schema(&self, name: &str) -> Option<Value> {
        self.tools.get(name).map(|(_, s)| s.clone())
    }

    /// 执行工具（等价 WIT 缝 `tools::execute`）；未注册 → 返回错误 JSON。
    pub fn execute(&self, name: &str, arguments: Value) -> Value {
        match self.tools.get(name) {
            Some((f, _)) => f(arguments),
            None => serde_json::json!({"error": format!("tool \"{name}\" not registered")}),
        }
    }

    /// 枚举全部工具：`(name, schema)` 对，按名排序（等价 WIT 缝 `tools::list`）。
    pub fn list(&self) -> Vec<(String, Value)> {
        let mut items: Vec<(String, Value)> = self
            .tools
            .iter()
            .map(|(n, (_, s))| (n.clone(), s.clone()))
            .collect();
        items.sort_by(|a, b| a.0.cmp(&b.0));
        items
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

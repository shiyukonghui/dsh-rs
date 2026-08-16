//! 核心标识与值类型。

/// 单一插件运行实例（fiber）的句柄，等价 Cordis `Fiber.uid` 之外的不透明 id。
pub type FiberId = u64;
/// 上下文节点句柄（M1 引入 isolate 时使用）。
pub type ContextId = u64;
/// 服务实现记录句柄。
pub type ImplId = u64;
/// 事件监听器句柄。
pub type HookId = u64;
/// 服务隔离作用域标签（等价 Cordis 的 isolate symbol）。
pub type ScopeId = u64;
/// 插件注册键（M0 用插件名；M2 起扩展为 manifest hash）。
pub type PluginKey = String;

/// 通用载荷类型：配置、事件参数、服务值统一用 lossless JSON。
pub type Value = serde_json::Value;

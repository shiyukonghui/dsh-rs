//! 服务仓库与实现记录（对应 PLAN §1.3）。

use std::any::Any;
use std::sync::Arc;

use crate::context::Cordis;
use crate::types::{ImplId, ScopeId, Value};

/// 服务可用性谓词（Cordis `check`；无参，捕获所需状态）。
pub type CheckFn = Box<dyn Fn() -> bool>;

/// 服务实现记录（Cordis `Impl`）。
pub struct Impl {
    pub id: ImplId,
    pub name: String,
    /// 服务值（`Arc<dyn Any>` 等价 `ctx.<name>` 的动态值，downcast 取回）。
    pub value: Arc<dyn Any + Send + Sync>,
    /// 提供该实现的 fiber（生命周期 owner）。
    pub owner: crate::types::FiberId,
    /// 所属隔离作用域。
    pub scope: ScopeId,
    /// 可选可用性谓词。
    pub check: Option<CheckFn>,
}

impl Impl {
    /// 求值可用性谓词（无谓词视为可用）。
    pub fn check_ok(&self) -> bool {
        match &self.check {
            Some(check) => check(),
            None => true,
        }
    }
}

/// 上下文属性定义（Cordis `Property`：service | accessor）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertyKind {
    Service,
    Accessor,
}

/// accessor 的 get 钩子：`(ctx) -> Option<Value>`。
pub type AccessorGet = Box<dyn Fn(&Cordis) -> Option<Value>>;
/// accessor 的 set 钩子：`(ctx, value) -> bool`（返回 false 拒绝写入）。
pub type AccessorSet = Box<dyn Fn(&Cordis, Value) -> bool>;

/// 上下文属性（Cordis `Property.Service | Property.Accessor`）。
pub enum Property {
    /// 服务属性（由 provide 声明）。
    Service,
    /// 计算属性（由 accessor 声明）。
    Accessor {
        get: AccessorGet,
        set: Option<AccessorSet>,
    },
}

/// 通用服务值便于测试/宿主传值。
pub type ServiceValue = Arc<dyn Any + Send + Sync>;

/// 从服务值构造（测试辅助）。
pub fn any_value<T: Any + Send + Sync>(value: T) -> ServiceValue {
    Arc::new(value)
}

/// 占位：确保 Value 类型被引用（M0 服务值不强制 JSON）。
#[allow(dead_code)]
fn _value_is_used(v: Value) -> Value {
    v
}

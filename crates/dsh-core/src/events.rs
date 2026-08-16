//! 事件类型：分派模式、监听器签名、钩子记录（对应 PLAN §1.5）。

use std::sync::Arc;

use crate::context::Cordis;
use crate::types::{FiberId, HookId, ScopeId, Value};

/// 事件分派策略（Cordis `DispatchMode`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchMode {
    /// 同步顺序观察，忽略返回值。
    Emit,
    /// 全部监听器运行（M0 同步实现；await 语义 M1 引入）。
    Parallel,
    /// 顺序运行，首个 bail 值即停。
    Serial,
    /// 同步顺序运行，首个 bail 值即停。
    Bail,
    /// 洋葱中间件：`next()` 委托，不调用则短路。
    Waterfall,
}

/// 监听器返回值。
#[derive(Debug, Clone, PartialEq)]
pub enum HookResult {
    /// 未产生 bail 值 / 未调用 next。
    Continue,
    /// 产生一个值（serial/bail 的 bail 值；waterfall 的链结果）。
    /// `None` 表示 undefined；`Some(null)` / `Some(false)` 不算 bail。
    Returned(Option<Value>),
}

impl HookResult {
    /// Cordis `isBailed`：`value !== null && value !== false && value !== undefined`。
    pub fn is_bailed(&self) -> bool {
        match self {
            HookResult::Continue => false,
            HookResult::Returned(None) => false,
            HookResult::Returned(Some(v)) => !(v.is_null() || v.as_bool() == Some(false)),
        }
    }

    /// 提取值（Continue → None）。
    pub fn value(self) -> Option<Value> {
        match self {
            HookResult::Continue => None,
            HookResult::Returned(v) => v,
        }
    }
}

/// waterfall 中传给监听器的 `next` 引用。
pub type NextRef<'a> = Option<&'a dyn Fn(&Cordis, &mut Vec<Value>) -> Option<Value>>;

/// 监听器：`(ctx, args, next)`。非 waterfall 分派时 `next` 为 `None`。
/// 监听器可以修改 `args`（waterfall 中后续 next() 可见，等价 JS 共享数组）。
pub type Listener = Arc<dyn for<'a> Fn(&Cordis, &mut Vec<Value>, NextRef<'a>) -> HookResult + 'static>;

/// 已注册监听器记录（等价 Cordis `Hook`）。
pub struct Hook {
    pub id: HookId,
    /// 拥有该监听的 fiber。
    pub owner: FiberId,
    /// 忽略作用域过滤。
    pub global: bool,
    /// 插入到已有监听器之前。
    pub prepend: bool,
    /// 监听器所在作用域（M0 恒为根作用域；isolate M1 引入）。
    pub scope: ScopeId,
    pub cb: Listener,
}

//! 共享测试辅助：FnPlugin（闭包插件）+ 监听器构造。
#![allow(dead_code)]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use dsh_core::*;

/// 用闭包构造的插件（name/inject 为实例值）。
pub type PluginBody = Rc<dyn Fn(&Cordis, Value) -> Result<EffectOutcome, CordisError>>;

pub struct FnPlugin {
    pub name: &'static str,
    pub inject: &'static [&'static str],
    pub body: PluginBody,
}

impl FnPlugin {
    pub fn new(
        name: &'static str,
        inject: &'static [&'static str],
        body: impl Fn(&Cordis, Value) -> Result<EffectOutcome, CordisError> + 'static,
    ) -> FnPlugin {
        FnPlugin {
            name,
            inject,
            body: Rc::new(body),
        }
    }

    pub fn noop(name: &'static str) -> FnPlugin {
        FnPlugin::new(name, &[], |_ctx, _cfg| Ok(EffectOutcome::None))
    }
}

impl Plugin for FnPlugin {
    fn name(&self) -> &'static str {
        self.name
    }

    fn inject(&self) -> &'static [&'static str] {
        self.inject
    }

    fn apply(&self, ctx: &Cordis, config: Value) -> Result<EffectOutcome, CordisError> {
        (self.body)(ctx, config)
    }
}

/// 由闭包构造监听器。
pub fn make_listener<F>(f: F) -> Listener
where
    F: for<'a> Fn(&Cordis, &mut Vec<Value>, NextRef<'a>) -> HookResult + 'static,
{
    Arc::new(f)
}

/// 简单的字符串日志（Rc<RefCell>，测试断言用）。
pub fn log() -> Rc<RefCell<Vec<String>>> {
    Rc::new(RefCell::new(Vec::new()))
}

pub fn push(log: &Rc<RefCell<Vec<String>>>, s: impl Into<String>) {
    log.borrow_mut().push(s.into());
}

pub fn snapshot(log: &Rc<RefCell<Vec<String>>>) -> Vec<String> {
    log.borrow().clone()
}

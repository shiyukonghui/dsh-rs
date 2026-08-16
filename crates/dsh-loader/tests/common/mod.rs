//! 共享测试辅助（dsh-loader 测试用）。
#![allow(dead_code)]
#![allow(clippy::arc_with_non_send_sync)]

use std::cell::RefCell;
use std::rc::Rc;

use dsh_core::*;

/// 插件主体闭包类型。
pub type PluginBody = Rc<dyn Fn(&Cordis, Value) -> Result<EffectOutcome, CordisError>>;

/// 用闭包构造的插件。
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

pub fn log() -> Rc<RefCell<Vec<String>>> {
    Rc::new(RefCell::new(Vec::new()))
}

pub fn push(log: &Rc<RefCell<Vec<String>>>, s: impl Into<String>) {
    log.borrow_mut().push(s.into());
}

pub fn snapshot(log: &Rc<RefCell<Vec<String>>>) -> Vec<String> {
    log.borrow().clone()
}

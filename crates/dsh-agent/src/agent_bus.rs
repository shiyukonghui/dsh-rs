//! `AgentBus`：agent-subject 的 scoped 事件派发（对齐 Cordis `ctx.events` 的最小
//! Rust 语义 + `@deepseek-ai/dsh-scope` 的 `ScopeCarrier::adopts` 路由）。
//!
//! 模式支持：emit（通知，逐 listener 包含、不可 veto）、emit_veto（首抛传播——
//! 用于 `agent/created` 的发布否决）、serial（有序链）、waterfall（next 短路链）。
//! 派发**收集-再执行**：先把命中监听器克隆出借用再逐个运行，监听器内可重入注册。

use std::cell::RefCell;
use std::rc::Rc;

use dsh_scope::{ScopeCarrier, ScopeKey};
use serde_json::Value;

/// 监听器签名：`(name, payload)`。
pub type AgentListener = Rc<dyn Fn(&str, &Value)>;

/// waterfall/serial 的 next 回调：接收（可能被替换的）载荷，返回链的最终值。
pub type NextFn = Rc<dyn Fn(Value) -> Value>;

/// waterfall/serial 监听器签名：接收 `(payload, next)`，返回链的结果。
pub type ChainListener = Rc<dyn Fn(Value, NextFn) -> Value>;

struct BusItem {
    name: String,
    global: bool,
    tag: Option<ScopeKey>,
    cb: AgentListener,
    chain: Option<ChainListener>,
}

/// 作用域事件总线（与 dsh-scope `ScopedContext` 同构但带逐 listener 包含 + 链模式）。
#[derive(Default, Clone)]
pub struct AgentBus {
    items: Rc<RefCell<Vec<BusItem>>>,
}

impl AgentBus {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册监听器。`global=true` → 恒可见；`global=false` 需 `tag`（或 None → 无标签）。
    pub fn on(&self, name: &str, global: bool, tag: Option<ScopeKey>, cb: AgentListener) {
        self.items.borrow_mut().push(BusItem {
            name: name.to_string(),
            global: global || tag.is_none(),
            tag,
            cb,
            chain: None,
        });
    }

    /// 注册链路监听器（serial/waterfall 模式）。
    pub fn on_chain(&self, name: &str, global: bool, tag: Option<ScopeKey>, cb: ChainListener) {
        self.items.borrow_mut().push(BusItem {
            name: name.to_string(),
            global: global || tag.is_none(),
            tag,
            cb: Rc::new(|_, _| {}),
            chain: Some(cb),
        });
    }

    fn select(&self, carrier: &ScopeCarrier, name: &str) -> Vec<Rc<BusItem>> {
        let borrowed = self.items.borrow();
        let mut out: Vec<Rc<BusItem>> = Vec::new();
        for item in borrowed.iter() {
            if item.name != name {
                continue;
            }
            if item.global || carrier.adopts(item.tag.as_ref()) {
                out.push(Rc::new(BusItem {
                    name: item.name.clone(),
                    global: item.global,
                    tag: item.tag.clone(),
                    cb: item.cb.clone(),
                    chain: item.chain.clone(),
                }));
            }
        }
        out
    }

    /// **emit**：通知模式。逐监听器 catch_unwind 包含（同步抛 → 经 `warn` 回调
    /// 上报原始错误消息），不中断后续监听器（观察者不 starve、永不 veto）。
    pub fn emit(
        &self,
        carrier: &ScopeCarrier,
        name: &str,
        payload: Value,
        warn: &mut dyn FnMut(String),
    ) {
        let items = self.select(carrier, name);
        for item in items {
            let c = item.cb.clone();
            let name_owned = name.to_string();
            let payload = payload.clone();
            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                c(&name_owned, &payload)
            }));
            if let Err(e) = res {
                let msg = panic_message(&e);
                warn(msg);
            }
        }
    }

    /// **emit_veto**：首抛传播（发布否决）。用于 `agent/created`：任何监听器同步
    /// 抛 → 原 panic 消息以 Err 返回（交由 announce 回滚）。后序监听器不再运行。
    pub fn emit_veto(&self, carrier: &ScopeCarrier, name: &str, payload: Value) -> Result<(), String> {
        let items = self.select(carrier, name);
        for item in items {
            let c = item.cb.clone();
            let name_owned = name.to_string();
            let payload = payload.clone();
            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                c(&name_owned, &payload)
            }));
            if let Err(e) = res {
                return Err(panic_message(&e));
            }
        }
        Ok(())
    }

    /// **serial**：有序链，每个同步监听器 `(payload, next)`；不调 next 即短路。
    pub fn serial(&self, carrier: &ScopeCarrier, name: &str, payload: Value, innermost: NextFn) -> Value {
        let items = Rc::new(self.select(carrier, name));
        run_chain(items, 0, payload, innermost)
    }

    /// **waterfall**：与 serial 同构（Cordis 中 waterfall 允许替换参数）。
    pub fn waterfall(&self, carrier: &ScopeCarrier, name: &str, payload: Value, innermost: NextFn) -> Value {
        self.serial(carrier, name, payload, innermost)
    }

    pub fn listener_count(&self) -> usize {
        self.items.borrow().len()
    }
}

fn run_chain(items: Rc<Vec<Rc<BusItem>>>, idx: usize, payload: Value, innermost: NextFn) -> Value {
    if idx >= items.len() {
        return innermost(payload);
    }
    let item = &items[idx];
    let Some(chain) = &item.chain else {
        // 非链监听器在某时刻被收集到链事件：跳过（不短路）
        return run_chain(items, idx + 1, payload, innermost);
    };
    let items2 = items.clone();
    let next: NextFn = Rc::new(move |p: Value| run_chain(items2.clone(), idx + 1, p, innermost.clone()));
    chain(payload, next)
}

fn panic_message(e: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = e.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = e.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_string()
    }
}

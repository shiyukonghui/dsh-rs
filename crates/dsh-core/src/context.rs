//! `Cordis` 门面：插件可见 API。
//!
//! 设计（PLAN §2.1）：所有方法遵循「收集-再执行」纪律——先 `borrow_mut()`
//! 完成数据结构变更并收集需要运行的用户代码（监听器、disposer、插件 apply），
//! 释放借用后再执行用户代码。因此用户代码内可重入调用本门面的任何方法。

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use crate::error::CordisError;
use crate::events::{HookResult, Listener};
use crate::fiber::{Disposer, EffectBody, EffectOutcome, FiberHandle, FiberState, make_disposer};
use crate::logger::{Exporter, ExporterConfig, Logger};
use crate::reflect::{AccessorGet, AccessorSet, CheckFn, Property};
use crate::registry::Plugin;
use crate::runtime::{DeferredWork, Runtime, RuntimeCell, Transition};
use crate::service::Service;
use crate::types::{FiberId, Value};

/// waterfall 的最终内置行为（等价 Cordis 中 `args.pop()` 出的 inner）。
pub type InnerFn = Box<dyn Fn(&mut Vec<Value>) -> Option<Value>>;

/// waterfall 链状态：共享索引，next 可多次调用（等价 JS `cbs.shift()`）。
struct WfChain {
    cbs: Vec<Listener>,
    inner: InnerFn,
    idx: Cell<usize>,
}

fn run_chain(state: &Rc<WfChain>, ctx: &Cordis, args: &mut Vec<Value>) -> Option<Value> {
    let i = state.idx.get();
    if i < state.cbs.len() {
        state.idx.set(i + 1);
        let next: &dyn Fn(&Cordis, &mut Vec<Value>) -> Option<Value> =
            &|ctx, args| run_chain(state, ctx, args);
        match (state.cbs[i])(ctx, args, Some(next)) {
            HookResult::Continue => None,
            HookResult::Returned(v) => v,
        }
    } else {
        (state.inner)(args)
    }
}

/// 插件运行时门面（可 Clone，用户代码捕获它以便后续调用）。
#[derive(Clone)]
pub struct Cordis {
    pub(crate) rt: RuntimeCell,
}

impl Cordis {
    pub fn new() -> Self {
        Cordis {
            rt: Rc::new(RefCell::new(Runtime::new())),
        }
    }

    /// 内部访问（测试/宿主用）。
    pub fn with<T>(&self, f: impl FnOnce(&mut Runtime) -> T) -> T {
        f(&mut self.rt.borrow_mut())
    }

    /// 取规范轨迹（差分验证用）。
    pub fn take_trace(&self) -> Vec<String> {
        self.with(|rt| std::mem::take(&mut rt.trace))
    }

    // ---- 当前 fiber（动态作用域） ----

    pub fn current_fiber(&self) -> Option<FiberId> {
        self.with(|rt| rt.current_fiber())
    }

    /// 当前 fiber 是否可注册 effect（assertActive）。
    pub fn is_active(&self) -> bool {
        self.with(|rt| rt.current_active())
    }

    // ---- 插件注册 ----

    /// 注册插件并返回 fiber 句柄（等价 `ctx.plugin()`）。
    pub fn plugin<P: Plugin + 'static>(&self, plugin: P, config: Value) -> Result<FiberHandle, CordisError> {
        self.plugin_arc(Arc::new(plugin), config)
    }

    /// 以 `Arc<dyn Plugin>` 注册插件（loader 按名复用已注册实现时用）。
    pub fn plugin_arc(&self, plugin: Arc<dyn Plugin>, config: Value) -> Result<FiberHandle, CordisError> {
        let (fid, transitions) = {
            let mut rt = self.rt.borrow_mut();
            rt.register_plugin(&plugin, config)?
        };
        // internal/plugin 在加载转换前派发（与 Cordis fiber 构造顺序一致）
        self.drain_internal();
        for t in transitions {
            self.run_or_defer(t);
        }
        Ok(fid)
    }

    /// 执行转换；若正处某个插件的 apply 期间则延迟加载到 apply 收尾前后
    /// （对齐 Cordis 微任务让出：Loading 状态同步、apply 在父 Active 前、Active 在父 Active 后）。
    pub(crate) fn run_or_defer(&self, t: Transition) {
        let mid_apply = self.with(|rt| {
            rt.current
                .last()
                .map(|fid| {
                    rt.fiber(*fid)
                        .map(|f| f.state == FiberState::Loading)
                        .unwrap_or(false)
                })
                .unwrap_or(false)
        });
        if !mid_apply {
            self.run_transition(t);
            return;
        }
        match t {
            Transition::Load(fid) => {
                // 状态转换同步（与 Cordis `_setEpoch` 一致）；apply 体延迟
                let _ = {
                    let mut rt = self.rt.borrow_mut();
                    rt.begin_load(fid)
                };
                self.with(|rt| rt.deferred.push(DeferredWork::Apply(fid)));
            }
            Transition::Unload(fid) => {
                self.run_transition(t);
                let _ = fid;
            }
        }
    }

    /// 运行插件 apply 体（begin_load 之后）：internal/config 插值 + schema 校验 + apply。
    fn apply_body(
        &self,
        fid: FiberId,
        plugin: &Arc<dyn Plugin>,
        config0: Value,
    ) -> Result<EffectOutcome, CordisError> {
        let config = self
            .waterfall(
                "internal/config",
                vec![Value::from(fid), config0.clone()],
                Box::new(|args| args.get(1).cloned()),
            )
            .unwrap_or(config0);
        let config = self.validate_config(fid, &config)?;
        {
            let mut rt = self.rt.borrow_mut();
            rt.current.push(fid);
        }
        let result = plugin.apply(self, config);
        {
            let mut rt = self.rt.borrow_mut();
            rt.current.pop();
        }
        result
    }

    /// phase 1：运行延迟的 apply（父 fiber Active 之前），收集待 Finish 的 fiber。
    fn drain_phase1(&self) -> Vec<FiberId> {
        let mut finishes = Vec::new();
        loop {
            let batch = {
                let mut rt = self.rt.borrow_mut();
                std::mem::take(&mut rt.deferred)
            };
            if batch.is_empty() {
                return finishes;
            }
            for item in batch {
                match item {
                    DeferredWork::Apply(fid) => {
                        let plan = {
                            let mut rt = self.rt.borrow_mut();
                            rt.begin_load(fid)
                        };
                        let Some((plugin, config0)) = plan else { continue };
                        match self.apply_body(fid, &plugin, config0) {
                            Ok(outcome) => {
                                let _disposer = {
                                    let mut rt = self.rt.borrow_mut();
                                    rt.fiber_mut(fid)
                                        .map(|f| f.collect_effect("plugin-apply", outcome))
                                };
                                finishes.push(fid);
                            }
                            Err(e) => {
                                let mut rt = self.rt.borrow_mut();
                                rt.fail_fiber(fid, e);
                            }
                        }
                    }
                    DeferredWork::Finish(fid) => finishes.push(fid),
                }
            }
        }
    }

    /// phase 2：先跑父 finish_load 触发的依赖转换，再 Finish 延迟的 child（父 Active 之后）。
    fn drain_phase2(&self, finishes: Vec<FiberId>, initial: Vec<Transition>) {
        for t in initial {
            self.run_transition(t);
        }
        for fid in finishes {
            let transitions = {
                let mut rt = self.rt.borrow_mut();
                rt.finish_load(fid)
            };
            for t in transitions {
                self.run_transition(t);
            }
        }
        // 深层嵌套遗漏的 Apply/Finish（异常路径兜底）
        loop {
            let batch = {
                let mut rt = self.rt.borrow_mut();
                std::mem::take(&mut rt.deferred)
            };
            if batch.is_empty() {
                return;
            }
            for item in batch {
                match item {
                    DeferredWork::Finish(fid) => {
                        let transitions = {
                            let mut rt = self.rt.borrow_mut();
                            rt.finish_load(fid)
                        };
                        for t in transitions {
                            self.run_transition(t);
                        }
                    }
                    DeferredWork::Apply(fid) => {
                        let plan = {
                            let mut rt = self.rt.borrow_mut();
                            rt.begin_load(fid)
                        };
                        let Some((plugin, config0)) = plan else { continue };
                        match self.apply_body(fid, &plugin, config0) {
                            Ok(outcome) => {
                                let _disposer = {
                                    let mut rt = self.rt.borrow_mut();
                                    rt.fiber_mut(fid)
                                        .map(|f| f.collect_effect("plugin-apply", outcome))
                                };
                                let transitions = {
                                    let mut rt = self.rt.borrow_mut();
                                    rt.finish_load(fid)
                                };
                                for t in transitions {
                                    self.run_transition(t);
                                }
                            }
                            Err(e) => {
                                let mut rt = self.rt.borrow_mut();
                                rt.fail_fiber(fid, e);
                            }
                        }
                    }
                }
            }
        }
    }

    // ---- effect（核心原语） ----

    /// 注册 effect：body 立即运行，产出按注册顺序保存、卸载时逆序执行。
    /// 返回可共享、幂等的 disposer（等价 `ctx.effect()`）。
    pub fn effect(&self, label: &'static str, body: EffectBody) -> Result<Disposer, CordisError> {
        let fid = {
            let rt = self.rt.borrow();
            match rt.current_fiber() {
                Some(fid) if rt.fiber(fid).map(|f| f.is_active()).unwrap_or(false) => fid,
                _ => return Err(CordisError::InactiveEffect),
            }
        };
        let outcome = body(self)?;
        let disposer = {
            let mut rt = self.rt.borrow_mut();
            match rt.fiber_mut(fid) {
                Some(f) => f.collect_effect(label, outcome),
                None => return Err(CordisError::FiberNotFound(fid)),
            }
        };
        Ok(disposer)
    }

    // ---- 事件 ----

    /// 注册监听器（等价 `ctx.on()`）。
    pub fn on(&self, name: &str, listener: Listener) -> Result<Disposer, CordisError> {
        self.on_with(name, listener, false, false)
    }

    /// 注册监听器，可指定 global / prepend（等价 `ctx.on(name, cb, options)`）。
    pub fn on_with(
        &self,
        name: &str,
        listener: Listener,
        global: bool,
        prepend: bool,
    ) -> Result<Disposer, CordisError> {
        let fid = {
            let rt = self.rt.borrow();
            match rt.current_fiber() {
                Some(fid) if rt.fiber(fid).map(|f| f.is_active()).unwrap_or(false) => fid,
                _ => return Err(CordisError::InactiveEffect),
            }
        };
        let name_owned = name.to_string();
        self.effect("ctx.on()", Box::new(move |ctx| {
            let id = {
                let mut rt = ctx.rt.borrow_mut();
                rt.insert_hook(&name_owned, fid, global, prepend, listener)
            };
            Ok(EffectOutcome::One(crate::fiber::make_disposer(Box::new(
                move |ctx| {
                    let mut rt = ctx.rt.borrow_mut();
                    rt.remove_hook(&name_owned, id);
                },
            ))))
        }))
    }

    /// 同步顺序分派（等价 `ctx.emit()`）。
    pub fn emit(&self, name: &str, args: Vec<Value>) {
        let (cbs, scope) = {
            let mut rt = self.rt.borrow_mut();
            let scope = rt
                .current_fiber()
                .and_then(|fid| rt.fiber(fid).map(|f| f.scope))
                .unwrap_or(1);
            if !name.starts_with("internal/") {
                rt.trace_push(&format!("emit:{name}"));
            }
            (rt.collect_hooks(name, scope), scope)
        };
        let _ = scope;
        let mut args = args;
        for cb in cbs {
            let _ = (cb)(self, &mut args, None);
        }
    }

    /// 同步顺序分派，首个 bail 值即停（等价 `ctx.bail()`）。
    pub fn bail(&self, name: &str, args: Vec<Value>) -> Option<Value> {
        self.run_serialish(name, args, "bail")
    }

    /// 顺序分派（等价 `ctx.serial()`）：await 语义，首个 bail 即停。
    pub fn serial(&self, name: &str, args: Vec<Value>) -> Option<Value> {
        self.run_serialish(name, args, "serial")
    }

    fn run_serialish(&self, name: &str, args: Vec<Value>, tag: &str) -> Option<Value> {
        let cbs = {
            let mut rt = self.rt.borrow_mut();
            if !name.starts_with("internal/") {
                rt.trace_push(&format!("{tag}:{name}"));
            }
            let scope = rt
                .current_fiber()
                .and_then(|fid| rt.fiber(fid).map(|f| f.scope))
                .unwrap_or(1);
            rt.collect_hooks(name, scope)
        };
        let mut args = args;
        for cb in cbs {
            let r = (cb)(self, &mut args, None);
            if r.is_bailed() {
                return r.value();
            }
        }
        None
    }

    /// 全部监听器运行（M0 同步实现，等价 `ctx.parallel()` 的顺序语义）。
    pub fn parallel(&self, name: &str, args: Vec<Value>) {
        self.emit(name, args)
    }

    /// 洋葱中间件分派（等价 `ctx.waterfall()`）。
    pub fn waterfall(&self, name: &str, args: Vec<Value>, inner: InnerFn) -> Option<Value> {
        let cbs = {
            let mut rt = self.rt.borrow_mut();
            if !name.starts_with("internal/") {
                rt.trace_push(&format!("waterfall:{name}"));
            }
            let scope = rt
                .current_fiber()
                .and_then(|fid| rt.fiber(fid).map(|f| f.scope))
                .unwrap_or(1);
            rt.collect_hooks(name, scope)
        };
        let state = Rc::new(WfChain {
            cbs,
            inner,
            idx: Cell::new(0),
        });
        let mut args = args;
        run_chain(&state, self, &mut args)
    }

    // ---- 服务 ----

    /// 注册服务实现（等价 `ctx.provide()`；重复注册报错）。
    pub fn provide(&self, name: &str, value: Arc<dyn Any + Send + Sync>) -> Result<Disposer, CordisError> {
        self.provide_with(name, value, None)
    }

    /// 注册服务实现，带可用性谓词（Cordis `reflect.provide(name, value, check)`）。
    pub fn provide_with(
        &self,
        name: &str,
        value: Arc<dyn Any + Send + Sync>,
        check: Option<CheckFn>,
    ) -> Result<Disposer, CordisError> {
        let fid = {
            let rt = self.rt.borrow();
            match rt.current_fiber() {
                Some(fid) if rt.fiber(fid).map(|f| f.is_active()).unwrap_or(false) => fid,
                _ => return Err(CordisError::InactiveEffect),
            }
        };
        let name_owned = name.to_string();
        self.effect("ctx.provide()", Box::new(move |ctx| {
            // Cordis：仅提供者 ACTIVE 时立即 notify（apply 期间（LOADING）延迟到 Active）。
            let notify_now = ctx
                .with(|rt| rt.fiber(fid).map(|f| f.state == FiberState::Active).unwrap_or(false));
            let transitions = {
                let mut rt = ctx.rt.borrow_mut();
                rt.insert_impl(&name_owned, value, fid, check)?;
                if notify_now {
                    rt.notify(&[&name_owned])
                } else {
                    Vec::new()
                }
            };
            for t in transitions {
                ctx.run_or_defer(t);
            }
            Ok(EffectOutcome::One(make_disposer(Box::new(move |ctx| {
                let transitions = {
                    let mut rt = ctx.rt.borrow_mut();
                    rt.remove_impl(&name_owned, fid);
                    rt.notify(&[&name_owned])
                };
                for t in transitions {
                    ctx.run_or_defer(t);
                }
            }))))
        }))
    }

    /// 注册一个 Service（名字/check 由 trait 提供；随 fiber 卸载）。
    pub fn provide_service<S: Service + 'static>(&self, svc: Arc<S>) -> Result<Disposer, CordisError> {
        let name = svc.service_name();
        let check_svc: Arc<dyn Service> = svc.clone();
        self.provide_with(name, svc, Some(Box::new(move || check_svc.check())))
    }

    /// 覆盖服务值（Cordis `ctx.set`；仅提供者 fiber 可写）。
    pub fn set(&self, name: &str, value: Arc<dyn Any + Send + Sync>) -> Result<(), CordisError> {
        // accessor 优先（有 set 钩子则转发）
        let is_accessor = {
            let rt = self.rt.borrow();
            matches!(rt.props.get(name), Some(Property::Accessor { set: Some(_), .. }))
        };
        if is_accessor {
            let v = value
                .downcast::<Value>()
                .map(|v| (*v).clone())
                .map_err(|_| CordisError::Internal(format!("accessor \"{name}\" expects a JSON value")))?;
            let accepted = {
                let rt = self.rt.borrow();
                match rt.props.get(name) {
                    Some(Property::Accessor { set: Some(set), .. }) => set(self, v),
                    _ => false,
                }
            };
            return if accepted {
                Ok(())
            } else {
                Err(CordisError::Internal(format!("accessor \"{name}\" rejected the write")))
            };
        }
        let mut rt = self.rt.borrow_mut();
        let current = rt.current_fiber();
        rt.set_impl_value(name, value, current)
    }

    /// 读取服务（等价 `ctx.get(name)`）：accessor 优先，否则读全局服务仓库。
    pub fn get(&self, name: &str) -> Option<Arc<dyn Any + Send + Sync>> {
        let accessor_value = {
            let rt = self.rt.borrow();
            match rt.props.get(name) {
                Some(Property::Accessor { get, .. }) => get(self),
                _ => None,
            }
        };
        if let Some(v) = accessor_value {
            return Some(Arc::new(v));
        }
        let rt = self.rt.borrow();
        let fid = rt.current_fiber();
        rt.resolve_impl(name, fid).map(|i| i.value.clone())
    }

    /// 类型化读取服务（downcast）。
    pub fn get_typed<T: Any + Send + Sync>(&self, name: &str) -> Option<Arc<T>> {
        self.get(name).and_then(|v| v.downcast::<T>().ok())
    }

    /// 读取 JSON 值形态的服务（accessor/mixin 用）。
    pub fn get_value(&self, name: &str) -> Option<Value> {
        self.get(name)
            .and_then(|v| v.downcast::<Value>().ok())
            .map(|v| (*v).clone())
    }

    // ---- intercept / accessor / mixin ----

    /// 为当前 fiber 注册服务 intercept 配置（Cordis `ctx.intercept(name, config)`）。
    /// 卸载时移除；合并语义见 [`Cordis::resolve_config`]。
    pub fn intercept(&self, name: &str, config: Value) -> Result<Disposer, CordisError> {
        let fid = {
            let rt = self.rt.borrow();
            match rt.current_fiber() {
                Some(fid) if rt.fiber(fid).map(|f| f.is_active()).unwrap_or(false) => fid,
                _ => return Err(CordisError::InactiveEffect),
            }
        };
        let name_owned = name.to_string();
        self.effect("ctx.intercept()", Box::new(move |ctx| {
            let idx = {
                let mut rt = ctx.rt.borrow_mut();
                let f = rt.fiber_mut(fid).expect("fiber alive");
                f.intercept.push((name_owned.clone(), config));
                f.intercept.len() - 1
            };
            Ok(EffectOutcome::One(make_disposer(Box::new(move |ctx| {
                let mut rt = ctx.rt.borrow_mut();
                if let Some(f) = rt.fiber_mut(fid) {
                    if idx < f.intercept.len() {
                        f.intercept.remove(idx);
                    }
                }
            }))))
        }))
    }

    /// 合并当前上下文链上某服务的 intercept 配置（Cordis `Service[resolveConfig]`）。
    /// 合并顺序：`base`（最低优先级）→ 根 → … → 当前 fiber → `head`（最高优先级）；
    /// 同层同名后者覆盖，浅合并（Object.assign 语义）。
    pub fn resolve_config(&self, name: &str, base: Option<Value>, head: Option<Value>) -> Value {
        let levels = {
            let rt = self.rt.borrow();
            let mut levels: Vec<Value> = Vec::new();
            let mut fid = rt.current_fiber();
            while let Some(f) = fid {
                match rt.fiber(f) {
                    Some(fd) => {
                        // 每层同名合并（后者覆盖）
                        let mut merged: Option<Value> = None;
                        for (n, v) in &fd.intercept {
                            if n == name {
                                merged = Some(v.clone());
                            }
                        }
                        if let Some(m) = merged {
                            levels.push(m);
                        }
                        fid = fd.parent;
                    }
                    None => break,
                }
            }
            levels.reverse(); // 根 → 当前
            levels
        };
        let mut out = serde_json::Map::new();
        let mut assign = |v: Option<Value>| {
            if let Some(Value::Object(map)) = v {
                for (k, val) in map {
                    out.insert(k, val);
                }
            }
        };
        assign(base);
        for level in &levels {
            assign(Some(level.clone()));
        }
        assign(head);
        Value::Object(out)
    }

    /// 声明计算属性（Cordis `ctx.accessor(name, { get, set? })`）。
    pub fn accessor(
        &self,
        name: &str,
        get: AccessorGet,
        set: Option<AccessorSet>,
    ) -> Result<Disposer, CordisError> {
        let name_owned = name.to_string();
        self.effect("ctx.accessor()", Box::new(move |ctx| {
            {
                let mut rt = ctx.rt.borrow_mut();
                if rt.props.contains_key(&name_owned) {
                    return Err(CordisError::AlreadyRegistered(name_owned.clone()));
                }
                rt.props.insert(name_owned.clone(), Property::Accessor { get, set });
            }
            Ok(EffectOutcome::One(make_disposer(Box::new(move |ctx| {
                let mut rt = ctx.rt.borrow_mut();
                rt.props.remove(&name_owned);
            }))))
        }))
    }

    /// 把服务的 JSON 值成员转发为访问器（Cordis `ctx.mixin(source, keys)` 的数据形态）。
    pub fn mixin(&self, source: &str, keys: &[&str]) -> Result<Disposer, CordisError> {
        let source_owned = source.to_string();
        let mut disposers = Vec::new();
        for key in keys {
            let source_name = source_owned.clone();
            let key_owned = (*key).to_string();
            let d = self.accessor(
                key,
                Box::new(move |ctx| {
                    ctx.get_value(&source_name)
                        .and_then(|v| v.get(&key_owned).cloned())
                }),
                None,
            )?;
            disposers.push(d);
        }
        Ok(make_disposer(Box::new(move |ctx| {
            for d in disposers {
                d(ctx);
            }
        })))
    }

    // ---- logger ----

    /// 创建命名 logger（Cordis `ctx.logger(name)`）。
    pub fn logger(&self, name: &str) -> Logger {
        Logger {
            ctx: self.clone(),
            name: name.to_string(),
            level: None,
        }
    }

    /// 创建自动命名 logger：intercept `logger` 配置的 `name`/`level`，缺省取
    /// `hyphenate(fiber.name)`（Cordis `LoggerService[invoke]()`）。
    pub fn logger_auto(&self) -> Logger {
        let config = self.resolve_config("logger", None, None);
        let name = config
            .get("name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                self.current_fiber()
                    .and_then(|fid| self.fiber_name(fid))
                    .map(|n| crate::logger::hyphenate(&n))
            })
            .unwrap_or_else(|| "root".to_string());
        let level = config.get("level").and_then(|v| v.as_u64()).map(|l| l as u8);
        Logger {
            ctx: self.clone(),
            name,
            level,
        }
    }

    /// 注册日志导出器（Cordis `ctx.logger.exporter()`；随 fiber 卸载）。
    pub fn exporter(&self, exporter: Exporter, config: ExporterConfig) -> Result<Disposer, CordisError> {
        self.effect("ctx.logger.exporter()", Box::new(move |ctx| {
            let id = {
                let mut rt = ctx.rt.borrow_mut();
                rt.logger.register(exporter, config)
            };
            Ok(EffectOutcome::One(make_disposer(Box::new(move |ctx| {
                let mut rt = ctx.rt.borrow_mut();
                rt.logger.remove(id);
            }))))
        }))
    }

    /// 取已缓冲的日志消息（默认 buffer 导出器）。
    pub fn logger_buffer(&self) -> Vec<crate::logger::Message> {
        self.with(|rt| rt.logger.buffer_snapshot())
    }

    // ---- fiber 生命周期 ----

    /// 卸载 fiber（等价 `fiber.dispose()`）。
    pub fn unload(&self, fid: FiberHandle) -> Result<(), CordisError> {
        {
            let mut rt = self.rt.borrow_mut();
            rt.dispose_fiber(fid)?;
        }
        self.run_transition(Transition::Unload(fid));
        Ok(())
    }

    pub fn fiber_state(&self, fid: FiberHandle) -> Option<FiberState> {
        self.with(|rt| rt.fiber(fid).map(|f| f.state))
    }

    pub fn fiber_name(&self, fid: FiberHandle) -> Option<String> {
        self.with(|rt| rt.fiber(fid).and_then(|f| f.name.clone()))
    }

    /// epoch：`None` = INACTIVE（依赖未齐备）。
    pub fn fiber_epoch(&self, fid: FiberHandle) -> Option<Option<String>> {
        self.with(|rt| rt.fiber(fid).map(|f| f.epoch.clone()))
    }

    /// fiber 的 loader entry 关联（M2）。
    pub fn fiber_entry(&self, fid: FiberHandle) -> Option<String> {
        self.with(|rt| rt.fiber(fid).and_then(|f| f.entry.clone()))
    }

    /// fiber 的 uid（`None` = 已 dispose）。
    pub fn fiber_uid(&self, fid: FiberHandle) -> Option<u64> {
        self.with(|rt| rt.fiber(fid).and_then(|f| f.uid))
    }

    /// fiber 的加载错误（FAILED 时）。
    pub fn fiber_error(&self, fid: FiberHandle) -> Option<CordisError> {
        self.with(|rt| rt.fiber(fid).and_then(|f| f.error.clone()))
    }

    /// 更新 fiber 配置并重启（Cordis `fiber.update(config, noSave)`）。
    /// 先按插件 schema 校验（失败返回 Err），再走 `internal/update` waterfall
    /// （loader 写回监听器在此拦截），默认 inner = restart（卸载后按 epoch 重载）。
    pub fn update(&self, fid: FiberHandle, config: Value) -> Result<(), CordisError> {
        // schema 校验（Cordis `resolveConfig`）
        let validated = self.validate_config(fid, &config)?;
        let active = {
            let mut rt = self.rt.borrow_mut();
            let f = rt.fiber_mut(fid).ok_or(CordisError::FiberNotFound(fid))?;
            if f.uid.is_none() {
                return Err(CordisError::InactiveEffect);
            }
            f.config = validated.clone();
            f.state == FiberState::Active
        };
        if !active {
            return Ok(());
        }
        let cordis = self.clone();
        let _ = self.waterfall(
            "internal/update",
            vec![Value::from(fid), validated, Value::Bool(false)],
            Box::new(move |_args| {
                let mut rt = cordis.rt.borrow_mut();
                if let Some(f) = rt.fiber_mut(fid) {
                    f.error = None;
                }
                drop(rt);
                cordis.run_unload(fid);
                Some(Value::Null)
            }),
        );
        self.drain_internal();
        Ok(())
    }

    /// 按插件的 `config_schema` 校验配置（无 schema 原样通过）。
    fn validate_config(&self, fid: FiberHandle, config: &Value) -> Result<Value, CordisError> {
        let schema = self.with(|rt| {
            rt.fiber(fid)
                .and_then(|f| f.runtime.clone())
                .and_then(|k| rt.registry.get(&k))
                .and_then(|r| r.plugin.config_schema())
        });
        match schema {
            None => Ok(config.clone()),
            Some(s) => dsh_schema::resolve(config, &s, &dsh_schema::ResolveOptions::default())
                .map_err(|e| CordisError::Validation(e.to_string())),
        }
    }

    // ---- 转换编排（内部） ----

    pub(crate) fn run_transition(&self, t: Transition) {
        match t {
            Transition::Load(fid) => self.run_load(fid),
            Transition::Unload(fid) => self.run_unload(fid),
        }
    }

    fn run_load(&self, fid: FiberId) {
        let needs_unload_first = self
            .with(|rt| rt.fiber(fid).map(|f| f.state == FiberState::Active).unwrap_or(false));
        if needs_unload_first {
            self.run_unload(fid);
        }
        let plan = {
            let mut rt = self.rt.borrow_mut();
            rt.begin_load(fid)
        };
        let Some((plugin, config0)) = plan else { return };
        match self.apply_body(fid, &plugin, config0) {
            Ok(outcome) => {
                let _disposer = {
                    let mut rt = self.rt.borrow_mut();
                    rt.fiber_mut(fid)
                        .map(|f| f.collect_effect("plugin-apply", outcome))
                };
                // phase 1：延迟的嵌套 apply 在父 Active 之前运行
                let finishes = self.drain_phase1();
                // 父 Active（finish_load 通知已提供服务 → 依赖方转换）
                let transitions = {
                    let mut rt = self.rt.borrow_mut();
                    rt.finish_load(fid)
                };
                // phase 2：依赖转换 + 延迟 child 的 Finish（父 Active 之后）
                self.drain_phase2(finishes, transitions);
            }
            Err(e) => {
                {
                    let mut rt = self.rt.borrow_mut();
                    rt.fail_fiber(fid, e);
                }
                let _ = self.drain_phase1();
                self.drain_internal();
                return;
            }
        }
        self.drain_internal();
    }

    fn run_unload(&self, fid: FiberId) {
        let disposers = {
            let mut rt = self.rt.borrow_mut();
            rt.begin_unload(fid)
        };
        for d in disposers.iter().rev() {
            d(self);
        }
        let next = {
            let mut rt = self.rt.borrow_mut();
            rt.finish_unload(fid)
        };
        if let Some(t) = next {
            self.run_transition(t);
        }
        self.drain_internal();
    }

    /// 派发排队中的内部事件（internal/status、internal/plugin）到钩子。
    /// 在无借用上下文中运行；钩子内的重入转换会自行排空各自的事件。
    fn drain_internal(&self) {
        let events = {
            let mut rt = self.rt.borrow_mut();
            rt.take_internal()
        };
        for (name, args) in events {
            self.emit(&name, args);
        }
    }
}

impl Default for Cordis {
    fn default() -> Self {
        Self::new()
    }
}

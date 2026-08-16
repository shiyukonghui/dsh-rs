//! 运行时内部状态与纯变更方法（不调用用户代码）。
//!
//! 设计（PLAN §2.1）：`Runtime` 只做数据结构变更并返回「需要运行用户代码的工作」
//! （收集的监听器、需要执行的转换、disposer 等），由 `Cordis` 门面在
//! 「无借用」上下文中执行用户代码，从而保证重入安全。

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use crate::error::CordisError;
use crate::events::{Hook, Listener};
use crate::fiber::{FiberData, FiberState};
use crate::logger::LoggerState;
use crate::reflect::{CheckFn, Impl, Property};
use crate::registry::{Plugin, RuntimeRecord};
use crate::types::{FiberId, HookId, ImplId, ScopeId, Value};

/// 需要由门面执行的 fiber 转换。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    /// 加载（依赖齐备）：运行插件 apply。
    Load(FiberId),
    /// 卸载（依赖消失 / dispose）：逆序运行 disposer。
    Unload(FiberId),
}

/// 延迟加载工作项（apply 期间触发的嵌套加载）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeferredWork {
    /// 运行插件的 apply 体（在父 fiber Active 之前）。
    Apply(FiberId),
    /// 收尾为 Active（在父 fiber Active 之后）。
    Finish(FiberId),
}

/// 运行时内部状态。
pub struct Runtime {
    /// fiber 竞技场。
    pub fibers: Vec<Option<FiberData>>,
    /// 服务实现记录。
    pub impls: HashMap<ImplId, Impl>,
    /// 服务索引：(作用域, 服务名) → 实现 id。
    pub services: HashMap<(ScopeId, String), ImplId>,
    /// 事件钩子表。
    pub hooks: HashMap<String, Vec<Hook>>,
    /// 上下文属性表（service | accessor）。
    pub props: HashMap<String, Property>,
    /// 待派发的内部事件（internal/status、internal/plugin）。
    pub pending_internal: Vec<(String, Vec<Value>)>,
    /// 待执行的延迟加载（插件 apply 期间触发的嵌套加载；M5 对齐 Cordis 微任务让出）。
    pub deferred: Vec<DeferredWork>,
    /// Logger 状态。
    pub logger: LoggerState,
    /// 插件注册表（M0 按插件名键）。
    pub registry: HashMap<String, RuntimeRecord>,
    /// 当前 fiber 栈（动态作用域，等价 Cordis fiber.ctx）。
    pub current: Vec<FiberId>,
    /// 挂载入口时待赋给下一个新 fiber 的 loader entry id（M2）。
    pub pending_entry: Option<String>,
    /// 挂载入口时待注入新 fiber 的服务隔离映射（M3 isolate）。
    pub pending_isolate: HashMap<String, ScopeId>,
    /// 挂载入口时待注入新 fiber 的 intercept 条目（M3）。
    pub pending_intercept: Vec<(String, Value)>,
    /// 每服务名的根作用域（Cordis `root.isolate[name] ??= Symbol(name)`）。
    pub scopes: HashMap<String, ScopeId>,
    /// 规范化事件轨迹（差分验证用）。
    pub trace: Vec<String>,
    // 计数器
    pub next_fiber: u64,
    pub next_impl: u64,
    pub next_hook: u64,
    pub next_scope: u64,
    pub next_uid: u64,
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new()
    }
}

impl Runtime {
    pub fn new() -> Self {
        Runtime {
            fibers: Vec::new(),
            impls: HashMap::new(),
            services: HashMap::new(),
            hooks: HashMap::new(),
            props: HashMap::new(),
            pending_internal: Vec::new(),
            deferred: Vec::new(),
            logger: LoggerState::new(),
            registry: HashMap::new(),
            current: Vec::new(),
            pending_entry: None,
            pending_isolate: HashMap::new(),
            pending_intercept: Vec::new(),
            scopes: HashMap::new(),
            trace: Vec::new(),
            next_fiber: 0,
            next_impl: 0,
            next_hook: 0,
            next_scope: 1,
            next_uid: 0,
        }
    }

    // ---- fiber 竞技场 ----

    pub fn alloc_fiber(
        &mut self,
        parent: Option<FiberId>,
        runtime: Option<String>,
        name: Option<String>,
        inject: Vec<String>,
        config: Value,
    ) -> FiberId {
        let id = self.next_fiber;
        self.next_fiber += 1;
        self.next_uid += 1;
        let uid = self.next_uid;
        self.fibers.push(Some(FiberData {
            id,
            uid: Some(uid),
            parent,
            runtime,
            name,
            entry: None,
            isolate: HashMap::new(),
            state: FiberState::Pending,
            inject,
            store: HashMap::new(),
            intercept: Vec::new(),
            disposers: Vec::new(),
            config,
            error: None,
            epoch: None,
            scope: 1,
        }));
        id
    }

    pub fn fiber(&self, id: FiberId) -> Option<&FiberData> {
        self.fibers.get(id as usize).and_then(|f| f.as_ref())
    }

    pub fn fiber_mut(&mut self, id: FiberId) -> Option<&mut FiberData> {
        self.fibers.get_mut(id as usize).and_then(|f| f.as_mut())
    }

    /// 当前 fiber（栈顶）。
    pub fn current_fiber(&self) -> Option<FiberId> {
        self.current.last().copied()
    }

    /// 当前 fiber 的 uid 是否有效（assertActive 用）。
    pub fn current_active(&self) -> bool {
        match self.current_fiber() {
            Some(fid) => self.fiber(fid).map(|f| f.is_active()).unwrap_or(false),
            None => false,
        }
    }

    // ---- 作用域 ----

    /// 每服务名的根作用域（首次访问时分配）。
    pub fn scope_for(&mut self, name: &str) -> ScopeId {
        let next = &mut self.next_scope;
        *self
            .scopes
            .entry(name.to_string())
            .or_insert_with(|| {
                let s = *next;
                *next += 1;
                s
            })
    }

    /// 分配一个独立的隔离作用域（LocalRealm/GlobalRealm 用；不注册到 scopes 表）。
    pub fn alloc_scope(&mut self) -> ScopeId {
        let s = self.next_scope;
        self.next_scope += 1;
        s
    }

    /// 解析某 fiber 视角下服务 `name` 的作用域：
    /// 沿 fiber 链查 `isolate` 映射，无则用根作用域（从未提供则 None）。
    pub fn resolve_scope(&self, fiber_id: Option<FiberId>, name: &str) -> Option<ScopeId> {
        let mut cur = fiber_id;
        while let Some(fid) = cur {
            match self.fiber(fid) {
                Some(f) => {
                    if let Some(&s) = f.isolate.get(name) {
                        return Some(s);
                    }
                    cur = f.parent;
                }
                None => break,
            }
        }
        self.scopes.get(name).copied()
    }

    // ---- 服务实现 ----

    /// 注册服务实现（等价 `ctx.provide` 的注册部分；重复注册报错）。
    /// 作用域按提供者 fiber 的 isolate 映射解析；同时声明上下文属性为 service。
    pub fn insert_impl(
        &mut self,
        name: &str,
        value: Arc<dyn Any + Send + Sync>,
        owner: FiberId,
        check: Option<CheckFn>,
    ) -> Result<(), CordisError> {
        match self.props.get(name) {
            None => {
                self.props.insert(name.to_string(), Property::Service);
            }
            Some(Property::Accessor { .. }) => {
                return Err(CordisError::AlreadyRegistered(format!(
                    "property \"{name}\" is already declared as accessor"
                )));
            }
            Some(Property::Service) => {}
        }
        let scope = self
            .resolve_scope(Some(owner), name)
            .unwrap_or_else(|| self.scope_for(name));
        let key = (scope, name.to_string());
        if self.services.contains_key(&key) {
            return Err(CordisError::AlreadyRegistered(name.to_string()));
        }
        let id = self.next_impl;
        self.next_impl += 1;
        let imp = Impl {
            id,
            name: name.to_string(),
            value,
            owner,
            scope,
            check,
        };
        self.impls.insert(id, imp);
        self.services.insert(key, id);
        if let Some(f) = self.fiber_mut(owner) {
            f.store.insert(name.to_string(), id);
        }
        Ok(())
    }

    /// 移除服务实现（等价 provide disposer 的删除部分；按提供者作用域）。
    pub fn remove_impl(&mut self, name: &str, owner: FiberId) -> Option<ImplId> {
        let scope = self.resolve_scope(Some(owner), name)?;
        let key = (scope, name.to_string());
        let id = self.services.remove(&key)?;
        if let Some(f) = self.fiber_mut(owner) {
            f.store.remove(name);
        }
        self.impls.remove(&id);
        Some(id)
    }

    /// 覆盖服务值（Cordis `reflect.set`）：仅提供者 fiber 可写。
    pub fn set_impl_value(
        &mut self,
        name: &str,
        value: std::sync::Arc<dyn std::any::Any + Send + Sync>,
        current: Option<FiberId>,
    ) -> Result<(), CordisError> {
        let scope = self
            .resolve_scope(current, name)
            .ok_or_else(|| CordisError::NotProvided(name.to_string()))?;
        let key = (scope, name.to_string());
        let id = match self.services.get(&key) {
            Some(&id) => id,
            None => return Err(CordisError::NotProvided(name.to_string())),
        };
        let owner = self.impls.get(&id).map(|i| i.owner);
        if owner != current {
            return Err(CordisError::MultipleFibers(name.to_string()));
        }
        if let Some(i) = self.impls.get_mut(&id) {
            i.value = value;
        }
        Ok(())
    }

    /// 待派发内部事件入队。
    pub fn push_internal(&mut self, name: &str, args: Vec<Value>) {
        self.pending_internal.push((name.to_string(), args));
    }

    /// 取出待派发内部事件（门面在无借用上下文派发）。
    pub fn take_internal(&mut self) -> Vec<(String, Vec<Value>)> {
        std::mem::take(&mut self.pending_internal)
    }

    /// 解析服务（等价 Cordis `reflect.get`）：按调用方的 isolate 标签读**全局 store**。
    pub fn resolve_impl(&self, name: &str, ctx_fiber: Option<FiberId>) -> Option<&Impl> {
        let scope = self.resolve_scope(ctx_fiber, name)?;
        let id = self.services.get(&(scope, name.to_string()))?;
        self.impls.get(id)
    }

    // ---- 钩子 ----

    pub fn insert_hook(
        &mut self,
        name: &str,
        owner: FiberId,
        global: bool,
        prepend: bool,
        cb: Listener,
    ) -> HookId {
        let id = self.next_hook;
        self.next_hook += 1;
        let scope = self.fiber(owner).map(|f| f.scope).unwrap_or(1);
        let hook = Hook {
            id,
            owner,
            global,
            prepend,
            scope,
            cb,
        };
        let list = self.hooks.entry(name.to_string()).or_default();
        if prepend {
            list.insert(0, hook);
        } else {
            list.push(hook);
        }
        id
    }

    pub fn remove_hook(&mut self, name: &str, id: crate::types::HookId) -> bool {
        if let Some(list) = self.hooks.get_mut(name) {
            if let Some(pos) = list.iter().position(|h| h.id == id) {
                list.remove(pos);
                return true;
            }
        }
        false
    }

    /// 收集命中（global 或作用域匹配）的监听器。M0 恒匹配根作用域。
    pub fn collect_hooks(&self, name: &str, current_scope: ScopeId) -> Vec<Listener> {
        self.hooks
            .get(name)
            .map(|list| {
                list.iter()
                    .filter(|h| h.global || h.scope == current_scope)
                    .map(|h| h.cb.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    // ---- 插件注册 ----

    /// 注册插件：创建 fiber（PENDING），检查依赖，返回待执行转换。
    pub fn register_plugin(
        &mut self,
        plugin: &Arc<dyn Plugin>,
        config: Value,
    ) -> Result<(FiberId, Vec<Transition>), CordisError> {
        let name = plugin.name();
        let key = name.to_string();
        let parent = self.current_fiber();
        let inject = plugin.inject().iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let entry = self.pending_entry.take().or_else(|| {
            parent
                .and_then(|p| self.fiber(p))
                .and_then(|pf| pf.entry.clone())
        });
        let fid = self.alloc_fiber(
            parent,
            Some(key.clone()),
            Some(key.clone()),
            inject.clone(),
            config,
        );
        if let Some(e) = entry {
            if let Some(f) = self.fiber_mut(fid) {
                f.entry = Some(e);
            }
        }
        // isolate：继承 parent 映射 + 注入 pending_isolate（M3）
        let parent_isolate = parent
            .and_then(|p| self.fiber(p))
            .map(|pf| pf.isolate.clone());
        let pending_iso = std::mem::take(&mut self.pending_isolate);
        let pending_ic = std::mem::take(&mut self.pending_intercept);
        if let Some(f) = self.fiber_mut(fid) {
            if let Some(pi) = parent_isolate {
                f.isolate = pi;
            }
            f.isolate.extend(pending_iso);
            f.intercept.extend(pending_ic);
        }
        let record = self
            .registry
            .entry(key.clone())
            .or_insert_with(|| RuntimeRecord {
                key: key.clone(),
                name: Some(key.clone()),
                fibers: Vec::new(),
                plugin: plugin.clone(),
            });
        record.fibers.push(fid);
        self.trace_push(&format!("plugin:{name}"));
        self.push_internal(
            "internal/plugin",
            vec![
                Value::String(name.to_string()),
                Value::from(fid),
                Value::String("create".to_string()),
            ],
        );
        self.check_impls(&fid, &inject);
        let mut transitions = Vec::new();
        if let Some(t) = self.refresh_fiber(fid) {
            transitions.push(t);
        }
        Ok((fid, transitions))
    }

    // ---- 依赖驱动重载（notify） ----

    /// 服务变更后重算依赖方，返回需要执行的转换。
    pub fn notify(&mut self, names: &[&str]) -> Vec<Transition> {
        let mut affected: Vec<FiberId> = Vec::new();
        for record in self.registry.values() {
            for &fid in &record.fibers {
                let has = self
                    .fiber(fid)
                    .map(|f| f.inject.iter().any(|n| names.contains(&n.as_str())))
                    .unwrap_or(false);
                if has {
                    affected.push(fid);
                }
            }
        }
        let mut out = Vec::new();
        for fid in affected {
            let inject = self.fiber(fid).map(|f| f.inject.clone()).unwrap_or_default();
            self.check_impls(&fid, &inject);
            if let Some(t) = self.refresh_fiber(fid) {
                out.push(t);
            }
        }
        out
    }

    /// 重算单个 fiber 的依赖解析（fiber.store），求值可用性谓词；按该 fiber 的作用域。
    fn check_impls(&mut self, fid: &FiberId, names: &[String]) {
        for n in names {
            let scope = self.resolve_scope(Some(*fid), n);
            let found = scope
                .and_then(|s| self.services.get(&(s, n.clone())))
                .and_then(|&iid| {
                    let ok = self.impls.get(&iid).map(|i| i.check_ok()).unwrap_or(false);
                    if ok { Some(iid) } else { None }
                });
            if let Some(f) = self.fiber_mut(*fid) {
                match found {
                    Some(iid) => {
                        f.store.insert(n.clone(), iid);
                    }
                    None => {
                        f.store.remove(n);
                    }
                }
            }
        }
    }

    /// 重算 epoch 并返回需要执行的转换（Load/Unload）。
    fn refresh_fiber(&mut self, fid: FiberId) -> Option<Transition> {
        let inject = self.fiber(fid).map(|f| f.inject.clone()).unwrap_or_default();
        let mut epoch = String::new();
        let mut ok = true;
        for n in &inject {
            let iid = match self.fiber(fid).and_then(|f| f.store.get(n)) {
                Some(&iid) => iid,
                None => {
                    ok = false;
                    break;
                }
            };
            let owner_uid = self
                .impls
                .get(&iid)
                .and_then(|i| self.fiber(i.owner))
                .and_then(|f| f.uid)
                .unwrap_or(0);
            epoch.push(':');
            epoch.push_str(&owner_uid.to_string());
        }
        let new_epoch = if ok { Some(epoch) } else { None };
        let old_epoch = self.fiber(fid).and_then(|f| f.epoch.clone());
        if old_epoch == new_epoch {
            return None;
        }
        if let Some(f) = self.fiber_mut(fid) {
            f.epoch = new_epoch.clone();
        }
        if new_epoch.is_some() {
            Some(Transition::Load(fid))
        } else {
            Some(Transition::Unload(fid))
        }
    }

    // ---- 转换（供门面编排） ----

    /// 开始加载：置 Loading，返回插件与配置供门面执行 apply。
    pub fn begin_load(&mut self, fid: FiberId) -> Option<(Arc<dyn Plugin>, Value)> {
        let (runtime_key, config) = {
            let f = self.fiber(fid)?;
            (f.runtime.clone(), f.config.clone())
        };
        self.set_state(fid, FiberState::Loading);
        let plugin = runtime_key.and_then(|k| self.registry.get(&k).map(|r| r.plugin.clone()))?;
        Some((plugin, config))
    }

    /// 加载成功收尾：state → Active，清错误，返回依赖方转换（Cordis `_updateState` 的 notify）。
    pub fn finish_load(&mut self, fid: FiberId) -> Vec<Transition> {
        if let Some(f) = self.fiber_mut(fid) {
            f.error = None;
        }
        self.set_state(fid, FiberState::Active);
        // 通知本 fiber 提供的服务（Cordis：fiber 变 ACTIVE 时 notify 已提供实现）
        let names: Vec<String> = self
            .impls
            .values()
            .filter(|i| i.owner == fid)
            .map(|i| i.name.clone())
            .collect();
        if names.is_empty() {
            Vec::new()
        } else {
            let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
            self.notify(&refs)
        }
    }

    /// 加载失败：state → Failed，记录错误，epoch 置 INACTIVE。
    pub fn fail_fiber(&mut self, fid: FiberId, error: CordisError) {
        if let Some(f) = self.fiber_mut(fid) {
            f.error = Some(error);
            f.epoch = None;
        }
        self.set_state(fid, FiberState::Failed);
    }

    /// 开始卸载：state → Unloading，取出 disposer（注册顺序）。
    pub fn begin_unload(&mut self, fid: FiberId) -> Vec<Disposer> {
        self.set_state(fid, FiberState::Unloading);
        self.fiber_mut(fid)
            .map(|f| f.take_disposers())
            .unwrap_or_default()
    }

    /// 卸载收尾：根据 uid/epoch 决定终态；若仍需加载返回 Load 转换。
    pub fn finish_unload(&mut self, fid: FiberId) -> Option<Transition> {
        let should_reload = self
            .fiber(fid)
            .map(|f| f.uid.is_some() && f.epoch.is_some())
            .unwrap_or(false);
        if should_reload {
            self.set_state(fid, FiberState::Loading);
            Some(Transition::Load(fid))
        } else {
            let disposed = self.fiber(fid).map(|f| f.uid.is_none()).unwrap_or(true);
            if disposed {
                self.set_state(fid, FiberState::Disposed);
            } else {
                self.set_state(fid, FiberState::Pending);
            }
            None
        }
    }

    /// dispose：uid 置 None，从插件运行记录移除，排队 internal/plugin(dispose)。
    pub fn dispose_fiber(&mut self, fid: FiberId) -> Result<(), CordisError> {
        let (runtime_key, uid, name) = {
            let f = self.fiber(fid).ok_or(CordisError::FiberNotFound(fid))?;
            (f.runtime.clone(), f.uid, f.name.clone())
        };
        if uid.is_none() {
            return Ok(());
        }
        if let Some(f) = self.fiber_mut(fid) {
            f.uid = None;
        }
        if let Some(key) = runtime_key {
            if let Some(record) = self.registry.get_mut(&key) {
                record.fibers.retain(|&x| x != fid);
            }
        }
        self.push_internal(
            "internal/plugin",
            vec![
                Value::String(name.unwrap_or_default()),
                Value::from(fid),
                Value::String("dispose".to_string()),
            ],
        );
        Ok(())
    }

    /// 状态变更：记录 trace 并排队 internal/status 事件（门面负责派发）。
    pub fn set_state(&mut self, fid: FiberId, new: FiberState) {
        let name = self.fiber(fid).and_then(|f| f.name.clone()).unwrap_or_default();
        let old = self.fiber(fid).map(|f| f.state).unwrap_or(FiberState::Disposed);
        if old == new {
            return;
        }
        if let Some(f) = self.fiber_mut(fid) {
            f.state = new;
        }
        self.trace_push(&format!("status:{name}:{old:?}:{new:?}"));
        self.push_internal(
            "internal/status",
            vec![
                Value::String(name),
                Value::String(format!("{old:?}")),
                Value::String(format!("{new:?}")),
            ],
        );
    }

    /// 追加规范轨迹行（差分验证用）。
    pub fn trace_push(&mut self, line: &str) {
        self.trace.push(line.to_string());
    }
}

pub use crate::fiber::Disposer;

/// 门面内部用：包装 `Rc<RefCell<Runtime>>`。
pub type RuntimeCell = Rc<RefCell<Runtime>>;

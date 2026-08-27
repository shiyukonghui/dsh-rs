//! `Cordis` 门面：插件可见 API。
//!
//! 设计（PLAN §2.1）：所有方法遵循「收集-再执行」纪律——先 `borrow_mut()`
//! 完成数据结构变更并收集需要运行的用户代码（监听器、disposer、插件 apply），
//! 释放借用后再执行用户代码。因此用户代码内可重入调用本门面的任何方法。

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use crate::error::{AggregateError, CordisError};
use crate::events::{AsyncListener, HookCallback, HookResult, Listener};
use crate::fiber::{Disposer, EffectBody, EffectMeta, EffectOutcome, FiberHandle, FiberState, GenItem, make_disposer};
use crate::logger::{Exporter, ExporterConfig, Logger};
use crate::reflect::{AccessorGet, AccessorSet, CheckFn, Property};
use crate::registry::Plugin;
use crate::runtime::{AsyncTask, DeferredWork, Runtime, RuntimeCell, TimerKind, TimerSlot, Transition};
use crate::service::Service;
use crate::types::{FiberId, ScopeId, Value};

use futures_util::stream::{LocalBoxStream, StreamExt};

/// waterfall 的最终内置行为（等价 Cordis 中 `args.pop()` 出的 inner）。
pub type InnerFn = Box<dyn Fn(&mut Vec<Value>) -> Option<Value>>;

/// M40：debounce/throttle 返回的包装函数（参数经 `Value` 传递；单线程捕获）。
pub type TimerFn = Rc<dyn Fn(Value)>;

/// M64：`ctx.inject` 的回调体（等价 `Plugin::apply`：`(ctx, config) -> Outcome`）。
/// 非 `Send`——单线程纪律（`Rc<dyn Fn>`）。
pub type InjectBody = Rc<dyn Fn(&Cordis, Value) -> Result<EffectOutcome, CordisError>>;

/// M64：`ctx.inject(deps, callback)` 的底层插件——`inject()` 返回依赖、`apply()`
/// 调用回调。复用既有依赖驱动机制（`refresh_fiber` epoch + `notify`）实现
/// 「等服务就绪后启动」（Cordis `inject` 即 `plugin({ inject, apply: callback })`）。
pub struct InjectPlugin {
    inject: &'static [&'static str],
    body: InjectBody,
}

impl InjectPlugin {
    pub fn new(
        inject: &'static [&'static str],
        body: impl Fn(&Cordis, Value) -> Result<EffectOutcome, CordisError> + 'static,
    ) -> Self {
        InjectPlugin {
            inject,
            body: Rc::new(body),
        }
    }
}

impl Plugin for InjectPlugin {
    fn name(&self) -> &'static str {
        "inject"
    }
    fn inject(&self) -> &'static [&'static str] {
        self.inject
    }
    fn apply(&self, ctx: &Cordis, config: Value) -> Result<EffectOutcome, CordisError> {
        (self.body)(ctx, config)
    }
}

/// M41：`ctx.interval(delay)` 的 tick 流（等价 AsyncIterable）——每 delay
/// 毫秒产出一个 `()`；fiber 卸载（disposer 置 cancelled）→ 流结束。
pub struct IntervalTicks {
    next: Rc<Cell<u64>>,
    cancelled: Rc<Cell<bool>>,
    delay: u64,
    clock: Option<Cordis>,
}

impl futures_util::Stream for IntervalTicks {
    type Item = ();
    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<()>> {
        let this = self.get_mut();
        if this.cancelled.get() {
            return std::task::Poll::Ready(None);
        }
        let now = this
            .clock
            .as_ref()
            .map(|c| c.timer_now())
            .unwrap_or(0);
        if now >= this.next.get() {
            this.next.set(now + this.delay);
            std::task::Poll::Ready(Some(()))
        } else {
            // 未到期：注册 waker（宿主驱动时再次 poll）
            cx.waker().wake_by_ref();
            std::task::Poll::Pending
        }
    }
}

/// waterfall 链状态：共享索引，next 可多次调用（等价 JS `cbs.shift()`）。
struct WfChain {
    cbs: Vec<HookCallback>,
    inner: InnerFn,
    idx: Cell<usize>,
}

fn run_chain(state: &Rc<WfChain>, ctx: &Cordis, args: &mut Vec<Value>) -> Option<Value> {
    let i = state.idx.get();
    if i < state.cbs.len() {
        state.idx.set(i + 1);
        let next: &dyn Fn(&Cordis, &mut Vec<Value>) -> Option<Value> =
            &|ctx, args| run_chain(state, ctx, args);
        match &state.cbs[i] {
            HookCallback::Sync(l) => match (l)(ctx, args, Some(next)) {
                HookResult::Continue => None,
                HookResult::Returned(v) => v,
            },
            // 同步 waterfall 对 async listener fire-and-forget（M18）：调用但不
            // await（等价 Cordis `Reflect.apply` 丢弃 Promise，链继续）。
            HookCallback::Async(a) => {
                ctx.fire_async_listener(a.clone(), args.clone());
                run_chain(state, ctx, args)
            }
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

/// A6 生成器驱动结果（`drive_stream_sync`/`drive_stream_async` 的收敛）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamDrive {
    /// 生成完成 → 可 finish（Active）。
    Completed,
    /// 某步抛错 → 已 `fail_fiber`（失败前已收集 disposer 保留）。
    Failed,
    /// 驱动期间 fiber epoch 变化 → 中途取消（停止后续收集，已收集保留；
    /// fiber 已由 refresh/notify 转入卸载/重载，不 finish）。
    MidCancelled,
    /// sync 模式遇真 pending 步（无事件循环不重驱）→ 保持 Loading，不 finish。
    Pending,
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

    /// 注入 fire-and-forget 调度钩子（async listener 的 emit 驱动；tokio/LocalSet 宿主用）。
    /// `spawn` 接收一个非 Send future，驱动到完成（fire-and-forget）。
    pub fn set_spawn(&self, spawn: impl Fn(futures_util::future::LocalBoxFuture<'static, ()>) + 'static) {
        self.with(|rt| rt.spawn = Some(Box::new(spawn)));
    }

    // ---- M40：timer 服务（对齐 deepseek-harness `vendor/timer`） ----

    /// 注入宿主时钟（毫秒；timer 到期判定用）。None = 未注入 → timer 不触发
    /// （`drive_timers` 无 clock 时跳过，与 `set_spawn` 无钩子跳过一致）。
    pub fn set_timer_clock(&self, clock: impl Fn() -> u64 + 'static) {
        self.with(|rt| rt.timer_clock = Some(Box::new(clock)));
    }

    /// 宿主时钟毫秒（未注入 → 0）。
    fn timer_now(&self) -> u64 {
        self.with(|rt| rt.timer_clock.as_ref().map(|c| c()).unwrap_or(0))
    }

    /// 注册一次性 timer（等价 `ctx.timeout(cb, delay)`）：delay 毫秒后（宿主
    /// `drive_timers` 驱动）执行 cb——仅当注册者 fiber 仍 Active。返回 disposer
    /// （卸载/手动调用时取消 timer，生命周期绑定）。
    pub fn timeout(&self, cb: Rc<dyn Fn()>, delay: u64) -> Result<Disposer, CordisError> {
        let fid = {
            let rt = self.rt.borrow();
            match rt.current_fiber() {
                Some(fid) if rt.fiber(fid).map(|f| f.is_active()).unwrap_or(false) => fid,
                _ => return Err(CordisError::InactiveEffect),
            }
        };
        let now = self.timer_now();
        let idx = self.with(|rt| rt.register_timer(now + delay, 0, TimerKind::Once, cb, fid));
        Ok(make_disposer(Box::new(move |ctx| {
            ctx.with(|rt| rt.cancel_timer(idx));
        })))
    }

    /// 注册周期 timer（等价 `ctx.interval(cb, delay)`）：每 delay 毫秒执行 cb
    /// （驱动驱动）；返回 disposer（卸载取消）。
    pub fn interval(&self, cb: Rc<dyn Fn()>, delay: u64) -> Result<Disposer, CordisError> {
        let fid = {
            let rt = self.rt.borrow();
            match rt.current_fiber() {
                Some(fid) if rt.fiber(fid).map(|f| f.is_active()).unwrap_or(false) => fid,
                _ => return Err(CordisError::InactiveEffect),
            }
        };
        let now = self.timer_now();
        let idx = self.with(|rt| rt.register_timer(now + delay, delay, TimerKind::Interval, cb, fid));
        Ok(make_disposer(Box::new(move |ctx| {
            ctx.with(|rt| rt.cancel_timer(idx));
        })))
    }

    /// 别名（M46，对齐 `vendor/timer` 的 deprecated `setTimeout`）：
    /// 语义等价 [`Cordis::timeout`]——delay 毫秒后执行 cb（宿主驱动），返回
    /// disposer（卸载取消）。
    pub fn set_timeout(&self, cb: Rc<dyn Fn()>, delay: u64) -> Result<Disposer, CordisError> {
        self.timeout(cb, delay)
    }

    /// 别名（M46，对齐 `vendor/timer` 的 deprecated `setInterval`）：
    /// 语义等价 [`Cordis::interval`]——每 delay 毫秒执行 cb（宿主驱动），返回
    /// disposer（卸载取消）。
    pub fn set_interval(&self, cb: Rc<dyn Fn()>, delay: u64) -> Result<Disposer, CordisError> {
        self.interval(cb, delay)
    }

    /// 防抖（等价 `ctx.debounce(cb, delay)`）：返回包装函数——delay 毫秒内多次
    /// 调用只执行最后一次（每次调用重置计时）。返回的包装函数带 `.dispose()`？
    /// Rust 侧：包装函数捕获 slot，调用更新 pending；driver 到期执行。
    /// 返回 `(wrapper, disposer)`——wrapper 为 [`TimerFn`]（参数经 `Value`
    /// 传递），disposer 取消调度（fiber 卸载）。
    pub fn debounce(
        &self,
        cb: Rc<dyn Fn(Value)>,
        delay: u64,
    ) -> Result<(TimerFn, Disposer), CordisError> {
        let fid = {
            let rt = self.rt.borrow();
            match rt.current_fiber() {
                Some(fid) if rt.fiber(fid).map(|f| f.is_active()).unwrap_or(false) => fid,
                _ => return Err(CordisError::InactiveEffect),
            }
        };
        let now = self.timer_now();
        let slot = Rc::new(RefCell::new(TimerSlot {
            last_at: now,
            pending: None,
            pending_deadline: 0,
            cb,
            fid,
        }));
        let id = self.with(|rt| rt.register_timer_slot(slot.clone()));
        let wrapper = {
            let slot = slot.clone();
            let clock = self.clone();
            Rc::new(move |value: Value| {
                let now = clock.timer_now();
                let mut s = slot.borrow_mut();
                s.last_at = now;
                s.pending = Some(value);
                s.pending_deadline = now + delay;
            })
        };
        let disposer = make_disposer(Box::new(move |ctx| {
            ctx.with(|rt| rt.cancel_timer_slot(id));
        }));
        Ok((wrapper, disposer))
    }

    /// 节流（等价 `ctx.throttle(cb, delay)`）：leading edge 立即执行；delay 内
    /// 调用不立即执行，窗口结束时执行**最后一次**（trailing，对齐 Cordis 默认
    /// `noTrailing=false`）。返回 `(wrapper, disposer)`。
    pub fn throttle(
        &self,
        cb: Rc<dyn Fn(Value)>,
        delay: u64,
    ) -> Result<(TimerFn, Disposer), CordisError> {
        let fid = {
            let rt = self.rt.borrow();
            match rt.current_fiber() {
                Some(fid) if rt.fiber(fid).map(|f| f.is_active()).unwrap_or(false) => fid,
                _ => return Err(CordisError::InactiveEffect),
            }
        };
        let slot = Rc::new(RefCell::new(TimerSlot {
            last_at: TimerSlot::NEVER,
            pending: None,
            pending_deadline: 0,
            cb,
            fid,
        }));
        let id = self.with(|rt| rt.register_timer_slot(slot.clone()));
        let wrapper = {
            let slot = slot.clone();
            let clock = self.clone();
            Rc::new(move |value: Value| {
                let now = clock.timer_now();
                let mut s = slot.borrow_mut();
                // leading：从未执行 或 距上次执行 >= delay → 立即执行并记录时刻
                if s.last_at == TimerSlot::NEVER || now >= s.last_at + delay {
                    let cb = s.cb.clone();
                    s.last_at = now;
                    drop(s);
                    cb(value);
                } else {
                    // trailing：记录窗口内最后一次调用，窗口结束时执行
                    s.pending = Some(value);
                    s.pending_deadline = s.last_at + delay;
                }
            })
        };
        let disposer = make_disposer(Box::new(move |ctx| {
            ctx.with(|rt| rt.cancel_timer_slot(id));
        }));
        Ok((wrapper, disposer))
    }

    /// 宿主事件循环驱动：推进到期的 timer（once/interval）与 debounce/throttle
    /// 的 trailing。收集-再执行：先收集到期回调，释放借用后执行（用户代码可
    /// 重入）。fiber 卸载的 timer 已被 disposer 取消（collect 双重过滤 Active）。
    pub fn drive_timers(&self) {
        let now = self.timer_now();
        // 1. once/interval timer
        let due = self.with(|rt| rt.collect_due_timers(now));
        for (cb, _fid) in due {
            cb();
        }
        // 2. debounce/throttle trailing
        let due_slots = self.with(|rt| rt.collect_due_slots(now));
        for (slot, _fid) in due_slots {
            let (cb, value) = {
                let mut s = slot.borrow_mut();
                match s.pending.take() {
                    Some(v) => (s.cb.clone(), v),
                    None => continue,
                }
            };
            cb(value);
        }
    }

    // ---- M41：timer 无回调形态（对齐 `vendor/timer` 的
    // `timeout(delay): Promise` 与 `interval(delay): AsyncIterable`） ----

    /// 延迟 future（等价 `await ctx.timeout(delay)`）：delay 毫秒后 resolve；
    /// fiber 卸载（disposer 取消）→ 返回 `Err`（对齐 Promise reject
    /// "Context has been disposed"）。宿主驱动：`yield_now` 轮询时钟（与
    /// `fiber_await` 同模式；时钟经 `set_timer_clock` 注入）。
    pub fn timeout_async(
        &self,
        delay: u64,
    ) -> futures_util::future::LocalBoxFuture<'static, Result<(), CordisError>> {
        let deadline = self.timer_now() + delay;
        let cancelled = Rc::new(Cell::new(false));
        let cancelled2 = cancelled.clone();
        // disposer：fiber 卸载时置 cancelled（对齐 Promise reject on dispose）
        let _disposer = self
            .effect("ctx.timeout()", Box::new(move |_ctx| {
                Ok(EffectOutcome::One(make_disposer(Box::new(move |_| {
                    cancelled2.set(true);
                }))))
            }))
            .expect("timeout_async registers effect");
        let clock = self.clone();
        Box::pin(async move {
            loop {
                if cancelled.get() {
                    return Err(CordisError::Internal(
                        "Context has been disposed".into(),
                    ));
                }
                if clock.timer_now() >= deadline {
                    return Ok(());
                }
                // 让出（宿主 block_on / LocalSet 驱动；单线程不阻塞）
                Cordis::yield_now().await;
            }
        })
    }

    /// tick 流（等价 `for await (const _ of ctx.interval(delay))`）：
    /// 每 delay 毫秒产出一个 `()`；fiber 卸载（disposer 取消）→ 流结束。
    /// 自驱动：`next_tick` 存 `Rc<Cell>`，poll 检查时钟（不依赖 runtime 队列）。
    pub fn interval_ticks(&self, delay: u64) -> IntervalTicks {
        let first = self.timer_now() + delay;
        let next = Rc::new(Cell::new(first));
        let cancelled = Rc::new(Cell::new(false));
        let cancelled2 = cancelled.clone();
        let _disposer = self
            .effect("ctx.interval()", Box::new(move |_ctx| {
                Ok(EffectOutcome::One(make_disposer(Box::new(move |_| {
                    cancelled2.set(true);
                }))))
            }))
            .expect("interval_ticks registers effect");
        let clock = self.clone();
        IntervalTicks {
            next,
            cancelled,
            delay,
            clock: Some(clock),
        }
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

    // ---- 作用域（K1/C：agent-scope 组合挂载原语；对齐 harness mount.ts） ----

    /// 当前 fiber 的作用域标签（root=1；未在 fiber 上下文 → 1）。
    /// 等同 harness `scopeOf`：untagged（root）监听/提供全局可见，
    /// agent 打标的只在被挂载的会话作用域内可见。
    pub fn current_scope(&self) -> ScopeId {
        self.with(|rt| {
            rt.current_fiber()
                .and_then(|fid| rt.fiber(fid).map(|f| f.scope))
                .unwrap_or(1)
        })
    }

    /// 挂载一个新的 agent scope 子树（预设组合挂载点）。排队一个作用域标签：
    /// 下一次 `plugin`/`plugin_arc` 注册的 fiber 取得该标签，其后代经 parent 链
    /// 继承——挂载树内的注册（监听器/服务）只在本会话作用域可见，root 的全局
    /// 可见；root 看不见本会话的。返回 `(scope, unmount)`：unmount 卸载该作用域下
    /// 整棵子树（随 fiber 展开，等同 harness mount.ts 的 fiber 展开）。
    /// 保留 ScopeKey 单键：`ScopeId` 即不透明 join 键（值比，无第二键空间）。
    pub fn mount_scope(&self) -> Result<(ScopeId, Disposer), CordisError> {
        let scope = self.with(|rt| rt.alloc_scope());
        self.with(|rt| rt.pending_scope.push_back(scope));
        Ok((scope, make_disposer(Box::new(move |ctx| ctx.unmount_scope(scope)))))
    }

    /// 卸载挂载在 `scope` 下的整棵子树（该作用域全部 fiber，含子 fiber）。
    pub fn unmount_scope(&self, scope: ScopeId) {
        let fids = self.with(|rt| {
            rt.fibers
                .iter()
                .flatten()
                .filter(|f| f.scope == scope)
                .map(|f| f.id)
                .collect::<Vec<_>>()
        });
        for fid in fids {
            let _ = self.unload(fid);
        }
    }

    /// M3 补：把当前 fiber 的 isolate 映射指向 `scope`（Cordis
    /// `ctx[Context.isolate]`＝本 fiber 子树内对 `name` 的提供/读取落在 `scope`
    /// realm）。apply 内先调用再 `provide`，服务即进入该 realm（而非 ROOT）——
    /// 是 `audit_subtree` 判定「未泄漏」的正路（harness：preset 服务须置于
    /// isolate realm 或迁往宿主组合）。子 fiber 注册时继承该映射。
    pub fn isolate(&self, name: &str, scope: ScopeId) -> Result<(), CordisError> {
        let fid = self.with(|rt| rt.current_fiber());
        let Some(fid) = fid else {
            return Err(CordisError::InactiveEffect);
        };
        let ok = self.with(|rt| match rt.fiber_mut(fid) {
            Some(f) if f.is_active() => {
                f.isolate.insert(name.to_string(), scope);
                true
            }
            _ => false,
        });
        if ok {
            Ok(())
        } else {
            Err(CordisError::InactiveEffect)
        }
    }

    /// K1/C：root-realm 泄漏审计（harness `mount.ts` 的 `leakedServices` 语义）。
    /// 返回挂载子树 `scope` 下把服务发布进 root realm（或 hook 逃逸）的泄漏描述；
    /// 空列表 = 干净。宿主在发布/审计时若检测到泄漏应拒绝该挂载。
    pub fn audit_subtree(&self, scope: ScopeId) -> Vec<String> {
        self.with(|rt| rt.audit_subtree(scope))
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

    /// 等服务就绪后运行回调（Cordis `ctx.inject(deps, callback)`）。
    ///
    /// 把 `(deps, callback)` 包装为 `{ inject, apply }` 插件（等价 Cordis
    /// `plugin({ inject, apply: callback })`）。依赖服务**全部就绪**（提供者
    /// active）后启动 fiber；未就绪则不启动，服务变更经 `notify` 重算依赖方
    /// 再启动。回调收 `(ctx, config)`。
    ///
    /// M64：补齐 Cordis 公开 API（此前完全缺失；依赖等待复用既有
    /// `refresh_fiber` epoch + `notify` 机制）。
    pub fn inject(
        &self,
        deps: &'static [&'static str],
        callback: impl Fn(&Cordis, Value) -> Result<EffectOutcome, CordisError> + 'static,
    ) -> Result<FiberHandle, CordisError> {
        self.plugin_arc(Arc::new(InjectPlugin::new(deps, callback)), Value::Object(Default::default()))
    }

    /// M7：异步注册插件（等价 `await ctx.plugin()`）。加载编排用真实 `yield_now`
    /// 让出（替代两阶段延迟近似），嵌套加载按微任务 FIFO 顺序交错，与 Cordis
    /// `_reload` 的 `await Promise.resolve()` 语义对齐（3 层以上深嵌套一致）。
    pub async fn plugin_arc_async(
        &self,
        plugin: Arc<dyn Plugin>,
        config: Value,
    ) -> Result<FiberHandle, CordisError> {
        let (fid, transitions) = {
            let mut rt = self.rt.borrow_mut();
            rt.async_mode = true;
            rt.register_plugin(&plugin, config)?
        };
        self.drain_internal();
        self.run_transitions_async(transitions).await;
        // 收尾：等队列空（嵌套加载全部完成）
        self.drive_async_loads().await;
        {
            let mut rt = self.rt.borrow_mut();
            rt.async_mode = false;
        }
        Ok(fid)
    }

    /// 异步执行一组转换：Load 入队（Loading 同步 + 微任务 Apply）；Unload 同步执行。
    pub(crate) async fn run_transitions_async(&self, transitions: Vec<Transition>) {
        for t in transitions {
            match t {
                Transition::Load(fid) => {
                    // Loading 状态同步（与 Cordis `_setEpoch` 一致）；apply 体排队
                    let _ = {
                        let mut rt = self.rt.borrow_mut();
                        rt.begin_load(fid)
                    };
                    self.with(|rt| rt.pending_async_loads.push_back(AsyncTask::Apply(fid)));
                }
                Transition::Unload(fid) => {
                    self.run_transition(t);
                    let _ = fid;
                }
            }
        }
    }

    /// 驱动异步微任务队列（FIFO）：
    /// - `Apply(fid)`：yield_now（apply 前让出）→ apply（嵌套注册同步 Loading + 入队
    ///   `Apply`）→ 排入 `Finish`（在已入队的嵌套之后）。
    /// - `Finish(fid)`：yield_now（apply 后让出）→ finish_load（Active + notify 依赖方）。
    ///
    /// 该顺序精确复刻 Cordis：a 的 apply 后让出排在 b 的 apply 前让出之后，
    /// 因此「b 的 apply 在 a Active 前、c 的 apply 在 a Active 后」。
    pub(crate) async fn drive_async_loads(&self) {
        loop {
            let task = {
                let mut rt = self.rt.borrow_mut();
                rt.pending_async_loads.pop_front()
            };
            let Some(task) = task else { break };
            match task {
                AsyncTask::Apply(fid) => {
                    // apply 前让出（等价 Cordis `_reload` 的 `await Promise.resolve()`）
                    Self::yield_now().await;
                    let plan = {
                        let mut rt = self.rt.borrow_mut();
                        rt.begin_load(fid)
                    };
                    let Some((plugin, config0)) = plan else { continue };
                    match self.apply_body(fid, &plugin, config0) {
                        // A6：apply 返回生成器流 → async 驱动（逐项 await；中途取消 /
                        // 失败停止）。仅 Completed 入队 Finish。
                        Ok(EffectOutcome::Stream(s)) => {
                            let drive = self.drive_stream_async(fid, s).await;
                            if drive == StreamDrive::Completed {
                                self.with(|rt| {
                                    rt.pending_async_loads
                                        .push_back(AsyncTask::Finish(fid))
                                });
                            }
                            // Failed/MidCancelled：不 Finish（fiber 已 fail / 已转卸载或重载；
                            // 生成器与已收集 disposer 的收尾一致：保留至卸载）。
                        }
                        Ok(outcome) => {
                            // M27/M28-B：apply 返回 `Await`（如 Group 的 `[Service.init]`）。
                            // current 已由 apply_body 保留（fid 在栈顶），子入口注册
                            // parent = Group。
                            // D-169：Await future 的执行**延迟为一个队列 hop**——孙辈/子
                            // 入口注册落在兄弟扁平子的 `Finish(p)` 之后（对齐 cordis 子
                            // 入口 create 的 import→fiber→reload hop 链；DIV-nested-2 解决）。
                            // 先标记 await_children（组在延迟窗口内即被识别），存 future、
                            // 入队 `Await(fid)`；由 Await 臂执行 fut → collect → Finish。
                            if let EffectOutcome::Await(fut) = outcome {
                                let mut rt = self.rt.borrow_mut();
                                if let Some(f) = rt.fiber_mut(fid) {
                                    f.await_children = true;
                                }
                                rt.pending_awaits.insert(fid, fut);
                                rt.pending_async_loads.push_back(AsyncTask::Await(fid));
                            } else {
                                let _disposer = {
                                    let mut rt = self.rt.borrow_mut();
                                    rt.fiber_mut(fid)
                                        .map(|f| f.collect_effect("plugin-apply", outcome))
                                };
                                // Finish 排到队尾（在 apply 期间入队的嵌套 Apply 之后）
                                self.with(|rt| {
                                    rt.pending_async_loads.push_back(AsyncTask::Finish(fid))
                                });
                            }
                        }
                        Err(e) => {
                            // Await 失败路径：current 可能未 pop（apply_body 保留）——补 pop
                            let mut rt = self.rt.borrow_mut();
                            rt.current.retain(|&x| x != fid);
                            rt.fail_fiber(fid, e);
                        }
                    }
                }
                AsyncTask::Await(fid) => {
                    // M28-B（D-169）：延迟执行 `Await` future——子/孙入口注册在此发生。
                    // 晚于兄弟扁平子的 `Finish(p)`（其由 `Apply(p)` 更早入队），对齐
                    // cordis「扁平子 Active 抢在组兄弟孙辈注册之前」（DIV-nested-2）。
                    // 有 future 才执行（防御：无挂起 → 视为空 outcome）。
                    Self::yield_now().await;
                    {
                        // 延迟窗口内多个 deferred apply 都留在 current 栈（push 序），
                        // Await 任务按 FIFO 执行时栈顶未必是本组 fid——抬到栈顶，使
                        // future 内注册的子入口 parent = 本组（否则误挂兄弟组 → 其
                        // isolate 令注入依赖不可见，消费方永久 Pending）。
                        let mut rt = self.rt.borrow_mut();
                        rt.current.retain(|&x| x != fid);
                        rt.current.push(fid);
                    }
                    let fut = {
                        let mut rt = self.rt.borrow_mut();
                        rt.pending_awaits.remove(&fid)
                    };
                    let outcome = match fut {
                        Some(fut) => fut.await,
                        None => EffectOutcome::None,
                    };
                    {
                        // 运行毕移除本组 current 记录（保留其下其它 deferred 组）。
                        let mut rt = self.rt.borrow_mut();
                        rt.current.retain(|&x| x != fid);
                    }
                    let _disposer = {
                        let mut rt = self.rt.borrow_mut();
                        rt.fiber_mut(fid)
                            .map(|f| f.collect_effect("plugin-apply", outcome))
                    };
                    // Finish 排到队尾（在 future 期间入队的嵌套 Apply 之后）
                    self.with(|rt| {
                        rt.pending_async_loads.push_back(AsyncTask::Finish(fid))
                    });
                }
                AsyncTask::Finish(fid) => {
                    // M27：`await_children` 标记的 fiber（Group）等待 Loading 后代
                    // 完成后再 finish（等价 Cordis init await 子任务）。普通 fiber
                    // 不受影响（_reload 的父先 Active 时序保持不变）。
                    // M28（D-168）：对齐 Cordis 批次语义——组延迟条件 = ①Loading 后裔
                    // （父不先于子）OR ②批内仍有普通 fiber（await_children=false）的排队
                    // Apply/Finish 任务，使 Pending-only 子组的组也不提前 finish（C1 聚末尾）。
                    // 无死锁：仅组延迟；普通任务不延迟且必然排空；②消失后叶组（无 Loading
                    // 后裔）即刻 finish、父组经 ① 紧随，树序收敛。
                    let should_wait = self.with(|rt| {
                        rt.fiber(fid).map(|f| f.await_children).unwrap_or(false)
                            && {
                                let loading_desc = rt.fibers.iter().flatten().any(|f| {
                                    f.id != fid
                                        && f.state == FiberState::Loading
                                        && rt.fiber_chain_contains(f.id, fid)
                                });
                                let queued_plain = rt.pending_async_loads.iter().any(|t| {
                                    let c = match t {
                                        AsyncTask::Apply(c) | AsyncTask::Finish(c) => *c,
                                        // M28-B：延迟 Await 任务的目标是组（await_children），
                                        // 天然不被 `!await_children` 计数；仅需穷尽 match。
                                        AsyncTask::Await(c) => *c,
                                    };
                                    c != fid
                                        && rt.fiber(c).map(|f| !f.await_children).unwrap_or(false)
                                });
                                loading_desc || queued_plain
                            }
                    });
                    if should_wait {
                        self.with(|rt| {
                            rt.pending_async_loads.push_back(AsyncTask::Finish(fid))
                        });
                        Self::yield_now().await;
                        continue;
                    }
                    // apply 后让出（等价 Cordis `await this._execute(...)` 的 await 同步值）
                    Self::yield_now().await;
                    let transitions = {
                        let mut rt = self.rt.borrow_mut();
                        rt.finish_load(fid)
                    };
                    self.run_transitions_async(transitions).await;
                }
            }
            self.drain_internal();
        }
    }

    /// 执行转换；若正处某个插件的 apply 期间则延迟加载到 apply 收尾前后
    /// （对齐 Cordis 微任务让出：Loading 状态同步、apply 在父 Active 前、Active 在父 Active 后）。
    pub(crate) fn run_or_defer(&self, t: Transition) {
        // M7 async 模式：嵌套注册入微任务队列（Loading 同步、apply 排队），
        // 由 `drive_async_loads` 用真实 `yield_now` 驱动（替代两阶段延迟）。
        if self.with(|rt| rt.async_mode) {
            match t {
                Transition::Load(fid) => {
                    let _ = {
                        let mut rt = self.rt.borrow_mut();
                        rt.begin_load(fid)
                    };
                    self.with(|rt| rt.pending_async_loads.push_back(AsyncTask::Apply(fid)));
                }
                Transition::Unload(fid) => {
                    let _ = fid;
                    self.run_transition(t);
                }
            }
            return;
        }
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
        // M27：同步路径下 `Await`（如 Group 挂载子入口）在 current 上下文内
        // now_or_never 立即完成（future 为同步体）；async 模式保留 Await 由
        // `drive_async_loads` await（真异步完成、等子任务）。
        // A6：`Stream` 同步模式**不**在此消费——由 `drive_stream_sync` 在
        // run_load/drain_phase1 驱动（逐项收集）；current 保留至驱动结束。
        let is_deferred = matches!(
            &result,
            Ok(EffectOutcome::Await(_)) | Ok(EffectOutcome::Stream(_))
        );
        let result = if self.with(|rt| rt.async_mode) {
            result
        } else {
            match result {
                Ok(EffectOutcome::Await(fut)) => Ok(futures_util::FutureExt::now_or_never(fut)
                    .unwrap_or(EffectOutcome::None)),
                other => other,
            }
        };
        if !is_deferred {
            let mut rt = self.rt.borrow_mut();
            rt.current.pop();
        }
        result
    }

    // ---- A6 异步生成器 effect 驱动（等价 cordis `_execute` async-iterator 分支） ----

    /// 把 fid 从当前调用栈移除（驱动结束 / 异常路径补 pop）。
    fn pop_current(&self, fid: FiberId) {
        let mut rt = self.rt.borrow_mut();
        rt.current.retain(|&x| x != fid);
    }

    /// 处理一步产出：`Ok(disposer)` 立即逐项注册；`Err` 置纤维失败。
    /// 返回 `Some` = 应终止驱动。
    fn process_gen_item(&self, fid: FiberId, item: GenItem) -> Option<StreamDrive> {
        match item {
            Ok(d) => {
                let mut rt = self.rt.borrow_mut();
                if let Some(f) = rt.fiber_mut(fid) {
                    f.push_gen_disposer(d);
                }
                None
            }
            Err(e) => {
                {
                    let mut rt = self.rt.borrow_mut();
                    rt.fail_fiber(fid, e);
                }
                Some(StreamDrive::Failed)
            }
        }
    }

    /// 中途取消判定：fiber 当前 epoch 相对生成起点变化（cordis `runner.epoch` 语义）。
    fn gen_mid_cancelled(&self, fid: FiberId, old: &Option<String>) -> bool {
        let cur = self.rt.borrow().fiber(fid).and_then(|f| f.epoch.clone());
        cur != *old
    }

    /// A6：sync 驱动生成器（`now_or_never` 逐步；current 已由 apply_body 保留，
    /// 本方法结束时 pop）。
    fn drive_stream_sync(&self, fid: FiberId, stream: LocalBoxStream<'static, GenItem>) -> StreamDrive {
        let mut s = stream;
        let old = self.rt.borrow().fiber(fid).and_then(|f| f.epoch.clone());
        loop {
            if self.gen_mid_cancelled(fid, &old) {
                self.pop_current(fid);
                return StreamDrive::MidCancelled;
            }
            let mut next = s.next();
            match futures_util::FutureExt::now_or_never(&mut next) {
                Some(Some(item)) => {
                    if let Some(drive) = self.process_gen_item(fid, item) {
                        self.pop_current(fid);
                        return drive;
                    }
                }
                Some(None) => {
                    self.pop_current(fid);
                    return StreamDrive::Completed;
                }
                None => {
                    // 真 pending：保持 Loading，不 finish（与既有 Await sync 同限）
                    self.pop_current(fid);
                    return StreamDrive::Pending;
                }
            }
        }
    }

    /// A6：async 驱动生成器（逐项 `await`；也可在中途取消/失败时停止）。
    async fn drive_stream_async(&self, fid: FiberId, stream: LocalBoxStream<'static, GenItem>) -> StreamDrive {
        let mut s = stream;
        let old = self.rt.borrow().fiber(fid).and_then(|f| f.epoch.clone());
        loop {
            if self.gen_mid_cancelled(fid, &old) {
                self.pop_current(fid);
                return StreamDrive::MidCancelled;
            }
            match s.next().await {
                Some(item) => {
                    if let Some(drive) = self.process_gen_item(fid, item) {
                        self.pop_current(fid);
                        return drive;
                    }
                }
                None => {
                    self.pop_current(fid);
                    return StreamDrive::Completed;
                }
            }
        }
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
                            // A6：嵌套生成器——sync 驱动后仅 Completed 才进入 finishes
                            //（Failed/MidCancelled/Pending 不 Active，留给卸载/重载路径）。
                            Ok(EffectOutcome::Stream(s)) => {
                                if self.drive_stream_sync(fid, s) == StreamDrive::Completed {
                                    finishes.push(fid);
                                }
                            }
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
        self.on_cb(name, HookCallback::Sync(listener), global, prepend)
    }

    /// 注册异步监听器（等价 Cordis 中 async 函数监听器）。
    /// 异步监听器只在 `parallel_async` / `serial_async` 分派中被 await；
    /// 同步分派（emit/bail/serial/waterfall）跳过（记录差异）。
    pub fn on_async(
        &self,
        name: &str,
        listener: AsyncListener,
        global: bool,
        prepend: bool,
    ) -> Result<Disposer, CordisError> {
        self.on_cb(name, HookCallback::Async(listener), global, prepend)
    }

    /// 注册一次性同步监听器（M42，等价 `ctx.once()`）：首次触发时先移除自身
    /// 再调用监听器；返回 disposer（与 `on` 同语义——移除监听器，幂等）。
    pub fn once(
        &self,
        name: &str,
        listener: Listener,
        global: bool,
        prepend: bool,
    ) -> Result<Disposer, CordisError> {
        let dispose_slot: Rc<RefCell<Option<Disposer>>> = Rc::new(RefCell::new(None));
        // 包装：首次触发先 dispose 自身再调用监听器（对齐 Cordis `once`：
        // `const dispose = this.on(name, (...args) => { dispose(); listener(...) })`）。
        let wrapped: Listener = {
            let dispose_slot = dispose_slot.clone();
            Arc::new(move |ctx, args, next| {
                if let Some(d) = dispose_slot.borrow_mut().take() {
                    d(ctx);
                }
                listener(ctx, args, next)
            })
        };
        let disposer = self.on_cb(name, HookCallback::Sync(wrapped), global, prepend)?;
        *dispose_slot.borrow_mut() = Some(disposer.clone());
        Ok(disposer)
    }

    /// 注册一次性异步监听器（M42，等价 `ctx.once()` 对 async 监听器）。
    /// 首次触发时先移除自身再调用；异步分派（parallel_async/serial_async）await。
    pub fn once_async(
        &self,
        name: &str,
        listener: AsyncListener,
        global: bool,
        prepend: bool,
    ) -> Result<Disposer, CordisError> {
        let dispose_slot: Rc<RefCell<Option<Disposer>>> = Rc::new(RefCell::new(None));
        let wrapped: AsyncListener = {
            let dispose_slot = dispose_slot.clone();
            Arc::new(move |ctx, args| {
                // 首次触发先 dispose（同步移除），再 await 监听器
                if let Some(d) = dispose_slot.borrow_mut().take() {
                    d(ctx);
                }
                listener(ctx, args)
            })
        };
        let disposer = self.on_cb(name, HookCallback::Async(wrapped), global, prepend)?;
        *dispose_slot.borrow_mut() = Some(disposer.clone());
        Ok(disposer)
    }

    fn on_cb(
        &self,
        name: &str,
        cb: HookCallback,
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
        // M44：`internal/listener` bail 拦截（对齐 Cordis `ctx.on()`——
        // `const result = this.bail(this.ctx, 'internal/listener', name, listener,
        // options); if (result) return result`）。bail 值非 null → 注册被拦截，
        // 返回 no-op disposer（调用方拿到 disposer 但实际未注册；Value-land
        // 限制：bail 值无法表达 Rust disposer，仅作拦截标记）。
        let intercepted = self.bail(
            "internal/listener",
            vec![
                Value::String(name.to_string()),
                Value::Bool(global),
                Value::Bool(prepend),
            ],
        );
        if intercepted.is_some() {
            return Ok(crate::fiber::make_disposer(Box::new(|_| {})));
        }
        let name_owned = name.to_string();
        self.effect("ctx.on()", Box::new(move |ctx| {
            let id = {
                let mut rt = ctx.rt.borrow_mut();
                rt.insert_hook(&name_owned, fid, global, prepend, cb)
            };
            Ok(EffectOutcome::One(crate::fiber::make_disposer(Box::new(
                move |ctx| {
                    let mut rt = ctx.rt.borrow_mut();
                    rt.remove_hook(&name_owned, id);
                },
            ))))
        }))
    }

    /// `internal/dispatch` 统一钩子（M65，对齐 Cordis `EventsService.dispatch`）：
    /// 派发**非 internal** 事件前同步 emit `internal/dispatch(type, name, args,
    /// thisArg)`。emit 语义（无 next、返回值丢弃）；`internal/` 前缀跳过。
    /// 模式值照抄 Cordis：parallel 经 emit 报 "emit"；bail/serial/waterfall 各报
    /// 自身。必须在 borrow `rt` 前调用（内部经 `emit` 重新 borrow）。
    fn report_dispatch(&self, mode: &str, name: &str, args: &[Value]) {
        if name.starts_with("internal/") {
            return;
        }
        self.emit(
            "internal/dispatch",
            vec![
                Value::String(mode.to_string()),
                Value::String(name.to_string()),
                Value::Array(args.to_vec()),
                Value::Null,
            ],
        );
    }

    /// 同步顺序分派（等价 `ctx.emit()`）。
    pub fn emit(&self, name: &str, args: Vec<Value>) {
        self.report_dispatch("emit", name, &args);
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
            match cb {
                HookCallback::Sync(l) => {
                    let _ = (l)(self, &mut args, None);
                }
                // fire-and-forget：经宿主注入的 spawn 钩子驱动异步监听器
                // （等价 Cordis emit 调用 async listener 但不 await；无钩子则跳过）
                HookCallback::Async(a) => {
                    self.fire_async_listener(a, args.clone());
                }
            }
        }
    }

    /// fire-and-forget 驱动异步监听器（emit 用；经 `Runtime.spawn` 钩子）。
    /// 无 spawn 钩子（同步宿主）时跳过（记录 trace）。
    fn fire_async_listener(&self, listener: AsyncListener, args: Vec<Value>) {
        let ctx = self.clone();
        let fut: futures_util::future::LocalBoxFuture<'static, ()> = Box::pin(async move {
            let _ = listener(&ctx, args).await;
        });
        let spawned = self.with(|rt| match &rt.spawn {
            Some(spawn) => {
                spawn(fut);
                true
            }
            None => false,
        });
        if !spawned {
            self.with(|rt| rt.trace_push("async-listener-skipped"));
        }
    }

    /// 同步顺序分派，首个 bail 值即停（等价 `ctx.bail()`）。
    pub fn bail(&self, name: &str, args: Vec<Value>) -> Option<Value> {
        self.run_serialish(name, args, "bail")
    }

    /// 顺序分派（等价 `ctx.serial()`）：await 语义，首个 bail 即停。
    /// 同步路径：仅处理同步监听器（异步监听器跳过，见 `serial_async`）。
    pub fn serial(&self, name: &str, args: Vec<Value>) -> Option<Value> {
        self.run_serialish(name, args, "serial")
    }

    fn run_serialish(&self, name: &str, args: Vec<Value>, tag: &str) -> Option<Value> {
        self.report_dispatch(tag, name, &args);
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
            let r = match cb {
                HookCallback::Sync(l) => (l)(self, &mut args, None),
                // 同步 bail/serial 对 async listener fire-and-forget（M18）：
                // 调用但不 await（等价 Cordis `Reflect.apply` 丢弃 Promise，
                // bail 值不可同步判定 → 继续）。
                HookCallback::Async(a) => {
                    self.fire_async_listener(a.clone(), args.clone());
                    continue;
                }
            };
            if r.is_bailed() {
                return r.value();
            }
        }
        None
    }

    /// 全部监听器运行（M0 同步实现，等价 `ctx.parallel()` 的顺序语义）。
    /// 同步路径：仅处理同步监听器（异步监听器跳过，见 `parallel_async`）。
    pub fn parallel(&self, name: &str, args: Vec<Value>) {
        self.emit(name, args)
    }

    /// 洋葱中间件分派（等价 `ctx.waterfall()`）。
    pub fn waterfall(&self, name: &str, args: Vec<Value>, inner: InnerFn) -> Option<Value> {
        self.report_dispatch("waterfall", name, &args);
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

    // ---- M7 async 分派 ----

    /// 异步顺序分派：全部监听器并发执行（等价 `ctx.parallel()`），
    /// 同步与异步监听器混跑；错误聚合为 `AggregateError`（allSettled 语义）。
    /// 异步顺序分派：全部监听器并发执行（等价 `ctx.parallel()`——Promise.all
    /// 结果数组），同步与异步监听器混跑；错误聚合为 `AggregateError`
    /// （allSettled 语义）。M60：返回各监听器返回值（`Continue` → null）。
    pub async fn parallel_async(&self, name: &str, args: Vec<Value>) -> Result<Vec<Value>, AggregateError> {
        let cbs = {
            let mut rt = self.rt.borrow_mut();
            if !name.starts_with("internal/") {
                rt.trace_push(&format!("parallel:{name}"));
            }
            let scope = rt
                .current_fiber()
                .and_then(|fid| rt.fiber(fid).map(|f| f.scope))
                .unwrap_or(1);
            rt.collect_hooks(name, scope)
        };
        let mut futs: Vec<futures_util::future::LocalBoxFuture<'static, Result<HookResult, CordisError>>> =
            Vec::new();
        for cb in cbs {
            match cb {
                HookCallback::Sync(l) => {
                    let ctx = self.clone();
                    let mut args = args.clone();
                    futs.push(Box::pin(async move {
                        Ok((l)(&ctx, &mut args, None))
                    }));
                }
                HookCallback::Async(a) => {
                    let ctx = self.clone();
                    let args = args.clone();
                    futs.push(a(&ctx, args));
                }
            }
        }
        let results = futures_util::future::join_all(futs).await;
        let mut out = Vec::with_capacity(results.len());
        let mut errors: Vec<CordisError> = Vec::new();
        for r in results {
            match r {
                Ok(h) => out.push(h.value().unwrap_or(Value::Null)),
                Err(e) => errors.push(e),
            }
        }
        if errors.is_empty() {
            Ok(out)
        } else {
            Err(AggregateError { errors })
        }
    }

    /// 异步顺序分派：逐个 await 监听器，首个 bail 值即停（等价 `ctx.serial()`）。
    /// 监听器错误向上传播（`Err`）。
    pub async fn serial_async(
        &self,
        name: &str,
        args: Vec<Value>,
    ) -> Result<Option<Value>, CordisError> {
        let cbs = {
            let mut rt = self.rt.borrow_mut();
            if !name.starts_with("internal/") {
                rt.trace_push(&format!("serial:{name}"));
            }
            let scope = rt
                .current_fiber()
                .and_then(|fid| rt.fiber(fid).map(|f| f.scope))
                .unwrap_or(1);
            rt.collect_hooks(name, scope)
        };
        let mut args = args;
        for cb in cbs {
            let r = match cb {
                HookCallback::Sync(l) => (l)(self, &mut args, None),
                HookCallback::Async(a) => a(self, args.clone()).await?,
            };
            if r.is_bailed() {
                return Ok(r.value());
            }
        }
        Ok(None)
    }

    /// 让出当前异步任务（等价 Cordis 微任务边界 `await Promise.resolve()`）。
    /// 无 `futures_util::task`（默认特性关闭）时自实现一个 ready 后再 pending 的 future。
    pub async fn yield_now() {
        struct YieldNow(bool);
        impl std::future::Future for YieldNow {
            type Output = ();
            fn poll(
                mut self: std::pin::Pin<&mut Self>,
                cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<()> {
                if self.0 {
                    std::task::Poll::Ready(())
                } else {
                    self.0 = true;
                    cx.waker().wake_by_ref();
                    std::task::Poll::Pending
                }
            }
        }
        YieldNow(false).await;
    }

    /// 等待 fiber 完成加载/卸载（等价 `fiber.await()`）：
    /// 轮询直到状态离开 Loading/Unloading；FAILED 时返回错误。
    pub async fn fiber_await(&self, fid: FiberHandle) -> Result<(), CordisError> {
        loop {
            let state = self.with(|rt| rt.fiber(fid).map(|f| f.state));
            match state {
                Some(FiberState::Loading) | Some(FiberState::Unloading) => {
                    Self::yield_now().await;
                }
                _ => break,
            }
        }
        if let Some(e) = self.fiber_error(fid) {
            return Err(e);
        }
        Ok(())
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
    /// B1：追加 Service 类型直达通道（srv 同键注册）——供 `get_extended`/`call_service`。
    pub fn provide_service<S: Service + 'static>(&self, svc: Arc<S>) -> Result<Disposer, CordisError> {
        let name = svc.service_name();
        let check_svc: Arc<dyn Service> = svc.clone();
        // Any 通道（既有：属性声明/notify/依赖可见性）+ Service 通道（B1）
        let any_impl: Arc<dyn Any + Send + Sync> = svc.clone();
        let svc_trait: Arc<dyn Service> = svc;
        let d1 = self.provide_with(name, any_impl, Some(Box::new(move || check_svc.check())))?;
        let fid = self.current_fiber().ok_or(CordisError::InactiveEffect)?;
        let name_s = name.to_string();
        let d2 = self.effect("ctx.svc()", Box::new(move |ctx| {
            let key = {
                let mut rt = ctx.rt.borrow_mut();
                // 与 insert_impl 的作用域解析**完全一致**（scope_for 兜底），确保 srv 键
                // 与 services 键对齐（拆分 effect 自身执行晚于 insert_impl——不能依赖
                // 执行顺带来的 scopes 预填，必须显式同源解析）。
                let scope = rt
                    .resolve_scope(Some(fid), &name_s)
                    .unwrap_or_else(|| rt.scope_for(&name_s));
                rt.srv.insert((scope, name_s.clone()), svc_trait.clone());
                (scope, name_s.clone())
            };
            Ok(EffectOutcome::One(make_disposer(Box::new(move |ctx| {
                ctx.with(|rt| {
                    rt.srv.remove(&key);
                });
            }))))
        }))?;
        // 组合 disposer：Any 通道释放 + Service 通道释放
        Ok(Rc::new(move |ctx| {
            d1(ctx);
            d2(ctx);
        }))
    }

    /// B1：按当前纤维作用域链解析 Service 类型实例（镜像 impl 解析；仅 `provide_service`
    /// 注册的 Service 型服务，DIV-7-2）。
    pub fn srv_lookup(&self, name: &str) -> Option<Arc<dyn Service>> {
        let rt = self.rt.borrow();
        let scope = rt.resolve_scope(rt.current_fiber(), name)?;
        rt.srv.get(&(scope, name.to_string())).cloned()
    }

    /// B1：获取**派生作用域实例**（Cordis `Service[extend]`）——`extend` 返回
    /// `Some(derived)` → 派生实例；`None`（默认）→ 原实例（恒等）。
    pub fn get_extended(&self, name: &str) -> Option<Arc<dyn Service>> {
        let svc = self.srv_lookup(name)?;
        match svc.extend(self) {
            Some(derived) => Some(derived),
            None => Some(svc),
        }
    }

    /// B1：调用可调用服务（Cordis 可调用服务调用）——未提供或不可调用 → 明确错误。
    pub fn call_service(&self, name: &str, args: &[Value]) -> Result<Value, CordisError> {
        match self.srv_lookup(name) {
            Some(svc) => svc.invoke(self, args),
            None => Err(CordisError::Internal(format!("service `{name}` is not provided"))),
        }
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
        // accessor 优先（先释放借用再调用 get——用户代码可重入）
        let accessor_get = {
            let rt = self.rt.borrow();
            match rt.props.get(name) {
                Some(Property::Accessor { get, .. }) => Some(get.clone()),
                _ => None,
            }
        };
        if let Some(get) = accessor_get {
            let v = get(self);
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
    /// M43：经 `internal/get` waterfall 拦截（对齐 Cordis `ctx.get` 的 Proxy
    /// handler）——监听器可返回替代值（短路 inner 查表）或 next 委托。
    pub fn get_value(&self, name: &str) -> Option<Value> {
        let this = self.clone();
        let name_owned = name.to_string();
        self.waterfall(
            "internal/get",
            vec![Value::String(name_owned), Value::Null],
            Box::new(move |args| {
                // inner：实际查表（accessor 或 Value 服务）
                let name = args.first().and_then(|a| a.as_str()).unwrap_or("");
                this.get_raw_value(name)
            }),
        )
        .and_then(|v| {
            // 拦截器短路值或 inner 返回值；Null 表示「未找到」
            if v.is_null() {
                None
            } else {
                Some(v)
            }
        })
    }

    /// A2 收口（D-171）：按**指定上下文**（而非 current_fiber）解析 Value 服务——
    /// 目标视图解析（父链 + isolate），供 loader 绑目标纤维 / 入口决策上下文。
    /// 与 `get_value` 同款 Value 暴露面（DIV-6-1，Value 服务才可读）；不经
    /// `internal/get` 拦截（决策期取决策时刻快照，文档化为边界）。
    pub fn get_value_from(&self, ctx_fiber: Option<FiberId>, name: &str) -> Option<Value> {
        let impl_arc = self.with(|rt| rt.resolve_impl(name, ctx_fiber).map(|i| i.value.clone()));
        impl_arc
            .and_then(|a| a.downcast::<Value>().ok())
            .map(|v| (*v).clone())
    }

    /// 原始查表（不经拦截）：accessor 或 Value 服务（M43 inner）。
    fn get_raw_value(&self, name: &str) -> Option<Value> {
        // accessor 优先（clone 闭包，无借用调用——get 内可重入）
        let accessor_get = {
            let rt = self.rt.borrow();
            match rt.props.get(name) {
                Some(Property::Accessor { get, .. }) => Some(get.clone()),
                _ => None,
            }
        };
        if let Some(get) = accessor_get {
            return get(self);
        }
        self.get(name).and_then(|v| v.downcast::<Value>().ok()).map(|v| (*v).clone())
    }

    /// 写 JSON 值形态的服务（M43：对齐 Cordis `ctx.set` 的 Proxy handler）。
    /// 经 `internal/set` waterfall——监听器可 veto（返回 false，不调用 next）
    /// 或 next 委托 inner 实际写入。inner 只对**当前 fiber 提供的** Value 服务
    /// 有效（对齐 Cordis `set` 的所有者校验）；accessor 有 set 钩子则转发。
    pub fn set_value(&self, name: &str, value: Value) -> Result<(), CordisError> {
        let this = self.clone();
        let name_owned = name.to_string();
        let accepted = self
            .waterfall(
                "internal/set",
                vec![Value::String(name_owned), value.clone(), Value::Null],
                Box::new(move |args| {
                    let name = args.first().and_then(|a| a.as_str()).unwrap_or("");
                    let value = args.get(1).cloned().unwrap_or(Value::Null);
                    // inner：accessor set 钩子优先；否则覆盖 Value 服务值
                    let ok = this.set_raw_value(name, value);
                    Some(Value::Bool(ok))
                }),
            )
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if accepted {
            Ok(())
        } else {
            Err(CordisError::Internal(format!("set_value \"{name}\" rejected (vetoed or not writable)")))
        }
    }

    /// 原始写入（不经拦截）：accessor set 钩子或覆盖 Value 服务值。
    fn set_raw_value(&self, name: &str, value: Value) -> bool {
        // accessor 优先（有 set 钩子则转发）
        let is_accessor = {
            let rt = self.rt.borrow();
            matches!(rt.props.get(name), Some(Property::Accessor { set: Some(_), .. }))
        };
        if is_accessor {
            let accepted = {
                let rt = self.rt.borrow();
                match rt.props.get(name) {
                    Some(Property::Accessor { set: Some(set), .. }) => set(self, value),
                    _ => false,
                }
            };
            return accepted;
        }
        // 覆盖 Value 服务值（仅提供者 fiber 可写，对齐 Cordis `set` 所有者校验）
        let mut rt = self.rt.borrow_mut();
        let current = rt.current_fiber();
        rt.set_impl_value(name, Arc::new(value), current).is_ok()
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
                Rc::new(move |ctx| {
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

    /// 异步卸载：同步 disposer 逆序执行 + 异步 disposer 并行执行（等价 Cordis
    /// `_unload` 的 `Promise.all` 清理）。异步 disposer 的错误含化（记录 trace）。
    pub async fn unload_async(&self, fid: FiberHandle) -> Result<(), CordisError> {
        {
            let mut rt = self.rt.borrow_mut();
            rt.dispose_fiber(fid)?;
        }
        let (disposers, async_futs) = {
            let mut rt = self.rt.borrow_mut();
            let disposers = rt.begin_unload(fid);
            let async_futs = rt
                .fiber_mut(fid)
                .map(|f| f.take_async_disposers())
                .unwrap_or_default();
            (disposers, async_futs)
        };
        // M28：卸载让出——多个 fiber 并行卸载（如 Group 子入口）时，Unloading
        // 状态先全部提交，disposers/finish 再交错执行（对齐 TS `Promise.all`
        // 卸载：先全部 Unloading 再逐个 Disposed）。
        Self::yield_now().await;
        // 同步 disposer：逆序
        for d in disposers.iter().rev() {
            d(self);
        }
        Self::yield_now().await;
        // 异步 disposer：并行（join_all），错误含化
        let cordis = self.clone();
        let results = futures_util::future::join_all(async_futs.into_iter().map(|fut| {
            let cordis = cordis.clone();
            async move {
                let outcome = fut.await;
                let mut collected: Vec<Disposer> = Vec::new();
                let mut stack: Vec<EffectOutcome> = vec![outcome];
                while let Some(o) = stack.pop() {
                    match o {
                        EffectOutcome::None => {}
                        EffectOutcome::One(d) => collected.push(d),
                        EffectOutcome::Many(ds) => collected.extend(ds),
                        EffectOutcome::Async(f) => stack.push(f.await),
                        // 卸载时 Await 同 Async（await 得到最终 outcome）。
                        EffectOutcome::Await(f) => stack.push(f.await),
                        // A6：Stream 项已由驱动逐项收集（此处仅作为 async-disposer 的
                        // 结束标记；卸载前未产出项按 cordis 语义不追溯）。
                        EffectOutcome::Stream(_) => {}
                    }
                }
                for d in collected.iter().rev() {
                    d(&cordis);
                }
            }
        }))
        .await;
        let _ = results;
        // M28：finish 前让出——多个 fiber 并行卸载时 Disposed 状态交错提交。
        Self::yield_now().await;
        let next = {
            let mut rt = self.rt.borrow_mut();
            rt.finish_unload(fid)
        };
        if let Some(t) = next {
            self.run_transition(t);
        }
        self.drain_internal();
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

    /// fiber 当前生效配置（`this.config`；veto 的 update 不更新它）。
    pub fn fiber_config(&self, fid: FiberHandle) -> Option<Value> {
        self.with(|rt| rt.fiber(fid).map(|f| f.config.clone()))
    }

    /// fiber 已注册 effect 的元数据列表（注册序；等价 Cordis `fiber.getEffects()`）。
    /// 当前仅 label 有值，`children` 恒空（dsh-core 无 effect 父子结构）。
    pub fn get_effects(&self, fid: FiberHandle) -> Option<Vec<EffectMeta>> {
        self.with(|rt| rt.fiber(fid).map(|f| f.effects.clone()))
    }

    /// 更新 fiber 配置并重启（Cordis `fiber.update(config)`）。
    /// 等价 `update_with(fid, config, false)`。
    pub fn update(&self, fid: FiberHandle, config: Value) -> Result<(), CordisError> {
        self.update_with(fid, config, false)
    }

    /// 更新 fiber 配置并重启，带 `noSave`（提示持久化钩子跳过写回；loader 写回
    /// 监听器依赖）。先按插件 schema 校验（失败返回 Err），再走 `internal/update`
    /// waterfall（loader 写回监听器在此拦截），默认 inner = restart。`this.config`
    /// 在 waterfall inner 内赋值（Cordis）——**veto 的 update 不致 config 生效**。
    pub fn update_with(&self, fid: FiberHandle, config: Value, no_save: bool) -> Result<(), CordisError> {
        // schema 校验（Cordis `resolveConfig`）
        let validated = self.validate_config(fid, &config)?;
        let active = {
            let mut rt = self.rt.borrow_mut();
            let f = rt.fiber_mut(fid).ok_or(CordisError::FiberNotFound(fid))?;
            if f.uid.is_none() {
                return Err(CordisError::InactiveEffect);
            }
            // 注意：不在 waterfall 前 eager 赋 `config`——`this.config` 在 inner
            // 内赋值，veto（inner 不执行）时配置不生效。
            f.state == FiberState::Active
        };
        if !active {
            return Ok(());
        }
        let cordis = self.clone();
        let _ = self.waterfall(
            "internal/update",
            vec![Value::from(fid), validated, Value::Bool(no_save)],
            Box::new(move |args| {
                // inner：赋 config（生效）后重启（Cordis `this.config = config;
                // return this.restart()`）。
                let cfg = args.get(1).cloned().unwrap_or(Value::Null);
                let mut rt = cordis.rt.borrow_mut();
                if let Some(f) = rt.fiber_mut(fid) {
                    f.config = cfg;
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
            // A6：apply 返回生成器流 → sync 驱动（逐项收集/失败/中途取消/完成）。
            Ok(EffectOutcome::Stream(s)) => {
                let drive = self.drive_stream_sync(fid, s);
                match drive {
                    StreamDrive::Completed => {
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
                    _ => {
                        // Failed / Pending：不 finish（fiber 已 fail / 保持 Loading）。
                        // MidCancelled：epoch 已变 → 等价 cordis `_reload` 在 `_execute`
                        // 早退后的 `_unload()`——运行已收集 disposer（保留的逆序）并落
                        // Disposed；不 finish 到 Active。run_unload 对已卸载纤维幂等。
                        if drive == StreamDrive::MidCancelled {
                            self.run_unload(fid);
                        }
                        let _ = self.drain_phase1();
                    }
                }
            }
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
        let (disposers, has_async) = {
            let mut rt = self.rt.borrow_mut();
            let disposers = rt.begin_unload(fid);
            // 同步 unload 无法 await 异步 disposer：显式取出并记录（不静默丢弃）。
            // 需用异步卸载（`unload_async`）执行完整异步清理。
            let has_async = rt
                .fiber(fid)
                .map(|f| !f.async_disposers.is_empty())
                .unwrap_or(false);
            (disposers, has_async)
        };
        for d in disposers.iter().rev() {
            d(self);
        }
        if has_async {
            self.with(|rt| rt.trace_push("async-disposers-skipped"));
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

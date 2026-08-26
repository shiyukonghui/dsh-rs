//! Fiber 状态机、effect 与 disposer（对应 PLAN §1.4）。

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use futures_util::future::LocalBoxFuture;
use futures_util::stream::LocalBoxStream;

use crate::context::Cordis;
use crate::error::CordisError;
use crate::types::{FiberId, ImplId, ScopeId, Value};

/// Fiber 生命周期状态（Cordis `FiberState`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FiberState {
    Pending,
    Loading,
    Active,
    Failed,
    Unloading,
    Disposed,
}

/// fiber 句柄（M0 直接复用 id）。
pub type FiberHandle = FiberId;

/// disposer：可共享、幂等的一次性副作用。
/// 内部用 `Cell<Option<FnOnce>>` 保证只执行一次；fiber 卸载与调用方共享同一 `Rc`。
pub type Disposer = Rc<dyn Fn(&Cordis)>;

/// 把一个一次性闭包包装为可共享、幂等的 disposer。
pub fn make_disposer(once: Box<dyn FnOnce(&Cordis)>) -> Disposer {
    let cell = Rc::new(Cell::new(Some(once)));
    Rc::new(move |ctx| {
        if let Some(f) = cell.take() {
            f(ctx)
        }
    })
}

/// effect 主体产出的结果（Cordis `Effect`）。
pub enum EffectOutcome {
    None,
    One(Disposer),
    Many(Vec<Disposer>),
    /// 异步 effect：future resolve 后得到最终 outcome（支持嵌套 `Async`）。
    /// 由 `unload_async` 在卸载时并行执行（等价 Cordis async disposer）。
    Async(LocalBoxFuture<'static, EffectOutcome>),
    /// apply 期间异步完成（M27：等价 Cordis `[Service.init]` async generator）：
    /// future resolve 后得到最终 outcome；在此期间 fiber 保持 Loading，
    /// 排入的子任务（如 Group 的子入口）先完成，之后才 finish（Active）。
    Await(LocalBoxFuture<'static, EffectOutcome>),
    /// A6：异步生成器 effect（等价 Cordis `_execute` 的 async-iterator 分支，
    /// `[Service.init]` 完整形态）——pull 式**逐项**产出 disposer（`GenItem`），
    /// 驱动方跨 await 步进逐个**立即收集**（注册序），流结束 = 生成完成；
    /// `Err` 项 = 生成器后续步抛错（`fail_fiber`，失败前已收集 disposer 保留）。
    /// 驱动期间 fiber epoch 变化 → **中途取消**（停止后续收集，已收集保留）。
    Stream(LocalBoxStream<'static, GenItem>),
}

/// 异步生成器 effect 的单步产出（A6）：`Ok(disposer)` = 一个 disposer；
/// 流结束（`None`） = 生成完成；`Err` = 生成器抛错。
pub type GenItem = Result<Disposer, CordisError>;

/// effect 主体：`FnOnce(&Cordis) -> Result<EffectOutcome, CordisError>`。
/// 在「无借用」上下文中运行，可重入调用其它门面方法。
pub type EffectBody = Box<dyn FnOnce(&Cordis) -> Result<EffectOutcome, CordisError>>;

/// 单个插件运行实例的数据。
pub struct FiberData {
    pub id: FiberId,
    /// `None` 表示已 dispose（等价 Cordis `uid === null`）。
    pub uid: Option<u64>,
    /// 父 fiber（从哪个 fiber 加载）。
    pub parent: Option<FiberId>,
    /// 插件注册键（M0 = 插件名）。
    pub runtime: Option<String>,
    /// 显示名。
    pub name: Option<String>,
    /// 所属 loader entry（M2 loader 关联；沿 parent 链继承）。
    pub entry: Option<String>,
    /// 服务隔离映射（服务名 → 作用域标签；继承 parent + 自身覆盖）。
    /// 对应 Cordis `ctx[Context.isolate]`。
    pub isolate: HashMap<String, ScopeId>,
    pub state: FiberState,
    /// M27：apply 返回 `Await`——finish 前等待所有 Loading 后代完成
    /// （等价 Cordis `[Service.init]` await 子任务；Group 挂载子入口用）。
    pub await_children: bool,
    /// 依赖的服务名。
    pub inject: Vec<String>,
    /// 已解析的依赖：服务名 → 实现 id。
    pub store: HashMap<String, ImplId>,
    /// 本 fiber 的 intercept 配置（服务名 → 配置；注册顺序，同名后者覆盖）。
    /// 等价 Cordis `ctx[Context.intercept]` 中本 fiber 自己的 own entries。
    pub intercept: Vec<(String, Value)>,
    /// 已注册的 disposer（注册顺序；卸载时逆序运行）。
    pub disposers: Vec<Disposer>,
    /// 异步 disposer（M7；注册顺序；`unload_async` 并行执行）。
    pub async_disposers: Vec<LocalBoxFuture<'static, EffectOutcome>>,
    /// 校验后的配置。
    pub config: Value,
    pub error: Option<CordisError>,
    /// `None` = INACTIVE；`Some(joined uids)` = 依赖齐备的 epoch。
    pub epoch: Option<String>,
    /// fiber 所属作用域（M0 恒为根作用域）。
    pub scope: ScopeId,
    /// 已注册 effect 的元数据（M66；注册序，等价 Cordis `fiber.getEffects()`）。
    /// 当前仅记录 label；`children` 恒空（dsh-core 无 effect 父子结构，树形为
    /// 自觉边界）。卸载时清空。
    pub effects: Vec<EffectMeta>,
}

/// effect 元数据（对应 Cordis `EffectMeta`：label + children 树）。
#[derive(Debug, Clone, PartialEq)]
pub struct EffectMeta {
    /// 语义标签（当前为注册时的 `&'static str`，如 "plugin-apply"；
    /// `ctx.on`→"ctx.on('ev')" 形式的精确标签为后续增强）。
    pub label: String,
    /// 子 effect（当前恒空）。
    pub children: Vec<EffectMeta>,
}

impl FiberData {
    /// 收集 effect 产出，注册到本 fiber，返回可共享的幂等 disposer。
    ///
    /// 对应 Cordis `Fiber.effect()` 的收集/包装部分：产出按注册顺序保存，
    /// 运行时逆序执行；包装器带一次性标志，fiber 卸载与调用方共享同一实例。
    ///
    /// `EffectOutcome::Async` 被存入 `async_disposers`（同步 wrapper 不含该部分，
    /// 由 `unload_async` 并行执行）。
    pub fn collect_effect(&mut self, label: &'static str, outcome: EffectOutcome) -> Disposer {
        // M66：记录 effect 元数据（注册序，含 async/await 去向）。
        self.effects.push(EffectMeta { label: label.to_string(), children: Vec::new() });
        let mut collected: Vec<Disposer> = Vec::new();
        match outcome {
            EffectOutcome::None => {}
            EffectOutcome::One(d) => collected.push(d),
            EffectOutcome::Many(ds) => collected.extend(ds),
            EffectOutcome::Async(fut) => {
                self.async_disposers.push(fut);
                return make_disposer(Box::new(|_| {}));
            }
            // apply 期间的 Await 由 `drive_async_loads` 先 await 再 collect（不直接出现于此）。
            EffectOutcome::Await(_) => {
                return make_disposer(Box::new(|_| {}));
            }
            // A6：Stream 由驱动方在 apply 期间逐项 `push_gen_disposer`（不在此整批收集）；
            // 此臂仅保编译（防止未经驱动直接 collect）。
            EffectOutcome::Stream(_) => {
                return make_disposer(Box::new(|_| {}));
            }
        }
        let ran = Rc::new(Cell::new(false));
        let _label = label;
        let wrapper: Disposer = Rc::new(move |ctx| {
            if ran.get() {
                return;
            }
            ran.set(true);
            for d in collected.iter().rev() {
                d(ctx);
            }
        });
        self.disposers.push(wrapper.clone());
        wrapper
    }

    /// 是否仍可注册新 effect（Cordis `assertActive`）。
    pub fn is_active(&self) -> bool {
        self.uid.is_some() && self.state != FiberState::Unloading
    }

    /// A6：逐项注册生成器产出的 disposer（**立即**、注册序），等价 cordis
    /// `_execute` async-iterator 分支的 `safeCollect`（每步收集即时发生）。
    /// 卸载时 `take_disposers` 逆序执行，失败/中途取消前已收集项保留。
    pub fn push_gen_disposer(&mut self, d: Disposer) {
        self.effects
            .push(EffectMeta { label: "gen-item".to_string(), children: Vec::new() });
        self.disposers.push(d);
    }

    /// 卸载：取出全部 disposer（注册顺序），由调用方逆序执行。
    pub fn take_disposers(&mut self) -> Vec<Disposer> {
        std::mem::take(&mut self.disposers)
    }

    /// 取出异步 disposer（M7；`unload_async` 用）。
    pub fn take_async_disposers(&mut self) -> Vec<LocalBoxFuture<'static, EffectOutcome>> {
        std::mem::take(&mut self.async_disposers)
    }
}

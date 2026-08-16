//! Fiber 状态机、effect 与 disposer（对应 PLAN §1.4）。

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

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
}

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
    /// 依赖的服务名。
    pub inject: Vec<String>,
    /// 已解析的依赖：服务名 → 实现 id。
    pub store: HashMap<String, ImplId>,
    /// 本 fiber 的 intercept 配置（服务名 → 配置；注册顺序，同名后者覆盖）。
    /// 等价 Cordis `ctx[Context.intercept]` 中本 fiber 自己的 own entries。
    pub intercept: Vec<(String, Value)>,
    /// 已注册的 disposer（注册顺序；卸载时逆序运行）。
    pub disposers: Vec<Disposer>,
    /// 校验后的配置。
    pub config: Value,
    pub error: Option<CordisError>,
    /// `None` = INACTIVE；`Some(joined uids)` = 依赖齐备的 epoch。
    pub epoch: Option<String>,
    /// fiber 所属作用域（M0 恒为根作用域）。
    pub scope: ScopeId,
}

impl FiberData {
    /// 收集 effect 产出，注册到本 fiber，返回可共享的幂等 disposer。
    ///
    /// 对应 Cordis `Fiber.effect()` 的收集/包装部分：产出按注册顺序保存，
    /// 运行时逆序执行；包装器带一次性标志，fiber 卸载与调用方共享同一实例。
    pub fn collect_effect(&mut self, label: &'static str, outcome: EffectOutcome) -> Disposer {
        let mut collected: Vec<Disposer> = Vec::new();
        match outcome {
            EffectOutcome::None => {}
            EffectOutcome::One(d) => collected.push(d),
            EffectOutcome::Many(ds) => collected.extend(ds),
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

    /// 卸载：取出全部 disposer（注册顺序），由调用方逆序执行。
    pub fn take_disposers(&mut self) -> Vec<Disposer> {
        std::mem::take(&mut self.disposers)
    }
}

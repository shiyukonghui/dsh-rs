//! `dsh-scope`：作用域注册原语（key-agnostic）。
//!
//! 权威参考：`deepseek-harness/packages/core/scope`（`@deepseek-ai/dsh-scope`）。
//! M2a 迁移（见 `PLAN-rust-full-harness-migration.md` §6 M2 范围 core/scope）。
//!
//! 第一性原理：
//! - 本包**与具体领域无关**：只做「打标签 / 读标签 / 路由事件」三件事。不定义
//!   `AgentScope`/`ScopedMessage`/`Agent` 等 agent 语义——那是消费方（dsh-agent）
//!   用本包原语组合出来的。本包不是沙箱/权限边界（README 明确）。
//! - **无任何 JSON 序列化**：`ScopeKey` 是对象（引用相等）；`ScopeCarrier` 只做
//!   路由。Rust 侧映射：`ScopeKey(Rc<()>)` 不透明身份句柄（`Eq`/`Hash` 按指针），
//!   永不跨进程、从不落盘。
//! - 父子链一条关系双向驱动：**向下继承**（子作用域注册视图能看祖先 layer，
//!   `ScopedLayers.chainLayers`/`merge`）与**向上准入**（事件只沿链上行、绝不下行，
//!   `ScopeCarrier::adopts`）。无标签监听者全局可见。
//!
//! 与 TS 的差异（记录于 DECISIONS.md D-023）：
//! - `create_scope` 的 `dispose` 为**同步幂等**（memo 单发 + 逆序跑 disposers），
//!   不等价 Cordis 的异步 quiescence（`fiber.inertia` 反复排空）；单线程核心无
//!   异步 disposer，故取同步语义。`raw_dispose` 仍暴露为精确 disposer 身份。
//! - `ScopeCarrier` 是类型化结构而非 `unknown` 上的 WeakMap 查询：`is_scope_carrier`
//!   由类型系统保证（无需运行时判定函数）；base filter 以闭包提供（TS 是
//!   `base[cordis.filter]` 方法，`this=base`——Rust 用捕获 base 的闭包等价）。
//! - `Scoped<T>` phantom brand 由类型擦除（无运行时行为），Rust 不强加。
//! - 迷你 `ScopedBus`（`ScopedContext`）复刻 Cordis 派发的**最小路由语义**
//!   （global 监听者绕过 filter；带标签者 `adopts` 判定），供消费方（dsh-agent 等）
//!   在 Rust 核心内做作用域事件派发——与 D-015「最小观察者表」同构，不引入 Cordis
//!   内核改造。

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

pub mod invariant;
pub mod store;

pub use invariant::{
    check_scoped_dispatch, mismatched_subject_message, no_carrier_message,
    scoped_subject_resolver_for, SubjectResolver, PACKAGE_NAME, SCOPED_EVENTS,
};
pub use store::{AnonymousEntries, NamedEntries, ScopeLayer, ScopedLayers, Undo};

// ---------------------------------------------------------------------------
// ScopeKey：不透明身份句柄
// ---------------------------------------------------------------------------

/// 不透明作用域键：身份比较（指针相等）。对齐 TS `ScopeKey = object`。
/// 不序列化、不可克隆语义；`create_scope`/`scope_target`/`ScopedLayers` 用它做键。
#[derive(Clone)]
pub struct ScopeKey(Rc<()>);

impl ScopeKey {
    /// 铸造一枚全新作用域键（幂等身份）：任何时候都不与其它键相等。
    pub fn new() -> Self {
        ScopeKey(Rc::new(()))
    }

    fn ptr(&self) -> *const () {
        Rc::as_ptr(&self.0)
    }
}

impl Default for ScopeKey {
    fn default() -> Self {
        Self::new()
    }
}

impl PartialEq for ScopeKey {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.ptr(), other.ptr())
    }
}
impl Eq for ScopeKey {}
impl Hash for ScopeKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (self.ptr() as usize).hash(state);
    }
}
impl std::fmt::Debug for ScopeKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ScopeKey({:#x})", self.ptr() as usize)
    }
}

// ---------------------------------------------------------------------------
// 父链（模块级状态：对齐 TS 模块级 WeakMap `scopeParents`）
// ---------------------------------------------------------------------------

/// 模块级作用域状态（thread_local：跨测试线程隔离；单线程内与 TS 的模块级
/// WeakMap 语义一致——键被创建后即登记，随键析构自然释放）。
#[derive(Default)]
struct ScopeState {
    parents: HashMap<ScopeKey, ScopeKey>,
}

thread_local! {
    static SCOPE_STATE: RefCell<ScopeState> = RefCell::new(ScopeState::default());
}

fn with_parents<R>(f: impl FnOnce(&mut HashMap<ScopeKey, ScopeKey>) -> R) -> R {
    SCOPE_STATE.with(|s| f(&mut s.borrow_mut().parents))
}

/// 在 `key → parent` 上链接父关系（含环检测）。
/// 从 `parent` 起沿父链向上，任一 cursor === key → 环，拒绝。
fn link_scope_parent(key: &ScopeKey, parent: &ScopeKey) -> Result<(), String> {
    with_parents(|parents| {
        let mut cursor = parent.clone();
        loop {
            if &cursor == key {
                return Err("dsh-scope: scope parent link would form a cycle".to_string());
            }
            match parents.get(&cursor).cloned() {
                Some(p) => cursor = p,
                None => break,
            }
        }
        parents.insert(key.clone(), parent.clone());
        Ok(())
    })
}

/// 重新链接句柄：只有 `bind_scope_parent` 返回值能 rebind（对齐 TS
/// `ScopeParentBinding`）。
#[derive(Debug)]
pub struct ScopeParentBinding {
    key: ScopeKey,
}

impl ScopeParentBinding {
    /// 把持有的键重新绑定到新父（自带同样的环检测）。
    pub fn rebind(&self, parent: ScopeKey) -> Result<(), String> {
        link_scope_parent(&self.key, &parent)
    }
}

/// 绑定 `key` 的父为 `parent`。每个键只能绑定一次；重绑必须用原绑定句柄。
pub fn bind_scope_parent(key: ScopeKey, parent: ScopeKey) -> Result<ScopeParentBinding, String> {
    let already = with_parents(|p| p.contains_key(&key));
    if already {
        return Err(
            "dsh-scope: scope key is already bound to a parent; re-linking requires the binding returned by the original bind"
                .to_string(),
        );
    }
    link_scope_parent(&key, &parent)?;
    Ok(ScopeParentBinding { key })
}

/// 读 `key` 的父（root → None）。只读。
pub fn scope_parent_of(key: &ScopeKey) -> Option<ScopeKey> {
    with_parents(|p| p.get(key).cloned())
}

/// 从 key 起沿父链走到 root，**近者优先**：`[key, parent, grandparent, …]`；
/// `None` → 空。
pub fn scope_chain_of(key: Option<&ScopeKey>) -> Vec<ScopeKey> {
    let Some(mut cursor) = key.cloned() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    loop {
        out.push(cursor.clone());
        match with_parents(|p| p.get(&cursor).cloned()) {
            Some(parent) => cursor = parent,
            None => break,
        }
    }
    out
}

// ---------------------------------------------------------------------------
// scope_target：只做路由的 carrier（对齐 TS `scopeTarget`/`isScopeCarrier`）
// ---------------------------------------------------------------------------

/// base filter（对齐 TS `base[cordis.filter]`，以 base 为 `this` 调用）：
/// Rust 用捕获 base 的闭包表达；返回 `true` = 放行 base 层面。
pub type BaseFilter = Rc<dyn Fn() -> bool>;

/// 铸造出现成可以过滤监听者的 carrier：
/// `tags: Option<ScopeKey>` 为路由键；`base_filter` 缺省 = 放行（无自定义 filter）。
pub fn scope_target(tag: Option<ScopeKey>, base_filter: Option<BaseFilter>) -> ScopeCarrier {
    ScopeCarrier::new(tag, base_filter)
}

/// 一个 scope-filtered 事件的派发 carrier（对齐 TS `scopeTarget` 的返回值）。
/// 只做路由：真实 subject 只能经事件参数拿到，carrier 自身不暴露 subject 属性。
#[derive(Clone)]
pub struct ScopeCarrier {
    key: Option<ScopeKey>,
    base_filter: Option<BaseFilter>,
}

impl ScopeCarrier {
    pub fn new(key: Option<ScopeKey>, base_filter: Option<BaseFilter>) -> Self {
        ScopeCarrier { key, base_filter }
    }

    /// 路由键（`None` = 无 key carrier：只接纳无标签监听者）。
    pub fn key(&self) -> Option<&ScopeKey> {
        self.key.as_ref()
    }

    /// 由 `carrierKeys` 语义读回：carrier 若非空则返回其键。
    pub fn carrier_key_of(&self) -> Option<&ScopeKey> {
        self.key()
    }

    /// **路由谓词**（对齐 TS `scopeTarget` 的 filter）：
    /// - 先 base filter（以 base 为 this；`false` → 不接纳）；
    /// - 无标签监听者（`tag == None`）恒接纳（全局）；
    /// - 带标签者：`tag ∈ chain(this.key)`（key 自身及其祖先）→ 接纳。
    ///
    /// 事件沿链上行、绝不下行；`key == None` 时链为空 → 只接纳无标签者。
    pub fn adopts(&self, tag: Option<&ScopeKey>) -> bool {
        if let Some(bf) = &self.base_filter {
            if !bf() {
                return false;
            }
        }
        match tag {
            None => true,
            Some(tag) => scope_chain_of(self.key.as_ref()).iter().any(|k| k == tag),
        }
    }
}

// ---------------------------------------------------------------------------
// createScope：带标签上下文 + 生命周期（按作用域）
// ---------------------------------------------------------------------------

/// `CreateScopeOptions`：可选父键。
#[derive(Default)]
pub struct CreateScopeOptions {
    pub parent: Option<ScopeKey>,
}

/// 一个作用域内注册的拆除器句柄（`Rc<dyn Fn()>` 的共享别名）。
pub type ScopeDisposer = Rc<dyn Fn()>;

/// 作用域生命周期句柄（对齐 TS `Scope`）。
pub struct Scope {
    /// 带标签的上下文：经它注册的监听者/d disposers 按作用域可见与管理。
    pub ctx: ScopedContext,
    /// 底层精确 disposer（for 有序复合拆除；见 scope.spec 测试 4）。
    raw_dispose: Rc<dyn Fn()>,
    /// dispose 的幂等 memo：首次调用后共享同一次拆除完成。
    disposing: Rc<Cell<bool>>,
    /// 已经注册到本 scope 的 disposers（逆序拆除）。
    disposers: ScopeDisposerList,
}

/// 逆序拆除列表（类型别名避免过深嵌套类型）。
type ScopeDisposerList = Rc<RefCell<Vec<ScopeDisposer>>>;

impl Scope {
    /// 幂等拆除：首次调用跑 `raw_dispose` + 逆序跑已注册 disposers；之后 no-op。
    pub fn dispose(&self) {
        if self.disposing.replace(true) {
            return;
        }
        // 逆序（对齐 Cordis effect disposers 逆序执行）
        let items = {
            let mut v = self.disposers.borrow_mut();
            std::mem::take(&mut *v)
        };
        for d in items.iter().rev() {
            d();
        }
        (self.raw_dispose)();
    }

    /// 向本 scope 注册一个拆除器（scope 拆除时逆序执行）。
    pub fn on_dispose(&self, disposer: impl Fn() + 'static) {
        if self.disposing.get() {
            return; // 已拆完毕，拒绝注册（对齐 Cordis INACTIVE_EFFECT 语义）
        }
        self.disposers.borrow_mut().push(Rc::new(disposer));
    }

    /// 作用域上下文引用。
    pub fn ctx(&self) -> &ScopedContext {
        &self.ctx
    }
}

/// 铸造一个带 `key` 标签的上下文（对齐 TS `createScope`）。
/// - 若 `options.parent` 指定 → `bind_scope_parent`（此时尚未创建任何状态；
///   已绑定会抛错，事务性）。
/// - 返回的 `Scope.ctx` 带标签；`dispose` 幂等。
pub fn create_scope(
    base: &ScopedContext,
    key: ScopeKey,
    options: CreateScopeOptions,
) -> Result<Scope, String> {
    if let Some(parent) = options.parent {
        bind_scope_parent(key.clone(), parent)?;
    }
    let ctx = base.extend_tagged(key.clone());
    // 同步可用：无「fiber 激活前」门控——本实现无异步启用。
    Ok(Scope {
        ctx,
        raw_dispose: Rc::new(|| {}),
        disposing: Rc::new(Cell::new(false)),
        disposers: Rc::new(RefCell::new(Vec::new())),
    })
}

// ---------------------------------------------------------------------------
// ScopedContext：迷你作用域派发模型
// ---------------------------------------------------------------------------

/// 一条总线监听记录。
struct BusItem {
    name: String,
    global: bool,
    tag: Option<ScopeKey>,
    cb: Listener,
}

/// 监听器（迷你总线带名派发）：`(event_name, args)`。
pub type Listener = Box<dyn Fn(&str, &[serde_json::Value])>;

/// 带标签（可选）的上下文：`extend` 继承标签；`on` 注册（按 tag/global 路由）；
/// `emit` 经 carrier 做作用域过滤。
#[derive(Default)]
pub struct ScopedContext {
    tag: Option<ScopeKey>,
    bus: Rc<RefCell<Vec<BusItem>>>,
}

impl Clone for ScopedContext {
    fn clone(&self) -> Self {
        ScopedContext {
            tag: self.tag.clone(),
            bus: self.bus.clone(),
        }
    }
}

impl ScopedContext {
    /// 无标签上下文（全局注册本就走 global 路径）。
    pub fn new() -> Self {
        ScopedContext::default()
    }

    /// 当前上下文的标签（`None` = context-global）。
    pub fn scope_of(&self) -> Option<&ScopeKey> {
        self.tag.as_ref()
    }

    /// 派生上下文：继承（同一）标签（对齐 Cordis `ctx.extend` 继承 Symbol 标签）。
    pub fn extend(&self) -> ScopedContext {
        self.clone()
    }

    /// 内部：派生一个带**新标签**的上下文（create_scope 用；嵌套时最近标签胜——
    /// 因为新 context 直接带新 key）。
    fn extend_tagged(&self, tag: ScopeKey) -> ScopedContext {
        ScopedContext {
            tag: Some(tag),
            bus: self.bus.clone(),
        }
    }

    /// 注册监听器。`global=true` → 永远全局可见（绕过 filter，对齐
    /// `{global:true}` 语义）；`global=false` 且上下文带标签 → 该标签的 scope
    /// 监听者；无标签上下文注册的监听者天然全局。
    pub fn on(&self, name: &str, global: bool, cb: Listener) {
        self.bus.borrow_mut().push(BusItem {
            name: name.to_string(),
            global: global || self.tag.is_none(),
            tag: self.tag.clone(),
            cb,
        });
    }

    /// 无 carrier 派发（普通事件：所有匹配名的监听者都收到——无过滤）。
    pub fn emit(&self, name: &str, args: Vec<serde_json::Value>) {
        let items = self.bus.borrow();
        for item in items.iter() {
            if item.name == name {
                (item.cb)(name, &args);
            }
        }
    }

    /// 经 carrier 派发：只有 `global || carrier.adopts(item.tag)` 的监听者收到
    /// （对齐 `ctx.emit(thisArg, name, …)` 的 filter 语义）。
    pub fn emit_scoped(&self, carrier: &ScopeCarrier, name: &str, args: Vec<serde_json::Value>) {
        let items = self.bus.borrow();
        for item in items.iter() {
            if item.name != name {
                continue;
            }
            if item.global || carrier.adopts(item.tag.as_ref()) {
                (item.cb)(name, &args);
            }
        }
    }

    /// 当前监听者数（诊断）。
    pub fn listener_count(&self) -> usize {
        self.bus.borrow().len()
    }
}

/// 便捷：`Scope` 的上下文之后续传入（对齐 TS `scope.ctx`）。
impl Scope {
    /// 当前上下文标签（若无则 None）。
    pub fn scope_key(&self) -> Option<&ScopeKey> {
        self.ctx.scope_of()
    }
}

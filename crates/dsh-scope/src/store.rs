//! 可复用「按作用域 registry」存储（对齐 TS `packages/core/scope/src/store.ts`）。
//!
//! - `NamedEntries<V>`：命名条目表（插入序迭代 + live 迭代 + 表清空换新代 +
//!   精确幂等 undo + 调用方自有重复诊断）。
//! - `AnonymousEntries<V>`：匿名条目表（每次 append 唯一键；相等值独立条目）。
//! - `ScopedLayers<L>`：全局/精确作用域 layer 聚合（惰性创建、只回收空聚合、
//!   远→近 chain 合并、effect 集成 + 通知/回滚语义）。

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use crate::{scope_chain_of, ScopeKey};

// ---------------------------------------------------------------------------
// 共享有序表（插入序 + 代数）
// ---------------------------------------------------------------------------

/// 可迭代表的共享主体：`items` 按插入序；`gen` 在表清空换新代时递增。
struct Table<K, V> {
    items: Vec<(K, V)>,
    gen: u64,
}

impl<K, V> Table<K, V> {
    fn new() -> Self {
        Table { items: Vec::new(), gen: 0 }
    }
}

type Shared<K, V> = Rc<RefCell<Table<K, V>>>;

/// 精确幂等的 undo 闭包（对齐 TS：首次删除 + `active=false`；之后 no-op）。
pub type Undo = Rc<dyn Fn()>;

fn make_undo<K: Clone + PartialEq + 'static, V: 'static>(
    shared: &Shared<K, V>,
    key: K,
) -> Undo {
    let shared = shared.clone();
    let active = Rc::new(Cell::new(true));
    Rc::new(move || {
        if !active.replace(false) {
            return;
        }
        let mut t = shared.borrow_mut();
        if let Some(i) = t.items.iter().position(|(k, _)| *k == key) {
            t.items.remove(i);
        }
        if t.items.is_empty() {
            // 表清空 → 换新代（旧迭代器被脱离；后续插入进新代）
            t.gen += 1;
        }
    })
}

// ---------------------------------------------------------------------------
// NamedEntries
// ---------------------------------------------------------------------------

/// 命名条目表（`Map<string, V>` 的 Rust 形态）。
pub struct NamedEntries<V> {
    data: Shared<String, V>,
    duplicate_error: Rc<dyn Fn(&str) -> String>,
}

impl<V: 'static> NamedEntries<V> {
    /// `duplicate_error(name)`：调用方自有重复诊断（本类不造错误）。
    pub fn new(duplicate_error: impl Fn(&str) -> String + 'static) -> Self {
        NamedEntries {
            data: Rc::new(RefCell::new(Table::new())),
            duplicate_error: Rc::new(duplicate_error),
        }
    }

    /// 插入命名条目。已有同名 → Err(调用方重复诊断)。返回精确幂等 undo。
    pub fn insert(&self, name: &str, value: V) -> Result<Undo, String> {
        let mut t = self.data.borrow_mut();
        if t.items.iter().any(|(k, _)| k == name) {
            return Err((self.duplicate_error)(name));
        }
        t.items.push((name.to_string(), value));
        drop(t);
        Ok(make_undo(&self.data, name.to_string()))
    }

    pub fn get(&self, name: &str) -> Option<V>
    where
        V: Clone,
    {
        let t = self.data.borrow();
        t.items
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    }

    pub fn has(&self, name: &str) -> bool {
        let t = self.data.borrow();
        t.items.iter().any(|(k, _)| k == name)
    }

    pub fn is_empty(&self) -> bool {
        let t = self.data.borrow();
        t.items.is_empty()
    }

    /// 命名字符串（插入序）。
    pub fn keys(&self) -> Vec<String> {
        let t = self.data.borrow();
        t.items.iter().map(|(k, _)| k.clone()).collect()
    }

    /// 条目对（插入序）。
    pub fn entries(&self) -> Vec<(String, V)>
    where
        V: Clone,
    {
        let t = self.data.borrow();
        t.items.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }

    /// live 迭代器（同代内 live——此后插入可见；表清空换新代后脱离）。
    pub fn values(&self) -> IterKeyed<V> {
        IterKeyed {
            data: self.data.clone(),
            gen: self.data.borrow().gen,
            cursor: 0,
        }
    }

    /// live 迭代器（同 `values` 语义），但每步返回 `(key, value)` 对（对齐 TS
    /// `Map.entries()` 的 live 迭代：消费方在迭代期间新注册的条目本轮可见）。
    pub fn entries_live(&self) -> IterKeyedPair<String, V> {
        IterKeyedPair {
            data: self.data.clone(),
            gen: self.data.borrow().gen,
            cursor: 0,
        }
    }
}

/// 命名条目 live 迭代器（对齐 TS Map 迭代器）。
pub struct IterKeyed<V> {
    data: Shared<String, V>,
    gen: u64,
    cursor: usize,
}

impl<V> Iterator for IterKeyed<V>
where
    V: Clone,
{
    type Item = V;
    fn next(&mut self) -> Option<V> {
        let t = self.data.borrow();
        if t.gen != self.gen {
            return None; // 已换新代 → 脱离
        }
        if self.cursor >= t.items.len() {
            return None;
        }
        let v = t.items[self.cursor].1.clone();
        self.cursor += 1;
        Some(v)
    }
}

/// `entries_live` 的 `(key, value)` live 迭代器（对齐 TS `Map.entries`）。
pub struct IterKeyedPair<K, V> {
    data: Shared<K, V>,
    gen: u64,
    cursor: usize,
}

impl<K, V> Iterator for IterKeyedPair<K, V>
where
    K: Clone,
    V: Clone,
{
    type Item = (K, V);
    fn next(&mut self) -> Option<(K, V)> {
        let t = self.data.borrow();
        if t.gen != self.gen {
            return None; // 已换新代 → 脱离
        }
        if self.cursor >= t.items.len() {
            return None;
        }
        let pair = (
            t.items[self.cursor].0.clone(),
            t.items[self.cursor].1.clone(),
        );
        self.cursor += 1;
        Some(pair)
    }
}

// ---------------------------------------------------------------------------
// AnonymousEntries
// ---------------------------------------------------------------------------

thread_local! {
    static ANON_UID: Cell<u64> = const { Cell::new(0) };
}

fn anon_uid() -> u64 {
    ANON_UID.with(|c| {
        let n = c.get() + 1;
        c.set(n);
        n
    })
}

/// 匿名条目表（`Map<symbol, V>` 的 Rust 形态：唯一 id 键，相等值独立条目）。
pub struct AnonymousEntries<V> {
    data: Shared<u64, V>,
}

impl<V: 'static> AnonymousEntries<V> {
    pub fn new() -> Self {
        AnonymousEntries { data: Rc::new(RefCell::new(Table::new())) }
    }

    /// 追加匿名条目，返回精确幂等 undo。每次新建唯一 id（相等值独立注册）。
    pub fn append(&self, value: V) -> Undo {
        let uid = anon_uid();
        let mut t = self.data.borrow_mut();
        t.items.push((uid, value));
        drop(t);
        make_undo(&self.data, uid)
    }

    pub fn is_empty(&self) -> bool {
        let t = self.data.borrow();
        t.items.is_empty()
    }

    /// live 迭代器（同 NamedEntries.values 语义）。
    pub fn values(&self) -> IterAnon<V> {
        IterAnon {
            data: self.data.clone(),
            gen: self.data.borrow().gen,
            cursor: 0,
        }
    }
}

impl<V: 'static> Default for AnonymousEntries<V> {
    fn default() -> Self {
        Self::new()
    }
}

/// 匿名条目 live 迭代器。
pub struct IterAnon<V> {
    data: Shared<u64, V>,
    gen: u64,
    cursor: usize,
}

impl<V> Iterator for IterAnon<V>
where
    V: Clone,
{
    type Item = V;
    fn next(&mut self) -> Option<V> {
        let t = self.data.borrow();
        if t.gen != self.gen {
            return None;
        }
        if self.cursor >= t.items.len() {
            return None;
        }
        let v = t.items[self.cursor].1.clone();
        self.cursor += 1;
        Some(v)
    }
}

// ---------------------------------------------------------------------------
// ScopedLayers
// ---------------------------------------------------------------------------

/// 一个 scope 对某 registry 的完整聚合贡献（其 `isEmpty` 控制精确层回收）。
pub trait ScopeLayer {
    fn is_empty(&self) -> bool;
}

type LayerCreate<L> = Rc<dyn Fn(Option<&ScopeKey>) -> L>;
type ChangeNotify = Rc<dyn Fn()>;

/// 全局/精确作用域 layer 聚合。
pub struct ScopedLayers<L> {
    global: L,
    scoped: Rc<RefCell<HashMap<ScopeKey, Rc<L>>>>,
    create_layer: LayerCreate<L>,
    on_change: ChangeNotify,
}

impl<L: 'static> ScopedLayers<L> {
    /// 构造：**贪婪创建**全局层（`create_layer(None)` 立即执行一次）。
    pub fn new(
        create_layer: impl Fn(Option<&ScopeKey>) -> L + 'static,
        on_change: impl Fn() + 'static,
    ) -> Self {
        let global = create_layer(None);
        ScopedLayers {
            global,
            scoped: Rc::new(RefCell::new(HashMap::new())),
            create_layer: Rc::new(create_layer),
            on_change: Rc::new(on_change),
        }
    }

    pub fn global(&self) -> &L {
        &self.global
    }

    /// 精确层查询：**绝不创建**、**chain-blind**（不静默取祖先层）。
    pub fn peek(&self, scope: Option<&ScopeKey>) -> Option<Rc<L>> {
        let key = scope?;
        self.scoped.borrow().get(key).cloned()
    }

    /// 远→近祖先层链（farthest-first，exact-scope-last），跳过不存在的层。
    pub fn chain_layers(&self, scope: Option<&ScopeKey>) -> Vec<Rc<L>> {
        let chain = scope_chain_of(scope);
        let scoped = self.scoped.borrow();
        chain
            .iter()
            .rev()
            .filter_map(|k| scoped.get(k).cloned())
            .collect::<Vec<_>>()
    }

    /// 合并命名条目：全局基 + 按远→近覆盖；**最近作用域赢名字**，覆盖既有 key
    /// 不移动其插入位置（对齐 TS Map 语义）。
    pub fn merge<V>(
        &self,
        scope: Option<&ScopeKey>,
        pick: &dyn Fn(&L) -> Vec<(String, V)>,
    ) -> Vec<(String, V)>
    where
        V: Clone,
    {
        let mut merged: Vec<(String, V)> = pick(&self.global);
        for layer in self.chain_layers(scope) {
            for (name, value) in pick(&layer) {
                match merged.iter_mut().find(|(n, _)| *n == name) {
                    Some(slot) => slot.1 = value,
                    None => merged.push((name, value)),
                }
            }
        }
        merged
    }

    /// effect 集成：在（可选）作用域上运行 `action` 收集 undo，同步通知；返回
    /// **精确** disposer（undo → 回收空精确层 → 通知）。
    /// - `action` 失败：仅回收「本 effect 新建且当前为空」的精确层；既有层保留；
    ///   错误重抛。
    /// - 通知失败：先跑已收集 disposer（含回收）再重抛（对齐 Cordis effect，
    ///   事件序 `['notify','undo','notify']`）。
    ///
    /// 差异：Rust 的 disposer 以 `Rc` 克隆共享同一目标（可观察为同一 disposer
    /// 引用由调用方保管；无「精确同一函数 identity」概念）。
    pub fn effect(
        &self,
        scope: Option<&ScopeKey>,
        action: impl Fn(&L) -> Undo,
        _label: &'static str,
        notify: bool,
    ) -> Undo
    where
        L: ScopeLayer,
    {
        // 目标：全局层 or 精确层（惰性创建）
        let target_key: Option<ScopeKey> = scope.cloned();
        let created;
        if let Some(key) = &target_key {
            let mut scoped = self.scoped.borrow_mut();
            if !scoped.contains_key(key) {
                scoped.insert(key.clone(), Rc::new((self.create_layer)(scope)));
                created = true;
            } else {
                created = false;
            }
        } else {
            created = false;
        }

        // 执行 action（对目标层）
        let undo: Undo = match &target_key {
            None => {
                let layer = &self.global;
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| action(layer))) {
                    Ok(u) => u,
                    Err(payload) => std::panic::resume_unwind(payload),
                }
            }
            Some(key) => {
                let layer = self
                    .scoped
                    .borrow()
                    .get(key)
                    .cloned()
                    .expect("scoped layer present");
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| action(&layer))) {
                    Ok(u) => u,
                    Err(payload) => {
                        // action 失败：新建且空 → 回收；重抛
                        if created {
                            let empty = self
                                .scoped
                                .borrow()
                                .get(&target_key.clone().unwrap())
                                .map(|l| l.is_empty())
                                .unwrap_or(true);
                            if empty {
                                self.scoped.borrow_mut().remove(key);
                            }
                        }
                        std::panic::resume_unwind(payload);
                    }
                }
            }
        };

        // 同步通知一次（缺省开）——事件序 [action, notify]
        if notify {
            let notified = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                (self.on_change)();
            }));
            if let Err(payload) = notified {
                // 通知失败 → 先跑已收集 disposer（含回收空层）再重抛
                // 事件序 [notify, undo, notify]
                let undo_rollback = undo.clone();
                let data = self.scoped.clone();
                let key = target_key.clone();
                let notify2 = self.on_change.clone();
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    undo_rollback();
                    if let Some(k) = &key {
                        let empty = data.borrow().get(k).map(|l| l.is_empty()).unwrap_or(false);
                        if empty {
                            data.borrow_mut().remove(k);
                        }
                    }
                    notify2();
                }));
                std::panic::resume_unwind(payload);
            }
        }

        // 精确 disposer：undo → 回收空精确层 → 通知；**幂等单发**（对齐 Cordis
        // disposer 第二/再次调用 no-op）。
        let disposed = Rc::new(Cell::new(true));
        let undo_final = undo;
        let data = self.scoped.clone();
        let key = target_key;
        let on_change = self.on_change.clone();
        Rc::new(move || {
            if !disposed.replace(false) {
                return;
            }
            undo_final();
            if let Some(k) = &key {
                let empty = data.borrow().get(k).map(|l| l.is_empty()).unwrap_or(false);
                if empty {
                    data.borrow_mut().remove(k);
                }
            }
            if notify {
                on_change();
            }
        })
    }
}

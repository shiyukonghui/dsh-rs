//! Loader 服务与入口树（对应 PLAN §1.8）。
//!
//! 借用纪律与 dsh-core 一致：`LoaderState` 的借用绝不跨 `ctx` 调用持有
//! （用户代码/监听器可能在 `ctx` 调用内重入本状态）。

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use dsh_core::{
    AggregateError, Cordis, CordisError, EffectOutcome, FiberId, FiberState, HookResult, Listener,
    Plugin, ScopeId, Value,
};

use crate::entry::{Entry, EntryOptions};
use crate::group::EntryGroup;
use crate::identity::{PluginIdentity, PluginRecord};

const ROOT_GROUP: &str = "root";

/// allSettled 并行 create/update 的 future 结果（id, result, is_new）。
type SyncResult = (String, Result<(), CordisError>, bool);

/// Loader 运行时状态（entry 树 + 插件仓库 + 反查索引）。
#[derive(Default)]
pub struct LoaderState {
    /// loader 插件自身的 fiber（7-case case 5 判定树载体卸载）。
    pub loader_fiber: Option<FiberId>,
    /// 根组 id。
    pub root_group: String,
    /// 全部组（id → 有序子入口）。
    pub groups: HashMap<String, EntryGroup>,
    /// 组 → 拥有它的 group 入口 id（disabled 继承用）。
    pub group_owner: HashMap<String, String>,
    /// 全部入口。
    pub entries: HashMap<String, Entry>,
    /// fiber → 入口（internal/plugin 检测用）。
    pub fiber_to_entry: HashMap<FiberId, String>,
    /// 插件仓库（按名解析，等价 Cordis 的模块 import 结果缓存）。
    /// A1：记录承载「实现 + 身份 + 换代」——身份 = 实现本体（Arc token 指针），
    /// 同名同实现=同身份（幂等）、同名新实现=新身份（换代）。
    pub plugins: HashMap<String, PluginRecord>,
    /// 写回记录（持久化 no-op；测试断言写回内容）。
    pub writes: Vec<String>,
    /// 入口本地 realm：`"{entry}:{service}"` → 作用域（isolate: true）。
    pub local_realms: HashMap<String, ScopeId>,
    /// 全局 realm：label → (服务名 → 作用域)（isolate: label）。
    pub global_realms: HashMap<String, HashMap<String, ScopeId>>,
    next_group: u64,
}

impl LoaderState {
    fn new() -> Self {
        let mut state = LoaderState {
            root_group: ROOT_GROUP.to_string(),
            ..LoaderState::default()
        };
        state
            .groups
            .insert(ROOT_GROUP.to_string(), EntryGroup::default());
        state
    }

    fn alloc_group(&mut self) -> String {
        self.next_group += 1;
        format!("g{}", self.next_group)
    }
}

/// 入口快照（测试/宿主查询）。
#[derive(Debug, Clone)]
pub struct EntrySnapshot {
    pub id: String,
    pub name: String,
    pub disabled: bool,
    pub group: bool,
    pub fiber: Option<FiberId>,
}

/// Loader 插件：注册 internal/* 监听器（7-case 自处置检测 + config 写回）。
pub struct LoaderPlugin {
    pub state: Rc<RefCell<LoaderState>>,
}

/// 入口是否禁用：自身（布尔或 `!!js` 表达式求值 truthy）或沿父组链的 group 入口禁用。
fn entry_disabled(st: &LoaderState, id: &str) -> bool {
    let mut cur = id.to_string();
    loop {
        let (disabled, expr, config, parent_group) = match st.entries.get(&cur) {
            Some(e) => (
                e.options.disabled,
                e.options.disabled_expr.clone(),
                e.options.config.clone(),
                e.parent_group.clone(),
            ),
            None => return false,
        };
        if disabled {
            return true;
        }
        // `!!js` disabled 表达式（fail-closed：求值失败视为禁用）
        if let Some(expr) = expr {
            let scope = eval_scope(&config);
            let truthy = dsh_eval::evaluate(&scope, &expr)
                .map(|v| dsh_eval::truthy(&v))
                .unwrap_or(true);
            if truthy {
                return true;
            }
        }
        if parent_group == st.root_group {
            return false;
        }
        match st.group_owner.get(&parent_group) {
            Some(owner) => cur = owner.clone(),
            None => return false,
        }
    }
}

/// `!!js` 求值作用域（默认 `process` 门面 = 真实 OS 事实，D-103/P2-a）：
/// `{ config, process, ctx, env }`。
fn eval_scope(config: &Value) -> HashMap<String, Value> {
    eval_scope_with_process(config, &dsh_eval::process_facade())
}

/// `!!js` 求值作用域 + 可注入 `process` 门面（确定性测试用；host 装配真实 facade）。
pub fn eval_scope_with_process(config: &Value, process: &Value) -> HashMap<String, Value> {
    let mut scope = HashMap::new();
    scope.insert("config".to_string(), config.clone());
    scope.insert("process".to_string(), process.clone());
    scope.insert("ctx".to_string(), serde_json::json!({}));
    scope.insert("env".to_string(), serde_json::json!({}));
    scope
}

impl Plugin for LoaderPlugin {
    fn name(&self) -> &'static str {
        "loader"
    }

    fn apply(&self, ctx: &Cordis, _config: Value) -> Result<EffectOutcome, CordisError> {
        let fid = ctx.current_fiber().ok_or_else(|| {
            CordisError::Internal("loader plugin must be mounted inside a fiber".to_string())
        })?;
        {
            let mut st = self.state.borrow_mut();
            st.loader_fiber = Some(fid);
        }

        // --- internal/plugin：7-case 自处置检测 ---
        let state = self.state.clone();
        let seven_case: Listener = {
            let state = state.clone();
            Arc::new(move |ctx: &Cordis, args: &mut Vec<Value>, _next| {
                let kind = args.get(2).and_then(|a| a.as_str()).unwrap_or("");
                if kind != "dispose" {
                    return HookResult::Continue;
                }
                let fid = args.get(1).and_then(|a| a.as_u64()).unwrap_or(0);
                let st = state.borrow();
                // case 2: fiber 是否被 loader 跟踪
                let Some(entry_id) = st.fiber_to_entry.get(&fid).cloned() else {
                    return HookResult::Continue;
                };
                // case 3: 父 fiber 同属本入口（子插件 dispose，非入口根）
                let parent_entry = ctx.with(|rt| {
                    rt.fiber(fid)
                        .and_then(|f| f.parent)
                        .and_then(|p| rt.fiber(p))
                        .and_then(|pf| pf.entry.clone())
                });
                if parent_entry.as_deref() == Some(entry_id.as_str()) {
                    return HookResult::Continue;
                }
                // case 4: 插件已被 registry 删除（如 hmr）
                let runtime_key = ctx.with(|rt| rt.fiber(fid).and_then(|f| f.runtime.clone()));
                let has = runtime_key
                    .map(|k| ctx.with(|rt| rt.registry.contains_key(&k)))
                    .unwrap_or(false);
                if !has {
                    return HookResult::Continue;
                }
                // case 5: 树载体（loader fiber）正在卸载
                if let Some(lf) = st.loader_fiber {
                    if let Some(s) = ctx.fiber_state(lf) {
                        if matches!(s, FiberState::Unloading | FiberState::Disposed) {
                            return HookResult::Continue;
                        }
                    }
                }
                // case 6: loader 自己正在 dispose 本入口
                if st
                    .entries
                    .get(&entry_id)
                    .map(|e| e.disposing > 0)
                    .unwrap_or(false)
                {
                    return HookResult::Continue;
                }
                // case 7: 入口已禁用
                if entry_disabled(&st, &entry_id) {
                    return HookResult::Continue;
                }
                drop(st);
                // 自处置：标记 disabled 并写回
                let mut st = state.borrow_mut();
                if let Some(e) = st.entries.get_mut(&entry_id) {
                    e.options.disabled = true;
                }
                st.writes.push(format!("disable:{entry_id}"));
                HookResult::Continue
            })
        };
        ctx.on_with("internal/plugin", seven_case, true, false)?;

        // --- internal/update：config 写回（prepend，先委托再写回） ---
        let state = self.state.clone();
        let write_back: Listener = {
            let state = state.clone();
            Arc::new(move |ctx: &Cordis, args: &mut Vec<Value>, next| {
                let fid = args.first().and_then(|a| a.as_u64()).unwrap_or(0);
                let no_save = args.get(2).and_then(|a| a.as_bool()).unwrap_or(false);
                let result = match next {
                    Some(n) => n(ctx, args),
                    None => None,
                };
                if !no_save {
                    let mut st = state.borrow_mut();
                    if let Some(entry_id) = st.fiber_to_entry.get(&fid).cloned() {
                        if let Some(cfg) = args.get(1) {
                            if let Some(e) = st.entries.get_mut(&entry_id) {
                                e.options.config = cfg.clone();
                            }
                        }
                        st.writes.push(format!("write:{entry_id}"));
                    }
                }
                HookResult::Returned(result)
            })
        };
        ctx.on_with("internal/update", write_back, true, true)?;

        // --- internal/config：`!!js` 配置插值（global waterfall） ---
        let state = self.state.clone();
        let config_interp: Listener = {
            let state = state.clone();
            Arc::new(move |_ctx, args, next| {
                let resolved = match next {
                    Some(n) => n(_ctx, args),
                    None => args.get(1).cloned(),
                };
                let config = resolved.unwrap_or(Value::Null);
                let scope = eval_scope(&config);
                match dsh_eval::interpolate(&scope, &config) {
                    Ok(v) => HookResult::Returned(Some(v)),
                    Err(e) => {
                        // fail loud：保留原配置并在写回记录中标记
                        state.borrow_mut().writes.push(format!("eval-error:{e}"));
                        HookResult::Returned(Some(config))
                    }
                }
            })
        };
        ctx.on_with("internal/config", config_interp, true, false)?;

        Ok(EffectOutcome::None)
    }
}

/// 持久化 sink（A7）：宿主注入的落盘实现——loader 每次成功变更后接到**权威入口列表**，
/// 返回错误 → 该变更 fail-loud（`CordisError::Internal`）。
pub type PersistSink = Rc<dyn Fn(&[EntryOptions]) -> Result<(), String>>;

/// Loader 宿主 API。
#[derive(Clone)]
pub struct Loader {
    pub ctx: Cordis,
    pub state: Rc<RefCell<LoaderState>>,
    /// loader 插件 fiber。
    pub fid: FiberId,
    /// 持久化 sink（A7；None = 关闭写回）。
    pub persist: RefCell<Option<PersistSink>>,
}

/// Group 插件（M22：对应 Cordis `Group extends EntryGroup`）。
///
/// group 入口经本插件注册为真实 fiber（`plugin:Group`/`status:Group`），
/// apply 时挂载子入口（同步 `plugin_arc` → 子入口 parent 自动 = Group fiber，
/// 与 TS 的嵌套注册一致）；卸载时递归 stop 子入口。
struct GroupPlugin {
    /// 归属 Loader（经其挂载子入口）。
    loader: Loader,
    /// group 入口 id（apply 时经 pending_entry 关联；插件的 config 是子入口数组）。
    entry_id: String,
}

impl Plugin for GroupPlugin {
    fn name(&self) -> &'static str {
        "Group"
    }

    fn apply(&self, ctx: &Cordis, config: Value) -> Result<EffectOutcome, CordisError> {
        // 挂载子入口：config = 子 EntryOptions 数组。返回 `Await`——future 内
        // 异步挂载子入口（M27：等价 Cordis `[Service.init]` await update）；
        // 子入口 parent 自动 = Group fiber，且全部 Active 后 Group 才 finish。
        let loader = self.loader.clone();
        let gid = self.entry_id.clone();

        // 卸载 disposer：递归卸载子入口（stop 语义；随 Group fiber 卸载执行）。
        // 用 `EffectOutcome::Async`——`unload_async` 并行 await：子入口**并行**
        // 卸载（对齐 TS `Promise.allSettled(stop)`：先全部 Unloading 再全部
        // Disposed）。
        let stop_loader = self.loader.clone();
        let stop_gid = self.entry_id.clone();
        ctx.effect(
            "group-stop",
            Box::new(move |_ctx| {
                let loader = stop_loader.clone();
                let gid = stop_gid.clone();
                Ok(EffectOutcome::Async(Box::pin(async move {
                    let children: Vec<String> = {
                        let st = loader.state.borrow();
                        match st.entries.get(&gid).and_then(|e| e.subgroup.clone()) {
                            Some(sg) => st
                                .groups
                                .get(&sg)
                                .map(|g| g.data.clone())
                                .unwrap_or_default(),
                            None => Vec::new(),
                        }
                    };
                    // 并行卸载子入口（join_all；顺序无关——各自 Unloading→Disposed）
                    let futs: Vec<_> = children.iter().map(|c| {
                        let loader = loader.clone();
                        let c = c.clone();
                        async move { loader.dispose_entry_async(&c).await }
                    }).collect();
                    let _ = futures_util::future::join_all(futs).await;
                    EffectOutcome::None
                })))
            }),
        )?;

        // 子入口 async 挂载（Await）：同步注册（async 模式入队由 drive 驱动）
        // 或 async await（async 路径）。用 `start_entry`（同步注册，parent 正确）：
        // async 模式下 run_or_defer 入队 → 子入口 Apply 在 Group 的 Await future
        // 之后处理 → Finish 延迟逻辑保证子入口先 Active。
        let children: Vec<EntryOptions> = match config {
            Value::Array(items) => items
                .iter()
                .map(|v| serde_json::from_value(v.clone()))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| {
                    CordisError::Internal(format!("group {gid} config invalid: {e}"))
                })?,
            _ => {
                return Err(CordisError::Internal(format!(
                    "group {gid} config must be an array of entries"
                )))
            }
        };
        let fut: futures_util::future::LocalBoxFuture<'static, EffectOutcome> = Box::pin(async move {
            for child in children {
                let cid = child.id.clone();
                loader.insert_child(&gid, child);
                loader.start_entry(&cid).ok();
            }
            EffectOutcome::None
        });
        Ok(EffectOutcome::Await(fut))
    }
}

impl Loader {
    /// 挂载 loader 插件。
    pub fn new(ctx: &Cordis) -> Result<Self, CordisError> {
        let state = Rc::new(RefCell::new(LoaderState::new()));
        let fid = ctx.plugin(
            LoaderPlugin {
                state: state.clone(),
            },
            serde_json::json!({}),
        )?;
        Ok(Loader {
            ctx: ctx.clone(),
            state,
            fid,
            persist: RefCell::new(None),
        })
    }

    /// 注册可加载的插件（等价 Cordis 模块 import 的结果缓存）。
    ///
    /// A1 身份语义（对齐 harness「回调为身份」）：
    /// - 同名**同一实现**（同一 Arc）重复注册 → 幂等（身份与 generation 不变）；
    /// - 同名**新实现**（不同 Arc）→ 铸新身份、generation += 1（re-import=新身份 口径）。
    pub fn register_plugin(&self, name: &str, plugin: Arc<dyn Plugin>) {
        let mut st = self.state.borrow_mut();
        match st.plugins.get_mut(name) {
            Some(rec) if Arc::ptr_eq(&rec.plugin, &plugin) => {
                // 同实现幂等：身份/generation 不变
            }
            Some(rec) => {
                // 同名新实现：铸新身份 + 换代
                rec.identity = PluginIdentity::new();
                rec.plugin = plugin;
                rec.generation += 1;
            }
            None => {
                st.plugins.insert(name.to_string(), PluginRecord::new(plugin));
            }
        }
    }

    // ---- 热更（B3：插件实现级 replace/reload） ----

    /// B3 HMR 模块热更：把 name 的实现换成新实现（A1 身份换代）→ 以**旧身份**加载的
    /// entry 自动 reload 新实现（entry 保真：id/options/group 不变，仅 fiber 以新实现
    /// 重挂载；身份重新记录为新身份）。同实现（同一 Arc）幂等 → `Ok(0)`。
    /// 返回受影响（已 reload）的 entry 数。依赖方经 fiber uid/epoch 自然重活
    /// （externals→全重载 的 Rust 同构，DIV-3-1）。
    pub fn replace_plugin(&self, name: &str, plugin: Arc<dyn Plugin>) -> Result<usize, CordisError> {
        let same = self
            .state
            .borrow()
            .plugins
            .get(name)
            .map(|rec| Arc::ptr_eq(&rec.plugin, &plugin))
            .unwrap_or(false);
        if same {
            return Ok(0);
        }
        // A1 登记换代（新身份 + generation 递增）
        self.register_plugin(name, plugin);
        let stale = self.stale_entry_ids(name);
        let mut reloaded = 0usize;
        for id in stale {
            self.reload_entry(&id)?;
            reloaded += 1;
        }
        Ok(reloaded)
    }

    /// name 下以「非当前实现身份」加载的 entry id（供宿主/HMR 观测 stale 集）。
    pub fn stale_entry_ids(&self, name: &str) -> Vec<String> {
        let st = self.state.borrow();
        let current = st.plugins.get(name).map(|r| r.identity.clone());
        let mut out = Vec::new();
        for (id, e) in &st.entries {
            let is_stale = e.options.name == name
                && e.identity.is_some()
                && current.as_ref() != e.identity.as_ref();
            if is_stale {
                out.push(id.clone());
            }
        }
        out
    }

    /// entry 保真 reload：dispose 旧 fiber + 按当前注册实现重挂载（`load_plugin` 把身份
    /// 重新记录为新身份）。disabled entry → no-op。
    fn reload_entry(&self, id: &str) -> Result<(), CordisError> {
        if self.is_disabled(id) {
            return Ok(());
        }
        self.dispose_entry(id)?;
        self.start_entry(id)
    }

    // ---- 查询 ----

    /// 设置持久化 sink（A7：宿主注入落盘实现；`None` = 关闭写回）。
    pub fn set_persist(&self, sink: Option<PersistSink>) {
        *self.persist.borrow_mut() = sink;
    }

    /// 权威入口列表（root 组声明顺序；含每个 root 入口的 `EntryOptions`，
    /// 供 `persist`/导出——`serde_yaml::to_string` 即 cordis.yml 拓扑形态）。
    pub fn entry_options(&self) -> Vec<EntryOptions> {
        let st = self.state.borrow();
        let root = st.root_group.clone();
        let order = st
            .groups
            .get(&root)
            .map(|g| g.data.clone())
            .unwrap_or_default();
        let mut out = Vec::with_capacity(order.len());
        for id in &order {
            if let Some(e) = st.entries.get(id) {
                out.push(e.options.clone());
            }
        }
        out
    }

    /// 当前 name 注册的实现身份（未注册 → `None`）。
    pub fn plugin_identity(&self, name: &str) -> Option<PluginIdentity> {
        self.state.borrow().plugins.get(name).map(|r| r.identity.clone())
    }

    /// 当前 name 注册的换代计数（未注册 → `None`）。
    pub fn plugin_generation(&self, name: &str) -> Option<u64> {
        self.state.borrow().plugins.get(name).map(|r| r.generation)
    }

    /// entry 上次加载所解析的实现身份（未加载/未知 → `None`）。
    pub fn entry_identity(&self, id: &str) -> Option<PluginIdentity> {
        self.state
            .borrow()
            .entries
            .get(id)
            .and_then(|e| e.identity.clone())
    }

    pub fn fiber(&self, id: &str) -> Option<FiberId> {
        self.state.borrow().entries.get(id).and_then(|e| e.fiber)
    }

    pub fn is_disabled(&self, id: &str) -> bool {
        entry_disabled(&self.state.borrow(), id)
    }

    pub fn entries(&self) -> Vec<EntrySnapshot> {
        let st = self.state.borrow();
        let mut list: Vec<EntrySnapshot> = st
            .entries
            .values()
            .map(|e| EntrySnapshot {
                id: e.id.clone(),
                name: e.options.name.clone(),
                disabled: e.options.disabled,
                group: e.options.group,
                fiber: e.fiber,
            })
            .collect();
        list.sort_by(|a, b| a.id.cmp(&b.id));
        list
    }

    pub fn take_writes(&self) -> Vec<String> {
        self.state.borrow_mut().writes.drain(..).collect()
    }

    // ---- 生命周期 ----

    /// 创建入口（disabled 时不启动）。
    pub fn create(&self, options: EntryOptions) -> Result<String, CordisError> {
        let id = options.id.clone();
        {
            let mut st = self.state.borrow_mut();
            if st.entries.contains_key(&id) {
                return Err(CordisError::Internal(format!(
                    "duplicate loader entry id: {id}"
                )));
            }
            let root = st.root_group.clone();
            st.entries.insert(
                id.clone(),
                Entry {
                    id: id.clone(),
                    options,
                    fiber: None,
                    parent_group: root.clone(),
                    subgroup: None,
                    disposing: 0,
                    identity: None,
                },
            );
            if let Some(g) = st.groups.get_mut(&root) {
                g.data.push(id.clone());
            }
        }
        if !self.is_disabled(&id) {
            if let Err(e) = self.start_entry(&id) {
                let _ = self.dispose_entry(&id);
                let mut st = self.state.borrow_mut();
                st.entries.remove(&id);
                let root = st.root_group.clone();
                if let Some(g) = st.groups.get_mut(&root) {
                    g.data.retain(|x| x != &id);
                }
                return Err(e);
            }
        }
        self.write(&format!("create:{id}"))?;
        Ok(id)
    }

    /// 更新入口（四分支事务 + 回滚，对应 Cordis `Entry.update`）。
    pub fn update(&self, id: &str, options: EntryOptions) -> Result<(), CordisError> {
        let (prev, is_group, active) = {
            let st = self.state.borrow();
            let e = st
                .entries
                .get(id)
                .ok_or_else(|| CordisError::Internal(format!("no such loader entry: {id}")))?;
            let is_group = e.options.group;
            // group 入口没有 fiber：以子组是否创建判定「已启动」
            let active = if is_group {
                e.subgroup.is_some()
            } else {
                e.fiber
                    .map(|f| self.ctx.fiber_uid(f).is_some())
                    .unwrap_or(false)
            };
            (e.options.clone(), is_group, active)
        };
        let mut candidate = prev.clone();
        candidate.name = options.name;
        candidate.config = options.config;
        candidate.disabled = options.disabled;
        // 部分更新语义（Cordis：仅合并传入的键）：None/空 = 保留现值
        if options.disabled_expr.is_some() {
            candidate.disabled_expr = options.disabled_expr;
        }
        candidate.group = options.group;
        candidate.inject = options.inject;
        if !options.isolate.is_empty() {
            candidate.isolate = options.isolate;
        }
        if !options.intercept.is_empty() {
            candidate.intercept = options.intercept;
        }
        let diff = options_diff(&prev, &candidate);
        if diff.is_empty() {
            return Ok(());
        }
        let replace = diff.name || diff.group || diff.inject;

        // 写入候选配置（失败回滚 prev）
        {
            let mut st = self.state.borrow_mut();
            st.entries.get_mut(id).unwrap().options = candidate.clone();
        }

        if !active {
            // 分支 1：未启动 —— 设置配置；未禁用则启动
            if !self.is_disabled(id) {
                if let Err(e) = self.start_entry(id) {
                    self.rollback_options(id, &prev);
                    return Err(e);
                }
            }
            self.write(&format!("create:{id}"))?;
            return Ok(());
        }

        if self.is_disabled(id) {
            // 分支 2：候选禁用 —— 卸载
            if let Err(e) = self.dispose_entry(id) {
                self.rollback_options(id, &prev);
                return Err(e);
            }
            self.write(&format!("disable:{id}"))?;
            return Ok(());
        }

        if is_group {
            // 组入口：同步子入口（增删改）
            if let Err(e) = self.sync_children(id) {
                self.rollback_options(id, &prev);
                return Err(e);
            }
            self.write(&format!("update:{id}"))?;
            return Ok(());
        }

        if !replace {
            // 分支 3：仅 config 变化 —— fiber.update（internal/update waterfall）
            let config = candidate.config.clone();
            let fid = {
                let st = self.state.borrow();
                st.entries.get(id).and_then(|e| e.fiber)
            }
            .ok_or_else(|| CordisError::Internal(format!("entry {id} has no fiber")))?;
            if let Err(e) = self.ctx.update(fid, config) {
                self.rollback_options(id, &prev);
                let _ = self.ctx.update(fid, prev.config.clone());
                return Err(e);
            }
            self.write(&format!("update:{id}"))?;
            return Ok(());
        }

        // 分支 4：替换（name/group/inject 变化）—— dispose 旧 + start 新；失败回滚
        if let Err(e) = self.dispose_entry(id) {
            self.rollback_options(id, &prev);
            return Err(e);
        }
        match self.start_entry(id) {
            Ok(()) => {
                self.write(&format!("replace:{id}"))?;
                Ok(())
            }
            Err(e) => {
                self.rollback_options(id, &prev);
                // 回滚启动旧插件
                if let Err(rb) = self.start_entry(id) {
                    return Err(CordisError::Internal(format!(
                        "loader replace rollback failed for {id}: {e} (rollback: {rb})"
                    )));
                }
                Err(e)
            }
        }
    }

    /// 移除入口（含子组递归）。
    pub fn remove(&self, id: &str) -> Result<(), CordisError> {        let parent_group = {
            let st = self.state.borrow();
            st.entries.get(id).map(|e| e.parent_group.clone())
        };
        self.dispose_entry(id)?;
        {
            let mut st = self.state.borrow_mut();
            st.entries.remove(id);
            if let Some(g) = parent_group.and_then(|g| st.groups.get_mut(&g)) {
                g.data.retain(|x| x != id);
            }
            // realm GC：本地 realm 清理 + 无引用的全局 realm 清理
            st.local_realms
                .retain(|k, _| !k.starts_with(&format!("{id}:")));
            let live_labels: HashSet<String> = st
                .entries
                .values()
                .flat_map(|e| e.options.isolate.values())
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
            st.global_realms.retain(|label, _| live_labels.contains(label));
        }
        self.write(&format!("remove:{id}"))?;
        Ok(())
    }

    // ---- 内部 ----

    /// 整树同步：把根组收敛到给定入口列表（include 用；对应 `EntryGroup.update`）。
    pub fn sync(&self, entries: &[EntryOptions]) -> Result<(), CordisError> {
        let root = {
            let st = self.state.borrow();
            st.root_group.clone()
        };
        let existing: Vec<String> = {
            let st = self.state.borrow();
            st.groups.get(&root).map(|g| g.data.clone()).unwrap_or_default()
        };
        let new_ids: HashSet<String> = entries.iter().map(|e| e.id.clone()).collect();
        // 移除缺席
        for id in existing.iter().filter(|e| !new_ids.contains(*e)) {
            self.remove(id)?;
        }
        // 更新既有 / 创建新增
        for e in entries {
            if existing.contains(&e.id) {
                self.update(&e.id, e.clone())?;
            } else {
                self.create(e.clone())?;
            }
        }
        Ok(())
    }

    /// 等待入口树收敛（等价 `EntryTree.await()`）：轮询直到没有 fiber 处于
    /// Loading/Unloading（同步核心下立即返回；异步加载路径下让出直到稳定）。
    pub async fn await_idle(&self) -> Result<(), CordisError> {
        loop {
            let busy = {
                let st = self.state.borrow();
                st.entries.values().any(|e| {
                    e.fiber
                        .map(|f| {
                            matches!(
                                self.ctx.fiber_state(f),
                                Some(FiberState::Loading) | Some(FiberState::Unloading)
                            )
                        })
                        .unwrap_or(false)
                })
            };
            if !busy {
                return Ok(());
            }
            dsh_core::Cordis::yield_now().await;
        }
    }

    /// 记录写回事件 + 触发持久化（A7：sink 存在则把权威入口列表落盘；错误 fail-loud）。
    fn write(&self, record: &str) -> Result<(), CordisError> {
        self.state.borrow_mut().writes.push(record.to_string());
        let sink = self.persist.borrow().clone();
        if let Some(sink) = sink {
            let entries = self.entry_options();
            sink(&entries).map_err(CordisError::Internal)?;
        }
        Ok(())
    }

    fn rollback_options(&self, id: &str, prev: &EntryOptions) {
        let mut st = self.state.borrow_mut();
        if let Some(e) = st.entries.get_mut(id) {
            e.options = prev.clone();
        }
    }

    /// 启动入口（group → 挂子组；否则按名加载插件）。
    fn start_entry(&self, id: &str) -> Result<(), CordisError> {
        let is_group = {
            let st = self.state.borrow();
            st.entries.get(id).map(|e| e.options.group).unwrap_or(false)
        };
        if is_group {
            self.load_group_plugin(id)
        } else {
            self.load_plugin(id)
        }
    }

    /// 注册 Group 插件（group 入口的 fiber 形态）：config = 子入口数组。
    fn load_group_plugin(&self, id: &str) -> Result<(), CordisError> {
        let config = {
            let st = self.state.borrow();
            st.entries
                .get(id)
                .map(|e| e.options.config.clone())
                .unwrap_or_default()
        };
        let plugin = Arc::new(GroupPlugin {
            loader: self.clone(),
            entry_id: id.to_string(),
        });
        self.ctx.with(|rt| rt.pending_entry = Some(id.to_string()));
        let fid = self.ctx.plugin_arc(plugin, config)?;
        {
            let mut st = self.state.borrow_mut();
            if let Some(e) = st.entries.get_mut(id) {
                e.fiber = Some(fid);
            }
            st.fiber_to_entry.insert(fid, id.to_string());
        }
        if let Some(err) = self.ctx.fiber_error(fid) {
            return Err(err);
        }
        Ok(())
    }

    /// 加载插件：pending_entry/isolate/intercept → plugin_arc → 关联 fiber。
    fn load_plugin(&self, id: &str) -> Result<(), CordisError> {
        let (name, config) = {
            let st = self.state.borrow();
            let e = st
                .entries
                .get(id)
                .ok_or_else(|| CordisError::Internal(format!("no such loader entry: {id}")))?;
            (e.options.name.clone(), e.options.config.clone())
        };
        let record = {
            let st = self.state.borrow();
            st.plugins.get(&name).cloned()
        }
        .ok_or_else(|| CordisError::Internal(format!("loader: unknown plugin \"{name}\"")))?;
        let plugin = record.plugin.clone();
        let identity = record.identity;

        // isolate / intercept 注入（M3）
        let (isolate_opts, intercept_opts) = {
            let st = self.state.borrow();
            let e = st
                .entries
                .get(id)
                .ok_or_else(|| CordisError::Internal(format!("no such loader entry: {id}")))?;
            (e.options.isolate.clone(), e.options.intercept.clone())
        };
        let (isolate_map, intercept_vec) = {
            let mut st = self.state.borrow_mut();
            let mut iso = HashMap::new();
            let mut ic = Vec::new();
            for (sname, spec) in &isolate_opts {
                let scope = match spec {
                    Value::Bool(true) => {
                        let key = format!("{id}:{sname}");
                        *st.local_realms.entry(key).or_insert_with(|| {
                            self.ctx.with(|rt| rt.alloc_scope())
                        })
                    }
                    Value::String(label) => {
                        let realms = st.global_realms.entry(label.clone()).or_default();
                        *realms.entry(sname.clone()).or_insert_with(|| {
                            self.ctx.with(|rt| rt.alloc_scope())
                        })
                    }
                    _ => continue,
                };
                iso.insert(sname.clone(), scope);
            }
            for (k, v) in &intercept_opts {
                ic.push((k.clone(), v.clone()));
            }
            (iso, ic)
        };
        if !isolate_map.is_empty() {
            self.ctx.with(|rt| rt.pending_isolate = isolate_map);
        }
        if !intercept_vec.is_empty() {
            self.ctx.with(|rt| rt.pending_intercept = intercept_vec);
        }

        self.ctx.with(|rt| rt.pending_entry = Some(id.to_string()));
        let fid = self.ctx.plugin_arc(plugin, config)?;
        {
            let mut st = self.state.borrow_mut();
            if let Some(e) = st.entries.get_mut(id) {
                e.fiber = Some(fid);
                e.identity = Some(identity);
            }
            st.fiber_to_entry.insert(fid, id.to_string());
        }
        if let Some(err) = self.ctx.fiber_error(fid) {
            return Err(err);
        }
        Ok(())
    }

    /// 组入口：同步子入口（移除缺席、创建新增、更新既有）。
    fn sync_children(&self, id: &str) -> Result<(), CordisError> {
        let children = self.parse_children(id)?;
        let (subgroup, existing) = {
            let st = self.state.borrow();
            let sg = st
                .entries
                .get(id)
                .and_then(|e| e.subgroup.clone())
                .ok_or_else(|| CordisError::Internal(format!("group {id} not started")))?;
            let existing = st
                .groups
                .get(&sg)
                .map(|g| g.data.clone())
                .unwrap_or_default();
            (sg, existing)
        };
        let new_ids: HashSet<String> = children.iter().map(|c| c.id.clone()).collect();
        // 创建新增 / 更新既有（先；对应 Cordis `EntryGroup.update` 顺序：
        // allSettled create 全部 → 全成功后才移除缺席）
        for child in children {
            if new_ids.contains(&child.id) && existing.contains(&child.id) {
                self.update(&child.id, child.clone())?;
            } else if !existing.contains(&child.id) {
                let cid = child.id.clone();
                self.insert_child(id, child);
                self.start_entry(&cid)?;
            }
        }
        // 移除缺席（后）
        for cid in existing.iter().filter(|c| !new_ids.contains(*c)) {
            self.dispose_entry(cid)?;
            let mut st = self.state.borrow_mut();
            st.entries.remove(cid);
            if let Some(g) = st.groups.get_mut(&subgroup) {
                g.data.retain(|x| x != cid);
            }
        }
        Ok(())
    }

    fn parse_children(&self, id: &str) -> Result<Vec<EntryOptions>, CordisError> {
        let config = {
            let st = self.state.borrow();
            st.entries
                .get(id)
                .map(|e| e.options.config.clone())
                .unwrap_or_default()
        };
        match config {
            Value::Array(items) => items
                .iter()
                .map(|v| serde_json::from_value(v.clone()))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| CordisError::Internal(format!("group {id} config invalid: {e}"))),
            _ => Err(CordisError::Internal(format!(
                "group {id} config must be an array of entries"
            ))),
        }
    }

    /// 在组入口的子组中插入一个子入口（不启动）。
    fn insert_child(&self, id: &str, child: EntryOptions) {
        let mut st = self.state.borrow_mut();
        let gid = match st.entries.get(id).and_then(|e| e.subgroup.clone()) {
            Some(gid) => gid,
            None => {
                let gid = st.alloc_group();
                st.groups.insert(gid.clone(), EntryGroup::default());
                st.group_owner.insert(gid.clone(), id.to_string());
                if let Some(e) = st.entries.get_mut(id) {
                    e.subgroup = Some(gid.clone());
                }
                gid
            }
        };
        let cid = child.id.clone();
        if st.entries.contains_key(&cid) {
            return;
        }
        st.entries.insert(
            cid.clone(),
            Entry {
                id: cid.clone(),
                options: child,
                fiber: None,
                parent_group: gid.clone(),
                subgroup: None,
                disposing: 0,
                identity: None,
            },
        );
        if let Some(g) = st.groups.get_mut(&gid) {
            g.data.push(cid);
        }
    }

    /// 卸载入口（组 → 卸载 Group fiber，disposer 递归 stop 子入口；普通 → 卸载 fiber）。
    /// `disposing` 保护 7-case case 6。
    fn dispose_entry(&self, id: &str) -> Result<(), CordisError> {
        // group 入口：同步路径无法 await Group 的 Async stop disposer——
        // 先同步串行卸载子入口（Async 并行留给 unload_async 路径）
        let is_group = {
            let st = self.state.borrow();
            st.entries.get(id).map(|e| e.options.group).unwrap_or(false)
        };
        if is_group {
            let children: Vec<String> = {
                let st = self.state.borrow();
                match st.entries.get(id).and_then(|e| e.subgroup.clone()) {
                    Some(sg) => st.groups.get(&sg).map(|g| g.data.clone()).unwrap_or_default(),
                    None => Vec::new(),
                }
            };
            for c in children {
                self.dispose_entry(&c)?;
            }
        }
        // 自身 fiber（group 入口 = Group fiber；普通 = 插件 fiber）
        let fid = {
            let st = self.state.borrow();
            st.entries.get(id).and_then(|e| e.fiber)
        };
        if let Some(fid) = fid {
            {
                let mut st = self.state.borrow_mut();
                if let Some(e) = st.entries.get_mut(id) {
                    e.disposing += 1;
                }
            }
            let r = self.ctx.unload(fid);
            {
                let mut st = self.state.borrow_mut();
                if let Some(e) = st.entries.get_mut(id) {
                    e.disposing = e.disposing.saturating_sub(1);
                    e.fiber = None;
                }
                st.fiber_to_entry.remove(&fid);
            }
            r?;
        }
        // group 结构清理（子入口已卸载；此处移除 subgroup 引用）
        {
            let mut st = self.state.borrow_mut();
            if let Some(sg) = st.entries.get(id).and_then(|e| e.subgroup.clone()) {
                st.groups.remove(&sg);
                st.group_owner.remove(&sg);
                if let Some(e) = st.entries.get_mut(id) {
                    e.subgroup = None;
                }
            }
        }
        Ok(())
    }

    // ---- M14：async 生命周期（create/update/remove + allSettled 事务） ----

    /// 异步创建入口（等价 `Loader::create`，加载走 `plugin_arc_async`）。
    pub async fn create_async(&self, options: EntryOptions) -> Result<String, CordisError> {
        let id = options.id.clone();
        {
            let mut st = self.state.borrow_mut();
            if st.entries.contains_key(&id) {
                return Err(CordisError::Internal(format!(
                    "duplicate loader entry id: {id}"
                )));
            }
            let root = st.root_group.clone();
            st.entries.insert(
                id.clone(),
                Entry {
                    id: id.clone(),
                    options,
                    fiber: None,
                    parent_group: root.clone(),
                    subgroup: None,
                    disposing: 0,
                    identity: None,
                },
            );
            if let Some(g) = st.groups.get_mut(&root) {
                g.data.push(id.clone());
            }
        }
        if !self.is_disabled(&id) {
            if let Err(e) = self.start_entry_async(&id).await {
                let _ = self.dispose_entry_async(&id).await;
                let mut st = self.state.borrow_mut();
                st.entries.remove(&id);
                let root = st.root_group.clone();
                if let Some(g) = st.groups.get_mut(&root) {
                    g.data.retain(|x| x != &id);
                }
                return Err(e);
            }
        }
        self.write(&format!("create:{id}"))?;
        Ok(id)
    }

    /// 异步更新入口（四分支事务 + 回滚，生命周期走 async 路径）。
    /// group 子入口更新经 Box::pin 递归（嵌套 group 不爆栈）。
    pub async fn update_async(&self, id: &str, options: EntryOptions) -> Result<(), CordisError> {
        let fut: futures_util::future::LocalBoxFuture<'_, Result<(), CordisError>> =
            Box::pin(async move { self.update_one_async(id, options).await });
        fut.await
    }

    /// 单入口更新（四分支）；group 分支子入口按 config 序立即处理（递归）。
    async fn update_one_async(
        &self,
        id: &str,
        options: EntryOptions,
    ) -> Result<(), CordisError> {
        let (prev, is_group, active) = {
            let st = self.state.borrow();
            let e = st
                .entries
                .get(id)
                .ok_or_else(|| CordisError::Internal(format!("no such loader entry: {id}")))?;
            let is_group = e.options.group;
            // group 入口没有 fiber：以子组是否创建判定「已启动」
            let active = if is_group {
                e.subgroup.is_some()
            } else {
                e.fiber
                    .map(|f| self.ctx.fiber_uid(f).is_some())
                    .unwrap_or(false)
            };
            (e.options.clone(), is_group, active)
        };
        let mut candidate = prev.clone();
        candidate.name = options.name;
        candidate.config = options.config;
        candidate.disabled = options.disabled;
        // 部分更新语义（Cordis：仅合并传入的键）：None/空 = 保留现值
        if options.disabled_expr.is_some() {
            candidate.disabled_expr = options.disabled_expr;
        }
        candidate.group = options.group;
        candidate.inject = options.inject;
        if !options.isolate.is_empty() {
            candidate.isolate = options.isolate;
        }
        if !options.intercept.is_empty() {
            candidate.intercept = options.intercept;
        }
        let diff = options_diff(&prev, &candidate);
        if diff.is_empty() {
            return Ok(());
        }
        let replace = diff.name || diff.group || diff.inject;

        // 写入候选配置（失败回滚 prev）
        {
            let mut st = self.state.borrow_mut();
            st.entries.get_mut(id).unwrap().options = candidate.clone();
        }

        if !active {
            // 分支 1：未启动 —— 设置配置；未禁用则启动
            if !self.is_disabled(id) {
                if let Err(e) = self.start_entry_async(id).await {
                    self.rollback_options(id, &prev);
                    return Err(e);
                }
            }
            self.write(&format!("create:{id}"))?;
            return Ok(());
        }

        if self.is_disabled(id) {
            // 分支 2：候选禁用 —— 卸载
            if let Err(e) = self.dispose_entry_async(id).await {
                self.rollback_options(id, &prev);
                return Err(e);
            }
            self.write(&format!("disable:{id}"))?;
            return Ok(());
        }

        if is_group {
            // 组入口：同步子入口（对应 Cordis `EntryGroup.update(config)` 顺序：
            // allSettled create 全部 → 全成功后才移除缺席）
            let children = self.parse_children(id)?;
            let (subgroup, existing) = {
                let st = self.state.borrow();
                let sg = st
                    .entries
                    .get(id)
                    .and_then(|e| e.subgroup.clone())
                    .ok_or_else(|| CordisError::Internal(format!("group {id} not started")))?;
                let existing = st
                    .groups
                    .get(&sg)
                    .map(|g| g.data.clone())
                    .unwrap_or_default();
                (sg, existing)
            };
            let new_ids: HashSet<String> = children.iter().map(|c| c.id.clone()).collect();
            // 按 config 顺序处理（对应 Cordis `config.map(create)`：create 对既有
            // = update；顺序 = config 序）。更新既有立即处理（Box::pin 打破嵌套
            // group 的 async 递归），新建立即 start——保持 c1 热更在 c3 新建之前。
            for child in children {
                if new_ids.contains(&child.id) && existing.contains(&child.id) {
                    let id = child.id.clone();
                    let opts = child.clone();
                    let fut: futures_util::future::LocalBoxFuture<'_, Result<(), CordisError>> =
                        Box::pin(async move {
                            let r = self.update_one_async(&id, opts).await;
                            r
                        });
                    fut.await?;
                } else if !existing.contains(&child.id) {
                    let cid = child.id.clone();
                    self.insert_child(id, child);
                    self.start_entry_async(&cid).await?;
                }
            }
            // 移除缺席（后；对应 Cordis 全成功后的 remove）
            for cid in existing.iter().filter(|c| !new_ids.contains(*c)) {
                self.dispose_entry_async(cid).await?;
                let mut st = self.state.borrow_mut();
                st.entries.remove(cid);
                if let Some(g) = st.groups.get_mut(&subgroup) {
                    g.data.retain(|x| x != cid);
                }
            }
            self.write(&format!("update:{id}"))?;
            return Ok(());
        }

        if !replace {
            // 分支 3：仅 config 变化 —— fiber.update（internal/update waterfall）
            let config = candidate.config.clone();
            let fid = {
                let st = self.state.borrow();
                st.entries.get(id).and_then(|e| e.fiber)
            }
            .ok_or_else(|| CordisError::Internal(format!("entry {id} has no fiber")))?;
            if let Err(e) = self.ctx.update(fid, config) {
                self.rollback_options(id, &prev);
                let _ = self.ctx.update(fid, prev.config.clone());
                return Err(e);
            }
            self.write(&format!("update:{id}"))?;
            return Ok(());
        }

        // 分支 4：替换（name/group/inject 变化）—— dispose 旧 + start 新；失败回滚
        if let Err(e) = self.dispose_entry_async(id).await {
            self.rollback_options(id, &prev);
            return Err(e);
        }
        match self.start_entry_async(id).await {
            Ok(()) => {
                self.write(&format!("replace:{id}"))?;
                Ok(())
            }
            Err(e) => {
                self.rollback_options(id, &prev);
                // 回滚启动旧插件
                if let Err(rb) = self.start_entry_async(id).await {
                    return Err(CordisError::Internal(format!(
                        "loader replace rollback failed for {id}: {e} (rollback: {rb})"
                    )));
                }
                Err(e)
            }
        }
    }

    /// 异步移除入口（含子组递归；生命周期走 async 路径）。
    pub async fn remove_async(&self, id: &str) -> Result<(), CordisError> {
        let parent_group = {
            let st = self.state.borrow();
            st.entries.get(id).map(|e| e.parent_group.clone())
        };
        self.dispose_entry_async(id).await?;
        {
            let mut st = self.state.borrow_mut();
            st.entries.remove(id);
            if let Some(g) = parent_group.and_then(|g| st.groups.get_mut(&g)) {
                g.data.retain(|x| x != id);
            }
            // realm GC：本地 realm 清理 + 无引用的全局 realm 清理
            st.local_realms
                .retain(|k, _| !k.starts_with(&format!("{id}:")));
            let live_labels: HashSet<String> = st
                .entries
                .values()
                .flat_map(|e| e.options.isolate.values())
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
            st.global_realms.retain(|label, _| live_labels.contains(label));
        }
        self.write(&format!("remove:{id}"))?;
        Ok(())
    }

    /// **整树同步（async 事务，对应 Cordis `EntryGroup.update(config)`）**：
    /// allSettled 语义——全部入口都尝试 create/update（一个失败不阻断其他），
    /// 全部完成后聚合失败（1 个失败 = 原错误；多个失败 = AggregateError）。
    /// 全成功才移除缺席旧入口；任一失败则**整事务回滚**（逆序移除新建 +
    /// 重建旧配置），回滚错误并入 AggregateError。
    pub async fn sync_async(&self, entries: &[EntryOptions]) -> Result<(), AggregateError> {
        // 重复 id 校验（Cordis：seen set，重复直接抛错）
        let mut seen = HashSet::new();
        for e in entries {
            if !seen.insert(e.id.clone()) {
                return Err(AggregateError {
                    errors: vec![CordisError::Internal(format!(
                        "duplicate loader entry id: {}",
                        e.id
                    ))],
                });
            }
        }
        // old_map：仅**根组**入口（group 子入口由 group 分支管理——若收集全部，
        // sync 的「移除缺席」会误删子入口：update_async(g) 已移除缺席子入口后
        // 又被根级 remove 一次）。
        let old_map: HashMap<String, EntryOptions> = {
            let st = self.state.borrow();
            let root = st.root_group.clone();
            st.entries
                .values()
                .filter(|e| e.parent_group == root)
                .map(|e| (e.id.clone(), e.options.clone()))
                .collect()
        };
        let new_map: HashMap<String, EntryOptions> = entries
            .iter()
            .map(|e| (e.id.clone(), e.clone()))
            .collect();

        // allSettled：全部 create/update **并行**（join_all 交错，与 TS
        // `Promise.allSettled(config.map(create))` 一致——plugin/status/apply
        // 的 trace 交错顺序可复现；pending_* 在 register 同步段 take，无竞争）。
        let futures: Vec<futures_util::future::LocalBoxFuture<'_, SyncResult>> = entries
                .iter()
                .map(|options| {
                    let id = options.id.clone();
                    let is_new = !old_map.contains_key(&id);
                    let fut: futures_util::future::LocalBoxFuture<'_, SyncResult> = if is_new {
                            let options = options.clone();
                            let id = id.clone();
                            Box::pin(async move {
                                let r = self.create_async(options).await;
                                (id, r.map(|_| ()), true)
                            })
                        } else {
                            let options = options.clone();
                            let id = id.clone();
                            Box::pin(async move {
                                let r = self.update_async(&id, options).await;
                                (id, r, false)
                            })
                        };
                    fut
                })
                .collect();
        let results = futures_util::future::join_all(futures).await;
        let mut failures: Vec<CordisError> = Vec::new();
        let mut created_new: Vec<String> = Vec::new();
        for (id, result, is_new) in results {
            match result {
                Ok(()) => {
                    if is_new {
                        created_new.push(id);
                    }
                }
                Err(e) => failures.push(e),
            }
        }

        // 全部成功：移除缺席旧入口
        if failures.is_empty() {
            for id in old_map.keys() {
                if !new_map.contains_key(id) {
                    if let Err(e) = self.remove_async(id).await {
                        failures.push(e);
                    }
                }
            }
            if failures.is_empty() {
                return Ok(());
            }
        }

        // 失败 → 整事务回滚：逆序移除新建 + 重建旧配置
        let mut rollback_errors: Vec<CordisError> = Vec::new();
        for id in created_new.iter().rev() {
            if let Err(e) = self.remove_async(id).await {
                rollback_errors.push(e);
            }
        }
        for (id, options) in &old_map {
            // 重建旧配置：已在树中的入口用 update 恢复（保留位置），否则 create
            let still_exists = self.state.borrow().entries.contains_key(id);
            let r = if still_exists {
                self.update_async(id, options.clone()).await
            } else {
                self.create_async(options.clone()).await.map(|_| ())
            };
            if let Err(e) = r {
                rollback_errors.push(e);
            }
        }
        let mut errors = failures.clone();
        errors.extend(rollback_errors);
        Err(AggregateError { errors })
    }

    /// 异步启动入口（group → 注册 Group 插件；否则按名加载插件）。
    /// 迭代式（显式栈）避免 async 递归；顺序与原递归一致。
    /// 中途失败 → 逆序清理全部已启动入口（含 group 结构），等价 `EntryGroup.create` 失败删除。
    async fn start_entry_async(&self, id: &str) -> Result<(), CordisError> {
        let mut started: Vec<String> = Vec::new();
        let mut stack: Vec<String> = vec![id.to_string()];
        while let Some(cur) = stack.pop() {
            let is_group = {
                let st = self.state.borrow();
                st.entries.get(&cur).map(|e| e.options.group).unwrap_or(false)
            };
            if is_group {
                // Group 插件 apply 内挂载子入口（async_mode 下入队并行驱动）
                if let Err(e) = self.load_group_plugin_async(&cur).await {
                    self.rollback_started(&started).await;
                    return Err(e);
                }
            } else if let Err(e) = self.load_plugin_async(&cur).await {
                self.rollback_started(&started).await;
                return Err(e);
            }
            started.push(cur);
        }
        Ok(())
    }

    /// 异步注册 Group 插件（group 入口的 fiber 形态）：config = 子入口数组。
    async fn load_group_plugin_async(&self, id: &str) -> Result<(), CordisError> {
        let config = {
            let st = self.state.borrow();
            st.entries
                .get(id)
                .map(|e| e.options.config.clone())
                .unwrap_or_default()
        };
        let plugin = Arc::new(GroupPlugin {
            loader: self.clone(),
            entry_id: id.to_string(),
        });
        self.ctx.with(|rt| rt.pending_entry = Some(id.to_string()));
        let fid = self.ctx.plugin_arc_async(plugin, config).await?;
        {
            let mut st = self.state.borrow_mut();
            if let Some(e) = st.entries.get_mut(id) {
                e.fiber = Some(fid);
            }
            st.fiber_to_entry.insert(fid, id.to_string());
        }
        if let Some(err) = self.ctx.fiber_error(fid) {
            return Err(err);
        }
        Ok(())
    }

    /// 逆序清理已启动入口：dispose（含子树）+ 从 entries/父组移除。
    async fn rollback_started(&self, started: &[String]) {
        for s in started.iter().rev() {
            let _ = self.dispose_entry_async(s).await;
            let pg = {
                let st = self.state.borrow();
                st.entries.get(s).map(|e| e.parent_group.clone())
            };
            let mut st = self.state.borrow_mut();
            st.entries.remove(s);
            if let Some(g) = pg.and_then(|g| st.groups.get_mut(&g)) {
                g.data.retain(|x| x != s);
            }
        }
    }

    /// 异步加载插件：pending_entry/isolate/intercept → `plugin_arc_async` → 关联 fiber。
    async fn load_plugin_async(&self, id: &str) -> Result<(), CordisError> {
        let (name, config) = {
            let st = self.state.borrow();
            let e = st
                .entries
                .get(id)
                .ok_or_else(|| CordisError::Internal(format!("no such loader entry: {id}")))?;
            (e.options.name.clone(), e.options.config.clone())
        };
        let record = {
            let st = self.state.borrow();
            st.plugins.get(&name).cloned()
        }
        .ok_or_else(|| CordisError::Internal(format!("loader: unknown plugin \"{name}\"")))?;
        let plugin = record.plugin.clone();
        let identity = record.identity;

        // isolate / intercept 注入（与同步 `load_plugin` 相同）
        let (isolate_opts, intercept_opts) = {
            let st = self.state.borrow();
            let e = st
                .entries
                .get(id)
                .ok_or_else(|| CordisError::Internal(format!("no such loader entry: {id}")))?;
            (e.options.isolate.clone(), e.options.intercept.clone())
        };
        let (isolate_map, intercept_vec) = {
            let mut st = self.state.borrow_mut();
            let mut iso = HashMap::new();
            let mut ic = Vec::new();
            for (sname, spec) in &isolate_opts {
                let scope = match spec {
                    Value::Bool(true) => {
                        let key = format!("{id}:{sname}");
                        *st.local_realms.entry(key).or_insert_with(|| {
                            self.ctx.with(|rt| rt.alloc_scope())
                        })
                    }
                    Value::String(label) => {
                        let realms = st.global_realms.entry(label.clone()).or_default();
                        *realms.entry(sname.clone()).or_insert_with(|| {
                            self.ctx.with(|rt| rt.alloc_scope())
                        })
                    }
                    _ => continue,
                };
                iso.insert(sname.clone(), scope);
            }
            for (k, v) in &intercept_opts {
                ic.push((k.clone(), v.clone()));
            }
            (iso, ic)
        };
        if !isolate_map.is_empty() {
            self.ctx.with(|rt| rt.pending_isolate = isolate_map);
        }
        if !intercept_vec.is_empty() {
            self.ctx.with(|rt| rt.pending_intercept = intercept_vec);
        }

        self.ctx.with(|rt| rt.pending_entry = Some(id.to_string()));
        let fid = self.ctx.plugin_arc_async(plugin, config).await?;
        {
            let mut st = self.state.borrow_mut();
            if let Some(e) = st.entries.get_mut(id) {
                e.fiber = Some(fid);
                e.identity = Some(identity);
            }
            st.fiber_to_entry.insert(fid, id.to_string());
        }
        if let Some(err) = self.ctx.fiber_error(fid) {
            return Err(err);
        }
        Ok(())
    }

    /// 异步卸载入口（组 → 卸载 Group fiber，disposer 递归 stop 子入口；普通 → `unload_async`）。
    async fn dispose_entry_async(&self, id: &str) -> Result<(), CordisError> {
        // 自身 fiber（group 入口 = Group fiber；普通 = 插件 fiber）
        let fid = {
            let st = self.state.borrow();
            st.entries.get(id).and_then(|e| e.fiber)
        };
        if let Some(fid) = fid {
            {
                let mut st = self.state.borrow_mut();
                if let Some(e) = st.entries.get_mut(id) {
                    e.disposing += 1;
                }
            }
            let r = self.ctx.unload_async(fid).await;
            {
                let mut st = self.state.borrow_mut();
                if let Some(e) = st.entries.get_mut(id) {
                    e.disposing = e.disposing.saturating_sub(1);
                    e.fiber = None;
                }
                st.fiber_to_entry.remove(&fid);
            }
            r?;
        }
        // group 结构清理（子入口已由 Group disposer stop；此处移除 subgroup 引用）
        {
            let mut st = self.state.borrow_mut();
            if let Some(sg) = st.entries.get(id).and_then(|e| e.subgroup.clone()) {
                st.groups.remove(&sg);
                st.group_owner.remove(&sg);
                if let Some(e) = st.entries.get_mut(id) {
                    e.subgroup = None;
                }
            }
        }
        Ok(())
    }
}

/// 入口选项差异（决定 update 分支）。
struct OptionsDiff {
    name: bool,
    config: bool,
    disabled: bool,
    group: bool,
    inject: bool,
}

impl OptionsDiff {
    fn is_empty(&self) -> bool {
        !self.name && !self.config && !self.disabled && !self.group && !self.inject
    }
}

fn options_diff(prev: &EntryOptions, next: &EntryOptions) -> OptionsDiff {
    OptionsDiff {
        name: prev.name != next.name,
        config: prev.config != next.config
            || prev.disabled_expr != next.disabled_expr
            || prev.isolate != next.isolate
            || prev.intercept != next.intercept,
        disabled: prev.disabled != next.disabled,
        group: prev.group != next.group,
        inject: prev.inject != next.inject,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    /// P2-a（spike-6 结论）回归：`process.platform` 门控现在按真实平台求值，
    /// **不再** fail-closed 全禁用（结构性 bug）。entry_disabled 对该表达式做
    /// `evaluate(scope).map(truthy).unwrap_or(true)`——这里复刻同一行求值。
    fn disabled_truthy(expr: &str, platform: &str) -> bool {
        let config = Value::Null;
        let process = serde_json::json!({
            "platform": platform,
            "env": { "DSH_CWD": "x" },
            "cwd": "x",
        });
        let scope = eval_scope_with_process(&config, &process);
        dsh_eval::evaluate(&scope, expr)
            .map(|v| dsh_eval::truthy(&v))
            .unwrap_or(true) // fail-closed 语义保留：求值失败 = 禁用
    }

    #[test]
    fn platform_gates_are_now_platform_specific() {
        // `dsh-tool-bash` 的门控：win32 上禁用；非 win32 可用。
        assert!(disabled_truthy("process.platform === 'win32'", "win32"));
        assert!(!disabled_truthy("process.platform === 'win32'", "linux"));
        // `dsh-tool-pwsh` 的门控：win32 可用；非 win32 禁用。
        assert!(!disabled_truthy("process.platform !== 'win32'", "win32"));
        assert!(disabled_truthy("process.platform !== 'win32'", "linux"));
    }

    #[test]
    fn process_env_and_cwd_available_in_scope() {
        let config = Value::Null;
        let process = serde_json::json!({
            "platform": "win32",
            "env": { "DSH_CWD": "C:\\w" },
            "cwd": "C:\\repo",
        });
        let scope = eval_scope_with_process(&config, &process);
        assert_eq!(
            dsh_eval::evaluate(&scope, "process.env.DSH_CWD ?? process.cwd()").unwrap(),
            serde_json::json!("C:\\w")
        );
        assert_eq!(
            dsh_eval::evaluate(&scope, "process.cwd()").unwrap(),
            serde_json::json!("C:\\repo")
        );
    }

    /// 真实 eval_scope（默认 facade）不吃惊：本机平台表达式的求值绝不报错。
    #[test]
    fn real_facade_evaluates_cleanly() {
        let config = Value::Null;
        let scope = eval_scope(&config);
        let v = dsh_eval::evaluate(&scope, "process.platform === 'win32'").unwrap();
        let _ = dsh_eval::truthy(&v);
        let v2 = dsh_eval::evaluate(&scope, "process.env.DSH_CWD ?? process.cwd()").unwrap();
        assert!(v2.is_string());
    }
}

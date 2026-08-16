//! Loader 服务与入口树（对应 PLAN §1.8）。
//!
//! 借用纪律与 dsh-core 一致：`LoaderState` 的借用绝不跨 `ctx` 调用持有
//! （用户代码/监听器可能在 `ctx` 调用内重入本状态）。

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use dsh_core::{
    Cordis, CordisError, EffectOutcome, FiberId, FiberState, HookResult, Listener, Plugin, ScopeId,
    Value,
};

use crate::entry::{Entry, EntryOptions};
use crate::group::EntryGroup;

const ROOT_GROUP: &str = "root";

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
    pub plugins: HashMap<String, Arc<dyn Plugin>>,
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

/// `!!js` 求值作用域：`{ config, ctx, env }`。
fn eval_scope(config: &Value) -> HashMap<String, Value> {
    let mut scope = HashMap::new();
    scope.insert("config".to_string(), config.clone());
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

/// Loader 宿主 API。
#[derive(Clone)]
pub struct Loader {
    pub ctx: Cordis,
    pub state: Rc<RefCell<LoaderState>>,
    /// loader 插件 fiber。
    pub fid: FiberId,
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
        })
    }

    /// 注册可加载的插件（等价 Cordis 模块 import 的结果缓存）。
    pub fn register_plugin(&self, name: &str, plugin: Arc<dyn Plugin>) {
        self.state.borrow_mut().plugins.insert(name.to_string(), plugin);
    }

    // ---- 查询 ----

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
        self.write(&format!("create:{id}"));
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
            self.write(&format!("create:{id}"));
            return Ok(());
        }

        if self.is_disabled(id) {
            // 分支 2：候选禁用 —— 卸载
            if let Err(e) = self.dispose_entry(id) {
                self.rollback_options(id, &prev);
                return Err(e);
            }
            self.write(&format!("disable:{id}"));
            return Ok(());
        }

        if is_group {
            // 组入口：同步子入口（增删改）
            if let Err(e) = self.sync_children(id) {
                self.rollback_options(id, &prev);
                return Err(e);
            }
            self.write(&format!("update:{id}"));
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
            self.write(&format!("update:{id}"));
            return Ok(());
        }

        // 分支 4：替换（name/group/inject 变化）—— dispose 旧 + start 新；失败回滚
        if let Err(e) = self.dispose_entry(id) {
            self.rollback_options(id, &prev);
            return Err(e);
        }
        match self.start_entry(id) {
            Ok(()) => {
                self.write(&format!("replace:{id}"));
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
        self.write(&format!("remove:{id}"));
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

    fn write(&self, record: &str) {
        self.state.borrow_mut().writes.push(record.to_string());
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
            self.mount_group_children(id)
        } else {
            self.load_plugin(id)
        }
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
        let plugin = {
            let st = self.state.borrow();
            st.plugins.get(&name).cloned()
        }
        .ok_or_else(|| CordisError::Internal(format!("loader: unknown plugin \"{name}\"")))?;

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
            }
            st.fiber_to_entry.insert(fid, id.to_string());
        }
        if let Some(err) = self.ctx.fiber_error(fid) {
            return Err(err);
        }
        Ok(())
    }

    /// 组入口：挂载子入口（config 为子 EntryOptions 数组）。
    fn mount_group_children(&self, id: &str) -> Result<(), CordisError> {
        let children = self.parse_children(id)?;
        let mut created: Vec<String> = Vec::new();
        for child in children {
            let cid = child.id.clone();
            self.insert_child(id, child);
            if let Err(e) = self.start_entry(&cid) {
                for c in &created {
                    let _ = self.dispose_entry(c);
                    let mut st = self.state.borrow_mut();
                    st.entries.remove(c);
                    if let Some(g) = st
                        .entries
                        .get(id)
                        .and_then(|e| e.subgroup.clone())
                        .and_then(|gid| st.groups.get_mut(&gid))
                    {
                        g.data.retain(|x| x != c);
                    }
                }
                return Err(e);
            }
            created.push(cid);
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
        // 移除缺席
        for cid in existing.iter().filter(|c| !new_ids.contains(*c)) {
            self.dispose_entry(cid)?;
            let mut st = self.state.borrow_mut();
            st.entries.remove(cid);
            if let Some(g) = st.groups.get_mut(&subgroup) {
                g.data.retain(|x| x != cid);
            }
        }
        // 创建新增 / 更新既有
        for child in children {
            if new_ids.contains(&child.id) && existing.contains(&child.id) {
                self.update(&child.id, child.clone())?;
            } else if !existing.contains(&child.id) {
                let cid = child.id.clone();
                self.insert_child(id, child);
                self.start_entry(&cid)?;
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
            },
        );
        if let Some(g) = st.groups.get_mut(&gid) {
            g.data.push(cid);
        }
    }

    /// 卸载入口（组 → 递归子入口；普通 → 卸载 fiber）。`disposing` 保护 7-case case 6。
    fn dispose_entry(&self, id: &str) -> Result<(), CordisError> {
        // 组：先递归卸载子入口
        let subgroup = {
            let st = self.state.borrow();
            st.entries.get(id).and_then(|e| e.subgroup.clone())
        };
        if let Some(sg) = subgroup {
            let children = {
                let st = self.state.borrow();
                st.groups.get(&sg).map(|g| g.data.clone()).unwrap_or_default()
            };
            for c in &children {
                self.dispose_entry(c)?;
            }
            let mut st = self.state.borrow_mut();
            st.groups.remove(&sg);
            st.group_owner.remove(&sg);
            if let Some(e) = st.entries.get_mut(id) {
                e.subgroup = None;
            }
        }
        // 自身 fiber
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

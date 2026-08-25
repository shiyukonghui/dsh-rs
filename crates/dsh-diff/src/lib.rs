//! `dsh-diff` —— 场景 DSL 与规范化 trace 差分（对应 PLAN §5）。
//!
//! 场景 DSL 是 JSON 剧本：`plugins` 描述插件（`apply` 为操作序列），`steps`
//! 为执行步骤。Rust 侧用 `dsh-core` 执行并输出**规范化 trace**；TS 侧宿主
//! （`diff/ts-host`）用 npm 原版 cordis 执行同一剧本并输出同格式 trace。
//! golden 文件固化 TS 侧输出，Rust 侧逐行对比。
//!
//! trace 行全部写入 `Runtime.trace`（与 Cordis 的事件/状态顺序自然交错）：
//! - 框架层：`plugin:{name}`、`status:{name}:{old}:{new}`、`emit:{event}`、
//!   `serial:{event}`、`bail:{event}`、`waterfall:{event}`
//! - 解释器层：`apply:{name}`、`log:{text}`、`log-config:{json}`、
//!   `effect-reg:{text}`、`dispose:{text}`、`on:{event}:{log}`、
//!   `on-return:{event}:{log}`、`provide:{service}:{json}`、
//!   `intercept:{service}:{json}`
//! - 宿主层：`serial-result:{json}`、`bail-result:{json}`、`waterfall-result:{json}`

// 同 dsh-core：单线程运行时，`Arc` 仅共享所有权。
#![allow(clippy::arc_with_non_send_sync)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use dsh_core::*;
use serde::Deserialize;

/// 场景剧本。
#[derive(Debug, Clone, Deserialize)]
pub struct Scenario {
    pub name: String,
    #[serde(default)]
    pub plugins: HashMap<String, PluginDesc>,
    pub steps: Vec<Step>,
}

/// 插件描述。
#[derive(Debug, Clone, Deserialize)]
pub struct PluginDesc {
    pub name: String,
    #[serde(default)]
    pub inject: Vec<String>,
    #[serde(default)]
    pub apply: Vec<ApplyOp>,
}

/// loader 场景的 entry 选项（与 `EntryOptions` 兼容的 JSON 形态）。
/// 用原始 `serde_json::Value` 承载以保留键序（与 TS 输入原序一致）。
pub type LoaderEntry = serde_json::Value;

/// 插件 apply 操作（微型 DSL）。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum ApplyOp {
    /// 记录 `log:{text}`。
    Log { text: String },
    /// 记录 `log-config:{json(config)}`。
    LogConfig,
    /// 注册 effect：注册时 `effect-reg:{dispose}`；disposer 运行时 `dispose:{dispose}`。
    Effect { dispose: String },
    /// 立即调用第 index 个已注册 effect 的 disposer（幂等性测试）。
    DisposeEffect { index: usize },
    /// 注册 emit 类监听：注册时 `on:{event}:{log}`，触发时记录 `log:{log}`。
    On { event: String, log: String },
    /// 同 `on`，但 prepend（插到已有监听器前）。
    OnPrepend { event: String, log: String },
    /// 注册监听并返回固定值（serial/bail 用）：触发时记录 `log:{log}`，返回 value。
    OnReturn {
        event: String,
        log: String,
        value: serde_json::Value,
    },
    /// 注册 waterfall 监听：触发时 `log:{log}` → next() → `log:{after}`。
    OnWaterfall {
        event: String,
        log: String,
        after: String,
    },
    /// 注册 waterfall 监听但短路（不调 next）：触发时记录 `log:{log}`。
    OnShort { event: String, log: String },
    /// 提供服务：`provide:{service}:{json(value)}`。
    Provide {
        service: String,
        value: serde_json::Value,
    },
    /// 注册 intercept：`intercept:{service}:{json(config)}`。
    Intercept {
        service: String,
        config: serde_json::Value,
    },
    /// 解析并记录 intercept 合并结果：`resolve-config:{service}:{json}`。
    ResolveConfig { service: String },
    /// 嵌套挂载插件（父 = 当前 fiber）。
    Plugin { id: String },
}

/// 执行步骤。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Step {
    Plugin { id: String },
    PluginWithConfig { id: String, config: serde_json::Value },
    Emit {
        event: String,
        args: Vec<serde_json::Value>,
    },
    Waterfall {
        event: String,
        args: Vec<serde_json::Value>,
    },
    Serial {
        event: String,
        args: Vec<serde_json::Value>,
    },
    Bail {
        event: String,
        args: Vec<serde_json::Value>,
    },
    Unload { id: String },
    Update { id: String, config: serde_json::Value },
    /// loader 场景：整树同步（事务；对应 TS `ctx.loader.root.update`）。
    LoaderSync { entries: Vec<LoaderEntry> },
    /// loader 场景：创建入口（对应 TS `ctx.loader.root.create`）。
    LoaderCreate { options: LoaderEntry },
    /// loader 场景：更新入口（对应 TS `ctx.loader.update(id, options)`）。
    LoaderUpdate { id: String, options: LoaderEntry },
    /// loader 场景：移除入口（对应 TS `ctx.loader.remove(id)`）。
    LoaderRemove { id: String },
    /// session 场景：创建会话（`SessionStore::create`；TS session-host 镜像）。
    SessionCreate { id: String },
    /// session 场景：追加事件（`Session.append`；seq=log 长度；kind 为事件 kind 名）。
    /// `surface: Some("append")` → 携带 `SurfaceIntent{Append}`（surface-eligible 事件
    /// 必需；与 dsh-session 契约一致），其余 None。
    SessionAppend {
        id: String,
        kind: String,
        #[serde(default)]
        surface: Option<String>,
        data: serde_json::Value,
    },
    /// session 场景：回读事件（`Session.events`；逐条 `session-event-read` 行，canonical JSON）。
    SessionEvents { id: String },
}

/// 场景解释器（Rust 侧）。
pub struct Runner {
    pub ctx: Cordis,
    plugins: Rc<RefCell<HashMap<String, Arc<dyn Plugin>>>>,
    fibers: HashMap<String, FiberId>,
    /// loader 场景专用（懒初始化；Loader 插件挂载在 ctx 上）。
    loader: Option<dsh_loader::Loader>,
    /// session 场景专用（懒初始化；真实 dsh-session store——会话事件模型的权威侧）。
    session_store: Option<Arc<dsh_session::store::SessionStore>>,
    /// session 场景已创建会话（id → store 会话）。
    sessions: HashMap<String, Arc<dsh_session::Session>>,
}

impl Runner {
    pub fn new() -> Self {
        Runner {
            ctx: Cordis::new(),
            plugins: Rc::new(RefCell::new(HashMap::new())),
            fibers: HashMap::new(),
            loader: None,
            session_store: None,
            sessions: HashMap::new(),
        }
    }

    fn trace_line(&self, line: &str) {
        self.ctx.with(|rt| rt.trace.push(line.to_string()));
    }

    /// 懒初始化 loader（挂载 Loader 插件 + 注册场景插件到仓库）。
    /// 挂载产生的 `plugin:loader`/`status:loader` trace 丢弃（TS 侧在框架
    /// 监听注册前挂载 Loader，golden 不含这些行）。
    fn ensure_loader(&mut self, scenario: &Scenario) -> Result<(), CordisError> {
        if self.loader.is_some() {
            return Ok(());
        }
        let loader = dsh_loader::Loader::new(&self.ctx)?;
        // 注册场景插件（entry.name 直接解析）
        for (id, desc) in &scenario.plugins {
            let plugin: Arc<dyn Plugin> = Arc::new(ScenarioPlugin {
                desc: desc.clone(),
                plugins: self.plugins.clone(),
            });
            loader.register_plugin(id, plugin);
        }
        self.loader = Some(loader);
        self.ctx.take_trace();
        Ok(())
    }

    /// 执行整个场景，返回规范化 trace。
    pub fn run(&mut self, scenario: &Scenario) -> Result<Vec<String>, CordisError> {
        // 构造插件
        let descs: Vec<(String, PluginDesc)> = scenario.plugins.clone().into_iter().collect();
        for (id, desc) in &descs {
            let plugin: Arc<dyn Plugin> = Arc::new(ScenarioPlugin {
                desc: desc.clone(),
                plugins: self.plugins.clone(),
            });
            self.plugins.borrow_mut().insert(id.clone(), plugin);
        }
        for step in &scenario.steps {
            self.run_step(step)?;
        }
        Ok(self.ctx.take_trace())
    }

    fn run_step(&mut self, step: &Step) -> Result<(), CordisError> {
        match step {
            Step::Plugin { id } => {
                self.mount(id)?;
            }
            Step::PluginWithConfig { id, config } => {
                self.mount_with_config(id, config.clone())?;
            }
            Step::Emit { event, args } => {
                self.ctx.emit(event, args.clone());
            }
            Step::Serial { event, args } => {
                let result = self.ctx.serial(event, args.clone());
                if let Some(v) = result {
                    self.trace_line(&format!(
                        "serial-result:{}",
                        serde_json::to_string(&v).unwrap_or_default()
                    ));
                }
            }
            Step::Bail { event, args } => {
                let result = self.ctx.bail(event, args.clone());
                if let Some(v) = result {
                    self.trace_line(&format!(
                        "bail-result:{}",
                        serde_json::to_string(&v).unwrap_or_default()
                    ));
                }
            }
            Step::Waterfall { event, args } => {
                let result = self.ctx.waterfall(
                    event,
                    args.clone(),
                    Box::new(|_| Some(serde_json::Value::Null)),
                );
                if let Some(v) = result {
                    self.trace_line(&format!(
                        "waterfall-result:{}",
                        serde_json::to_string(&v).unwrap_or_default()
                    ));
                }
            }
            Step::Unload { id } => {
                let fid = self.fiber_of(id)?;
                self.ctx.unload(fid)?;
            }
            Step::Update { id, config } => {
                let fid = self.fiber_of(id)?;
                self.ctx.update(fid, config.clone())?;
            }
            // loader 场景必须走 async 路径（事务 allSettled）
            Step::LoaderSync { .. }
            | Step::LoaderCreate { .. }
            | Step::LoaderUpdate { .. }
            | Step::LoaderRemove { .. } => {
                return Err(CordisError::Internal(
                    "loader scenario steps require --async".into(),
                ))
            }
            // ---- M6 step10（D-090）：session 场景（同步；真实 dsh-session store） ----
            Step::SessionCreate { id } => {
                let store = self.ensure_session_store()?;
                let session = store
                    .create(
                        Some(dsh_session::types::SessionId::from_raw(id.clone())),
                        &dsh_session::CreateSessionOptions { seed: None, meta: None },
                    )
                    .map_err(|e| CordisError::Internal(format!("session create: {e}")))?;
                self.sessions.insert(id.clone(), session);
                self.trace_line(&format!("session-create:{id}"));
            }
            Step::SessionAppend { id, kind, surface, data } => {
                let session = self.session_of(id)?;
                let ev_kind = kind
                    .parse::<dsh_session::types::EventKind>()
                    .map_err(|_| CordisError::Internal(format!("session append: unknown kind {kind}")))?;
                use dsh_session::types::SurfaceIntent;
                // dsh-session 表面契约（SURFACE_EVENT_TYPES）：surface-eligible 事件
                // 必须携带 surfaceOp；非 surface 事件不得携带。两侧 fail-loud 对称。
                let surface_eligible = matches!(kind.as_str(), "user/message" | "assistant/message" | "tool/result");
                let wants_surface = surface.as_deref() == Some("append");
                match (surface_eligible, wants_surface) {
                    (true, false) => {
                        return Err(CordisError::Internal(format!(
                            "session append: surface-eligible kind {kind} requires surface: \"append\""
                        )))
                    }
                    (false, true) => {
                        return Err(CordisError::Internal(format!(
                            "session append: surface marker on non-surface kind {kind}"
                        )))
                    }
                    _ => {}
                }
                let surface = wants_surface.then_some(SurfaceIntent {
                    surface_op: dsh_session::types::SurfaceOp::Append,
                    source_event_seqs: None,
                });
                let ev = session
                    .append(ev_kind, data.clone(), surface.as_ref())
                    .map_err(|e| CordisError::Internal(format!("session append: {e}")))?;
                self.trace_line(&format!(
                    "session-append:{id}:{}:{}",
                    ev.seq,
                    ev.kind.as_str()
                ));
            }
            Step::SessionEvents { id } => {
                let session = self.session_of(id)?;
                for ev in session.events() {
                    self.trace_line(&format!(
                        "session-event-read:{id}:{}:{}:{}",
                        ev.seq,
                        ev.kind.as_str(),
                        sorted_json(&ev.data)
                    ));
                }
            }
        }
        Ok(())
    }

    /// 懒初始化 session store（session 场景专用）。
    fn ensure_session_store(&mut self) -> Result<Arc<dsh_session::store::SessionStore>, CordisError> {
        if self.session_store.is_none() {
            self.session_store = Some(Arc::new(dsh_session::store::SessionStore::new()));
        }
        Ok(self.session_store.clone().expect("just set"))
    }

    /// 取已创建会话；缺失 → fail-loud。
    fn session_of(
        &self,
        id: &str,
    ) -> Result<Arc<dsh_session::Session>, CordisError> {
        self.sessions
            .get(id)
            .cloned()
            .ok_or_else(|| CordisError::Internal(format!("session {id} not created")))
    }

    /// M7：异步执行步骤（`plugin_arc_async` 真实微任务让出；深嵌套场景用）。
    async fn run_step_async(&mut self, scenario: &Scenario, step: &Step) -> Result<(), CordisError> {
        match step {
            Step::Plugin { id } => {
                let plugin = self
                    .plugins
                    .borrow()
                    .get(id)
                    .cloned()
                    .ok_or_else(|| CordisError::Internal(format!("scenario: unknown plugin {id}")))?;
                let fid = self.ctx.plugin_arc_async(plugin, serde_json::json!({})).await?;
                self.fibers.insert(id.to_string(), fid);
            }
            Step::PluginWithConfig { id, config } => {
                let plugin = self
                    .plugins
                    .borrow()
                    .get(id)
                    .cloned()
                    .ok_or_else(|| CordisError::Internal(format!("scenario: unknown plugin {id}")))?;
                let fid = self.ctx.plugin_arc_async(plugin, config.clone()).await?;
                self.fibers.insert(id.to_string(), fid);
            }
            // ---- M20：loader 场景（事务 allSettled；对应 TS loader-host） ----
            Step::LoaderSync { entries } => {
                self.ensure_loader(scenario)?;
                self.trace_line(&format!("loader-sync:{}", serde_json::to_string(entries).unwrap_or_default()));
                let opts: Vec<dsh_loader::EntryOptions> =
                    entries.iter().map(to_entry_options).collect();
                match self.loader.as_ref().unwrap().sync_async(&opts).await {
                    Ok(()) => {}
                    Err(e) => self.trace_line(&format!("loader-error:{}", e.errors.len())),
                }
            }
            Step::LoaderCreate { options } => {
                self.ensure_loader(scenario)?;
                self.trace_line(&format!("loader-create:{}", serde_json::to_string(options).unwrap_or_default()));
                let opts = to_entry_options(options);
                match self.loader.as_ref().unwrap().create_async(opts).await {
                    Ok(_) => {}
                    Err(_e) => self.trace_line(&format!("loader-error:{}", 1)),
                }
            }
            Step::LoaderUpdate { id, options } => {
                self.ensure_loader(scenario)?;
                self.trace_line(&format!("loader-update:{id}:{}", serde_json::to_string(options).unwrap_or_default()));
                let opts = to_entry_options(options);
                match self.loader.as_ref().unwrap().update_async(id, opts).await {
                    Ok(()) => {}
                    Err(_e) => self.trace_line(&format!("loader-error:{}", 1)),
                }
            }
            Step::LoaderRemove { id } => {
                self.ensure_loader(scenario)?;
                self.trace_line(&format!("loader-remove:{id}"));
                match self.loader.as_ref().unwrap().remove_async(id).await {
                    Ok(()) => {}
                    Err(_e) => self.trace_line(&format!("loader-error:{}", 1)),
                }
            }
            other => self.run_step(other)?,
        }
        Ok(())
    }

    /// M7：以异步编排执行整个场景（挂载走 `plugin_arc_async`）。
    pub async fn run_async(&mut self, scenario: &Scenario) -> Result<Vec<String>, CordisError> {
        let descs: Vec<(String, PluginDesc)> = scenario.plugins.clone().into_iter().collect();
        for (id, desc) in &descs {
            let plugin: Arc<dyn Plugin> = Arc::new(ScenarioPlugin {
                desc: desc.clone(),
                plugins: self.plugins.clone(),
            });
            self.plugins.borrow_mut().insert(id.clone(), plugin);
        }
        for step in &scenario.steps {
            self.run_step_async(scenario, step).await?;
        }
        Ok(self.ctx.take_trace())
    }

    fn fiber_of(&self, id: &str) -> Result<FiberId, CordisError> {
        self.fibers.get(id).copied().ok_or_else(|| {
            CordisError::Internal(format!("scenario: unknown plugin {id}"))
        })
    }

    fn mount(&mut self, id: &str) -> Result<(), CordisError> {
        self.mount_with_config(id, serde_json::json!({}))
    }

    fn mount_with_config(&mut self, id: &str, config: serde_json::Value) -> Result<(), CordisError> {
        let plugin = self
            .plugins
            .borrow()
            .get(id)
            .cloned()
            .ok_or_else(|| CordisError::Internal(format!("scenario: unknown plugin {id}")))?;
        let fid = self.ctx.plugin_arc(plugin, config)?;
        self.fibers.insert(id.to_string(), fid);
        Ok(())
    }
}

impl Default for Runner {
    fn default() -> Self {
        Self::new()
    }
}

/// 把 ApplyOp 解释为 dsh-core 插件行为的插件。
struct ScenarioPlugin {
    desc: PluginDesc,
    plugins: Rc<RefCell<HashMap<String, Arc<dyn Plugin>>>>,
}

impl ScenarioPlugin {
    fn push(&self, ctx: &Cordis, line: &str) {
        ctx.with(|rt| rt.trace.push(line.to_string()));
    }
}

impl Plugin for ScenarioPlugin {
    fn name(&self) -> &'static str {
        // 场景插件名唯一（JSON 约束）；泄漏一次换取 &'static。
        Box::leak(self.desc.name.clone().into_boxed_str())
    }

    fn inject(&self) -> &'static [&'static str] {
        // 泄漏注入表（场景级有限）
        let leaked: Vec<&'static str> = self
            .desc
            .inject
            .iter()
            .map(|s| Box::leak(s.clone().into_boxed_str()) as &'static str)
            .collect();
        Box::leak(leaked.into_boxed_slice())
    }

    fn apply(&self, ctx: &Cordis, config: serde_json::Value) -> Result<EffectOutcome, CordisError> {
        self.push(ctx, &format!("apply:{}", self.desc.name));
        let mut disposers: Vec<Disposer> = Vec::new();
        for op in &self.desc.apply {
            self.apply_op(ctx, op, &config, &mut disposers)?;
        }
        Ok(EffectOutcome::None)
    }
}

impl ScenarioPlugin {
    fn apply_op(
        &self,
        ctx: &Cordis,
        op: &ApplyOp,
        config: &serde_json::Value,
        disposers: &mut Vec<Disposer>,
    ) -> Result<(), CordisError> {
        match op {
            ApplyOp::Log { text } => self.push(ctx, &format!("log:{text}")),
            ApplyOp::LogConfig => {
                self.push(
                    ctx,
                    &format!("log-config:{}", serde_json::to_string(config).unwrap_or_default()),
                )
            }
            ApplyOp::Effect { dispose } => {
                let d = dispose.clone();
                self.push(ctx, &format!("effect-reg:{d}"));
                let disposer = ctx.effect(
                    "scenario-effect",
                    Box::new(move |_ctx| {
                        let d = d.clone();
                        Ok(EffectOutcome::One(Rc::new(move |ctx| {
                            ctx.with(|rt| rt.trace.push(format!("dispose:{d}")));
                        })))
                    }),
                )?;
                disposers.push(disposer);
            }
            ApplyOp::DisposeEffect { index } => {
                if let Some(d) = disposers.get(*index) {
                    d(ctx);
                }
            }
            ApplyOp::On { event, log } => {
                let log = log.clone();
                let event = event.clone();
                self.push(ctx, &format!("on:{event}:{log}"));
                ctx.on(
                    &event,
                    Arc::new(move |ctx, _args, _next| {
                        ctx.with(|rt| rt.trace.push(format!("log:{log}")));
                        HookResult::Continue
                    }),
                )?;
            }
            ApplyOp::OnPrepend { event, log } => {
                let log = log.clone();
                let event = event.clone();
                self.push(ctx, &format!("on-prepend:{event}:{log}"));
                ctx.on_with(
                    &event,
                    Arc::new(move |ctx, _args, _next| {
                        ctx.with(|rt| rt.trace.push(format!("log:{log}")));
                        HookResult::Continue
                    }),
                    false,
                    true,
                )?;
            }
            ApplyOp::OnReturn {
                event,
                log,
                value,
            } => {
                let log = log.clone();
                let event = event.clone();
                let value = value.clone();
                self.push(ctx, &format!("on-return:{event}:{log}"));
                ctx.on(
                    &event,
                    Arc::new(move |ctx, _args, _next| {
                        ctx.with(|rt| rt.trace.push(format!("log:{log}")));
                        HookResult::Returned(Some(value.clone()))
                    }),
                )?;
            }
            ApplyOp::OnWaterfall { event, log, after } => {
                let log = log.clone();
                let after = after.clone();
                let event = event.clone();
                self.push(ctx, &format!("on-waterfall:{event}:{log}"));
                ctx.on(
                    &event,
                    Arc::new(move |ctx, args, next| {
                        ctx.with(|rt| rt.trace.push(format!("log:{log}")));
                        let result = match next {
                            Some(n) => n(ctx, args),
                            None => None,
                        };
                        ctx.with(|rt| rt.trace.push(format!("log:{after}")));
                        HookResult::Returned(result)
                    }),
                )?;
            }
            ApplyOp::OnShort { event, log } => {
                let log = log.clone();
                let event = event.clone();
                self.push(ctx, &format!("on-short:{event}:{log}"));
                ctx.on(
                    &event,
                    Arc::new(move |ctx, _args, _next| {
                        ctx.with(|rt| rt.trace.push(format!("log:{log}")));
                        HookResult::Continue
                    }),
                )?;
            }
            ApplyOp::Provide { service, value } => {
                let service = service.clone();
                let value = value.clone();
                self.push(
                    ctx,
                    &format!("provide:{service}:{}", serde_json::to_string(&value).unwrap_or_default()),
                );
                ctx.provide(&service, Arc::new(value))?;
            }
            ApplyOp::Intercept { service, config } => {
                let service = service.clone();
                let config = config.clone();
                self.push(
                    ctx,
                    &format!(
                        "intercept:{service}:{}",
                        serde_json::to_string(&config).unwrap_or_default()
                    ),
                );
                ctx.intercept(&service, config)?;
            }
            ApplyOp::ResolveConfig { service } => {
                let merged = ctx.resolve_config(service, None, None);
                self.push(
                    ctx,
                    &format!(
                        "resolve-config:{service}:{}",
                        serde_json::to_string(&merged).unwrap_or_default()
                    ),
                );
            }
            ApplyOp::Plugin { id } => {
                let plugin = self
                    .plugins
                    .borrow()
                    .get(id)
                    .cloned()
                    .ok_or_else(|| {
                        CordisError::Internal(format!("scenario: unknown plugin {id}"))
                    })?;
                let _ = ctx.plugin_arc(plugin, serde_json::json!({}))?;
            }
        }
        Ok(())
    }
}

/// 校验 trace 与 golden 文件逐行一致；返回差异行（空 = 一致）。
pub fn diff_trace(actual: &[String], golden: &[String]) -> Vec<String> {
    let mut diffs = Vec::new();
    let n = actual.len().max(golden.len());
    for i in 0..n {
        let a = actual.get(i);
        let g = golden.get(i);
        if a != g {
            diffs.push(format!(
                "line {i}: rust={} ts={}",
                a.map(|s| format!("{s:?}")).unwrap_or_else(|| "<none>".to_string()),
                g.map(|s| format!("{s:?}")).unwrap_or_else(|| "<none>".to_string())
            ));
        }
    }
    diffs
}

/// loader 场景 entry（原始 JSON）→ `EntryOptions`（name 原样；disabled/group 透传）。
fn to_entry_options(e: &LoaderEntry) -> dsh_loader::EntryOptions {
    let id = e.get("id").and_then(|v| v.as_str()).unwrap_or_default();
    let name = e.get("name").and_then(|v| v.as_str()).unwrap_or_default();
    let mut opts = dsh_loader::EntryOptions::new(id, name);
    if let Some(cfg) = e.get("config") {
        if !cfg.is_null() {
            opts.config = cfg.clone();
        }
    }
    opts.disabled = e.get("disabled").and_then(|v| v.as_bool()).unwrap_or(false);
    opts.group = e.get("group").and_then(|v| v.as_bool()).unwrap_or(false);
    // isolate / intercept 透传（M62）：服务隔离与拦截配置原样透传，使 Rust 侧
    // 差分场景不静默丢弃 entry 的服务接线字段（与 TS 宿主 `{...e}` 透传一致）。
    opts.isolate = obj_map(e, "isolate");
    opts.intercept = obj_map(e, "intercept");
    opts
}

/// 从 loader entry JSON 中取对象字段 → `HashMap<String, Value>`。
fn obj_map(e: &LoaderEntry, key: &str) -> HashMap<String, Value> {
    e.get(key)
        .and_then(|v| v.as_object())
        .map(|o| {
            o.iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// 把可序列化值规范化为**按键字典序**的 JSON 文本（`serde_json::Value` 的
/// Object 默认是 `BTreeMap`）。与 TS 宿主 `canonical`（`Object.keys().sort()`）
/// 对齐——include 差分场景的 `data`/`result` 行。
fn sorted_json<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(&serde_json::to_value(v).unwrap_or_default()).unwrap_or_default()
}

/// M63：include 差分场景——纯函数级 `apply_entry_patches` 对比（无 Fiber）。
///
/// 场景 JSON：`{ "data": [entry...], "patches": [patch...] }`。执行
/// `apply_entry_patches_with_warn`（对齐 TS `applyEntryPatches(data, patches,
/// warn)`），输出规范化 trace：
/// - `include-data:{json(data)}`    输入 entry 列表（按键序）
/// - `include-warn:{message}`        每条 warn，按序（`%C` 已展开）
/// - `include-result:{json(out)}`    最终 entry 列表（按键序）
pub fn run_include(text: &str) -> Result<Vec<String>, String> {
    #[derive(Debug, Deserialize)]
    struct Inc {
        #[serde(default)]
        data: Vec<dsh_loader::EntryOptions>,
        #[serde(default)]
        patches: Vec<dsh_loader::Patch>,
    }
    let inc: Inc = serde_json::from_str(text).map_err(|e| format!("include parse: {e}"))?;
    let mut warns: Vec<String> = Vec::new();
    let out = dsh_loader::apply_entry_patches_with_warn(&inc.data, &inc.patches, &mut |w| {
        warns.push(w);
    });
    let mut lines = Vec::new();
    lines.push(format!("include-data:{}", sorted_json(&inc.data)));
    for w in warns {
        lines.push(format!("include-warn:{w}"));
    }
    lines.push(format!("include-result:{}", sorted_json(&out)));
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION_01_JSON: &str = r#"{
      "name": "session-01-simple",
      "plugins": {},
      "steps": [
        { "op": "session-create", "id": "default" },
        { "op": "session-append", "id": "default", "kind": "user/message", "surface": "append", "data": {"text": "hello", "role": "user"} },
        { "op": "session-append", "id": "default", "kind": "tool/call", "data": {"tool": "todo_write", "arguments": {"content": "x"}} },
        { "op": "session-append", "id": "default", "kind": "assistant/message", "surface": "append", "data": { "role": "assistant", "text": "done"} },
        { "op": "session-events", "id": "default" }
      ]
    }"#;

    /// M6 step10（D-090）：session 差分对齐——dsh-session 真实 store 的事件模型
    /// （create / append seq=log 长度 / 事件回读）与共享参考契约逐字节一致（TS
    /// session-host.mjs 镜像同契约；golden 冻结）。canonical 键序：serde_json 默认
    /// BTreeMap（与 `sorted_json` 一致）。红：Step 无 session 变体 → 解析失败。
    #[test]
    fn session_scenario_trace_aligns_contract() {
        let scenario: Scenario = serde_json::from_str(SESSION_01_JSON).expect("scenario parses");
        let mut runner = Runner::new();
        let trace = runner.run(&scenario).expect("session scenario runs");
        let expected = [
            "session-create:default",
            "session-append:default:0:user/message",
            "session-append:default:1:tool/call",
            "session-append:default:2:assistant/message",
            "session-event-read:default:0:user/message:{\"role\":\"user\",\"text\":\"hello\"}",
            "session-event-read:default:1:tool/call:{\"arguments\":{\"content\":\"x\"},\"tool\":\"todo_write\"}",
            "session-event-read:default:2:assistant/message:{\"role\":\"assistant\",\"text\":\"done\"}",
        ];
        assert_eq!(trace, expected.to_vec());
    }
}

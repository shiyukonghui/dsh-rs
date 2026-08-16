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
}

/// 场景解释器（Rust 侧）。
pub struct Runner {
    pub ctx: Cordis,
    plugins: Rc<RefCell<HashMap<String, Arc<dyn Plugin>>>>,
    fibers: HashMap<String, FiberId>,
}

impl Runner {
    pub fn new() -> Self {
        Runner {
            ctx: Cordis::new(),
            plugins: Rc::new(RefCell::new(HashMap::new())),
            fibers: HashMap::new(),
        }
    }

    fn trace_line(&self, line: &str) {
        self.ctx.with(|rt| rt.trace.push(line.to_string()));
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
        }
        Ok(())
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

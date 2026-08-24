//! ToolRuntime 注册表 + 执行管线（镜像 TS `@deepseek-ai/dsh-tools/index.ts` 的
//! 注册/遮蔽/限制/呈现/执行；依赖 dsh-scope 的 `ScopedLayers` + `NamedEntries`）。
//!
//! M2b-2 交付的可验证语义：
//! - `register`（output 子集断言 / timeoutMs / run_code 保留名 / 重复检测，全局与
//!   scoped 两种消息）、`get`/`schemas`/`known_names`（view 解析：全局基 + 祖先链
//!   遮蔽 + 全链限制交集 + 自有层覆盖 + run_code 注入）。
//! - `restrict`/`present_as`/`add_guard`（逐字消息 + 基于层的作用域隔离）。
//! - `execution_mode`（fail-closed：仅显式 `true` → parallel）。
//! - `execute`：resolve → guards → body → output 校验（`ToolOutputError`）→ render →
//!   finalize → 取消合成（`ABORTED`/`ABORTED_BEFORE_DISPATCH`）。
//!
//! 差异声明（D-024/D-028/D-034，见 DECISIONS.md）：TS 的 Cordis waterfall 阶段
//! （`tools/pre-execute`/`tools/post-execute`）在同步侧以 pre-decision 钩子表达 pre 阶段、
//! 审批通道以同步决策者注入（M2f 已接）；post-execute 仍留宿主（M3）。Code Mode 传输
//! （run_code）依赖 dsh-code-runtime（M5），本轮注入占位；`execute` 对 collapsed 名给出
//! 精确路由错误。

use crate::json_schema::validate_json_schema_value;
use crate::types::*;
use dsh_scope::{NamedEntries, ScopeKey, ScopedLayers};
use serde_json::Value;
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;

/// 执行模式（TS `ToolExecutionMode`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolExecutionMode {
    Native,
    Code,
    Both,
}

impl ToolExecutionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ToolExecutionMode::Native => "native",
            ToolExecutionMode::Code => "code",
            ToolExecutionMode::Both => "both",
        }
    }

    pub fn parse(s: &str) -> Option<ToolExecutionMode> {
        match s {
            "native" => Some(ToolExecutionMode::Native),
            "code" => Some(ToolExecutionMode::Code),
            "both" => Some(ToolExecutionMode::Both),
            _ => None,
        }
    }
}

/// fail-closed 执行分类（对齐 TS `executionMode`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolExecutionClass {
    Parallel,
    Exclusive,
}

/// 一个 effect 返回的拆除器。
pub type ToolDisposer = Rc<dyn Fn()>;

/// guard 谓词：`(name, args)` → 拒绝理由（None = 放行）。单调拒绝（无 allow 结果）。
pub type ToolGuard = Rc<dyn Fn(&str, &Value) -> Option<String>>;

/// pre-dispatch 决策（对齐 TS `PreToolDecision`）：`allow` 放行、`deny` 物化错误、
/// `ask` 仅在审批通道返回 `allowed-once` 后才放行、否则按原因拒绝。
#[derive(Debug, Clone, PartialEq)]
pub enum PreToolDecision {
    Allow,
    Deny { reason: String },
    Ask { reason: Option<String> },
}

/// 审批通道的一次性结果（对齐 TS `ApprovalOutcome` 四态）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalOutcome {
    /// 本次放行一次（工具仍受 guards 约束）。
    AllowedOnce,
    Rejected,
    Cancelled,
    Unavailable,
}

/// pre-execute 等价钩子：返回 None = 放行（delegate 到 allow）；首个非 None 即最终决策。
pub type ToolPreDecision = Rc<dyn Fn(&ToolExecution) -> Option<PreToolDecision>>;

/// 审批决策者（同步；宿主以回调把「ask」解析为 allow/deny——UI 往返在 loop 之外，M3）。
pub type ApprovalProvider = Rc<dyn Fn(&ToolExecution, Option<&str>) -> ApprovalOutcome>;

/// 限制谓词（allow/deny ReadonlySet；`admits` 语义对齐 TS）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolRestriction {
    /// Some(集合) = 白名单（空集合 = 拒绝全部）。
    pub allow: Option<BTreeSet<String>>,
    /// Some(集合) = 黑名单。
    pub deny: Option<BTreeSet<String>>,
}

impl ToolRestriction {
    fn admits(&self, name: &str) -> bool {
        let in_allow = self.allow.as_ref().is_none_or(|a| a.contains(name));
        let in_deny = self.deny.as_ref().is_some_and(|d| d.contains(name));
        in_allow && !in_deny
    }

    pub fn allow(names: &[&str]) -> ToolRestriction {
        ToolRestriction {
            allow: Some(names.iter().map(|s| s.to_string()).collect()),
            deny: None,
        }
    }

    pub fn deny(names: &[&str]) -> ToolRestriction {
        ToolRestriction {
            allow: None,
            deny: Some(names.iter().map(|s| s.to_string()).collect()),
        }
    }
}

/// 一个作用域的工具层（对齐 TS `ToolLayer implements ScopeLayer`）。
///
/// 可变字段用 `Rc<RefCell<...>>`（dsh-scope philosophy：effect 的 action 与 undo
/// 都只拿 `&L`，经内部共享句柄写入/回滚）。
pub struct ToolLayer {
    tools: NamedEntries<Rc<ToolDefinition>>,
    mode: Rc<RefCell<Option<ToolExecutionMode>>>,
    restrictions: Rc<RefCell<Vec<ToolRestriction>>>,
    guards: Rc<RefCell<Vec<ToolGuard>>>,
    pre_decisions: Rc<RefCell<Vec<ToolPreDecision>>>,
}

impl ToolLayer {
    fn new() -> Self {
        ToolLayer {
            tools: NamedEntries::new(|name| format!("tool \"{name}\" is already registered")),
            mode: Rc::new(RefCell::new(None)),
            restrictions: Rc::new(RefCell::new(Vec::new())),
            guards: Rc::new(RefCell::new(Vec::new())),
            pre_decisions: Rc::new(RefCell::new(Vec::new())),
        }
    }

    fn admits(&self, name: &str) -> bool {
        self.restrictions.borrow().iter().all(|r| r.admits(name))
    }
}

impl dsh_scope::ScopeLayer for ToolLayer {
    fn is_empty(&self) -> bool {
        self.tools.is_empty()
            && self.mode.borrow().is_none()
            && self.restrictions.borrow().is_empty()
            && self.guards.borrow().is_empty()
    }
}

/// 一次执行的输入（对齐 TS `ToolExecutionInput` 核心字段）。
#[derive(Debug, Clone)]
pub struct ToolExecutionInput {
    pub call_id: String,
    pub root_call_id: String,
    pub name: String,
    pub agent: Option<String>,
    pub arguments: Value,
    pub parent: Option<String>,
    pub signal: ToolSignal,
}

impl ToolExecutionInput {
    pub fn new(
        call_id: impl Into<String>,
        name: impl Into<String>,
        arguments: Value,
        agent: Option<String>,
    ) -> Self {
        let call_id = call_id.into();
        let root_call_id = call_id.clone();
        ToolExecutionInput {
            call_id,
            root_call_id,
            name: name.into(),
            agent,
            arguments,
            parent: None,
            signal: ToolSignal::new(),
        }
    }
}

/// 结果里的错误信息（对齐 TS `ToolErrorInfo`：message + 可选 info）。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolErrorInfo {
    pub message: String,
    pub info: Option<ToolFailureData>,
}

/// 规范化执行结果（对齐 TS `ToolExecutionResult` 的核心产物字段）。
#[derive(Debug, Clone)]
pub struct ToolExecutionResult {
    pub execution: ToolExecution,
    /// 验证过的 canonical 值（成功时）；失败 None。
    pub value: Option<Value>,
    /// 已渲染的 model-facing 内容块（materialized）。
    pub content: Vec<ContentBlock>,
    pub content_annotation: Option<ContentBlock>,
    pub is_error: bool,
    pub error: Option<ToolErrorInfo>,
    pub additional_contexts: Vec<Value>,
    /// `concludesTurn` 标记：携带者在其步骤结束时关停 turn（M2e-3 调度读取）。
    pub concludes_turn: bool,
}

/// 视图结果：可见工具（保插入序）+ 限制前全名集 + 可限制名（全局层名）。
pub struct ToolView {
    pub visible: Vec<(String, Rc<ToolDefinition>)>,
    pub known_names: BTreeSet<String>,
    pub restrictable_names: Vec<String>,
}

impl ToolView {
    pub fn get(&self, name: &str) -> Option<&Rc<ToolDefinition>> {
        self.visible.iter().find(|(n, _)| n == name).map(|(_, d)| d)
    }
}

/// 工具注册表（Rust 面 `ToolRuntime`）。
pub struct ToolRegistry {
    layers: ScopedLayers<ToolLayer>,
    default_mode: ToolExecutionMode,
    on_change: Rc<dyn Fn()>,
    /// 审批决策者（全局，M2f；TS `ctx.get('approval')` 等价——缺省 = 无通道，ask 即拒绝）。
    approval: Rc<RefCell<Option<ApprovalProvider>>>,
    /// Code Mode run_code 注入传输的 execute 覆盖（M5 真实执行；缺省 = 占位桩，D-073）。
    run_code_executor: Rc<RefCell<Option<ToolExecute>>>,
}

impl ToolRegistry {
    pub fn new(default_mode: ToolExecutionMode) -> Self {
        Self::with_change(default_mode, || {})
    }

    pub fn with_change(default_mode: ToolExecutionMode, on_change: impl Fn() + 'static) -> Self {
        let on_change: Rc<dyn Fn()> = Rc::new(on_change);
        let change = Rc::clone(&on_change);
        ToolRegistry {
            layers: ScopedLayers::new(|_| ToolLayer::new(), move || change()),
            default_mode,
            on_change,
            approval: Rc::new(RefCell::new(None)),
            run_code_executor: Rc::new(RefCell::new(None)),
        }
    }

    // -----------------------------------------------------------------------
    // 注册
    // -----------------------------------------------------------------------

    /// 注册一个工具；返回 disposer。`scope`：`None` → 全局；`Some(k)` → 该作用域层。
    pub fn register(
        &self,
        def: Rc<ToolDefinition>,
        scope: Option<&ScopeKey>,
    ) -> Result<ToolDisposer, String> {
        // 1. output schema 重新断言（防御 raw 构造的 definition）。
        if let Err(e) = crate::json_schema::assert_supported_json_schema(&def.output.schema.to_json()) {
            return Err(format!(
                "tool \"{}\" must declare output {{ schema, render, presentationMeta? }} ({e})",
                def.name
            ));
        }
        // 2. timeoutMs
        if let Some(t) = def.timeout_ms {
            if !t.is_finite() || t <= 0.0 {
                return Err(format!(
                    "tool \"{}\" timeoutMs must be a positive finite number",
                    def.name
                ));
            }
        }
        // 3. run_code 无条件保留
        if def.name == RUN_CODE_NAME {
            return Err(format!(
                "tool name \"{RUN_CODE_NAME}\" is reserved for the Code Mode presentation transport and cannot be registered or shadowed"
            ));
        }
        // 4. 重复预检（单线程、无竞争）
        let dup = match scope {
            Some(k) => self.layers.peek(Some(k)).is_some_and(|l| l.tools.has(&def.name)),
            None => self.layers.global().tools.has(&def.name),
        };
        if dup {
            return Err(match scope {
                None => format!(
                    "tool \"{}\" is already registered (for a per-agent variant, register through that agent's `agent.ctx` instead)",
                    def.name
                ),
                Some(_) => format!("tool \"{}\" is already registered in this scope", def.name),
            });
        }
        let name = def.name.clone();
        let disposer = self.layers.effect(
            scope,
            move |layer| {
                layer
                    .tools
                    .insert(&name, def.clone())
                    .unwrap_or_else(|_| unreachable!("pre-checked duplicate"))
            },
            "tools.register()",
            true,
        );
        (self.on_change)();
        Ok(disposer)
    }

    /// 注册到全局层。
    pub fn register_global(&self, def: Rc<ToolDefinition>) -> Result<ToolDisposer, String> {
        self.register(def, None)
    }

    /// 覆盖 Code Mode run_code 注入传输的 execute（M5 真实执行；D-073）。命名/schema
    /// 注入与保留名守卫不变；返回先前覆盖（None = 之前是占位桩）。全局幂等设置。
    pub fn set_run_code_executor(&self, executor: ToolExecute) -> Option<ToolExecute> {
        self.run_code_executor.borrow_mut().replace(executor)
    }

    // -----------------------------------------------------------------------
    // 视图解析
    // -----------------------------------------------------------------------

    /// 视图：全局基 + 祖先链遮蔽 → 全链限制交集过滤 → 自有层覆盖 → run_code 注入。
    fn view(&self, scope: Option<&ScopeKey>) -> ToolView {
        let mut visible: Vec<(String, Rc<ToolDefinition>)> = Vec::new();
        let mut known: BTreeSet<String> = BTreeSet::new();
        let put = |visible: &mut Vec<(String, Rc<ToolDefinition>)>, name: &str, def: Rc<ToolDefinition>| {
            match visible.iter_mut().find(|(n, _)| n == name) {
                Some(slot) => slot.1 = def,
                None => visible.push((name.to_string(), def)),
            }
        };

        // 全链（全局 + 祖先 + 自有）限制交集：过滤「继承」名用。
        let full_chain = self.layers.chain_layers(scope);
        let admits_all = |name: &str| {
            self.layers.global().admits(name) && full_chain.iter().all(|l| l.admits(name))
        };
        // 祖先链（远→近；不含自有 scope）。**注意**：chain_layers 只含「有层」的
        // scope——当查询 scope 自身无层时（如 agent scope 无工具、父 standing scope
        // 有），不得 pop 掉祖先（P3-a 回归：standing 工具遮蔽曾因此丢失）。
        let own_layer = self.layers.peek(scope);
        let mut ancestors = full_chain.clone();
        if own_layer.is_some() {
            ancestors.pop();
        }

        // 1. 全局层（基，插入序）
        for (name, def) in self.layers.global().tools.entries() {
            known.insert(name.clone());
            if admits_all(&name) {
                put(&mut visible, &name, def);
            }
        }
        // 2. 祖先链遮蔽（最近同名覆盖更远；仍受全链限制过滤）
        for layer in &ancestors {
            for (name, def) in layer.tools.entries() {
                known.insert(name.clone());
                if admits_all(&name) {
                    put(&mut visible, &name, def);
                }
            }
        }
        // 3. 自有层覆盖（免过滤）
        if let Some(own) = self.layers.peek(scope) {
            for (name, def) in own.tools.entries() {
                known.insert(name.clone());
                put(&mut visible, &name, def);
            }
        }
        // 4. 非 native 呈现注入 run_code 传输（占位实现；Code Mode 属 M5）。宿主可
        //    `set_run_code_executor` 覆盖 execute 为真实执行（D-073）。
        if self.mode_for(scope) != ToolExecutionMode::Native {
            known.insert(RUN_CODE_NAME.to_string());
            let def = match self.run_code_executor.borrow().as_ref() {
                Some(exec) => run_code_def(exec.clone()),
                None => placeholder_run_code(),
            };
            put(&mut visible, RUN_CODE_NAME, Rc::new(def));
        }

        let restrictable_names = self
            .layers
            .global()
            .tools
            .entries()
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        ToolView {
            visible,
            known_names: known,
            restrictable_names,
        }
    }

    pub fn get(&self, name: &str, scope: Option<&ScopeKey>) -> Option<Rc<ToolDefinition>> {
        self.view(scope).get(name).cloned()
    }

    /// 模型可见 schema 投影（allowlist：仅 name/description/parameters；不排序）。
    pub fn schemas(&self, scope: Option<&ScopeKey>) -> Vec<dsh_llm::ToolSchema> {
        self.view(scope)
            .visible
            .iter()
            .map(|(_, def)| def.to_tool_schema())
            .collect()
    }

    /// 限制前全名集（已知可见名）。
    pub fn known_names(&self, scope: Option<&ScopeKey>) -> Vec<String> {
        self.view(scope).known_names.into_iter().collect()
    }

    /// 当前全局层可限制名（restrict 的已知名集）。
    pub fn restrictable_names(&self) -> Vec<String> {
        self.layers
            .global()
            .tools
            .entries()
            .into_iter()
            .map(|(n, _)| n)
            .collect()
    }

    /// 作用域链近→远第一个声明的 mode；否则默认。
    pub fn mode_for(&self, scope: Option<&ScopeKey>) -> ToolExecutionMode {
        if let Some(key) = scope {
            let chain = dsh_scope::scope_chain_of(Some(key));
            for k in &chain {
                if let Some(l) = self.layers.peek(Some(k)) {
                    if let Some(m) = *l.mode.borrow() {
                        return m;
                    }
                }
            }
        }
        self.default_mode
    }

    // -----------------------------------------------------------------------
    // restrict / presentAs / guards
    // -----------------------------------------------------------------------

    pub fn restrict(
        &self,
        restriction: ToolRestriction,
        scope: &ScopeKey,
    ) -> Result<ToolDisposer, String> {
        if restriction.allow.is_none() && restriction.deny.is_none() {
            return Err("tools.restrict({}) is a no-op: pass `allow` and/or `deny` (an empty filter is almost always a materialized-empty-config bug)".to_string());
        }
        let names: BTreeSet<String> = restriction
            .allow
            .iter()
            .flat_map(|a| a.iter())
            .chain(restriction.deny.iter().flat_map(|d| d.iter()))
            .cloned()
            .collect();
        if names.contains(RUN_CODE_NAME) {
            return Err(format!(
                "tools.restrict() cannot name reserved Code Mode presentation transport \"{RUN_CODE_NAME}\"; restrict end-capability tools instead"
            ));
        }
        let known_global: BTreeSet<String> = self.restrictable_names().into_iter().collect();
        let unknown: Vec<&String> = names
            .iter()
            .filter(|n| !known_global.contains(*n))
            .collect();
        if !unknown.is_empty() {
            let mut sorted = unknown.clone();
            sorted.sort();
            let listed = sorted
                .iter()
                .map(|s| format!("\"{s}\""))
                .collect::<Vec<_>>()
                .join(", ");
            let noun = if sorted.len() == 1 { "tool" } else { "tools" };
            let known_list = if known_global.is_empty() {
                "(none)".to_string()
            } else {
                let mut k: Vec<&String> = known_global.iter().collect();
                k.sort();
                k.into_iter().map(|s| s.to_string()).collect::<Vec<_>>().join(", ")
            };
            return Err(format!(
                "tools.restrict() names unknown global {noun} {listed}; known global tools: {known_list}"
            ));
        }
        // 追加限制（经内部共享句柄；不随注册变更广播）
        let disposer = self.layers.effect(
            Some(scope),
            move |layer| {
                let restrictions = layer.restrictions.clone();
                let mut buf = restrictions.borrow_mut();
                buf.push(restriction.clone());
                let idx = buf.len() - 1;
                drop(buf);
                Rc::new(move || {
                    let mut buf = restrictions.borrow_mut();
                    if idx < buf.len() {
                        buf.remove(idx);
                    }
                })
            },
            "tools.restrict()",
            false,
        );
        (self.on_change)();
        Ok(disposer)
    }

    pub fn present_as(
        &self,
        mode: ToolExecutionMode,
        scope: &ScopeKey,
    ) -> Result<ToolDisposer, String> {
        let existing_mode = self.layers.peek(Some(scope)).and_then(|l| *l.mode.borrow());
        if let Some(other) = existing_mode {
            return Err(format!(
                "tools.presentAs(\"{}\") conflicts with \"{}\" already declared for this scope; one composition selects one presentation",
                mode.as_str(),
                other.as_str()
            ));
        }
        let disposer = self.layers.effect(
            Some(scope),
            move |layer| {
                *layer.mode.borrow_mut() = Some(mode);
                let mode_handle = layer.mode.clone();
                Rc::new(move || {
                    *mode_handle.borrow_mut() = None;
                })
            },
            "tools.presentAs()",
            true,
        );
        (self.on_change)();
        Ok(disposer)
    }

    /// 追加一个 guard（同步 pre 决策谓词）。
    pub fn add_guard(
        &self,
        guard: ToolGuard,
        scope: Option<&ScopeKey>,
    ) -> Result<ToolDisposer, String> {
        let disposer = self.layers.effect(
            scope,
            move |layer| {
                let guards = layer.guards.clone();
                let mut buf = guards.borrow_mut();
                buf.push(guard.clone());
                let idx = buf.len() - 1;
                drop(buf);
                Rc::new(move || {
                    let mut buf = guards.borrow_mut();
                    if idx < buf.len() {
                        buf.remove(idx);
                    }
                })
            },
            "tools.guard()",
            false,
        );
        Ok(disposer)
    }

    /// 追加一个 pre-execute 等价决策钩子（对齐 `tools/pre-execute` waterfall）。
    /// `None` = 放行（delegate 到 allow）；首个非 None 即最终决策。
    pub fn add_pre_decision(
        &self,
        pre: ToolPreDecision,
        scope: Option<&ScopeKey>,
    ) -> Result<ToolDisposer, String> {
        let disposer = self.layers.effect(
            scope,
            move |layer| {
                let pre_decisions = layer.pre_decisions.clone();
                let mut buf = pre_decisions.borrow_mut();
                buf.push(pre.clone());
                let idx = buf.len() - 1;
                drop(buf);
                Rc::new(move || {
                    let mut buf = pre_decisions.borrow_mut();
                    if idx < buf.len() {
                        buf.remove(idx);
                    }
                })
            },
            "tools.preDecision()",
            false,
        );
        Ok(disposer)
    }

    /// 设置审批决策者（`None` 清除通道——ask 退化为「not yet supported」拒绝）。
    /// 返回被替换的前值（便于宿主回滚组合）。
    pub fn set_approval_provider(&self, provider: Option<ApprovalProvider>) -> Option<ApprovalProvider> {
        std::mem::replace(&mut *self.approval.borrow_mut(), provider)
    }

    /// 当前审批决策者。
    pub fn approval_provider(&self) -> Option<ApprovalProvider> {
        self.approval.borrow().clone()
    }

    // -----------------------------------------------------------------------
    // executionMode / execute
    // -----------------------------------------------------------------------

    /// fail-closed 执行分类。
    pub fn execution_mode(
        &self,
        input: &ToolExecutionInput,
        scope: Option<&ScopeKey>,
    ) -> ToolExecutionClass {
        let Some(def) = self.get(&input.name, scope) else {
            return ToolExecutionClass::Exclusive;
        };
        if self.collapses(&input.name, scope) {
            return ToolExecutionClass::Exclusive;
        }
        match &def.is_concurrency_safe {
            Some(f) => {
                let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    f(&input.arguments)
                }));
                match ok {
                    Ok(true) => ToolExecutionClass::Parallel,
                    _ => ToolExecutionClass::Exclusive,
                }
            }
            None => ToolExecutionClass::Exclusive,
        }
    }

    /// collapsed：模式 code 且名字不是 run_code。
    fn collapses(&self, name: &str, scope: Option<&ScopeKey>) -> bool {
        self.mode_for(scope) == ToolExecutionMode::Code && name != RUN_CODE_NAME
    }

    /// 一次同步执行（M2b-2 管线；approval 接线在 M2f 接入 pre 阶段）。
    pub fn execute(
        &self,
        input: &ToolExecutionInput,
        scope: Option<&ScopeKey>,
    ) -> ToolExecutionResult {
        let exec = ToolExecution {
            call: ToolRunContext {
                call_id: input.call_id.clone(),
                root_call_id: input.root_call_id.clone(),
                name: input.name.clone(),
                agent: input.agent.clone(),
                signal: input.signal.clone(),
                concludes_turn: std::cell::Cell::new(false),
            },
            args: input.arguments.clone(),
        };
        let mut result = self.execute_inner(&exec, scope);
        result.execution = exec;
        result
    }

    fn execute_inner(
        &self,
        exec: &ToolExecution,
        scope: Option<&ScopeKey>,
    ) -> ToolExecutionResult {
        let name = exec.call.name.clone();
        let def = match self.view(scope).get(&name).cloned() {
            Some(d) => d,
            None => return self.tool_error_result(exec, tool_not_found(&name, None)),
        };

        // collapsed（code 模式下除 run_code 外的直接调用）
        if self.collapses(&name, scope) {
            if exec.call.signal.aborted() {
                return self.aborted_before_dispatch_result(exec);
            }
            let data = tool_not_found(
                &name,
                Some(format!(
                    "only `run_code` is callable directly — call `{name}` from inside a `run_code` program instead"
                )),
            );
            return self.tool_error_result(exec, data);
        }

        if exec.call.signal.aborted() {
            return self.aborted_before_dispatch_result(exec);
        }

        // pre-phase（对齐 prepareExecution）：pre-execute 决策（waterfall 到 allow）→
        // ask 经审批通道解析 → deny 物化为错误结果 → guards（单调拒绝）→ dispatch。
        let decided = match self.first_pre_decision(exec, scope) {
            PreToolDecision::Allow => None,
            PreToolDecision::Deny { reason } => Some(reason),
            PreToolDecision::Ask { reason } => self.resolve_approval(exec, reason),
        };
        if let Some(reason) = decided {
            return self.post_blocked_result(exec, &reason);
        }
        if let Some(reason) = self.guard_reason(&name, &exec.args, scope) {
            return self.post_blocked_result(exec, &reason);
        }

        // body
        let call_ctx = ToolRunContext {
            call_id: exec.call.call_id.clone(),
            root_call_id: exec.call.root_call_id.clone(),
            name: name.clone(),
            agent: exec.call.agent.clone(),
            signal: exec.call.signal.clone(),
            concludes_turn: std::cell::Cell::new(false),
        };
        // M3e（timeout-policy 最小 executor 路径）：声明了 timeoutMs 的工具按
        // wall-clock 判定（同步执行无可抢占信号——对齐 TS `deadline`+`exec.signal`
        // 的诚实降级，见 DECISIONS）。body 返回后若 elapsed >= 预算 → 用 TOOL_TIMEOUT
        // 结构化结果替换工具自身结果（独有 code 防嵌套外层误读）。
        use std::time::Instant;
        let started = Instant::now();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            (def.execute)(&exec.args, &call_ctx)
        }));
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let body = match outcome {
            Ok(Ok(value)) => value,
            Ok(Err(failure)) => return self.tool_error_result(exec, failure),
            Err(_) => {
                return self.tool_error_result(
                    exec,
                    ToolFailureData::new(
                        "tool panicked during execution",
                        CODE_INVALID_TOOL_OUTPUT,
                        "Error",
                    ),
                );
            }
        };
        // 超时替换（自己的 timer 胜出 → 无论工具返回什么都换掉；对齐 TS）。
        if crate::guard::timeout_exceeded(def.timeout_ms, elapsed_ms) {
            return crate::guard::tool_timeout_result(exec, def.timeout_ms.unwrap() as u64);
        }

        // 取消于 body 后 → ABORTED
        if exec.call.signal.aborted() {
            return self.aborted_result(exec);
        }

        // output 校验 + render
        let (value, content) = match create_success_result(exec, &def, &body) {
            Ok(pair) => pair,
            Err(failure) => return self.tool_error_result(exec, failure),
        };

        // finalize（content 总变换；抛错即保留原内容）
        let snapshot = ToolExecutionSnapshot {
            value: value.clone(),
            content: content.clone(),
            content_annotation: None,
        };
        let content = match &def.finalize_content {
            Some(f) => std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                f(exec, &snapshot)
            }))
            .ok()
            .flatten()
            .unwrap_or(content),
            None => content,
        };

        ToolExecutionResult {
            execution: exec.clone(),
            value: Some(value),
            content,
            content_annotation: None,
            is_error: false,
            error: None,
            additional_contexts: Vec::new(),
            concludes_turn: call_ctx.concludes_turn(),
        }
    }

    /// guards 拒绝理由（全局 + 祖先 + 自有层，同步谓词；None = 放行）。
    fn guard_reason(&self, name: &str, args: &Value, scope: Option<&ScopeKey>) -> Option<String> {
        let chain = self.layers.chain_layers(scope);
        for layer in std::iter::once(self.layers.global()).chain(chain.iter().map(|l| l.as_ref())) {
            for g in layer.guards.borrow().iter() {
                if let Some(reason) = g(name, args) {
                    return Some(reason);
                }
            }
        }
        None
    }

    /// 首个非 None 的 pre-execute 决策（waterfall 到 allow：None = delegate）。
    fn first_pre_decision(&self, exec: &ToolExecution, scope: Option<&ScopeKey>) -> PreToolDecision {
        let chain = self.layers.chain_layers(scope);
        for layer in std::iter::once(self.layers.global()).chain(chain.iter().map(|l| l.as_ref())) {
            for pre in layer.pre_decisions.borrow().iter() {
                if let Some(decision) = pre(exec) {
                    return decision;
                }
            }
        }
        PreToolDecision::Allow
    }

    /// 解析 `ask` → allow/deny（对齐 `serviceAsk` 的逐字拒绝原因）。sync 差值：
    /// `approvalCancelled` 驱动的 aborted-before-dispatch 分支无中流抢占不可达（D-034）；
    /// 取消态仍以其逐字拒绝原因物化错误结果。
    fn resolve_approval(
        &self,
        exec: &ToolExecution,
        ask_reason: Option<String>,
    ) -> Option<String> {
        let name = exec.call.name.clone();
        let Some(provider) = self.approval_provider() else {
            return Some(ask_reason.unwrap_or_else(|| {
                format!("tool \"{name}\" requires approval (not yet supported)")
            }));
        };
        if exec.call.agent.is_none() {
            return Some(format!(
                "tool \"{name}\" requires approval, but the call has no agent to route it through"
            ));
        }
        match provider(exec, ask_reason.as_deref()) {
            ApprovalOutcome::AllowedOnce => None,
            ApprovalOutcome::Rejected => Some(format!("the user rejected tool \"{name}\"")),
            ApprovalOutcome::Cancelled => {
                Some(format!("approval for tool \"{name}\" was cancelled"))
            }
            ApprovalOutcome::Unavailable => Some(format!(
                "tool \"{name}\" requires approval, but no approval channel is available"
            )),
        }
    }

    // -----------------------------------------------------------------------
    // 失败/取消结果构造
    // -----------------------------------------------------------------------

    fn tool_error_result(&self, exec: &ToolExecution, failure: ToolFailureData) -> ToolExecutionResult {
        let info = ToolErrorInfo {
            message: failure.message.clone(),
            info: Some(failure),
        };
        ToolExecutionResult {
            execution: exec.clone(),
            value: None,
            content: vec![ContentBlock::text(format!("Error: {}", info.message))],
            content_annotation: None,
            is_error: true,
            error: Some(info),
            additional_contexts: Vec::new(),
            concludes_turn: false,
        }
    }

    fn aborted_before_dispatch_result(&self, exec: &ToolExecution) -> ToolExecutionResult {
        self.aborted_result_with(
            exec,
            ToolFailureData::new(
                "tool call aborted before dispatch",
                TOOL_ABORTED_BEFORE_DISPATCH,
                "AbortError",
            ),
        )
    }

    fn aborted_result(&self, exec: &ToolExecution) -> ToolExecutionResult {
        self.aborted_result_with(
            exec,
            ToolFailureData::new("tool call aborted", TOOL_ABORTED, "AbortError"),
        )
    }

    fn aborted_result_with(&self, exec: &ToolExecution, info: ToolFailureData) -> ToolExecutionResult {
        ToolExecutionResult {
            execution: exec.clone(),
            value: None,
            content: vec![ContentBlock::text(format!("Error: {}", info.message))],
            content_annotation: None,
            is_error: true,
            error: Some(ToolErrorInfo {
                message: info.message.clone(),
                info: Some(info),
            }),
            additional_contexts: Vec::new(),
            concludes_turn: false,
        }
    }

    /// 阻断结果（guard 拒绝 reason）。
    fn post_blocked_result(&self, exec: &ToolExecution, reason: &str) -> ToolExecutionResult {
        ToolExecutionResult {
            execution: exec.clone(),
            value: None,
            content: vec![ContentBlock::text(format!("Error: {reason}"))],
            content_annotation: None,
            is_error: true,
            error: Some(ToolErrorInfo {
                message: reason.to_string(),
                info: None,
            }),
            additional_contexts: Vec::new(),
            concludes_turn: false,
        }
    }
}

/// 成功规范：校验 output → render；任何问题 → ToolOutputError 数据。
fn create_success_result(
    exec: &ToolExecution,
    def: &ToolDefinition,
    value: &Value,
) -> Result<(Value, Vec<ContentBlock>), ToolFailureData> {
    let violations = validate_json_schema_value(&def.output.schema, value, "value");
    if !violations.is_empty() {
        return Err(ToolFailureData::new(
            format!(
                "tool \"{}\" returned invalid output: {}",
                def.name,
                violations.join("; ")
            ),
            CODE_INVALID_TOOL_OUTPUT,
            "ToolOutputError",
        ));
    }
    let blocks = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        (def.output.render)(&exec.args, value)
    }));
    let content = match blocks {
        Ok(b) => b,
        Err(payload) => {
            let msg = panic_message(&payload);
            return Err(ToolFailureData::new(
                format!(
                    "tool \"{}\" returned invalid output: output.render failed: {msg}",
                    def.name
                ),
                CODE_INVALID_TOOL_OUTPUT,
                "ToolOutputError",
            ));
        }
    };
    Ok((value.clone(), content))
}

fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<unprintable thrown value>".to_string()
    }
}

/// `unknown tool "<name>"` 数据（对齐 TS `ToolNotFoundError`；collapsed 加路由后缀）。
fn tool_not_found(name: &str, reachable_from: Option<String>) -> ToolFailureData {
    let message = match reachable_from {
        Some(r) => format!("unknown tool \"{name}\": {r}"),
        None => format!("unknown tool \"{name}\""),
    };
    ToolFailureData::new(message, CODE_UNKNOWN_TOOL, "ToolNotFoundError")
}

/// run_code 占位传输（Code Mode runtime 属 M5；执行给精确错误）。
fn placeholder_run_code() -> ToolDefinition {
    let msg = "dsh-tools: mode \"code\" requires a code runtime — load a ctx.codeRuntime implementation (e.g. @deepseek-ai/dsh-code-runtime-worker-thread) or set tools mode to \"native\"";
    ToolDefinition {
        name: RUN_CODE_NAME.to_string(),
        description: "run code in a sandbox".to_string(),
        parameters: serde_json::json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        output: ToolOutputDefinition {
            schema: crate::json_schema::JsonSchemaNode::default(),
            render: Rc::new(|_, _| vec![ContentBlock::text("(run_code completed with no output)")]),
            presentation_meta: None,
        },
        timeout_ms: None,
        execute: Rc::new(move |_, _| {
            Err(ToolFailureData::new(msg, TOOL_ABORTED_BEFORE_DISPATCH, "Error"))
        }),
        finalize_content: None,
        is_concurrency_safe: None,
        present_call: None,
        present_result: None,
    }
}

/// 覆盖版 run_code 定义：宿主 executor 替换占位传输（M5 真实执行）。命名/schema 注入
/// 与保留名守卫不变（register_global 仍拒 run_code）；渲染依 execute 规范化值
/// （{value?, logs?, error?}）产出模型可见文本。
fn run_code_def(exec: ToolExecute) -> ToolDefinition {
    ToolDefinition {
        name: RUN_CODE_NAME.to_string(),
        description: "run code in a sandbox".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "code": { "type": "string", "required": true, "description": "Program body to run (async body with top-level await/return)." },
                "description": { "type": "string", "required": true, "description": "One-line description of what the program does." }
            },
            "additionalProperties": false,
        }),
        output: ToolOutputDefinition {
            schema: crate::json_schema::JsonSchemaNode::default(),
            render: Rc::new(|_, v| vec![ContentBlock::text(render_run_code_value(v))]),
            presentation_meta: None,
        },
        timeout_ms: None,
        execute: exec,
        finalize_content: None,
        is_concurrency_safe: None,
        present_call: None,
        present_result: None,
    }
}

/// run_code 覆盖值的模型可见渲染：优先失败错误，否则 logs 接 value（字符串原样，否则
/// JSON 紧凑），空则 "completed with no output"。
fn render_run_code_value(v: &Value) -> String {
    if let Some(e) = v.get("error") {
        let kind = e["kind"].as_str().unwrap_or("?");
        let message = e["message"].as_str().unwrap_or("?");
        return format!("[run_code error: {kind}] {message}");
    }
    let logs: Vec<&str> = v["logs"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
        .unwrap_or_default();
    let mut out = logs.join("\n");
    if let Some(val) = v.get("value") {
        if !val.is_null() {
            if !out.is_empty() {
                out.push('\n');
            }
            match val {
                Value::String(s) => out.push_str(s),
                other => out.push_str(&other.to_string()),
            }
        }
    }
    if out.is_empty() {
        "(run_code completed with no output)".to_string()
    } else {
        out
    }
}

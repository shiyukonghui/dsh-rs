//! `dsh-system-prompt`：系统提示组装注册表（镜像 TS `@deepseek-ai/dsh-system-prompt`）。
//!
//! 组装 = 注册项（sections/contexts/tools/variables）→ `assemble(context)` →
//! `PromptAssembly` → 渲染纯函数 → 模型文本。本包不决定 model 调用时机；路由与
//! transcript 归属 loop 层——本包只保证「组装文本可纯函数重建」。
//!
//! 差异（记录于 DECISIONS.md D-028）：
//! - `AssembleContext.signal`（AbortSignal）未入 Rust 面（栈中无取消令牌对象），
//!   显式组装的控制信号由调用方经 `AssembleContext` 关闭；本包不保留它去控制后续
//!   行为的语义不变。
//! - `PromptAssembly.variables` 用 `Vec<(String, Option<String>)>` 保序（对齐 JS
//!   `Record` 的 own-property 语义与错误消息里的注册名插入序；无原型穿透）。
//! - 水岭监听器注册/拆除在本服务内 `Rc<RefCell<Vec<..>>>`；`prepend` 插到最前。
//! - `system-prompt/change` 为调用方提供的 `Rc<dyn Fn()>` 通知（全局、unfiltered）。

use std::cell::RefCell;
use std::rc::Rc;

use dsh_llm::{ContextSnapshotSection, ToolSchema};
use dsh_scope::{AnonymousEntries, NamedEntries, ScopeKey, ScopeLayer, ScopedLayers, Undo};

pub mod invariant;

// ---------------------------------------------------------------------------
// 常量
// ---------------------------------------------------------------------------

/// 部署 persona 的 section 名与顺序（可被 scoped section 遮蔽/替换）。
pub const PERSONA_SECTION: &str = "deployment:persona";
/// Persona 槽顺序；模型读到的第一个 section。
pub const PERSONA_ORDER: f64 = 0.0;
/// `toolOrder` 预留的未列出工具插入标记。
pub const TOOL_ORDER_REST: &str = "<unlisted-tools>";
/// 内建 harness 身份 section。
pub const HARNESS_IDENTITY_SECTION: &str = "harness:identity";
/// harness 身份 order。
pub const HARNESS_IDENTITY_ORDER: f64 = -100.0;
/// harness 身份固定文本（逐字节）。
pub const HARNESS_IDENTITY_TEXT: &str = "You are an AI agent powered by DeepSeek Harness.";

/// 变量名：`/^[a-z][a-z0-9_]*$/`。
pub(crate) fn is_variable_name(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

pub(crate) fn quoted(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

// ---------------------------------------------------------------------------
// 类型
// ---------------------------------------------------------------------------

/// 一次组装的合并扩展上下文（`scope` 之外可由消费方扩展）。
#[derive(Debug, Clone, Default)]
pub struct AssembleContext {
    /// 参与者 scope：缺省 = 仅全局 provider 与无主题 listener。
    pub scope: Option<ScopeKey>,
}

/// section 文本：静态或按组装求值的 provider。
#[derive(Clone)]
pub enum PromptSectionText {
    Static(String),
    Fn(Rc<dyn Fn(&AssembleContext) -> String>),
}

impl PromptSectionText {
    fn resolve(&self, context: &AssembleContext) -> String {
        match self {
            PromptSectionText::Static(s) => s.clone(),
            PromptSectionText::Fn(f) => f(context),
        }
    }
}

/// 一条系统提示 section（注册输入）。
#[derive(Clone)]
pub struct PromptSection {
    /// 唯一名（重复注册抛；scoped 遮蔽全局同名校）。
    pub name: String,
    /// 升序拼接；约定 -100=harness 身份、0=persona、100–199=工具指引。
    pub order: f64,
    pub text: PromptSectionText,
    /// true = 视作完整 system prompt（瀑布后强制恢复为唯一 section）。
    pub complete: bool,
}

/// context 文本：静态或按组装求值的 provider。
#[derive(Clone)]
pub enum PromptContextText {
    Static(String),
    Fn(Rc<dyn Fn(&AssembleContext) -> String>),
}

impl PromptContextText {
    fn resolve(&self, context: &AssembleContext) -> String {
        match self {
            PromptContextText::Static(s) => s.clone(),
            PromptContextText::Fn(f) => f(context),
        }
    }
}

/// 一条动态 context 贡献（注册输入）。
#[derive(Clone)]
pub struct PromptContext {
    pub name: String,
    /// 升序 join。
    pub order: f64,
    pub text: PromptContextText,
}

/// 一个工具 schema provider 的结果。
#[derive(Debug, Clone)]
pub struct ToolProviderResult {
    /// 本组装贡献的 schemas。
    pub schemas: Vec<ToolSchema>,
    /// 预限制名全集（缺省 = `schemas` 之名）供 toolOrder 校验。
    pub known_names: Option<Vec<String>>,
}

/// 工具 schema provider。
pub type ToolProvider = Rc<dyn Fn(&AssembleContext) -> ToolProviderResult>;
/// 变量 provider（返回 `None` = 注册但本组装无值，渲染引用到即失败）。
pub type VariableProvider = Rc<dyn Fn(&AssembleContext) -> Option<String>>;

/// 组装里的一个 section（已解析、未插值）。
#[derive(Debug, Clone, PartialEq)]
pub struct AssembledSection {
    pub name: String,
    pub text: String,
}

/// 组装里的一个动态 context（已解析、未插值）。
#[derive(Debug, Clone, PartialEq)]
pub struct AssembledContext {
    pub name: String,
    pub text: String,
}

/// 组装产物：sections/contexts 未插值；tools 已在 canonical 序；variables 保插入序
/// （`None` = 已注册但无值，own-property 语义）。
#[derive(Debug, Clone, PartialEq)]
pub struct PromptAssembly {
    pub sections: Vec<AssembledSection>,
    pub contexts: Vec<AssembledContext>,
    pub tools: Vec<ToolSchema>,
    pub variables: Vec<(String, Option<String>)>,
}

/// 插件配置：部署级 system-prompt 片段。
#[derive(Debug, Clone)]
pub struct Config {
    /// 在部署 persona 前固定包含 DeepSeek Harness 身份（默认 true）。
    pub include_harness_identity: bool,
    /// 在模型历史中包含动态 runtime-context 快照（默认 true）。
    pub include_runtime_context: bool,
    /// 部署级 order-0 persona 模板（scoped `deployment:persona` 遮蔽；`{{var}}` 严格）。
    pub persona: String,
    /// 模型面工具名顺序（含 `<unlisted-tools>` 恰一次）；省略 = 字典序。
    pub tool_order: Option<Vec<String>>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            include_harness_identity: true,
            include_runtime_context: true,
            persona: String::new(),
            tool_order: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Config 校验
// ---------------------------------------------------------------------------

/// 校验 toolOrder 的重复名与必需 rest 标记（注册名稍后校验——插件尚未加载）。
pub fn validate_tool_order(tool_order: Option<Vec<String>>) -> Result<Option<Vec<String>>, String> {
    let Some(order) = tool_order else {
        return Ok(None);
    };
    let mut seen = std::collections::BTreeSet::new();
    for name in &order {
        if !seen.insert(name.clone()) {
            return Err(format!("toolOrder lists {} more than once", quoted(name)));
        }
    }
    if !seen.contains(TOOL_ORDER_REST) {
        return Err(format!(
            "toolOrder must contain the \"{}\" rest entry (where unlisted tools are inserted)",
            TOOL_ORDER_REST
        ));
    }
    Ok(Some(order))
}

// ---------------------------------------------------------------------------
// PromptLayer
// ---------------------------------------------------------------------------

/// 一个 prompt layer（全局或某个 scope 的注册面）。
pub struct PromptLayer {
    pub sections: NamedEntries<PromptSection>,
    pub contexts: NamedEntries<PromptContext>,
    pub runtime_context_suppressors: AnonymousEntries<()>,
    pub tool_providers: AnonymousEntries<ToolProvider>,
    pub variables: NamedEntries<VariableProvider>,
}

impl PromptLayer {
    fn new(scope: Option<&ScopeKey>) -> Self {
        let scoped = scope.is_some();
        PromptLayer {
            sections: NamedEntries::new(move |name| {
                if scoped {
                    format!("prompt section {} is already registered in this scope", quoted(name))
                } else {
                    format!(
                        "prompt section {} is already registered (for a per-agent override, register through that agent's `agent.ctx` instead)",
                        quoted(name)
                    )
                }
            }),
            contexts: NamedEntries::new(move |name| {
                if scoped {
                    format!("prompt context {} is already registered in this scope", quoted(name))
                } else {
                    format!(
                        "prompt context {} is already registered (for a per-agent override, register through that agent's `agent.ctx` instead)",
                        quoted(name)
                    )
                }
            }),
            runtime_context_suppressors: AnonymousEntries::new(),
            tool_providers: AnonymousEntries::new(),
            variables: NamedEntries::new(move |name| {
                if scoped {
                    format!("prompt variable {} is already registered in this scope", quoted(name))
                } else {
                    format!(
                        "prompt variable {} is already registered (for a per-agent value, register through that agent's `agent.ctx` instead)",
                        quoted(name)
                    )
                }
            }),
        }
    }
}

impl ScopeLayer for PromptLayer {
    fn is_empty(&self) -> bool {
        self.sections.is_empty()
            && self.contexts.is_empty()
            && self.runtime_context_suppressors.is_empty()
            && self.tool_providers.is_empty()
            && self.variables.is_empty()
    }
}

// ---------------------------------------------------------------------------
// 水岭监听器
// ---------------------------------------------------------------------------

/// 剩余链回调（`next`）：跑余下监听器；`None` 层无监听器时为恒等。
pub type AssembleNext = Rc<dyn Fn(PromptAssembly) -> Result<PromptAssembly, String>>;
/// 一条 assemble 水岭监听器：`(assembly, context, next) -> Result`；不调 `next`
/// 即短路。
pub type AssembleListener =
    Rc<dyn Fn(PromptAssembly, &AssembleContext, AssembleNext) -> Result<PromptAssembly, String>>;

struct WaterfallItem {
    scope: Option<ScopeKey>,
    cb: AssembleListener,
}

// ---------------------------------------------------------------------------
// SystemPrompt
// ---------------------------------------------------------------------------

/// 注册表服务：system-prompt 输入的组装。
pub struct SystemPrompt {
    layers: ScopedLayers<PromptLayer>,
    tool_order: Option<Vec<String>>,
    listeners: Rc<RefCell<Vec<WaterfallItem>>>,
    change_notify: Rc<dyn Fn()>,
}

impl SystemPrompt {
    /// 构造并注册内建 section（harness 身份 + persona）+ 可选全局上下文抑制。
    /// `tool_order` 无效（重复/缺 rest）→ Err。
    pub fn new(config: &Config, change_notify: Rc<dyn Fn()>) -> Result<Self, String> {
        let tool_order = validate_tool_order(config.tool_order.clone())?;
        let notify = change_notify.clone();
        let layers = ScopedLayers::new(PromptLayer::new, move || notify());
        let service = SystemPrompt {
            layers,
            tool_order,
            listeners: Rc::new(RefCell::new(Vec::new())),
            change_notify,
        };
        if config.include_harness_identity {
            let s = PromptSection {
                name: HARNESS_IDENTITY_SECTION.to_string(),
                order: HARNESS_IDENTITY_ORDER,
                text: PromptSectionText::Static(HARNESS_IDENTITY_TEXT.to_string()),
                complete: false,
            };
            service
                .section(None, &s)
                .expect("harness:identity built-in must not collide");
        }
        let persona = PromptSection {
            name: PERSONA_SECTION.to_string(),
            order: PERSONA_ORDER,
            text: PromptSectionText::Static(config.persona.clone()),
            complete: false,
        };
        service
            .section(None, &persona)
            .expect("deployment:persona built-in must not collide");
        if !config.include_runtime_context {
            service.suppress_runtime_context(None);
        }
        Ok(service)
    }

    /// `system-prompt/change` 通知回调（注册/注销各触发一次；全局 unfiltered）。
    pub fn change_notify(&self) -> &Rc<dyn Fn()> {
        &self.change_notify
    }

    /// 注册一个 assemble 水岭监听器。`prepend` → 队列最前（invariant 用）。
    pub fn register_assemble_listener(
        &self,
        scope: Option<ScopeKey>,
        prepend: bool,
        cb: AssembleListener,
    ) {
        let mut list = self.listeners.borrow_mut();
        let item = WaterfallItem { scope, cb };
        if prepend {
            list.insert(0, item);
        } else {
            list.push(item);
        }
    }

    /// 注册一个有序 section（调用方 scope）。
    pub fn section(&self, scope: Option<&ScopeKey>, section: &PromptSection) -> Result<Undo, String> {
        if !section.order.is_finite() {
            return Err(format!(
                "prompt section {} order must be a finite number",
                quoted(&section.name)
            ));
        }
        let dup = match scope {
            Some(k) => self.layers.peek(Some(k)).is_some_and(|l| l.sections.has(&section.name)),
            None => self.layers.global().sections.has(&section.name),
        };
        if dup {
            return Err(match scope {
                None => format!(
                    "prompt section {} is already registered (for a per-agent override, register through that agent's `agent.ctx` instead)",
                    quoted(&section.name)
                ),
                Some(_) => format!(
                    "prompt section {} is already registered in this scope",
                    quoted(&section.name)
                ),
            });
        }
        let name = section.name.clone();
        let value = section.clone();
        let undo = self.layers.effect(
            scope,
            move |layer| {
                layer
                    .sections
                    .insert(&name, value.clone())
                    .unwrap_or_else(|_| unreachable!("pre-checked duplicate"))
            },
            "systemPrompt.section()",
            true,
        );
        Ok(undo)
    }

    /// 注册一个有序动态 context（调用方 scope）。
    pub fn context(&self, scope: Option<&ScopeKey>, context: &PromptContext) -> Result<Undo, String> {
        if !context.order.is_finite() {
            return Err(format!(
                "prompt context {} order must be a finite number",
                quoted(&context.name)
            ));
        }
        let dup = match scope {
            Some(k) => self.layers.peek(Some(k)).is_some_and(|l| l.contexts.has(&context.name)),
            None => self.layers.global().contexts.has(&context.name),
        };
        if dup {
            return Err(match scope {
                None => format!(
                    "prompt context {} is already registered (for a per-agent override, register through that agent's `agent.ctx` instead)",
                    quoted(&context.name)
                ),
                Some(_) => format!(
                    "prompt context {} is already registered in this scope",
                    quoted(&context.name)
                ),
            });
        }
        let name = context.name.clone();
        let value = context.clone();
        let undo = self.layers.effect(
            scope,
            move |layer| {
                layer
                    .contexts
                    .insert(&name, value.clone())
                    .unwrap_or_else(|_| unreachable!("pre-checked duplicate"))
            },
            "systemPrompt.context()",
            true,
        );
        Ok(undo)
    }

    /// 在调用方 scope 抑制所有动态 runtime-context 贡献（多次抑制独立可销）。
    pub fn suppress_runtime_context(&self, scope: Option<&ScopeKey>) -> Undo {
        self.layers.effect(
            scope,
            |layer| layer.runtime_context_suppressors.append(()),
            "systemPrompt.suppressRuntimeContext()",
            true,
        )
    }

    /// 在调用方 scope 注册一个工具 schema provider。
    pub fn tools(&self, scope: Option<&ScopeKey>, provider: ToolProvider) -> Undo {
        self.layers.effect(
            scope,
            move |layer| layer.tool_providers.append(provider.clone()),
            "systemPrompt.tools()",
            true,
        )
    }

    /// 注册一个 prompt 变量（`[a-z][a-z0-9_]*`；非法名抛；scoped 遮蔽全局）。
    pub fn variable(
        &self,
        scope: Option<&ScopeKey>,
        name: &str,
        provider: VariableProvider,
    ) -> Result<Undo, String> {
        if !is_variable_name(name) {
            return Err(format!(
                "invalid prompt variable name {} (must match /^[a-z][a-z0-9_]*$/)",
                quoted(name)
            ));
        }
        let dup = match scope {
            Some(k) => self.layers.peek(Some(k)).is_some_and(|l| l.variables.has(name)),
            None => self.layers.global().variables.has(name),
        };
        if dup {
            return Err(match scope {
                None => format!(
                    "prompt variable {} is already registered (for a per-agent value, register through that agent's `agent.ctx` instead)",
                    quoted(name)
                ),
                Some(_) => format!("prompt variable {} is already registered in this scope", quoted(name)),
            });
        }
        let key = name.to_string();
        let undo = self.layers.effect(
            scope,
            move |layer| {
                layer
                    .variables
                    .insert(&key, provider.clone())
                    .unwrap_or_else(|_| unreachable!("pre-checked duplicate"))
            },
            "systemPrompt.variable()",
            true,
        );
        Ok(undo)
    }

    /// 组装全局与 scoped providers、分离工具参数、应用 canonical 排布、跑水岭。
    /// 返回的水岭值权威，除有效的 complete section 在之后强制恢复为唯一 section。
    pub fn assemble(&self, context: &AssembleContext) -> Result<PromptAssembly, String> {
        let scope = context.scope.clone();
        let scope_layers = self.layers.chain_layers(scope.as_ref());
        let runtime_context_suppressed = {
            !self.layers.global().runtime_context_suppressors.is_empty()
                || scope_layers
                    .iter()
                    .any(|l| !l.runtime_context_suppressors.is_empty())
        };

        // 变量：全局先，scope chain 远→近覆盖（最近 scope 同名胜出）；**live 迭代**
        //（provider 求值期新注册的变量本轮即可见——对齐 TS Map.entries live 语义）。
        let mut variables: Vec<(String, Option<String>)> = Vec::new();
        for (name, provider) in self.layers.global().variables.entries_live() {
            upsert_variable(&mut variables, name, provider(context));
        }
        for layer in &scope_layers {
            for (name, provider) in layer.variables.entries_live() {
                upsert_variable(&mut variables, name, provider(context));
            }
        }

        // Scoped sections/contexts shadow globals（merge = 全局基 + 远→近覆盖）。
        let section_by_name: Vec<(String, PromptSection)> =
            self.layers.merge(scope.as_ref(), &|l| l.sections.entries());
        let context_by_name: Vec<(String, PromptContext)> =
            self.layers.merge(scope.as_ref(), &|l| l.contexts.entries());

        // 工具：providers 在进入循环前**快照**（新增的下轮才见）。
        let mut providers: Vec<ToolProvider> = self.layers.global().tool_providers.values().collect();
        for layer in &scope_layers {
            providers.extend(layer.tool_providers.values());
        }
        let mut collected: Vec<ToolSchema> = Vec::new();
        let mut known_names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for provider in providers {
            let result = provider(context);
            let schemas: Vec<ToolSchema> = result
                .schemas
                .into_iter()
                .map(|s| ToolSchema {
                    name: s.name,
                    description: s.description,
                    // `structuredClone`：每组装深拷贝，跨组装零泄漏。
                    parameters: s.parameters,
                })
                .collect();
            // `knownNames` 缺省 = 本 provider 自己的 schemas 名（不是已收集全集）。
            let accepted = match &result.known_names {
                Some(names) => names.clone(),
                None => schemas.iter().map(|t| t.name.clone()).collect(),
            };
            for name in accepted {
                known_names.insert(name);
            }
            collected.extend(schemas);
        }

        // 稳定升序排序 sections/contexts。
        let mut section_definitions: Vec<PromptSection> =
            section_by_name.into_iter().map(|(_, v)| v).collect();
        sort_by_order(&mut section_definitions);
        let complete_sections: Vec<&PromptSection> = section_definitions
            .iter()
            .filter(|s| s.complete)
            .collect();
        if complete_sections.len() > 1 {
            let names = complete_sections
                .iter()
                .map(|s| quoted(&s.name))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "multiple complete prompt sections are active: {}",
                names
            ));
        }
        let mut complete_section: Option<AssembledSection> = None;
        let sections: Vec<AssembledSection> = section_definitions
            .iter()
            .map(|section| {
                let assembled = AssembledSection {
                    name: section.name.clone(),
                    text: section.text.resolve(context),
                };
                if section.complete {
                    complete_section = Some(assembled.clone());
                }
                assembled
            })
            .collect();

        let mut context_definitions: Vec<PromptContext> =
            context_by_name.into_iter().map(|(_, v)| v).collect();
        sort_by_order(&mut context_definitions);
        let contexts: Vec<AssembledContext> = if runtime_context_suppressed {
            Vec::new()
        } else {
            context_definitions
                .iter()
                .map(|c| AssembledContext {
                    name: c.name.clone(),
                    text: c.text.resolve(context),
                })
                .collect()
        };

        let tools = order_tools(collected, self.tool_order.as_deref(), &known_names)?;

        let assembly = PromptAssembly {
            sections,
            contexts,
            tools,
            variables,
        };
        let transformed = self.dispatch(assembly, context)?;
        if complete_section.is_none() && !runtime_context_suppressed {
            return Ok(transformed);
        }
        Ok(PromptAssembly {
            sections: match &complete_section {
                Some(cs) => vec![cs.clone()],
                None => transformed.sections,
            },
            contexts: if runtime_context_suppressed {
                Vec::new()
            } else {
                transformed.contexts
            },
            tools: transformed.tools,
            variables: transformed.variables,
        })
    }

    fn dispatch(
        &self,
        assembly: PromptAssembly,
        context: &AssembleContext,
    ) -> Result<PromptAssembly, String> {
        let adopted: Vec<AssembleListener> = {
            let list = self.listeners.borrow();
            let chain = dsh_scope::scope_chain_of(context.scope.as_ref());
            list.iter()
                .filter(|item| match &item.scope {
                    None => true,
                    Some(tag) => chain.iter().any(|k| k == tag),
                })
                .map(|item| item.cb.clone())
                .collect()
        };
        if adopted.is_empty() {
            return Ok(assembly);
        }
        dispatch_slice(&adopted, 0, assembly, context)
    }
}

fn upsert_variable(variables: &mut Vec<(String, Option<String>)>, name: String, value: Option<String>) {
    match variables.iter_mut().find(|(n, _)| *n == name) {
        Some(slot) => slot.1 = value,
        None => variables.push((name, value)),
    }
}

/// 稳定升序（`order` 已在注册时校验 finite）。
fn sort_by_order<T>(items: &mut [T])
where
    T: OrderAsc,
{
    items.sort_by(|a, b| a.order().partial_cmp(&b.order()).unwrap_or(std::cmp::Ordering::Equal));
}

trait OrderAsc {
    fn order(&self) -> f64;
}
impl OrderAsc for PromptSection {
    fn order(&self) -> f64 {
        self.order
    }
}
impl OrderAsc for PromptContext {
    fn order(&self) -> f64 {
        self.order
    }
}

/// 递推水岭：第 `i` 个监听器收 `(assembly, context, next=第 i+1 链)`。`next` 闭包
/// `'static`：捕获克隆的（剩余链、owned context）。
fn dispatch_slice(
    list: &[AssembleListener],
    i: usize,
    assembly: PromptAssembly,
    context: &AssembleContext,
) -> Result<PromptAssembly, String> {
    if i >= list.len() {
        return Ok(assembly);
    }
    let owned = AssembleContext {
        scope: context.scope.clone(),
    };
    let tail: Vec<AssembleListener> = list[i + 1..].iter().cloned().collect();
    let next: AssembleNext = Rc::new(move |a| dispatch_slice(&tail, 0, a, &owned));
    (list[i])(assembly, context, next)
}

/// 应用配置的 tool 顺序；未列出工具按字典序插入 rest 标记处。
fn order_tools(
    mut tools: Vec<ToolSchema>,
    tool_order: Option<&[String]>,
    known_names: &std::collections::BTreeSet<String>,
) -> Result<Vec<ToolSchema>, String> {
    let reserved = tools
        .iter()
        .find(|t| t.name == TOOL_ORDER_REST)
        .map(|t| t.name.clone());
    if let Some(name) = reserved {
        return Err(format!(
            "tool provider returned reserved tool name {} (reserved for toolOrder's rest entry)",
            quoted(&name)
        ));
    }
    let Some(order) = tool_order else {
        tools.sort_by(|a, b| a.name.cmp(&b.name));
        return Ok(tools);
    };
    let unknown: Vec<&String> = order
        .iter()
        .filter(|name| **name != TOOL_ORDER_REST && !known_names.contains(*name))
        .collect();
    if !unknown.is_empty() {
        let s = if unknown.len() > 1 { "s" } else { "" };
        let list = unknown
            .iter()
            .map(|n| quoted(n))
            .collect::<Vec<_>>()
            .join(", ");
        let known: Vec<String> = known_names.iter().cloned().collect();
        let shown = if known.is_empty() {
            "(none)".to_string()
        } else {
            known.join(", ")
        };
        return Err(format!(
            "toolOrder lists unregistered tool{s} {list}; known tools: {shown}"
        ));
    }
    let listed: std::collections::BTreeSet<String> = order.iter().cloned().collect();
    let mut rest: Vec<ToolSchema> = tools
        .iter()
        .filter(|t| !listed.contains(&t.name))
        .cloned()
        .collect();
    rest.sort_by(|a, b| a.name.cmp(&b.name));
    let mut out: Vec<ToolSchema> = Vec::new();
    for name in order {
        if name == TOOL_ORDER_REST {
            out.extend(rest.iter().cloned());
        } else {
            out.extend(tools.iter().filter(|t| t.name == *name).cloned());
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// 渲染纯函数
// ---------------------------------------------------------------------------

/// 插值一个 section/context 并把诊断归属于其拥有者。
fn interpolate(
    text: &str,
    variables: &[(String, Option<String>)],
    kind: &str,
    name: &str,
) -> Result<String, String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<char> = Vec::new();
    let mut last = 0usize; // 最近未消费字面起点（char 下标）
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] == '{' && i + 1 < chars.len() && chars[i + 1] == '{' {
            // GROUP_AT = /^\{\{([^{}]*)\}\}/：内段不含 `{`/`}`，其后必须紧跟 `}}`
            let mut j = i + 2;
            while j < chars.len() && chars[j] != '{' && chars[j] != '}' {
                j += 1;
            }
            let closed = j + 1 < chars.len() && chars[j] == '}' && chars[j + 1] == '}';
            if closed {
                let inner: String = chars[i + 2..j].iter().collect();
                if !is_variable_name(&inner) {
                    return Err(format!(
                        "malformed prompt variable reference \"{}{}{}\" in {} \"{}\" (variable names match /^[a-z][a-z0-9_]*$/)",
                        "{{", inner, "}}", kind, name
                    ));
                }
                match variables.iter().find(|(k, _)| k == &inner) {
                    None => {
                        let known = variables
                            .iter()
                            .map(|(k, _)| k.clone())
                            .collect::<Vec<_>>();
                        let shown = if known.is_empty() {
                            "(none)".to_string()
                        } else {
                            known.join(", ")
                        };
                        return Err(format!(
                            "unknown prompt variable \"{}{}{}\" in {} \"{}\"; registered variables: {}",
                            "{{", inner, "}}", kind, name, shown
                        ));
                    }
                    Some((_, Some(value))) => {
                        out.extend(chars[last..i].iter());
                        out.extend(value.chars());
                        last = j + 2;
                        i = j + 2;
                        continue;
                    }
                    Some((_, None)) => {
                        return Err(format!(
                            "prompt variable \"{}{}{}\" has no value for this assembly ({} \"{}\")",
                            "{{", inner, "}}", kind, name
                        ));
                    }
                }
            }
            // 无完整 group：后续还有 `}}` → malformed；否则是纯散文（字面保留 `{{`）。
            let has_close = (i + 2..chars.len()).any(|k| chars[k] == '}' && k + 1 < chars.len() && chars[k + 1] == '}');
            if has_close {
                let preview: String = chars[i..].iter().take(16).collect();
                return Err(format!(
                    "malformed prompt variable reference at \"{}…\" in {} \"{}\" (references are complete simple {{name}} groups)",
                    preview, kind, name
                ));
            }
            out.extend(chars[last..=(i + 1)].iter());
            last = i + 2;
            i += 2;
            continue;
        }
        i += 1;
    }
    out.extend(chars[last..].iter());
    Ok(out.into_iter().collect())
}

/// 渲染完整提示：各 section 插值、滤空、以空行连接。
pub fn render_prompt(assembly: &PromptAssembly) -> Result<String, String> {
    let mut parts: Vec<String> = Vec::new();
    for section in &assembly.sections {
        let text = interpolate(&section.text, &assembly.variables, "section", &section.name)?;
        if !text.is_empty() {
            parts.push(text);
        }
    }
    Ok(parts.join("\n\n"))
}

/// 渲染动态 context 为「具名贡献」列表（非空文本者保留）。
pub fn render_context_sections(
    assembly: &PromptAssembly,
) -> Result<Vec<ContextSnapshotSection>, String> {
    let mut out = Vec::new();
    for context in &assembly.contexts {
        let text = interpolate(&context.text, &assembly.variables, "context", &context.name)?;
        if !text.is_empty() {
            out.push(ContextSnapshotSection {
                name: context.name.clone(),
                text,
            });
        }
    }
    Ok(out)
}

/// 连接已渲染快照；空 body → `''`（无头）。
pub fn join_context_sections(sections: &[ContextSnapshotSection]) -> String {
    let body = sections
        .iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    if body.is_empty() {
        return String::new();
    }
    format!(
        "Current runtime context. This snapshot supersedes earlier runtime-context snapshots.\n\n{body}"
    )
}

/// 完整动态 context 快照（渲染 + 连接）。
pub fn render_context_snapshot(assembly: &PromptAssembly) -> Result<String, String> {
    Ok(join_context_sections(&render_context_sections(assembly)?))
}

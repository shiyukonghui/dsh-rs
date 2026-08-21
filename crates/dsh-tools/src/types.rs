//! 工具定义与执行管线类型（镜像 TS `@deepseek-ai/dsh-tools/index.ts`/`types.ts`/
//! `presentation.ts` 的 Rust 面）。
//!
//! 宿主核心单线程（D-004/D-006）：TS 的 `execute: Promise<...>` 收敛为同步
//! `Result<Value, ToolFailureData>`；`AbortSignal` 收敛为 [`ToolSignal`]
//! （`Rc<Cell<bool>>` 取消令牌 + reason）。这些差异连同其它 M2 差异记入 DECISIONS.md。

use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// 工具执行失败/取消的数据载体（对齐 TS `ToolFailure { message, info? }`，
/// `info.code` 即稳定路由 code）。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolFailureData {
    pub message: String,
    /// 内部错误类/路由 code（如 `UNKNOWN_TOOL`、`INVALID_TOOL_OUTPUT`、`ABORTED`）。
    pub code: String,
    /// 错误内部类名（如 `ToolNotFoundError`、`ToolArgsError`）。
    pub name: String,
}

impl ToolFailureData {
    pub fn new(message: impl Into<String>, code: impl Into<String>, name: impl Into<String>) -> Self {
        ToolFailureData {
            message: message.into(),
            code: code.into(),
            name: name.into(),
        }
    }
}

/// 工具体执行结果（值的规范形态：`ok` 携带 canonical value）。
#[derive(Debug, Clone, PartialEq)]
pub enum ToolResult {
    Ok(Value),
    Err(ToolFailureData),
}

// —— 工具钩子的闭包类型别名（收敛 clippy type_complexity，同时给出公开 API 面）——
/// 输出纯渲染：`(args, canonical value)` → model-facing ContentBlock。
pub type ToolRender = Rc<dyn Fn(&Value, &Value) -> Vec<ContentBlock>>;
/// 直接顶层调用的纯可重放呈现元数据。
pub type ToolPresentationMeta = Rc<dyn Fn(&Value, &Value) -> Value>;
/// 参数校验后的本体执行（同步）。
pub type ToolExecute = Rc<dyn Fn(&Value, &ToolRunContext) -> Result<Value, ToolFailureData>>;
/// 每个归一化结果的最后内容变换（返回 None 保留原内容）。
pub type ToolFinalize =
    Rc<dyn Fn(&ToolExecution, &ToolExecutionSnapshot) -> Option<Vec<ContentBlock>>>;
/// 仅返回 `true` 才允许并行。
pub type ToolIsConcurrencySafe = Rc<dyn Fn(&Value) -> bool>;
/// 待执行态呈现。
pub type ToolPresentCall = Rc<dyn Fn(&Value) -> Option<ToolCallView>>;
/// 已完成态呈现。
pub type ToolPresentResult = Rc<dyn Fn(&Value, &ToolResult) -> Option<ToolResultView>>;

/// 取消/调度信号（模拟 TS `AbortSignal` 的最小单线程面）。
#[derive(Clone, Default)]
pub struct ToolSignal {
    aborted: Rc<Cell<bool>>,
    reason: Rc<RefCell<Option<String>>>,
}

impl ToolSignal {
    pub fn new() -> Self {
        ToolSignal {
            aborted: Rc::new(Cell::new(false)),
            reason: Rc::new(RefCell::new(None)),
        }
    }

    pub fn abort(&self, reason: impl Into<String>) {
        self.aborted.set(true);
        *self.reason.borrow_mut() = Some(reason.into());
    }

    pub fn aborted(&self) -> bool {
        self.aborted.get()
    }

    /// 取消原因（未取消为 None）。
    pub fn reason(&self) -> Option<String> {
        self.reason.borrow().clone()
    }

    /// 级联取消（跟随外层 signal）——run 作用域、sub-call 等共用。
    pub fn follow(&self, outer: &ToolSignal) {
        if outer.aborted() {
            self.abort(outer.reason().unwrap_or_else(|| "aborted".to_string()));
        }
    }
}

impl std::fmt::Debug for ToolSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolSignal")
            .field("aborted", &self.aborted())
            .field("reason", &self.reason())
            .finish()
    }
}

pub use dsh_llm::ContentBlock;

/// 执行身份、调用方、取消与调用者数据（对齐 TS `ToolRunContext` 的核心字段）。
#[derive(Debug, Clone)]
pub struct ToolRunContext {
    pub call_id: String,
    pub root_call_id: String,
    pub name: String,
    /// 发起/归因 agent（InitiatorScope 标签；无则 None）。
    pub agent: Option<String>,
    pub signal: ToolSignal,
}

impl ToolRunContext {
    pub fn new(
        call_id: impl Into<String>,
        root_call_id: impl Into<String>,
        name: impl Into<String>,
        agent: Option<String>,
    ) -> Self {
        ToolRunContext {
            call_id: call_id.into(),
            root_call_id: root_call_id.into(),
            name: name.into(),
            agent,
            signal: ToolSignal::new(),
        }
    }
}

/// 一次执行的不可变身份与参数（调用者与 sub-call 共用）。
#[derive(Debug, Clone)]
pub struct ToolExecution {
    pub call: ToolRunContext,
    pub args: Value,
}

/// 规范化过程中的结果快照（finalize 钩子收到；对齐 TS `ToolExecutionResult` 的
/// 前归一化形态：value + 渲染内容）。
#[derive(Debug, Clone)]
pub struct ToolExecutionSnapshot {
    /// 验证过的 canonical value（output.schema 保证）。
    pub value: Value,
    /// 已渲染的 model-facing 内容块（未 finalize）。
    pub content: Vec<ContentBlock>,
    /// 呈现专用的注解块（可选传给 UI，model 不可见）。
    pub content_annotation: Option<ContentBlock>,
}

/// 输出定义：schema + 纯 render + 可选呈现元数据。
pub struct ToolOutputDefinition {
    pub schema: crate::json_schema::JsonSchemaNode,
    /// `render(args, canonical value)` → model-facing ContentBlock。
    pub render: ToolRender,
    /// 直接顶层调用的纯可重放呈现元数据。
    pub presentation_meta: Option<ToolPresentationMeta>,
}

impl std::fmt::Debug for ToolOutputDefinition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolOutputDefinition")
            .field("schema", &self.schema)
            .field("has_render", &true)
            .finish()
    }
}

/// 待执行状态的工具卡片（对齐 TS `ToolCallView` 核心：card/title/kind）。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallView {
    pub card: String,
    pub title: String,
    pub kind: Option<String>,
    pub raw_input: Option<Value>,
}

impl ToolCallView {
    /// `{ card:'generic', title, kind?, rawInput? }`。
    pub fn generic(title: impl Into<String>) -> ToolCallView {
        ToolCallView {
            card: "generic".to_string(),
            title: title.into(),
            kind: None,
            raw_input: None,
        }
    }
}

/// 已完成状态的工具卡片（对齐 TS `ToolResultView` 核心）。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolResultView {
    pub card: String,
    pub title: String,
    pub kind: Option<String>,
    pub value: Value,
}

impl ToolResultView {
    /// `{ card:'generic', title, value }`。
    pub fn generic(title: impl Into<String>, value: Value) -> ToolResultView {
        ToolResultView {
            card: "generic".to_string(),
            title: title.into(),
            kind: None,
            value,
        }
    }
}

/// 完整工具定义（registry-ready；对齐 TS `ToolDefinition`）。
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    /// 规范化 `parameters` JSON Schema（`parameterSchemaSpecToJsonSchema` 产出）。
    pub parameters: Value,
    pub output: ToolOutputDefinition,
    pub timeout_ms: Option<f64>,
    /// 参数已校验后的本体执行（同步；`args` 为 validated 值）。
    pub execute: ToolExecute,
    /// 每个归一化结果的最后内容变换；抛错即视为缺失省略。
    pub finalize_content: Option<ToolFinalize>,
    /// 仅返回 `true` 才允许并行。
    pub is_concurrency_safe: Option<ToolIsConcurrencySafe>,
    pub present_call: Option<ToolPresentCall>,
    pub present_result: Option<ToolPresentResult>,
}

impl std::fmt::Debug for ToolDefinition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolDefinition")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("parameters", &self.parameters)
            .field("timeout_ms", &self.timeout_ms)
            .finish_non_exhaustive()
    }
}

impl ToolDefinition {
    /// 提取模型可见的 schema 投影（allowlist：仅 name/description/parameters）。
    pub fn to_tool_schema(&self) -> dsh_llm::ToolSchema {
        dsh_llm::ToolSchema {
            name: self.name.clone(),
            description: self.description.clone(),
            parameters: self.parameters.clone(),
        }
    }
}

/// 预留的 Code Mode 呈现传输工具名（注册/restriction 无条件保留）。
pub const RUN_CODE_NAME: &str = "run_code";

/// 取消后工具体已被调用过的规范错误 code。
pub const TOOL_ABORTED: &str = "ABORTED";

/// 调度前被取消、从未启动工具体的规范错误 code。
pub const TOOL_ABORTED_BEFORE_DISPATCH: &str = "ABORTED_BEFORE_DISPATCH";

/// 工具执行稳定错误 code。
pub const CODE_UNKNOWN_TOOL: &str = "UNKNOWN_TOOL";
pub const CODE_INVALID_TOOL_OUTPUT: &str = "INVALID_TOOL_OUTPUT";
pub const CODE_INVALID_ARGS: &str = "INVALID_ARGS";

/// 工具 ARN（registry 内的稳定资源标识；`namespaces.tools`）。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ToolArn {
    pub namespace: String,
    pub name: String,
}

impl ToolArn {
    pub fn new(namespace: impl Into<String>, name: impl Into<String>) -> Self {
        ToolArn {
            namespace: namespace.into(),
            name: name.into(),
        }
    }

    pub fn as_str(&self) -> String {
        format!("{}:{}", self.namespace, self.name)
    }
}

/// 工具执行错误（含内部路由 data；对齐 TS `ToolExecutionError = HarnessError & {
/// failure?: ToolFailure }` 的 Rust 面）。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolExecutionError {
    pub data: ToolFailureData,
    pub failure: Option<ToolFailureData>,
}

impl ToolExecutionError {
    pub fn from_data(data: ToolFailureData) -> Self {
        ToolExecutionError { data, failure: None }
    }
}

/// 工具执行可能失败的载荷（统一容器）。
pub type ToolRunErrorCode = &'static str;

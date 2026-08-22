//! `dsh-workflow` 错误联合 —— 对齐 `packages/workflow/workflow/src/index.ts`
//! `WorkflowError` / `WorkflowErrorCode`（11 码全列，全部致命 `fatal=true`）。

/// 机器可路由的致命 workflow 失败码（对齐 TS WorkflowErrorCode）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowErrorCode {
    ScriptParse,
    MetaInvalid,
    InvalidArgument,
    UnsupportedOption,
    UnsupportedSchema,
    AgentCap,
    ItemCap,
    AgentStart,
    AgentResult,
    ResultUnserializable,
    Cancelled,
}

impl WorkflowErrorCode {
    /// wire 代码（对 TS 原样大写下划线）。
    pub fn as_str(&self) -> &'static str {
        match self {
            WorkflowErrorCode::ScriptParse => "SCRIPT_PARSE",
            WorkflowErrorCode::MetaInvalid => "META_INVALID",
            WorkflowErrorCode::InvalidArgument => "INVALID_ARGUMENT",
            WorkflowErrorCode::UnsupportedOption => "UNSUPPORTED_OPTION",
            WorkflowErrorCode::UnsupportedSchema => "UNSUPPORTED_SCHEMA",
            WorkflowErrorCode::AgentCap => "AGENT_CAP",
            WorkflowErrorCode::ItemCap => "ITEM_CAP",
            WorkflowErrorCode::AgentStart => "AGENT_START",
            WorkflowErrorCode::AgentResult => "AGENT_RESULT",
            WorkflowErrorCode::ResultUnserializable => "RESULT_UNSERIALIZABLE",
            WorkflowErrorCode::Cancelled => "CANCELLED",
        }
    }
}

/// 带码的 workflow 错误（默认致命——对齐 TS `fatal` 默认 true）。
#[derive(Debug, Clone)]
pub struct WorkflowError {
    pub code: WorkflowErrorCode,
    pub message: String,
    pub fatal: bool,
    /// META_INVALID 时逐条违规；其它错误为空。
    pub violations: Vec<crate::meta::MetaViolation>,
}

impl WorkflowError {
    pub fn new(message: impl Into<String>, code: WorkflowErrorCode) -> Self {
        WorkflowError {
            code,
            message: message.into(),
            fatal: true,
            violations: Vec::new(),
        }
    }
}

impl std::fmt::Display for WorkflowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for WorkflowError {}

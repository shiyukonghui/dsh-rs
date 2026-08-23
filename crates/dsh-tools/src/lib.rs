//! M2b: 工具能力缝核心（纯语义层）。
//!
//! 镜像 TS `@deepseek-ai/dsh-tools`：作者面 schema DSL 编译（`schema`）、强制
//! JSON Schema 子集断言与值校验（`json_schema`）、类型化工具定义（`types`）。
//! 注册表 / 执行管线 / Code Mode / ts-py SDK 生成在 M2b 后续子步（`runtime` 等）。
//!
//! - DSL 输入与强制 schema 统一用 `serde_json::Value`（D-014：BTreeMap 规范化键序，
//!   与 dsh-diff 的 canonical-sorted 一致；TS 保插入序，Rust 收敛为字典序——诊断
//!   顺序随之稳定，见 DECISIONS.md）。
//! - 错误语义对齐 `HarnessError(message, code)`：本 crate 提供 `SchemaErrorData`/
//!   `ToolArgsErrorData` 等轻量载体（宿主至今未引入统一 HarnessError 类型）。

pub mod guard;
pub mod json_schema;
pub mod m4;
pub mod m5;
pub mod py_types;
pub mod runtime;
pub mod schema;
pub mod ts_types;
pub mod types;

pub use guard::{
    canonicalize, detailed_reminder, preview_arguments, timeout_exceeded, tool_timeout_message,
    tool_timeout_result, validate_thresholds, wildcard_matches, Reminder, RepeatTracker,
    DEFAULT_THRESHOLDS, GENTLE_REMINDER, TOOL_TIMEOUT,
};
pub use json_schema::{
    assert_object_json_schema, assert_supported_json_schema, validate_json_schema_value,
    JsonSchemaError, JsonSchemaNode, JsonSchemaScalar, JsonSchemaType, ObjectJsonSchema,
};
pub use m4::{
    exit_plan_mode, job_kill, job_list, job_output, schedule_create, schedule_delete,
    schedule_list, todo_write, workflow, M4Tool, CODE_NOT_BOUND,
};
pub use m5::{define_m5_tool, M5Tool};
pub use py_types::{json_schema_to_py, render_tools_sdk_py};
pub use runtime::{
    ApprovalOutcome, ApprovalProvider, PreToolDecision, ToolErrorInfo, ToolExecutionClass,
    ToolExecutionInput, ToolExecutionMode, ToolExecutionResult, ToolGuard, ToolPreDecision,
    ToolRegistry, ToolRestriction, ToolView,
};
pub use schema::{
    define_tool, parameter_schema_spec_to_json_schema, validate_args,
    value_schema_spec_to_json_schema, DefineToolOptions, ToolArgsError, ToolDefinitionError,
};
pub use ts_types::{json_schema_to_ts, render_tools_sdk, ToolSdkSchema, SDK_INSTRUCTIONS};
pub use types::{
    ToolArn, ToolCallView, ToolDefinition, ToolExecute, ToolExecution, ToolExecutionError,
    ToolExecutionSnapshot, ToolFailureData, ToolFinalize, ToolIsConcurrencySafe,
    ToolOutputDefinition, ToolPresentCall, ToolPresentResult, ToolPresentationMeta, ToolRender,
    ToolResult, ToolResultView, ToolRunContext, ToolSignal, CODE_INVALID_ARGS,
    CODE_INVALID_TOOL_OUTPUT, CODE_UNKNOWN_TOOL, RUN_CODE_NAME, TOOL_ABORTED,
    TOOL_ABORTED_BEFORE_DISPATCH,
};

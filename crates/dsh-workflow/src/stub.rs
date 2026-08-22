//! `dsh-workflow` 诚实执行桩 —— JS 执行引擎 M4 不落地（D-044），对任何 start 请求返回
//! 结构化 `UNSUPPORTED_OPTION` 错误（`isError`），绝不伪装成功。

use crate::error::{WorkflowError, WorkflowErrorCode};

/// 一个 workflow start 请求（M4 桩：引擎只读 script/meta 名即拒）。
#[derive(Debug, Clone)]
pub struct StubRequest {
    pub script: String,
    pub meta_name: String,
}

/// 桩执行：恒 Err（结构化 isError）。
pub fn run_stub(request: StubRequest) -> Result<serde_json::Value, WorkflowError> {
    Err(WorkflowError::new(
        format!(
            "workflow execution is not implemented: script for meta \"{}\" ({} bytes) refused to start",
            request.meta_name,
            request.script.len()
        ),
        WorkflowErrorCode::UnsupportedOption,
    ))
}

//! TS worker-thread 后端 → 诚实桩（M5-DESIGN §7.4、DIV-3）：Rust 无 JS 引擎，
//! `run` 恒 `CodeRunFailure{kind: WorkerExit, message: "requires a code runtime"}`
//! （替换 M4 `placeholder_run_code` 占位，错误语义保留）。语言回退=TS 由接线方读取。

use crate::seam::CodeRuntime;
use crate::types::{CodeLanguage, CodeRunRequest, CodeRunResult, Isolation};

/// 诚实桩：语言='typescript'、基质='worker-thread'。
pub struct ThreadWorkerStub;

impl CodeRuntime for ThreadWorkerStub {
    fn language(&self) -> CodeLanguage {
        CodeLanguage::TypeScript
    }
    fn isolation(&self) -> Isolation {
        Isolation::WorkerThread
    }
    fn run(&self, _request: &CodeRunRequest) -> CodeRunResult {
        CodeRunResult {
            value: None,
            logs: Vec::new(),
            error: Some(crate::types::CodeRunFailure {
                kind: crate::types::CodeRunFailureKind::WorkerExit,
                message: "requires a code runtime".to_string(),
                detail: Some(
                    "the TypeScript worker backend has no Rust counterpart; only the \
                     python backend is available"
                        .to_string(),
                ),
            }),
        }
    }
}

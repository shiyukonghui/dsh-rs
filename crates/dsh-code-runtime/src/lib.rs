//! dsh-code-runtime — M5 code 执行缝（设计见 M5-DESIGN.md §7）。
//!
//! 阶段三 TDD：§7.1/7.2 缝契约（可移植标识符排除集 + 校验 + `CodeRuntime` trait +
//! 取消令牌）、§7.3 lossless-JSON 跨界校验、§7.4 `run_code` 工具纯面 + TS 诚实桩；
//! python 子进程后端随后续红测加入（std::process 在 Windows 不能建额外 fd → 协议
//! 走 stdin/stdout JSON-lines，用户输出经 `log` 帧，见 DECISIONS D-065）。

mod json_lossless;
mod seam;
mod tool_code;
mod types;
mod worker_thread_stub;

pub use json_lossless::{
    classify_admission, parse_lossless_json, validate_lossless_json, AdmissionError,
};
pub use seam::{
    is_dunder_member, validate_binding_namespace, CodeRuntime, PORTABLE_RESERVED_WORDS,
    RESERVED_BINDING_GLOBALS, RESERVED_ERROR_MEMBERS,
};
pub use types::{
    CancellationToken, CodeBindingErrorClass, CodeBindingFunction, CodeBindingNamespace,
    CodeLanguage, CodeRunFailure, CodeRunFailureKind, CodeRunRequest, CodeRunResult, Isolation,
};
pub use worker_thread_stub::ThreadWorkerStub;

/// 公共模块门面（测试/接线用）。
pub mod json {
    pub use crate::json_lossless::{
        classify_admission, parse_lossless_json, validate_lossless_json, AdmissionError,
    };
}
pub mod run_code {
    pub use crate::tool_code::{
        code_dispatch_id, exclude_run_code, parse_run_code_args, run_code_schema,
    };
}

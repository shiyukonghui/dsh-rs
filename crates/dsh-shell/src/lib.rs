//! dsh-shell — M5 shell 能力缝（设计见 M5-DESIGN.md §5）。
//!
//! 阶段三 TDD：request/spec 分裂 + resolve（缺省兜底/上限 clamp）+ bash-local 后端
//! （bash -c 前台 run / 后台 start）+ 模型面工具（tool-bash）随红测陆续落地。

mod executor;
mod resolve;
mod tool_bash;
mod types;

pub use executor::{LocalShellExecutor, ENV_OVERRIDES};
pub use resolve::{
    assert_serviceable_bash_config, clamp_timeout, resolve, resolve_bash_program,
    resolve_pwsh_program, BashConfig, DEFAULT_GRACE_MS, DEFAULT_MAX_OUTPUT_BYTES,
    DEFAULT_MAX_SPILL_BYTES, DEFAULT_MAX_TIMEOUT_MS, DEFAULT_TIMEOUT_MS, MAX_TIMER_DELAY_MS,
};
pub use tool_bash::{
    bash_tool_parameters, parse_bash_args, render_bash_process_read, render_bash_result,
    BashToolArgs,
};
pub use types::{
    ShellCollectedOutput, ShellError, ShellExecRequest, ShellExecSpec, ShellKind, ShellProcess,
    ShellProcessRead, ShellProcessStatus, ShellRunResult, ShellSandboxInfo, DSH_ENV_PREFIX,
};

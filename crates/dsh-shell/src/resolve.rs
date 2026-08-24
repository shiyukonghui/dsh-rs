//! dsh-shell 请求→规格解析（M5-DESIGN §5.2）。
//!
//! 参考 `bash-local/src/index.ts` + `util/timeout/src/index.ts` 的 `clampTimeout`：
//! 缺省兜底、上限 clamp、正值校验；配置「必须可服务」校验（positive-finite +
//! grace ≤ MAX_TIMER_DELAY_MS）。

use crate::types::{ShellExecRequest, ShellExecSpec};
use std::path::PathBuf;

/// 参考 util/timeout：Node 可调度的最大定时器延迟（ms）。
pub const MAX_TIMER_DELAY_MS: u64 = 2_147_483_647;

/// 默认前台超时（OpenCode 对齐 120s）。
pub const DEFAULT_TIMEOUT_MS: u64 = 120_000;

/// 单次调用超时上限。
pub const DEFAULT_MAX_TIMEOUT_MS: u64 = 600_000;

/// 每流内存输出上限（64 KiB，镜像 OpenCode 的 64_000）。
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 64_000;

/// 每流 spill 落盘上限（64 MiB）。
pub const DEFAULT_MAX_SPILL_BYTES: u64 = 64 * 1024 * 1024;

/// 默认 SIGTERM→SIGKILL 宽限（OpenCode 3s）。
pub const DEFAULT_GRACE_MS: u64 = 3_000;

/// bash-local 配置（全部可选，`Default` 供默认值）。`shell` 决定方言；pwsh 相关
/// 字段仅 `shell == PowerShell` 时生效。
#[derive(Debug, Clone)]
pub struct BashConfig {
    pub cwd: Option<PathBuf>,
    pub timeout_ms: u64,
    pub max_timeout_ms: u64,
    pub max_output_bytes: usize,
    pub max_spill_bytes: u64,
    pub grace_ms: u64,
    /// bash 可执行（显式注入/覆盖；缺省按候选顺序解析）。默认方言即 Bash。
    pub bash_path: Option<PathBuf>,
    /// shell 方言。`PowerShell` 时 argv 走 `-NoProfile -NonInteractive -Command`。
    pub shell: crate::types::ShellKind,
    /// pwsh 可执行（显式注入/覆盖；缺省按候选顺序解析，仅 shell=PowerShell 使用）。
    pub pwsh_path: Option<PathBuf>,
}

impl Default for BashConfig {
    fn default() -> Self {
        BashConfig {
            cwd: None,
            timeout_ms: DEFAULT_TIMEOUT_MS,
            max_timeout_ms: DEFAULT_MAX_TIMEOUT_MS,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            max_spill_bytes: DEFAULT_MAX_SPILL_BYTES,
            grace_ms: DEFAULT_GRACE_MS,
            bash_path: None,
            shell: crate::types::ShellKind::Bash,
            pwsh_path: None,
        }
    }
}

/// 参考 `assertServiceableBashConfig`：拒绝「本 executor 跑不了」的解析后配置。
pub fn assert_serviceable_bash_config(config: &BashConfig) -> Result<(), String> {
    if config.timeout_ms == 0 {
        return Err("bash-local: timeoutMs must be a positive finite number".into());
    }
    if config.max_timeout_ms == 0 {
        return Err("bash-local: maxTimeoutMs must be a positive finite number".into());
    }
    if config.max_output_bytes == 0 {
        return Err("bash-local: maxOutputBytes must be a positive finite number".into());
    }
    if config.max_spill_bytes == 0 {
        return Err("bash-local: maxSpillBytes must be a positive finite number".into());
    }
    if config.grace_ms == 0 || config.grace_ms > MAX_TIMER_DELAY_MS {
        return Err(format!(
            "bash-local: graceMs must be a positive finite number no greater than {MAX_TIMER_DELAY_MS}"
        ));
    }
    Ok(())
}

/// 参考 `clampTimeout`：`min(requested ?? default, max)`；给定值须正，非法即抛
/// `${name} must be a positive finite number`（零不是「禁超时」哨兵）。
pub fn clamp_timeout(
    requested: Option<u64>,
    default_ms: u64,
    max_ms: u64,
    name: &str,
) -> Result<u64, String> {
    if let Some(ms) = requested {
        if ms == 0 {
            return Err(format!("{name} must be a positive finite number"));
        }
        return Ok(ms.min(max_ms));
    }
    Ok(default_ms.min(max_ms))
}

/// 解析 bash 可执行：显式 bash_path > 候选顺序（Git Bash/WSL/裸名）。
pub fn resolve_bash_program(config: &BashConfig) -> String {
    if let Some(p) = &config.bash_path {
        return p.to_string_lossy().into_owned();
    }
    // Windows 候选：Git Bash 优先（system32\bash 是 WSL 启动器，行为依赖 WSL 安装）
    #[cfg(windows)]
    {
        for candidate in [
            "C:\\Program Files\\Git\\bin\\bash.exe",
            "C:\\Program Files\\Git\\usr\\bin\\bash.exe",
            "C:\\Windows\\System32\\bash.exe",
        ] {
            if std::path::Path::new(candidate).exists() {
                return candidate.to_string();
            }
        }
    }
    "bash".to_string()
}

/// 解析 pwsh 可执行：显式 pwsh_path > PowerShell 7 安装候选；Windows 恒有
/// powershell.exe（5.1）兜底，其余平台回落裸名 `pwsh`。
pub fn resolve_pwsh_program(config: &BashConfig) -> String {
    if let Some(p) = &config.pwsh_path {
        return p.to_string_lossy().into_owned();
    }
    #[cfg(windows)]
    {
        for candidate in [
            "C:\\Program Files\\PowerShell\\7\\pwsh.exe",
            "C:\\Program Files\\PowerShell\\7-preview\\pwsh.exe",
        ] {
            if std::path::Path::new(candidate).exists() {
                return candidate.to_string();
            }
        }
        "powershell.exe".to_string()
    }
    #[cfg(not(windows))]
    {
        "pwsh".to_string()
    }
}

/// 参考 `shell resolve()`：对 request 应用默认与 clamp，产出完全规格。
pub fn resolve(request: &ShellExecRequest, config: &BashConfig) -> Result<ShellExecSpec, String> {
    assert_serviceable_bash_config(config)?;
    let timeout_ms = clamp_timeout(
        request.timeout_ms,
        config.timeout_ms,
        config.max_timeout_ms,
        "bash-local: request.timeoutMs",
    )?;
    let stdout_max_bytes = match request.stdout_max_bytes {
        Some(n) if n > 0 => n,
        Some(_) => {
            return Err(
                "bash-local: request.stdoutMaxBytes must be a positive finite number".into(),
            )
        }
        None => config.max_output_bytes,
    };
    let workdir = request
        .workdir
        .clone()
        .or_else(|| config.cwd.clone())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let shell = config.shell;
    let program = match shell {
        crate::types::ShellKind::Bash => resolve_bash_program(config),
        crate::types::ShellKind::PowerShell => resolve_pwsh_program(config),
    };
    Ok(ShellExecSpec {
        command: request.command.clone(),
        workdir,
        timeout_ms,
        stdout_max_bytes,
        signal: request.signal,
        stdin: request.stdin.clone(),
        env: request.env.clone(),
        dsh_env: request.dsh_env.clone(),
        program,
        shell,
    })
}

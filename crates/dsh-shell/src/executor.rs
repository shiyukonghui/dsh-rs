//! dsh-shell 本地 shell 后端（M5-DESIGN §5.3；A 并行：bash + pwsh 双方言）。
//!
//! 参考 `bash-local/src/index.ts`：命令以 `bash -c` 或 `pwsh -Command` 在托管进程组/
//! Job 内执行（经 dsh-subprocess spawn），ENV_OVERRIDES 模型友好环境、前台 `run`
//! （默认/上限 + 同步超时杀 + 结果分类）、后台 `start`（无超时、增量读取、kill、done
//! settle）。

use crate::resolve::{resolve, BashConfig};
use crate::types::{
    ShellCollectedOutput, ShellError, ShellExecRequest, ShellExecSpec, ShellKind, ShellProcess,
    ShellRunResult,
};
use dsh_subprocess::{
    ChildStdio, StdinMode, StdoutMode, SubprocessCollect, SubprocessSpawnSpec, SubprocessSpill,
};
use std::path::PathBuf;

/// 模型友好环境覆盖：禁色、禁分页、禁交互终端特性（与 Codex 同集）。
/// 先并入 spawn 显式 env，可信调用方自己的条目仍可覆盖；DIV：本次不另行 scrubbing
/// （dsh-subprocess 在 spec.env 显式时按原样透传，scrub 语义见 DECISIONS）。
pub const ENV_OVERRIDES: [(&str, &str); 4] = [
    ("NO_COLOR", "1"),
    ("TERM", "dumb"),
    ("PAGER", "cat"),
    ("GIT_PAGER", "cat"),
];

/// `ctx.shell` 的本地实现：capability seam 的 Consumer/provider 角色。按 `spec.shell`
/// 分发 argv 形状（bash `-c` / pwsh `-NoProfile -NonInteractive -Command`）。
pub struct LocalShellExecutor {
    pub config: BashConfig,
    program: String,
}

impl LocalShellExecutor {
    /// 构造并校验配置（positive-finite + grace 上限）；解析本方言默认程序。
    pub fn new(config: BashConfig) -> Result<LocalShellExecutor, String> {
        let program = match config.shell {
            ShellKind::Bash => crate::resolve::resolve_bash_program(&config),
            ShellKind::PowerShell => crate::resolve::resolve_pwsh_program(&config),
        };
        Ok(LocalShellExecutor { config, program })
    }

    /// Clamp（字段缺省由实现兜底）。
    pub fn resolve(&self, request: &ShellExecRequest) -> Result<ShellExecSpec, String> {
        resolve(request, &self.config)
    }

    /// 前台执行 `bash -c <command>` / `pwsh -Command <command>`；非零退出/超时杀都
    /// resolve 成结果。
    pub fn run(&self, spec: &ShellExecSpec) -> Result<ShellRunResult, ShellError> {
        let spawn_spec = self.spawn_spec_for(spec, spec.stdin.clone(), spec.stdout_max_bytes);
        let mut handle =
            dsh_subprocess::spawn(&spawn_spec).map_err(|e| ShellError::Spawn(e.to_string()))?;
        let timeout = std::time::Duration::from_millis(spec.timeout_ms);
        let timed_out = match handle.wait_timeout(timeout) {
            Some(_) => false,
            None => {
                handle.terminate();
                true
            }
        };
        let outcome = handle.wait(); // 已 settle（terminate 或正常路径都缓存）
        Ok(ShellRunResult {
            exit_code: outcome.exit_code,
            signal: outcome.signal.as_ref().map(|s| s.as_str().to_string()),
            timed_out,
            aborted: false, // dsh-subprocess 信号面尚未驱动；见 DECISIONS
            timeout_ms: spec.timeout_ms,
            stdout: collect_final(&handle, true),
            stderr: collect_final(&handle, false),
            sandbox: None,
        })
    }

    /// 后台启动 `bash -c <command>` / `pwsh -Command <command>`；无超时；返回立即可读/
    /// 可杀/可等的句柄。
    pub fn start(&self, spec: &ShellExecSpec) -> Result<ShellProcess, ShellError> {
        let spawn_spec = self.spawn_spec_for(spec, None, spec.stdout_max_bytes);
        let handle =
            dsh_subprocess::spawn(&spawn_spec).map_err(|e| ShellError::Spawn(e.to_string()))?;
        Ok(ShellProcess::new(handle))
    }

    /// argv 形状随方言：bash `-c cmd`；pwsh `-NoProfile -NonInteractive -Command cmd`。
    fn program(&self, spec: &ShellExecSpec) -> String {
        if spec.program.is_empty() {
            self.program.clone()
        } else {
            spec.program.clone()
        }
    }

    fn spawn_spec_for(
        &self,
        spec: &ShellExecSpec,
        stdin_text: Option<String>,
        budget: usize,
    ) -> SubprocessSpawnSpec {
        let stdin = match stdin_text {
            Some(text) => StdinMode::WriteBytes(text.into_bytes()),
            None => StdinMode::Ignore,
        };
        let collect = SubprocessCollect {
            max_bytes: budget,
            spill: Some(SubprocessSpill {
                max_bytes: self.config.max_spill_bytes,
                dir: self.spill_dir(),
            }),
        };
        let env = self.assemble_env(spec);
        let program = self.program(spec);
        let argv = match spec.shell {
            ShellKind::Bash => vec![program, "-c".into(), spec.command.clone()],
            ShellKind::PowerShell => vec![
                program,
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
                spec.command.clone(),
            ],
        };
        SubprocessSpawnSpec {
            argv,
            cwd: spec.workdir.clone(),
            stdio: ChildStdio {
                stdin,
                stdout: StdoutMode::Collect(collect.clone()),
                stderr: StdoutMode::Collect(collect),
            },
            grace_ms: self.config.grace_ms,
            signal: spec.signal,
            env: Some(env),
        }
    }

    /// 参考 bash-local：父进程环境为基（凭据形/`DSH_*` 键 scrubbed——key 纪律，见
    /// dsh-subprocess::scrubbed_parent_env）→ ENV_OVERRIDES 覆盖 → 调用方 env →
    /// 托管 DSH_* 最后（不可被顶替）。修复前 base 恒为 4 个 override 键（env_clear
    /// 清掉 SystemRoot 等），Windows PowerShell 5.1 初始化 .NET/DPAPI 即崩
    /// （8009001d）。
    fn assemble_env(&self, spec: &ShellExecSpec) -> Vec<(String, String)> {
        let mut env: Vec<(String, String)> = dsh_subprocess::scrubbed_parent_env(
            &std::env::vars_os().collect::<Vec<dsh_subprocess::EnvEntry>>(),
        )
        .into_iter()
        .map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned()))
        .collect();
        let put = |env: &mut Vec<(String, String)>, k: String, v: String| {
            match env.iter_mut().find(|(ek, _)| *ek == k) {
                Some(entry) => entry.1 = v,
                None => env.push((k, v)),
            }
        };
        for (k, v) in ENV_OVERRIDES {
            put(&mut env, k.to_string(), v.to_string());
        }
        if let Some(entries) = &spec.env {
            for (k, v) in entries {
                put(&mut env, k.clone(), v.clone());
            }
        }
        if let Some(entries) = &spec.dsh_env {
            for (k, v) in entries {
                put(&mut env, k.clone(), v.clone());
            }
        }
        env
    }

    fn spill_dir(&self) -> PathBuf {
        std::env::temp_dir().join("dsh-shell-spill")
    }
}

/// 把 subprocess 收集槽投影成 shell 层最终输出（`finalOutput`）。
fn collect_final(
    handle: &dsh_subprocess::SubprocessHandle,
    is_stdout: bool,
) -> ShellCollectedOutput {
    let (text, truncated, spill) = if is_stdout {
        (
            handle.collected_stdout(),
            handle.stdout_lossy(),
            handle.stdout_spill_path(),
        )
    } else {
        (
            handle.collected_stderr(),
            handle.stderr_lossy(),
            handle.stderr_spill_path(),
        )
    };
    ShellCollectedOutput {
        text,
        truncated,
        spill_path: spill.map(|p| p.to_path_buf()),
    }
}

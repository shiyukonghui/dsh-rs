//! dsh-shell 本地 bash 后端（M5-DESIGN §5.3）。
//!
//! 参考 `bash-local/src/index.ts`：命令以 `bash -c` 在托管进程组/Job 内执行（经
//! dsh-subprocess spawn），ENV_OVERRIDES 模型友好环境、前台 `run`（默认/上限 + 同步
//! 超时杀 + 结果分类）、后台 `start`（无超时、增量读取、kill、done settle）。

use crate::resolve::{resolve, BashConfig};
use crate::types::{
    ShellCollectedOutput, ShellError, ShellExecRequest, ShellExecSpec, ShellProcess, ShellRunResult,
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

/// `ctx.shell` 的本地实现：capability seam 的 Consumer/provider 角色。
pub struct LocalBashExecutor {
    pub config: BashConfig,
    bash_program: String,
}

impl LocalBashExecutor {
    /// 构造并校验配置（positive-finite + grace 上限）。
    pub fn new(config: BashConfig) -> Result<LocalBashExecutor, String> {
        let bash_program = crate::resolve::resolve_bash_program(&config);
        Ok(LocalBashExecutor {
            config,
            bash_program,
        })
    }

    /// Clamp（字段缺省由实现兜底）。
    pub fn resolve(&self, request: &ShellExecRequest) -> Result<ShellExecSpec, String> {
        resolve(request, &self.config)
    }

    /// 前台执行 `bash -c <command>`；非零退出/超时杀都 resolve 成结果。
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

    /// 后台启动 `bash -c <command>`；无超时；返回立即可读/可杀/可等的句柄。
    pub fn start(&self, spec: &ShellExecSpec) -> Result<ShellProcess, ShellError> {
        let spawn_spec = self.spawn_spec_for(spec, None, spec.stdout_max_bytes);
        let handle =
            dsh_subprocess::spawn(&spawn_spec).map_err(|e| ShellError::Spawn(e.to_string()))?;
        Ok(ShellProcess::new(handle))
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
        SubprocessSpawnSpec {
            argv: vec![self.bash_program.clone(), "-c".into(), spec.command.clone()],
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

    /// overrides 先、调用方 env 次之、托管 DSH_* 最后（不可被顶替）。
    fn assemble_env(&self, spec: &ShellExecSpec) -> Vec<(String, String)> {
        let mut env: Vec<(String, String)> = Vec::new();
        for (k, v) in ENV_OVERRIDES {
            env.push((k.to_string(), v.to_string()));
        }
        if let Some(entries) = &spec.env {
            env.extend(entries.clone());
        }
        if let Some(entries) = &spec.dsh_env {
            env.extend(entries.clone());
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

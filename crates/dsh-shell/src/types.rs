//! dsh-shell 类型面（M5-DESIGN §5.1）。
//!
//! 参考 `shell/shell/src/types.ts`：request/spec 分裂（request 可缺省，resolve 兜底）、
//! 前台 run 结果、后台进程句柄（状态机 + 增量读取 + kill + done）。本 slice 的
//! `ShellExecRequest` 暂不承载 `sandbox_policy`（非 confining 后端忽略，sandboxing
//! executor 落地时随其类型一并加入，见 DECISIONS）。

use std::path::PathBuf;

/// 托管环境变量前缀（对齐 `dsh-subprocess` 的 scrubbed 词表）。
pub const DSH_ENV_PREFIX: &str = "DSH_";

/// shell 方言（决定 argv 形状与程序解析；A 并行：bash/pwsh 平行能力）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    /// `bash -c <command>`（Git Bash / WSL / 裸名）。
    Bash,
    /// `pwsh -NoProfile -NonInteractive -Command <command>`（PowerShell 7，Windows
    /// 上缺省回退 powershell.exe 5.1）。
    PowerShell,
}

/// 沙箱事实（仅沙箱化 executor 产生；本地 bash 后端恒为 None）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellSandboxInfo {
    pub mode: dsh_sandbox::SandboxMode,
    pub denied: bool,
    /// runner 是否在命令可运行前失败。
    pub runner_failed: Option<bool>,
}

/// 调用方执行请求：可缺省项由 `resolve()` 用实现配置兜底（显式 > 隐式）。
#[derive(Debug, Clone, Default)]
pub struct ShellExecRequest {
    pub command: String,
    /// 工作目录覆盖（缺省 = 后端配置 cwd，再缺省 = 当前目录）。
    pub workdir: Option<PathBuf>,
    /// 前台超时覆盖（后端 clamp/上限）。
    pub timeout_ms: Option<u64>,
    /// 前台 stdout 捕获预算（缺省 = 后端 max-output-bytes）。
    pub stdout_max_bytes: Option<usize>,
    /// 取消信号（当前 dsh-subprocess 信号面尚未驱动自动 terminate，见 DECISIONS）。
    pub signal: Option<dsh_subprocess::Signal>,
    /// 写入 stdin 后关闭（缺省 = 关闭/空）。
    pub stdin: Option<String>,
    /// 明文环境（凭据 scrubbed 后合并，托管 DSH_* 恒覆盖）。
    pub env: Option<Vec<(String, String)>>,
    /// 托管 DSH_* 快照（最后合并，不可被 env 顶替）。
    pub dsh_env: Option<Vec<(String, String)>>,
}

/// 完全解析的执行规格（`resolve()` 产物；run/start 只收 spec，不收 request）。
#[derive(Debug, Clone)]
pub struct ShellExecSpec {
    pub command: String,
    pub workdir: PathBuf,
    pub timeout_ms: u64,
    pub stdout_max_bytes: usize,
    pub signal: Option<dsh_subprocess::Signal>,
    pub stdin: Option<String>,
    pub env: Option<Vec<(String, String)>>,
    pub dsh_env: Option<Vec<(String, String)>>,
    /// 已解析的 shell 程序（随 `shell` 方言解析：bash 候选 / pwsh 候选）。
    pub program: String,
    /// shell 方言（decides argv 形状）。
    pub shell: ShellKind,
}

/// 最终收集输出（前台 run 结果用；镜像 `CollectedOutput` 三要素）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShellCollectedOutput {
    pub text: String,
    pub truncated: bool,
    pub spill_path: Option<PathBuf>,
}

/// 前台 `run` 结果：非零退出/超时杀/中止杀都**resolve 成结果**，不 reject。
#[derive(Debug, Clone, Default)]
pub struct ShellRunResult {
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
    pub timed_out: bool,
    pub aborted: bool,
    pub timeout_ms: u64,
    pub stdout: ShellCollectedOutput,
    pub stderr: ShellCollectedOutput,
    pub sandbox: Option<ShellSandboxInfo>,
}

/// 后台进程状态机。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellProcessStatus {
    Running,
    Completed,
    Killed,
}

/// 后台句柄的一次增量读取（消费性：连续读取不重复交付）。
#[derive(Debug, Clone, Default)]
pub struct ShellProcessRead {
    /// stdout 增量 + stderr 增量（模型面合并透出）。
    pub delta: String,
    /// 任一流损失型（非合法 UTF-8）。
    pub lossy: bool,
    pub stdout_spill_path: Option<PathBuf>,
    pub stderr_spill_path: Option<PathBuf>,
}

/// run/start 的**基础设施**失败（spawn 级；运行期任何状态都归入结果而非此错误）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellError {
    Spawn(String),
}

impl std::fmt::Display for ShellError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShellError::Spawn(msg) => write!(f, "shell spawn failed: {msg}"),
        }
    }
}

impl std::error::Error for ShellError {}

/// 后台进程句柄（readOutput 增量 / kill / done settle 一次）。
///
/// 单线程纪律：内部 `Rc<RefCell>` 承载状态 + subprocess 句柄（后台进程永不跨线程
/// 迁移；jobs 层在 step7 以 JobHandle 包装之）。
pub struct ShellProcess {
    inner: std::rc::Rc<std::cell::RefCell<ShellProcessInner>>,
}

struct ShellProcessInner {
    status: ShellProcessStatus,
    exit_code: Option<i32>,
    signal: Option<String>,
    handle: Option<dsh_subprocess::SubprocessHandle>,
    stdout_offset: usize,
    stderr_offset: usize,
    stdout_spill_path: Option<PathBuf>,
    stderr_spill_path: Option<PathBuf>,
}

impl ShellProcess {
    /// 由已 spawn 的 subprocess 句柄构造（状态 Running）。
    pub fn new(handle: dsh_subprocess::SubprocessHandle) -> ShellProcess {
        ShellProcess {
            inner: std::rc::Rc::new(std::cell::RefCell::new(ShellProcessInner {
                status: ShellProcessStatus::Running,
                exit_code: None,
                signal: None,
                handle: Some(handle),
                stdout_offset: 0,
                stderr_offset: 0,
                stdout_spill_path: None,
                stderr_spill_path: None,
            })),
        }
    }

    pub fn status(&self) -> ShellProcessStatus {
        self.inner.borrow().status
    }

    pub fn exit_code(&self) -> Option<i32> {
        self.inner.borrow().exit_code
    }

    pub fn signal(&self) -> Option<String> {
        self.inner.borrow().signal.clone()
    }

    /// 等待进程关闭并 settle 一次（空闲重复调用为 no-op；从不 reject）。
    /// settle 后同行职业读取器拿到的将是完整流（collector 已 join）。
    pub fn done(&self) {
        let mut inner = self.inner.borrow_mut();
        if inner.status != ShellProcessStatus::Running {
            return;
        }
        // take 出句柄避免 field-borrow 跨块冲突；settle 后放回（读取器仍需它）。
        if let Some(mut handle) = inner.handle.take() {
            let outcome = handle.wait();
            inner.exit_code = outcome.exit_code;
            inner.signal = outcome.signal.as_ref().map(|s| s.as_str().to_string());
            inner.stdout_spill_path = handle.stdout_spill_path().map(|p| p.to_path_buf());
            inner.stderr_spill_path = handle.stderr_spill_path().map(|p| p.to_path_buf());
            inner.handle = Some(handle);
            inner.status = ShellProcessStatus::Completed;
        }
    }

    /// 增量读取 stdout+stderr 合并增量（消费性：连续读取不重复）。
    pub fn read_output(&self) -> ShellProcessRead {
        let mut inner = self.inner.borrow_mut();
        let mut result = ShellProcessRead::default();
        let (mut new_stdout, mut new_stderr) = (inner.stdout_offset, inner.stderr_offset);
        if let Some(handle) = inner.handle.as_ref() {
            let delta_stdout = handle.read_stdout(inner.stdout_offset);
            new_stdout = handle.stdout_len();
            let delta_stderr = handle.read_stderr(inner.stderr_offset);
            new_stderr = handle.stderr_len();
            result.delta = format!("{delta_stdout}{delta_stderr}");
            result.lossy = handle.stdout_lossy() || handle.stderr_lossy();
            result.stdout_spill_path = inner
                .stdout_spill_path
                .clone()
                .or_else(|| handle.stdout_spill_path().map(|p| p.to_path_buf()));
            result.stderr_spill_path = inner
                .stderr_spill_path
                .clone()
                .or_else(|| handle.stderr_spill_path().map(|p| p.to_path_buf()));
        }
        inner.stdout_offset = new_stdout;
        inner.stderr_offset = new_stderr;
        result
    }

    /// 终止进程树。返回 false = 已结束（no-op）；幂等。
    pub fn kill(&self) -> bool {
        let mut inner = self.inner.borrow_mut();
        if inner.status != ShellProcessStatus::Running {
            return false;
        }
        if let Some(mut handle) = inner.handle.take() {
            handle.terminate();
            let outcome = handle.wait();
            inner.exit_code = outcome.exit_code;
            inner.signal = outcome.signal.as_ref().map(|s| s.as_str().to_string());
            inner.stdout_spill_path = handle.stdout_spill_path().map(|p| p.to_path_buf());
            inner.stderr_spill_path = handle.stderr_spill_path().map(|p| p.to_path_buf());
            inner.handle = Some(handle);
            inner.status = ShellProcessStatus::Killed;
            true
        } else {
            false
        }
    }
}

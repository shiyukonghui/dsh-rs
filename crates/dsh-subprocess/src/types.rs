//! dsh-subprocess 类型面（M5-DESIGN §2.2–§2.4）。
//!
//! 逐字对齐参考 `subprocess/subprocess/src/types.ts`：信号词汇、spawn spec（零默认）、
//! stdio 三态、有界收集/spill、句柄与 outcome。

use std::fmt;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// 信号词汇
// ---------------------------------------------------------------------------

/// 参考 `SubprocessTerminalSignal`：五个可终止信号，全平台映射。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SubprocessTerminalSignal {
    Sigint,
    Sigterm,
    Sigkill,
    Sigstp,
    Sighup,
}

impl SubprocessTerminalSignal {
    /// 规范字符串（与参考 wire 一致：`SIGINT` 等）。
    pub fn as_str(&self) -> &'static str {
        match self {
            SubprocessTerminalSignal::Sigint => "SIGINT",
            SubprocessTerminalSignal::Sigterm => "SIGTERM",
            SubprocessTerminalSignal::Sigkill => "SIGKILL",
            SubprocessTerminalSignal::Sigstp => "SIGTSTP",
            SubprocessTerminalSignal::Sighup => "SIGHUP",
        }
    }
}

/// 参考 `Signal`：可含平台保留信号集（如 `SIGUSR1`）或终结信号；未知拒绝（不静默默认）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Signal {
    Terminating(SubprocessTerminalSignal),
}

impl Signal {
    pub fn as_str(&self) -> &'static str {
        match self {
            Signal::Terminating(s) => s.as_str(),
        }
    }
}

impl From<SubprocessTerminalSignal> for Signal {
    fn from(s: SubprocessTerminalSignal) -> Self {
        Signal::Terminating(s)
    }
}

impl fmt::Display for Signal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Signal {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let sig = match s {
            "SIGINT" => SubprocessTerminalSignal::Sigint,
            "SIGTERM" => SubprocessTerminalSignal::Sigterm,
            "SIGKILL" => SubprocessTerminalSignal::Sigkill,
            "SIGTSTP" => SubprocessTerminalSignal::Sigstp,
            "SIGHUP" => SubprocessTerminalSignal::Sighup,
            other => return Err(format!("unknown signal: {other}")),
        };
        Ok(Signal::Terminating(sig))
    }
}

// ---------------------------------------------------------------------------
// spawn spec（零默认）
// ---------------------------------------------------------------------------

/// 参考 `SubprocessSpawnSpec`：**零默认**——argv/cwd/stdio/graceMs 全显式；
/// signal?/env? 可选。绝不在 `spawn()` 内部隐藏默认值。
#[derive(Debug, Clone)]
pub struct SubprocessSpawnSpec {
    /// 精确 argv（argv[0] 为可执行或已解析路径）。
    pub argv: Vec<String>,
    /// 显式工作目录。
    pub cwd: PathBuf,
    /// stdio 三态（见 [`ChildStdio`]）。
    pub stdio: ChildStdio,
    /// SIGTERM→SIGKILL 宽限毫秒（≤ MAX_TIMER_DELAY_MS）。
    pub grace_ms: u64,
    /// 取消 → 树级 terminate。
    pub signal: Option<Signal>,
    /// 显式环境（缺省时实现用 scrubbed 父环境）。
    pub env: Option<Vec<(String, String)>>,
}

/// 三路 stdio 描述（参考 `spawn.ts` 的 `stdio` 形）。
#[derive(Debug, Clone)]
pub struct ChildStdio {
    pub stdin: StdinMode,
    pub stdout: StdoutMode,
    pub stderr: StdoutMode,
}

/// 参考 stdin 三态：`'ignore' | 'pipe' | {data}`。
#[derive(Debug, Clone)]
pub enum StdinMode {
    /// 子进程读端接 null。
    Ignore,
    /// 不透写；宿主保留句柄（后续可写）。
    Pipe,
    /// 写入这些字节后关闭（一次性给数据）。
    WriteBytes(Vec<u8>),
}

/// 参考 stdout/stderr 三态（共用一型）。
#[derive(Debug, Clone)]
pub enum StdoutMode {
    /// 透传管道（宿主读）。
    Pipe,
    /// 继承宿主输出。
    Inherit,
    /// 有界收集 + 可选 spill（可恢复完整流）。
    Collect(SubprocessCollect),
}

/// 有界收集预算；`spill` 缺省 = 仅内存 tail（诊断形），带 spill = 完整流落盘。
#[derive(Debug, Clone)]
pub struct SubprocessCollect {
    pub max_bytes: usize,
    pub spill: Option<SubprocessSpill>,
}

/// 溢出落盘：总量上限 + 目录（0700）。
#[derive(Debug, Clone)]
pub struct SubprocessSpill {
    pub max_bytes: u64,
    pub dir: PathBuf,
}

// ---------------------------------------------------------------------------
// outcome / 收集
// ---------------------------------------------------------------------------

/// spawn 级错误（极少；编译/路径/权限等），区别于运行 outcome。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessError {
    Spawn(String),
    Io(String),
}

impl std::error::Error for ProcessError {}

impl fmt::Display for ProcessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProcessError::Spawn(msg) => write!(f, "spawn failed: {msg}"),
            ProcessError::Io(msg) => write!(f, "io: {msg}"),
        }
    }
}

impl From<std::io::Error> for ProcessError {
    fn from(e: std::io::Error) -> Self {
        ProcessError::Io(e.to_string())
    }
}

/// 参考 `SubprocessOutcome`：运行后的稳定事实（exitCode/signal）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubprocessOutcome {
    /// 退出码；`None` = 被信号终止。
    pub exit_code: Option<i32>,
    /// 终止信号名（若被信号终止）。
    pub signal: Option<Signal>,
}

/// 有界收集结果：offset-based 非消费 reader + lossy/spill 标记。
#[derive(Debug)]
pub struct CollectedOutput {
    data: Vec<u8>,
    lossy: bool,
    spill_path: Option<PathBuf>,
}

impl CollectedOutput {
    pub fn from_bytes(data: Vec<u8>, lossy: bool, spill_path: Option<PathBuf>) -> Self {
        Self {
            data,
            lossy,
            spill_path,
        }
    }

    /// 从 offset 起按 UTF-8 损失型转换（`readFrom(0)` = 全量）。
    pub fn read_from(&self, offset: usize) -> String {
        String::from_utf8_lossy(&self.data[offset.min(self.data.len())..]).into_owned()
    }

    /// 当前缓冲的字节长度（增量读取游标下界：`nextOffset = data_len()`）。
    pub fn data_len(&self) -> usize {
        self.data.len()
    }

    pub fn lossy(&self) -> bool {
        self.lossy
    }

    pub fn spill_path(&self) -> Option<&Path> {
        self.spill_path.as_deref()
    }
}

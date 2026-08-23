//! dsh-terminal 词汇面（M5-DESIGN §6.1，逐字 `terminal/*`）。
//!
//! 参考 `terminal/src/registry.ts`：Branded 会话 id、owner=精确 Agent、每会话单活跃
//! send（SEND_ACTIVE）、错误的可分类枚举、wait_reason 词汇（前端计时逻辑由后端
//! 拥有，注册表只做守卫与派发）。

use std::fmt;

/// Branded 会话 id（无 dsh-brand 依赖：私有 newtype + Display/FromStr/From）。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TerminalSessionId(String);

impl TerminalSessionId {
    /// 从原始字符串构造（服务内部自增，不验格式）。
    pub fn from_raw(raw: String) -> TerminalSessionId {
        TerminalSessionId(raw)
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TerminalSessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::str::FromStr for TerminalSessionId {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(TerminalSessionId(s.to_string()))
    }
}

/// 可发送并等待的信号（与 `SubprocessTerminalSignal` 同词表）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalSignal {
    Sigint,
    Sigterm,
    Sigkill,
    Sigstp,
    Sighup,
}

impl TerminalSignal {
    pub fn as_str(&self) -> &'static str {
        match self {
            TerminalSignal::Sigint => "SIGINT",
            TerminalSignal::Sigterm => "SIGTERM",
            TerminalSignal::Sigkill => "SIGKILL",
            TerminalSignal::Sigstp => "SIGTSTP",
            TerminalSignal::Sighup => "SIGHUP",
        }
    }
}

/// 发送如何被判定为「本段已交付」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalWaitReason {
    /// 服务端收到输入即返回。
    StdinRead,
    /// 静默期后推断空闲。
    InferredIdle,
    /// 达到 timeout 阈值。
    Timeout,
    /// 会话退出。
    SessionExit,
}

/// 会话生命周期状态（模型可见）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalSessionStatus {
    Running,
    Exited,
    Aborted,
}

/// 错误码（可逐字映射 `[terminal ...]` 文案）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalErrorCode {
    DuplicateBackend,
    DuplicateName,
    ForeignSession,
    NoBackend,
    NoSession,
    OwnerNotLive,
    SendActive,
    ServiceDisposing,
}

/// 分类错误：`[code] message`（参考 HarnessError 语义）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalError {
    pub code: TerminalErrorCode,
    pub message: String,
}

impl TerminalError {
    pub fn new(code: TerminalErrorCode, message: impl Into<String>) -> TerminalError {
        TerminalError {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for TerminalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}

impl std::error::Error for TerminalError {}

impl From<dsh_subprocess::ProcessError> for TerminalError {
    fn from(e: dsh_subprocess::ProcessError) -> Self {
        TerminalError::new(TerminalErrorCode::NoBackend, e.to_string())
    }
}

/// 发送请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalSendRequest {
    pub text: String,
    pub submit: bool,
    /// 发送输入前/后附加的信号（参考：提交前可选发信号）。
    pub signal: Option<TerminalSignal>,
}

/// 发送结果（viewport 由后端在等待窗口内捕获）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalSendResult {
    pub viewport: String,
    pub wait_reason: TerminalWaitReason,
    pub session_status: TerminalSessionStatus,
    pub truncated: bool,
}

/// 后端类型（未来可扩 pwsh；当前 Bash）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalBackendKind {
    Bash,
}

/// 会话视图（list 输出）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalSessionView {
    pub id: TerminalSessionId,
    pub owner: String,
    pub name: Option<String>,
    pub label: String,
    pub status: TerminalSessionStatus,
    pub backend: String,
}

/// 终端配置（逐字 §6.2 默认）。
#[derive(Debug, Clone)]
pub struct TerminalConfig {
    pub rows: u16,
    pub cols: u16,
    pub scrollback_lines: usize,
    pub scrollback_max_bytes: usize,
    pub max_read_bytes: usize,
    pub poll_interval_ms: u64,
    pub idle_silence_ms: u64,
    pub timeout_ms: u64,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        TerminalConfig {
            rows: 40,
            cols: 160,
            scrollback_lines: 10_000,
            scrollback_max_bytes: 4 * 1024 * 1024,
            max_read_bytes: 256 * 1024,
            poll_interval_ms: 50,
            idle_silence_ms: 3_000,
            timeout_ms: 30_000,
        }
    }
}

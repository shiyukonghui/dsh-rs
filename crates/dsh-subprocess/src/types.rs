//! dsh-subprocess 类型面（M5-DESIGN §2.2–§2.4）。
//!
//! 逐字对齐参考 `subprocess/subprocess/src/types.ts`：先落地由红测驱动的信号词汇，
//! 其余类型（SpawnSpec/Collect/Handle）随各自红测逐步加入。

use std::fmt;

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

//! dsh-subprocess — M5 执行世界最底层原语（设计见 M5-DESIGN.md §2）。
//!
//! 由红→绿测试驱动（tests/scrub.rs、tests/signal.rs 先行）：`scrubbed_parent_env` 与
//! `Signal`/`SubprocessTerminalSignal` 词汇已落地；其余（spawn/collect/terminate/
//! spawnTerminal）将在各自红测后陆续加入。

mod types;

use std::ffi::OsString;

pub use types::{Signal, SubprocessTerminalSignal, SubprocessTerminalSignal as TerminalSignal};

/// 子进程环境条目（键/值均透传 OsString，仅供 `scrubbed_parent_env` 使用）。
pub type EnvEntry = (OsString, OsString);

/// 从父进程环境派生子进程环境：− credential-shaped（键大写后含 KEY/PASSWORD/SECRET/TOKEN/
/// 子串，大小写无关）− 所有 `DSH_*` 前缀托管键。与 `subprocess/src/types.ts` 的
/// `scrubbedParentEnv()` 对齐：凭据与环境管理变量绝不透传给子进程。
///
/// 空源 → 空结果；仅保留既非凭据形又非 DSH_* 的条目（按键名原样保留）。
pub fn scrubbed_parent_env(src: &[EnvEntry]) -> Vec<EnvEntry> {
    src.iter()
        .filter(|(k, _)| {
            let upper = k.to_string_lossy().to_ascii_uppercase();
            !upper.starts_with("DSH_")
                && !upper.contains("KEY")
                && !upper.contains("PASSWORD")
                && !upper.contains("SECRET")
                && !upper.contains("TOKEN")
        })
        .cloned()
        .collect()
}

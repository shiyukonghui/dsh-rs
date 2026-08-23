//! dsh-subprocess：Signal + TerminalSignal 词汇（M5-DESIGN §2.4）。
//!
//! 参考 `subprocess/subprocess/src/types.ts`：`SubprocessTerminalSignal =
//! SIGINT|SIGTERM|SIGKILL|SIGTSTP|SIGHUP`。本测试先定义字符串↔枚举双向契约（红）。

use dsh_subprocess::{Signal, SubprocessTerminalSignal};

#[test]
fn signal_from_str_accepts_all_five() {
    for (s, expect) in [
        ("SIGINT", SubprocessTerminalSignal::Sigint),
        ("SIGTERM", SubprocessTerminalSignal::Sigterm),
        ("SIGKILL", SubprocessTerminalSignal::Sigkill),
        ("SIGTSTP", SubprocessTerminalSignal::Sigstp),
        ("SIGHUP", SubprocessTerminalSignal::Sighup),
    ] {
        let parsed: Signal = s.parse().unwrap_or_else(|_| panic!("parse {s}"));
        assert_eq!(parsed, Signal::from(expect), "parse {s}");
    }
}

#[test]
fn signal_to_str_roundtrip() {
    for s in ["SIGINT", "SIGTERM", "SIGKILL", "SIGTSTP", "SIGHUP"] {
        let parsed: Signal = s.parse().unwrap();
        assert_eq!(parsed.as_str(), s, "as_str roundtrip {s}");
    }
}

#[test]
fn signal_rejects_unknown() {
    assert!("SIGUSR1".parse::<Signal>().is_err());
    assert!("".parse::<Signal>().is_err());
}

#[test]
fn terminal_signal_error_names() {
    // 镜像参考错误词：非法信号必须是可诊断错误而非静默默认。
    let err = "SIGFOO".parse::<Signal>().unwrap_err();
    assert!(!err.is_empty(), "error should be non-empty diagnostic");
}

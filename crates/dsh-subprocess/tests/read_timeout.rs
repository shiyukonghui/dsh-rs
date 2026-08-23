//! dsh-subprocess：读取/超时契约（M5-DESIGN §2.2–§2.4 增量）。
//!
//! 锁定本增量新增的原语：`wait_timeout`（同步超时，单线程核心的轮询手段）与
//! potacked 增量读取（`read_stdout(offset)` / `stdout_len()`，dsh-shell 后台句柄
//! `readOutput` 的基座）。

use dsh_subprocess::{ChildStdio, StdinMode, StdoutMode, SubprocessCollect, SubprocessSpawnSpec};

fn spec(argv: Vec<String>) -> SubprocessSpawnSpec {
    SubprocessSpawnSpec {
        argv,
        cwd: std::env::current_dir().expect("cwd"),
        stdio: ChildStdio {
            stdin: StdinMode::Ignore,
            stdout: StdoutMode::Collect(SubprocessCollect {
                max_bytes: 4096,
                spill: None,
            }),
            stderr: StdoutMode::Collect(SubprocessCollect {
                max_bytes: 4096,
                spill: None,
            }),
        },
        grace_ms: 1000,
        signal: None,
        env: None,
    }
}

fn echo_argv(text: &str) -> Vec<String> {
    #[cfg(windows)]
    {
        vec![
            "cmd".to_string(),
            "/c".to_string(),
            "echo".to_string(),
            text.to_string(),
        ]
    }
    #[cfg(not(windows))]
    {
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            format!("echo {text}"),
        ]
    }
}

fn slow_argv() -> Vec<String> {
    #[cfg(windows)]
    {
        vec![
            "cmd".to_string(),
            "/c".to_string(),
            "ping".to_string(),
            "-n".to_string(),
            "8".to_string(),
            "127.0.0.1".to_string(),
        ]
    }
    #[cfg(not(windows))]
    {
        vec!["/bin/sleep".to_string(), "2".to_string()]
    }
}

#[test]
fn wait_timeout_returns_none_for_slow_process() {
    let mut handle = dsh_subprocess::spawn(&spec(slow_argv())).expect("spawn ok");
    // 50ms 内 ping/sleep 2 秒的进程不可能退出
    let quick = handle.wait_timeout(std::time::Duration::from_millis(50));
    assert!(quick.is_none(), "slow process must not settle within 50ms");
    handle.terminate();
    let outcome = handle.wait();
    assert_ne!(
        outcome.exit_code,
        Some(0),
        "terminated process must not exit 0"
    );
}

#[test]
fn wait_timeout_returns_outcome_for_fast_process() {
    let mut handle = dsh_subprocess::spawn(&spec(echo_argv("fast-ok"))).expect("spawn ok");
    let within = handle.wait_timeout(std::time::Duration::from_secs(10));
    assert_eq!(within.as_ref().map(|o| o.exit_code), Some(Some(0)));
    let out = handle.collected_stdout();
    assert!(out.contains("fast-ok"), "stdout carries echo, got {out:?}");
}

#[test]
fn incremental_read_stdout_offset() {
    let mut handle = dsh_subprocess::spawn(&spec(echo_argv("first-line"))).expect("spawn ok");
    handle.wait();
    // 首读：0 起全量；此后 offset=len → 空；最后一次读取不重复。
    let all = handle.read_stdout(0);
    assert!(all.contains("first-line"));
    let len = handle.stdout_len();
    let rest = handle.read_stdout(len);
    assert_eq!(rest, "", "offset 到缓冲尾部后无增量");
}

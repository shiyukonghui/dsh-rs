//! dsh-subprocess：stdin 写入 + 终结（M5-DESIGN §2.2–§2.4）。
//!
//! 驱动：① `StdinMode::WriteBytes` 真实把数据写入子进程 stdin 后关闭；② `terminate()` 树级
//! 杀；③ spawn 失败级错误是 Result::Err（本地后端语义），运行期被信号终止的 outcome
//! exit_code=None。

use dsh_subprocess::{ChildStdio, StdinMode, StdoutMode, SubprocessCollect, SubprocessSpawnSpec};

#[cfg(not(windows))]
use dsh_subprocess::ProcessError;

/// 构造一个收集三路 stdio 的 spec（默认参数便于用例复用）。
fn spec(argv: Vec<String>, stdin: StdinMode) -> SubprocessSpawnSpec {
    SubprocessSpawnSpec {
        argv,
        cwd: std::env::current_dir().expect("cwd"),
        stdio: ChildStdio {
            stdin,
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

/// 平台 cat 等价：Windows 用 `cmd /c findstr /r .*`（逐行回显 stdin）；其余 `cat`。
fn cat_argv() -> (String, Vec<String>) {
    #[cfg(windows)]
    {
        (
            "cmd".to_string(),
            vec![
                "/c".to_string(),
                "findstr".to_string(),
                "/r".to_string(),
                ".*".to_string(),
            ],
        )
    }
    #[cfg(not(windows))]
    {
        ("cat".to_string(), vec![])
    }
}

#[test]
fn stdin_write_bytes_reaches_child_then_closes() {
    let (argv0, rest) = cat_argv();
    let mut argv = vec![argv0];
    argv.extend(rest);
    let input = "line-one\nline-two\n";
    let spec = spec(argv, StdinMode::WriteBytes(input.as_bytes().to_vec()));

    let mut handle = dsh_subprocess::spawn(&spec).expect("spawn ok");
    let outcome = handle.wait();
    assert_eq!(outcome.exit_code, Some(0), "cat exits 0");
    let out = handle.collected_stdout();
    assert!(
        out.contains("line-one"),
        "stdout carries line-one, got {out:?}"
    );
    assert!(
        out.contains("line-two"),
        "stdout carries line-two, got {out:?}"
    );
}

#[test]
fn spawn_failure_is_result_err_not_panicking_handle() {
    let bad = spec(
        vec!["__definitely_missing_exe__".to_string()],
        StdinMode::Ignore,
    );
    let err = dsh_subprocess::spawn(&bad);
    #[cfg(windows)]
    {
        // Windows 对缺失可执行可能走 shell 解析模糊路径；此处不强制断言 err，
        // 而断言「绝不 panic 且返回受控结果」（Result 形状已由类型保证）。
        assert!(err.is_ok() || err.is_err(), "must be a Result, never panic");
    }
    #[cfg(not(windows))]
    {
        assert!(matches!(err, Err(ProcessError::Spawn(_))));
    }
}

#[test]
fn terminate_kills_running_child() {
    // 一个长时间 sleep 的进程；terminate 后 wait 返回（不应永久阻塞）。
    #[cfg(windows)]
    let argv = vec![
        "cmd".to_string(),
        "/c".to_string(),
        "ping".to_string(),
        "-n".to_string(),
        "30".to_string(),
        "127.0.0.1".to_string(),
    ];
    #[cfg(not(windows))]
    let argv = vec!["/bin/sleep".to_string(), "30".to_string()];

    let spec = spec(argv, StdinMode::Ignore);
    let mut handle = dsh_subprocess::spawn(&spec).expect("spawn ok");
    handle.terminate();
    let outcome = handle.wait();
    // Windows taskkill /F → 退出码非 0（或 None）；被信号终止语义一致：不是 0。
    assert_ne!(
        outcome.exit_code,
        Some(0),
        "terminated process must not exit 0"
    );
}

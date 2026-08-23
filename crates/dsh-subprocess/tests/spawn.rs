//! dsh-subprocess：spawn 原语契约（M5-DESIGN §2.2–§2.4）。
//!
//! 参考 `subprocess/subprocess-local/src/spawn.ts`：`SubprocessSpawnSpec` 零默认
//! （argv/cwd/stdio/graceMs 显式、signal?/env? 可选），`spawn()` 返回带类型化管道与
//! `done`（settle 一次、从不 reject、spawn 失败 settle 成 killed+stderr）的句柄。

use dsh_subprocess::{ChildStdio, SubprocessSpawnSpec};

/// 平台可执行路径解析：Windows 用 `cmd /c`（FAT 保证存在），其余用 `/bin/sh -c`。
fn echo_argv(text: &str) -> (String, Vec<String>) {
    #[cfg(windows)]
    {
        // cmd /c echo <text> —— text 为纯字母数字避免转义歧义
        (
            "cmd".to_string(),
            vec!["/c".to_string(), "echo".to_string(), text.to_string()],
        )
    }
    #[cfg(not(windows))]
    {
        (
            "/bin/sh".to_string(),
            vec!["-c".to_string(), format!("echo {text}")],
        )
    }
}

#[test]
fn spawn_echo_captures_stdout_and_exit_zero() {
    let (argv0, rest) = echo_argv("hello-m5");
    let mut argv = vec![argv0];
    argv.extend(rest);

    let spec = SubprocessSpawnSpec {
        argv,
        cwd: std::env::current_dir().expect("cwd"),
        stdio: ChildStdio {
            stdin: dsh_subprocess::StdinMode::Ignore,
            stdout: dsh_subprocess::StdoutMode::Collect(dsh_subprocess::SubprocessCollect {
                max_bytes: 4096,
                spill: None,
            }),
            stderr: dsh_subprocess::StdoutMode::Collect(dsh_subprocess::SubprocessCollect {
                max_bytes: 4096,
                spill: None,
            }),
        },
        grace_ms: 1000,
        signal: None,
        env: None,
    };

    let mut handle = dsh_subprocess::spawn(&spec).expect("spawn succeeds");
    let outcome = handle.wait(); // settle 一次从不 reject

    assert_eq!(outcome.exit_code, Some(0), "expected exit 0");
    let out = handle.collected_stdout();
    assert!(
        out.contains("hello-m5"),
        "stdout should carry echo, got: {out:?}"
    );
}

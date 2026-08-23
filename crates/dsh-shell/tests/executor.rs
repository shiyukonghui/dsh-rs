//! dsh-shell：LocalBashExecutor 真实 bash 执行（M5-DESIGN §5.3）。
//!
//! Windows 开发机上 system32\bash.exe 是 WSL 启动器（依赖 WSL 安装）；Git Bash(msys)
//! 需创建 signal pipe/共享内存——在本 DSH 沙箱会话中被拒（Win32 error 5 / WSL
//! E_ACCESSDENIED）。因此每个用例先做一次可用性探测：不可用则打印明确原因并跳过，
//! 在 Linux/正常开发机/CI 上这些用例真实跑 bash（不降级架构、不假绿，见 DECISIONS）。

use dsh_shell::{BashConfig, LocalBashExecutor, ShellExecRequest, ShellProcessStatus};
use std::sync::OnceLock;

fn test_bash() -> String {
    #[cfg(windows)]
    {
        for candidate in [
            "C:\\Program Files\\Git\\bin\\bash.exe",
            "C:\\Program Files\\Git\\usr\\bin\\bash.exe",
        ] {
            if std::path::Path::new(candidate).exists() {
                return candidate.to_string();
            }
        }
        "bash".to_string()
    }
    #[cfg(not(windows))]
    {
        "bash".to_string()
    }
}

/// 探测 bash 是否在本环境真正可启动（一次缓存）。
fn bash_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        let mut child = match std::process::Command::new(test_bash())
            .args(["-c", "echo dsh-probe-ok"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                eprintln!("dsh-shell: bash 无法启动，跳过真实 bash 用例（spawn: {e}）");
                return false;
            }
        };
        let mut ok = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match child.try_wait() {
                Ok(Some(st)) => {
                    if st.success() {
                        use std::io::Read;
                        let mut out = String::new();
                        let _ = child.stdout.take().map(|mut f| f.read_to_string(&mut out));
                        ok = out.contains("dsh-probe-ok");
                    }
                    break;
                }
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
                Err(e) => {
                    eprintln!("dsh-shell: bash 探测异常（{e}），跳过真实 bash 用例");
                    break;
                }
            }
        }
        if !ok {
            eprintln!("dsh-shell: bash 在本沙箱不可用（msys/WSL 被环境拒绝），跳过真实 bash 用例");
        }
        ok
    })
}

/// 每个用例的入口：不可探测到可用 bash 时跳过。
fn require_bash() -> bool {
    if bash_available() {
        true
    } else {
        eprintln!("    ~ 跳过：当前环境无法启动 bash（非实现缺陷，见文件头）");
        false
    }
}

fn executor() -> LocalBashExecutor {
    LocalBashExecutor::new(BashConfig {
        bash_path: Some(test_bash().into()),
        ..Default::default()
    })
    .expect("config serviceable")
}

fn resolve_spec(cmd: &str) -> dsh_shell::ShellExecSpec {
    let ex = executor();
    ex.resolve(&ShellExecRequest {
        command: cmd.into(),
        ..Default::default()
    })
    .expect("resolve ok")
}

#[test]
fn run_echo_exit_zero_captures_stdout() {
    if !require_bash() {
        return;
    }
    let ex = executor();
    let spec = resolve_spec("echo shell-m5-ok");
    let result = ex.run(&spec).expect("run ok");
    assert_eq!(result.exit_code, Some(0));
    assert!(!result.timed_out);
    assert!(
        result.stdout.text.contains("shell-m5-ok"),
        "stdout: {}",
        result.stdout.text
    );
    assert_eq!(result.sandbox, None);
}

#[test]
fn run_nonzero_exit_is_result_not_err() {
    if !require_bash() {
        return;
    }
    let ex = executor();
    let spec = resolve_spec("exit 7");
    let result = ex.run(&spec).expect("run ok");
    assert_eq!(result.exit_code, Some(7));
    assert!(!result.timed_out);
}

#[test]
fn run_captures_stderr() {
    if !require_bash() {
        return;
    }
    let ex = executor();
    let spec = resolve_spec("echo boom >&2");
    let result = ex.run(&spec).expect("run ok");
    assert!(
        result.stderr.text.contains("boom"),
        "stderr: {}",
        result.stderr.text
    );
}

#[test]
fn run_writes_stdin_then_closes() {
    if !require_bash() {
        return;
    }
    let ex = executor();
    let spec = resolve_spec("read x; echo got:$x");
    let spec = dsh_shell::ShellExecSpec {
        stdin: Some("hello\n".into()),
        ..spec
    };
    let result = ex.run(&spec).expect("run ok");
    assert!(
        result.stdout.text.contains("got:hello"),
        "stdout: {}",
        result.stdout.text
    );
}

#[test]
fn run_timeout_kills_and_marks_timed_out() {
    if !require_bash() {
        return;
    }
    let ex = executor();
    let spec = resolve_spec("sleep 100");
    let spec = dsh_shell::ShellExecSpec {
        timeout_ms: 300,
        ..spec
    };
    let started = std::time::Instant::now();
    let result = ex.run(&spec).expect("run ok");
    assert!(result.timed_out, "long sleep must be killed by timeout");
    assert!(
        started.elapsed().as_secs() < 30,
        "timeout kill must be swift"
    );
}

#[test]
fn start_background_incremental_read_and_done() {
    if !require_bash() {
        return;
    }
    let ex = executor();
    let spec = resolve_spec("sleep 0.8; echo later-out");
    let proc = ex.start(&spec).expect("start ok");
    assert_eq!(proc.status(), ShellProcessStatus::Running);
    // 立即读：进程尚在 sleep，不应已有 "later-out"
    let early = proc.read_output();
    assert!(
        !early.delta.contains("later-out"),
        "early delta: {:?}",
        early.delta
    );
    proc.done();
    assert_eq!(proc.status(), ShellProcessStatus::Completed);
    assert_eq!(proc.exit_code(), Some(0));
    let final_read = proc.read_output();
    assert!(
        final_read.delta.contains("later-out"),
        "final delta: {:?}",
        final_read.delta
    );
    // 消费性：完成后再次读取不重复
    let again = proc.read_output();
    assert!(
        !again.delta.contains("later-out"),
        "must not re-deliver: {:?}",
        again.delta
    );
}

#[test]
fn start_kill_terminates_and_is_idempotent() {
    if !require_bash() {
        return;
    }
    let ex = executor();
    let spec = resolve_spec("sleep 100");
    let proc = ex.start(&spec).expect("start ok");
    assert_eq!(proc.status(), ShellProcessStatus::Running);
    assert!(proc.kill(), "kill on running returns true");
    assert_eq!(proc.status(), ShellProcessStatus::Killed);
    assert_ne!(proc.exit_code(), Some(0), "killed process must not exit 0");
    assert!(!proc.kill(), "second kill on finished is no-op");
    proc.done(); // settled：no-op，不阻塞
}

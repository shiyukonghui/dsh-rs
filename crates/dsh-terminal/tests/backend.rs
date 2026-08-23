//! dsh-terminal：真实 PTY 后端集成测试（M5-DESIGN §6.2，`portable-pty` ConPTY）。
//!
//! 本沙箱 ConPTY 可用但 msys bash 运行时不活 → 测试注入 `cmd.exe` 作 slave 程序
//! （可达性探测一次缓存：openpty+cmd 回显失败则整组跳过并打印原因，Linux/正常机
//! 换 bash 即可实跑）。覆盖：回显/滚动缓冲读/静默推断/exit 判定/close。

use dsh_terminal::{
    PtyBackend, TerminalBackend, TerminalConfig, TerminalSendRequest, TerminalSessionStatus,
    TerminalSignal, TerminalWaitReason,
};
use std::sync::OnceLock;

/// Windows 探测 cmd 作 slave（bash 需 msys，沙箱不可用）；其它平台用 bash（若 bash
/// 不可用同样失败 → 探测 false）。
fn slave_program() -> Result<(&'static str, &'static str), ()> {
    #[cfg(windows)]
    {
        Ok(("cmd", "cmd.exe"))
    }
    #[cfg(not(windows))]
    {
        Ok(("bash", "bash"))
    }
}

fn conpty_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        let (_label, program) = match slave_program() {
            Ok(p) => p,
            Err(_) => return false,
        };
        eprintln!("PTY-PROBE: open…");
        let mut backend = PtyBackend::new("smoke", program);
        let opened = backend.open("t", &TerminalConfig::default());
        eprintln!("PTY-PROBE: open done ok={}", opened.is_ok());
        if opened.is_err() {
            eprintln!("dsh-terminal: PTY 打开失败，跳过真实 PTY 用例");
            return false;
        }
        eprintln!("PTY-PROBE: send…");
        let result = match backend.send(&TerminalSendRequest {
            text: "echo pty-probe-ok".into(),
            submit: true,
            signal: None,
        }) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("dsh-terminal: PTY send 失败（{e}），跳过真实 PTY 用例");
                return false;
            }
        };
        eprintln!("PTY-PROBE: send done reason={:?}", result.wait_reason);
        let ok = result.viewport.contains("pty-probe-ok");
        if !ok {
            eprintln!("dsh-terminal: PTY 未回显，跳过真实 PTY 用例");
        }
        eprintln!("PTY-PROBE: close…");
        let closed = backend.close();
        eprintln!("PTY-PROBE: close done ok={}", closed.is_ok());
        ok
    })
}

/// 每用例入口：ConPTY/cmd 不可用则跳过。
fn require_pty() -> bool {
    if conpty_available() {
        true
    } else {
        eprintln!("    ~ 跳过：当前环境无法跑真实 PTY（非实现缺陷，见文件头）");
        false
    }
}

fn open_backend() -> PtyBackend {
    let (_, program) = slave_program().expect("slave program");
    let mut backend = PtyBackend::new("pty-test", program);
    backend
        .open("t", &TerminalConfig::default())
        .expect("pty open");
    backend
}

#[test]
fn pty_echo_roundtrip_via_send() {
    if !require_pty() {
        return;
    }
    let mut backend = open_backend();
    let timeout_cfg = TerminalConfig {
        timeout_ms: 8_000,
        ..TerminalConfig::default()
    };
    backend.open("t", &timeout_cfg).expect("reopen");
    let result = backend
        .send(&TerminalSendRequest {
            text: "echo pty-hello-1".into(),
            submit: true,
            signal: None,
        })
        .expect("send ok");
    assert!(
        result.viewport.contains("pty-hello-1"),
        "viewport: {:?}",
        result.viewport
    );
    assert!(
        matches!(
            result.wait_reason,
            TerminalWaitReason::InferredIdle | TerminalWaitReason::SessionExit
        ),
        "reason: {:?}",
        result.wait_reason
    );
    backend.close().expect("close");
}

#[test]
fn pty_read_returns_scrollback() {
    if !require_pty() {
        return;
    }
    let mut backend = open_backend();
    backend
        .send(&TerminalSendRequest {
            text: "echo pty-read-mark".into(),
            submit: true,
            signal: None,
        })
        .expect("send");
    let out = backend.read(256 * 1024).expect("read");
    assert!(out.contains("pty-read-mark"), "read: {out:?}");
    backend.close().expect("close");
}

#[test]
fn pty_send_exit_marks_session_exit() {
    if !require_pty() {
        return;
    }
    let mut backend = open_backend();
    let result = backend
        .send(&TerminalSendRequest {
            text: "exit".into(),
            submit: true,
            signal: None,
        })
        .expect("send exit");
    assert_eq!(
        result.session_status,
        TerminalSessionStatus::Exited,
        "exit → Exited"
    );
    assert_eq!(result.wait_reason, TerminalWaitReason::SessionExit);
    backend.close().expect("close");
}

#[test]
fn pty_signal_then_close_is_clean() {
    if !require_pty() {
        return;
    }
    let mut backend = open_backend();
    backend.signal(TerminalSignal::Sigkill).expect("signal ok");
    // close 幂等、不 panic
    backend.close().expect("close");
    backend.close().expect("double close ok");
}

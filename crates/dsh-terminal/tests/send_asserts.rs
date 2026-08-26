//! D-115 Phase 4 serve worker化：会话服务须 `Send` 以便放进 `Arc<Mutex<T>>` 从任意
//! worker 线程调用。backend 存 `Box<dyn TerminalBackend + Send>`（非 Sync），故只断言
//! Send。PtyBackend 直接断言 Send。编译期断言（不引入运行时行为）。

use dsh_terminal::{PtyBackend, TerminalSessionService};

fn assert_send<T: Send>() {}

#[test]
fn session_service_is_send() {
    assert_send::<TerminalSessionService>();
}

#[test]
fn pty_backend_is_send() {
    assert_send::<PtyBackend>();
}

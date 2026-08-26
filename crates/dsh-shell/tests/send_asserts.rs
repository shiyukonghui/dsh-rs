//! D-115 Phase 4 serve worker化：shell 句柄须 `Send`（+`Sync`）以便放进
//! `Arc<Mutex<T>>` 从任意 worker 线程调用。编译期断言（不引入运行时行为）。

use dsh_shell::{LocalShellExecutor, ShellProcess};

fn assert_send<T: Send>() {}
fn assert_sync<T: Sync>() {}

#[test]
fn shell_process_is_send_sync() {
    assert_send::<ShellProcess>();
    assert_sync::<ShellProcess>();
}

#[test]
fn executor_is_send_sync() {
    assert_send::<LocalShellExecutor>();
    assert_sync::<LocalShellExecutor>();
}

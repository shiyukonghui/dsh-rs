//! D-115 Phase 4 serve worker化：注册表须 `Send` 以便放进 `Arc<Mutex<T>>` 从任意
//! worker 线程调用。编译期断言（不引入运行时行为）。

use dsh_jobs::JobRegistry;

fn assert_send<T: Send>() {}

#[test]
fn registry_is_send() {
    assert_send::<JobRegistry>();
}

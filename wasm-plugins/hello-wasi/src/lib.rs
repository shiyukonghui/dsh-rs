//! hello-wasi WASM 插件：C ABI + WASI 能力验证（M19）。
//!
//! 与 hello 相同的 dsh-wasmrt ABI（`plugin_apply`/`plugin_dispose`），但经
//! **WASI preview1** 读取环境变量 `DSH_TEST`（wasm32-wasip1 构建）——验证
//! C ABI 路径的 WASI 能力授予（caps 含 wasi-env 时 env 可读，否则 import 解析
//! 失败）。
//!
//! `plugin_apply`：读取 `DSH_TEST` 环境变量 → `host_log("DSH_TEST=...")`。

use serde_json::Value;
use std::alloc::Layout;

#[link(wasm_import_module = "env")]
extern "C" {
    fn host_log(ptr: *const u8, len: usize);
}

fn log(msg: &str) {
    unsafe {
        host_log(msg.as_ptr(), msg.len());
    }
}

fn read<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    if len == 0 || ptr.is_null() {
        return &[];
    }
    unsafe { std::slice::from_raw_parts(ptr, len) }
}

fn read_value(ptr: *const u8, len: usize) -> Value {
    serde_json::from_slice(read(ptr, len)).unwrap_or(Value::Null)
}

#[no_mangle]
pub extern "C" fn alloc(len: usize) -> *mut u8 {
    if len == 0 {
        return std::ptr::null_mut();
    }
    unsafe { std::alloc::alloc(Layout::from_size_align(len, 1).unwrap()) }
}

#[no_mangle]
pub extern "C" fn dealloc(ptr: *mut u8, len: usize) {
    if len == 0 || ptr.is_null() {
        return;
    }
    unsafe { std::alloc::dealloc(ptr, Layout::from_size_align(len, 1).unwrap()) }
}

#[no_mangle]
pub extern "C" fn plugin_apply(config_ptr: *const u8, config_len: usize) -> i32 {
    let config = read_value(config_ptr, config_len);
    log(&format!("wasm apply config={config}"));

    // WASI env 读取（wasm32-wasip1：std::env 走 wasi_snapshot_preview1）；
    // 读两个变量名：M19_ENV_A / M19_ENV_B（并行测试各设各的，互不覆盖）
    let env_a = std::env::var("M19_ENV_A").unwrap_or_else(|_| "<unset>".to_string());
    log(&format!("ENV_A={env_a}"));
    let env_b = std::env::var("M19_ENV_B").unwrap_or_else(|_| "<unset>".to_string());
    log(&format!("ENV_B={env_b}"));

    // WASI fs 读取（caps 含 wasi-fs 时根目录预打开为 /；文件缺失 → 记录）
    let content = std::fs::read_to_string("/dsh_fs_test.txt")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|e| format!("<fs-error: {e}>"));
    log(&format!("FS_READ={content}"));
    0
}

#[no_mangle]
pub extern "C" fn plugin_handle_event(
    _name_ptr: *const u8,
    _name_len: usize,
    _payload_ptr: *const u8,
    _payload_len: usize,
) -> i32 {
    0
}

#[no_mangle]
pub extern "C" fn plugin_dispose() -> i32 {
    log("wasm dispose");
    0
}

//! hello WASM 插件：dsh-wasmrt ABI 的最小实现。
//!
//! 行为：
//! - `plugin_apply(config)`：提供服务 `greeting`（值来自 config 或默认），注册事件 `ping` 监听。
//! - `plugin_handle_event`：收到 `ping` → host_emit("pong")。
//! - `plugin_dispose`：无操作（副作用经 host 的 fiber 机制自动回滚）。

use serde_json::{json, Value};
use std::alloc::Layout;

#[link(wasm_import_module = "env")]
extern "C" {
    fn host_log(ptr: *const u8, len: usize);
    fn host_emit(ptr: *const u8, len: usize);
    fn host_on(ptr: *const u8, len: usize);
    fn host_provide(service: *const u8, slen: usize, value: *const u8, vlen: usize) -> i32;
    fn host_get(service: *const u8, slen: usize, out: *mut u8, out_len: *mut usize) -> i32;
}

fn log(msg: &str) {
    unsafe {
        host_log(msg.as_ptr(), msg.len());
    }
}

fn write_json(v: &Value) -> Vec<u8> {
    serde_json::to_vec(v).unwrap_or_default()
}

fn read<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    if len == 0 || ptr.is_null() {
        return &[];
    }
    unsafe { std::slice::from_raw_parts(ptr, len) }
}

fn read_str<'a>(ptr: *const u8, len: usize) -> &'a str {
    std::str::from_utf8(read(ptr, len)).unwrap_or("")
}

fn read_value(ptr: *const u8, len: usize) -> Value {
    serde_json::from_slice(read(ptr, len)).unwrap_or(Value::Null)
}

/// 读取服务：分配输出缓冲并调用 host_get。
fn get_service(name: &str) -> Option<Value> {
    let mut buf = vec![0u8; 4096];
    let mut out_len: usize = 0;
    let code = unsafe {
        host_get(
            name.as_ptr(),
            name.len(),
            buf.as_mut_ptr(),
            &mut out_len as *mut usize,
        )
    };
    if code != 0 {
        return None;
    }
    serde_json::from_slice(&buf[..out_len]).ok()
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
    let greeting = config
        .get("greeting")
        .and_then(|v| v.as_str())
        .unwrap_or("hello from wasm");
    log(&format!("wasm apply config={config}"));

    // 提供服务（能力被拒时返回 -1，记日志）
    let value = write_json(&json!({"text": greeting}));
    let code = unsafe {
        host_provide(
            b"greeting".as_ptr(),
            b"greeting".len(),
            value.as_ptr(),
            value.len(),
        )
    };
    if code != 0 {
        log("provide failed");
        return -1;
    }

    // 注册事件监听
    unsafe {
        host_on(b"ping".as_ptr(), b"ping".len());
    }
    log("listener registered");
    0
}

#[no_mangle]
pub extern "C" fn plugin_handle_event(
    name_ptr: *const u8,
    name_len: usize,
    payload_ptr: *const u8,
    payload_len: usize,
) -> i32 {
    let name = read_str(name_ptr, name_len);
    let payload = read_value(payload_ptr, payload_len);
    log(&format!("wasm handle_event name={name} payload={payload}"));
    if name == "ping" {
        // 回读服务（验证 host_get）
        if let Some(v) = get_service("greeting") {
            log(&format!("wasm read greeting={v}"));
        }
        let out = write_json(&json!({"from": "wasm", "echo": payload}));
        unsafe {
            host_emit(out.as_ptr(), out.len());
        }
    }
    0
}

#[no_mangle]
pub extern "C" fn plugin_dispose() -> i32 {
    log("wasm dispose");
    0
}

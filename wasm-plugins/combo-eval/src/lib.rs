//! combo-eval WASM 插件：把 dsh-eval（受限 JS 子集求值器）编译进 wasm——组合求值的
//! **WASM 面**（K4/F-05 spike）。宿主经 C ABI 传入 `{scope, expr}`，本模块用与
//! native 面**同源**的 `dsh_eval::evaluate` 求值，结果 JSON 经 `host_provide`
//! 回传宿主：
//!
//! ```json
//! {"ok": true,  "value": <求值结果>, "truthy": <dsh_eval::truthy(value)>}
//! {"ok": false, "error": "<eval error>"}
//! ```
//!
//! `truthy` 一并返回，使宿主可直接复刻 `row_disabled` 的 fail-closed 门控
//! （求值失败 = 禁用）而无需二次语义。
//!
//! 与 `wasm-plugins/hello` 同款 ABI：导出 `alloc`/`dealloc`/`plugin_apply`/
//! `plugin_handle_event`/`plugin_dispose`；仅导入 `host_provide`（能力 `provide`）。

use std::alloc::Layout;
use std::collections::HashMap;

use dsh_eval::{evaluate, truthy};
use serde_json::{json, Value};

#[link(wasm_import_module = "env")]
extern "C" {
    fn host_provide(service: *const u8, slen: usize, value: *const u8, vlen: usize) -> i32;
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
    let expr = config.get("expr").and_then(Value::as_str).unwrap_or("").to_string();
    let scope = config.get("scope").cloned().unwrap_or(Value::Null);
    // 与宿主 `dsh_eval::Scope` 同构：flat identifier → value（row_disabled 的做法）。
    let mut map: HashMap<String, Value> = HashMap::new();
    if let Some(obj) = scope.as_object() {
        for (k, v) in obj {
            map.insert(k.clone(), v.clone());
        }
    }
    let out = match evaluate(&map, &expr) {
        Ok(v) => json!({ "ok": true, "value": v, "truthy": truthy(&v) }),
        Err(e) => json!({ "ok": false, "error": e.0 }),
    };
    let bytes = serde_json::to_vec(&out).unwrap_or_default();
    let code = unsafe {
        host_provide(
            b"eval.result".as_ptr(),
            b"eval.result".len(),
            bytes.as_ptr(),
            bytes.len(),
        )
    };
    if code != 0 {
        return -1;
    }
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
    0
}

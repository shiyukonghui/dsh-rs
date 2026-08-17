//! hello 组件插件：cargo-component 构建的最小实现。
//!
//! 行为（与 C ABI 版 hello 等价）：
//! - `apply(config)`：提供服务 `greeting`（值来自 config 或默认），注册事件 `ping` 监听。
//! - `handle-event`：收到 `ping` → host.emit(pong 载荷)。
//! - `dispose`：无操作（副作用经 host 的 fiber 机制自动回滚）。

#[allow(warnings)]
mod bindings;

use bindings::exports::dsh::plugin::plugin_api::Guest;
use serde_json::{json, Value};

struct Component;

impl Guest for Component {
    fn apply(config: Vec<u8>) -> i32 {
        let config: Value = serde_json::from_slice(&config).unwrap_or(Value::Null);
        let greeting = config
            .get("greeting")
            .and_then(|v| v.as_str())
            .unwrap_or("hello from wasm component");
        bindings::dsh::plugin::host_api::log(&format!("wasm-component apply config={config}"));

        let value = serde_json::to_vec(&json!({"text": greeting})).unwrap_or_default();
        let code = bindings::dsh::plugin::host_api::provide("greeting", &value);
        if code != 0 {
            bindings::dsh::plugin::host_api::log("provide failed");
            return -1;
        }
        bindings::dsh::plugin::host_api::on("ping");
        bindings::dsh::plugin::host_api::log("listener registered");
        0
    }

    fn handle_event(event: String, payload: Vec<u8>) -> i32 {
        let payload: Value = serde_json::from_slice(&payload).unwrap_or(Value::Null);
        bindings::dsh::plugin::host_api::log(&format!(
            "wasm-component handle_event name={event} payload={payload}"
        ));
        if event == "ping" {
            // 经 host get 回读服务（组件模型 bytes 版）
            let got = bindings::dsh::plugin::host_api::get("greeting");
            let got: Value = serde_json::from_slice(&got).unwrap_or(Value::Null);
            let out = serde_json::to_vec(&json!({
                "from": "wasm-component",
                "echo": payload,
                "greeting": got,
            }))
            .unwrap_or_default();
            bindings::dsh::plugin::host_api::emit(&out);
        }
        0
    }

    fn dispose() -> i32 {
        bindings::dsh::plugin::host_api::log("wasm-component dispose");
        0
    }
}

bindings::export!(Component with_types_in bindings);

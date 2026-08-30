// panel-plugin-inventory 服务装配单元（C4，D-185）：首个 harness 面板改写——插件清单卡。
//
// 由 deepseek-harness 前端「插件面板」改写而来（UI + 逻辑同包的服务装配单元）：
// - `describeUI` → v2 list 卡声明（与静态 web/ui.json 逐字段一致，m33 断言）；
// - `list` → 经 host-services "loader" 投影拿真实条目并出行（group 过滤；
//   disabled/fiber → state 映射）。**服务失败透传错误，绝不伪造空表**（诚实纪律）。
//
// 复用 host-remote world 接口身份（export remote + import host-services），
// 宿主 `WasmRemoteEndpointPlugin` 零改动即可加载；行语义只在本单元定义（双权威禁令）。

#[allow(warnings)]
mod bindings;

use bindings::dsh::host_remote::host_services;
use bindings::exports::dsh::host_remote::remote::Guest;
use serde_json::{json, Value};

/// UI 声明（数据，非代码）。静态 web/ui.json 与其保持逐字段一致（m33 断言）。
fn ui_declaration() -> Value {
    json!({
        "$schema": "dsh.panel-ui/v2",
        "kind": "card",
        "cardId": "panel-plugin-inventory.list",
        "type": "runtime",
        "title": "插件清单",
        "description": "loader 已组装服务装配单元的实时清单（只读）",
        "size": { "w": 4, "h": 4 },
        "view": {
            "kind": "list",
            "dataRpc": ["panel-plugin-inventory", "list"],
            "columns": [
                { "key": "name", "label": "插件" },
                { "key": "id", "label": "入口" },
                { "key": "state", "label": "状态" }
            ],
            "rowsPath": "items",
            "actions": [],
            "emptyText": "暂无已组装入口"
        }
    })
}

fn error(code: &str, message: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "ok": false,
        "error": { "code": code, "message": message },
    }))
    .unwrap_or_default()
}

fn describe_ui(_body: &Value) -> Vec<u8> {
    serde_json::to_vec(&json!({ "ok": true, "value": ui_declaration() })).unwrap_or_default()
}

/// list：`host_services.get("loader")` → 行投影。
/// 行语义（本单元定义）：group 条目过滤；state = disabled→"disabled"（优先）/
/// fiber→"active"/其余→"ready"。服务失败 → 错误透传（**不伪造空表**）。
fn list(_body: &Value) -> Vec<u8> {
    let bytes = host_services::get("loader", b"{}");
    let proj: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return error("decode", "loader projection unparseable"),
    };
    if proj.get("ok").and_then(|o| o.as_bool()) != Some(true) {
        // 错误透传（不裹成功、不夹带 items）。
        let err = proj.get("error").cloned().unwrap_or_else(|| {
            json!({"code": "service", "message": "host service loader failure"})
        });
        return serde_json::to_vec(&json!({ "ok": false, "error": err })).unwrap_or_default();
    }
    let entries = proj.get("entries").and_then(Value::as_array).cloned().unwrap_or_default();
    let items: Vec<Value> = entries
        .iter()
        .filter(|e| e.get("group").and_then(Value::as_bool) != Some(true))
        .map(|e| {
            let state = if e.get("disabled").and_then(Value::as_bool) == Some(true) {
                "disabled"
            } else if e.get("fiber").map(|f| !f.is_null()).unwrap_or(false) {
                "active"
            } else {
                "ready"
            };
            json!({
                "name": e.get("name").cloned().unwrap_or(Value::Null),
                "id": e.get("id").cloned().unwrap_or(Value::Null),
                "state": state,
            })
        })
        .collect();
    serde_json::to_vec(&json!({ "ok": true, "value": { "items": items } })).unwrap_or_default()
}

struct PanelPluginInventory;

impl Guest for PanelPluginInventory {
    fn handle(namespace: String, method: String, body: Vec<u8>) -> Vec<u8> {
        let body_value: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        match (namespace.as_str(), method.as_str()) {
            ("panel-plugin-inventory", "describeUI") => describe_ui(&body_value),
            ("panel-plugin-inventory", "list") => list(&body_value),
            _ => error(
                "internal",
                &format!(
                    "panel-plugin-inventory: endpoint {namespace}/{method} not provided by this plugin"
                ),
            ),
        }
    }
}

bindings::export!(PanelPluginInventory with_types_in bindings);

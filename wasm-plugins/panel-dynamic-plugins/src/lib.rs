//! panel-dynamic-plugins 服务装配单元（面板改写 #3，D-188）：动态插件清单卡。
//!
//! 改写型第三次复制：`describeUI` 与静态 web/ui.json 逐字段一致（m35 断言）；
//! `list` 端点经 host-services "dynamicPlugins" 投影出行——行语义（name 取当前包、
//! state = activeRun→running/否则 defined）**只在本单元定义**（双权威禁令）；
//! 服务失败透传错误，**绝不伪造空表**。v1 只读（写动作需先定卡内确认的渲染形态）。

#[allow(warnings)]
mod bindings;

use bindings::dsh::host_remote::host_services;
use bindings::exports::dsh::host_remote::remote::Guest;
use serde_json::{json, Value};

/// UI 声明（数据，非代码）。静态 web/ui.json 与其保持逐字段一致（m35 断言）。
fn ui_declaration() -> Value {
    json!({
        "$schema": "dsh/plugin-ui/v2",
        "kind": "card",
        "cardId": "panel-dynamic-plugins.list",
        "type": "runtime",
        "title": "动态插件",
        "description": "dynamicCordisRunner 定义与运行态（只读）",
        "size": { "w": 4, "h": 4 },
        "view": {
            "kind": "list",
            "dataRpc": ["panel-dynamic-plugins", "list"],
            "columns": [
                { "key": "pluginId", "label": "插件 ID" },
                { "key": "name", "label": "包名" },
                { "key": "state", "label": "状态" }
            ],
            "rowsPath": "items",
            "actions": [],
            "emptyText": "没有已定义的动态插件"
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

/// 行投影：name = currentPackageId 对应包名（缺配回落首包/null）；
/// state = activeRun 存在 → running，否则 defined。
fn row_for(plugin_row: &Value) -> Value {
    let current_id = plugin_row.get("currentPackageId").and_then(Value::as_str);
    let packages = plugin_row
        .get("packages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let name = current_id
        .and_then(|cid| {
            packages.iter().find(|p| {
                p.get("packageId").and_then(Value::as_str) == Some(cid)
            })
        })
        .or_else(|| packages.first())
        .and_then(|p| p.get("name").cloned())
        .unwrap_or(Value::Null);
    let state = if plugin_row.get("activeRun").map(|v| !v.is_null()).unwrap_or(false) {
        "running"
    } else {
        "defined"
    };
    json!({
        "pluginId": plugin_row.get("pluginId").cloned().unwrap_or(Value::Null),
        "name": name,
        "state": state,
    })
}

fn list(_body: &Value) -> Vec<u8> {
    let bytes = host_services::get("dynamicPlugins", b"{}");
    let proj: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return error("decode", "dynamicPlugins projection unparseable"),
    };
    if proj.get("ok").and_then(|o| o.as_bool()) != Some(true) {
        let err = proj.get("error").cloned().unwrap_or_else(|| {
            json!({"code": "service", "message": "host service dynamicPlugins failure"})
        });
        return serde_json::to_vec(&json!({ "ok": false, "error": err })).unwrap_or_default();
    }
    let plugins = proj
        .get("plugins")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let items: Vec<Value> = plugins.iter().map(row_for).collect();
    serde_json::to_vec(&json!({ "ok": true, "value": { "items": items } })).unwrap_or_default()
}

struct PanelDynamicPlugins;

impl Guest for PanelDynamicPlugins {
    fn handle(namespace: String, method: String, body: Vec<u8>) -> Vec<u8> {
        let body_value: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        match (namespace.as_str(), method.as_str()) {
            ("panel-dynamic-plugins", "describeUI") => describe_ui(&body_value),
            ("panel-dynamic-plugins", "list") => list(&body_value),
            _ => error(
                "internal",
                &format!(
                    "panel-dynamic-plugins: endpoint {namespace}/{method} not provided by this plugin"
                ),
            ),
        }
    }
}

bindings::export!(PanelDynamicPlugins with_types_in bindings);

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
            "rowActions": [
                { "name": "stop", "label": "停止", "rpc": ["panel-dynamic-plugins", "stop"],
                  "scope": "row", "confirm": true },
                { "name": "undefine", "label": "卸载", "rpc": ["panel-dynamic-plugins", "undefine"],
                  "scope": "row", "confirm": true }
            ],
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
        return passthrough_error(&proj);
    }
    let plugins = proj
        .get("plugins")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let items: Vec<Value> = plugins.iter().map(row_for).collect();
    serde_json::to_vec(&json!({ "ok": true, "value": { "items": items } })).unwrap_or_default()
}

/// 透传宿主错误（不裹成功、不夹带数据）。
fn passthrough_error(proj: &Value) -> Vec<u8> {
    let err = proj.get("error").cloned().unwrap_or_else(|| {
        json!({"code": "service", "message": "host service failure"})
    });
    serde_json::to_vec(&json!({ "ok": false, "error": err })).unwrap_or_default()
}

/// C6（D-189）：行身份提取——**渲染器不是安全边界**，单元自己校验：
/// `row.pluginId` 必须是非空字符串，否则 fail-loud 且绝不触达宿主服务。
fn row_identity(body: &Value) -> Option<String> {
    body.get("row")
        .and_then(|r| r.get("pluginId"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// 行写动作（stop/undefine 同型）：校验身份 → `host_services.set` → 透传。
fn row_action(body: &Value, endpoint: &str, set_service: &str, done_key: &str) -> Vec<u8> {
    let Some(plugin_id) = row_identity(body) else {
        return error(
            "internal",
            &format!("panel-dynamic-plugins/{endpoint}: body.row.pluginId must be a non-empty string"),
        );
    };
    let payload = json!({ "pluginId": plugin_id });
    let bytes = host_services::set(set_service, &serde_json::to_vec(&payload).unwrap_or_default());
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(v) if v.get("ok").and_then(|o| o.as_bool()) == Some(true) => {
            let mut value = json!({ "pluginId": plugin_id });
            value[done_key] = json!(true);
            serde_json::to_vec(&json!({ "ok": true, "value": value })).unwrap_or_default()
        }
        Ok(v) => passthrough_error(&v),
        Err(_) => error("decode", &format!("{set_service} response unparseable")),
    }
}

fn stop(body: &Value) -> Vec<u8> {
    row_action(body, "stop", "dynamicStop", "stopped")
}

fn undefine(body: &Value) -> Vec<u8> {
    row_action(body, "undefine", "dynamicUndefine", "undefined")
}

struct PanelDynamicPlugins;

impl Guest for PanelDynamicPlugins {
    fn handle(namespace: String, method: String, body: Vec<u8>) -> Vec<u8> {
        let body_value: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        match (namespace.as_str(), method.as_str()) {
            ("panel-dynamic-plugins", "describeUI") => describe_ui(&body_value),
            ("panel-dynamic-plugins", "list") => list(&body_value),
            ("panel-dynamic-plugins", "stop") => stop(&body_value),
            ("panel-dynamic-plugins", "undefine") => undefine(&body_value),
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

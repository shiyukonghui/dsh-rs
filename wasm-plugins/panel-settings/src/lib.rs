//! panel-settings 服务装配单元（面板改写 #6，D-192）：设置概览卡（config 分类首卡）。
//!
//! `list` 端点读宿主 `settingsDescribe` 投影（与原生 settings.describe 同形状，
//! **redact 在源头 provider**——单元不展开 secrets、不自行脱敏也不解除脱敏），把各
//! namespace 的 resolved `value` 顶层字段拍平成 `{ns, field, value}` 概览行；
//! 非对象 value 单行 `field="—"`。服务失败透传，**绝不伪造空表**。
//! 只读卡：写端（编辑）依赖动态 fields 契约演进（D-187 裁定独立决策）。

#[allow(warnings)]
mod bindings;

use bindings::dsh::host_remote::host_services;
use bindings::exports::dsh::host_remote::remote::Guest;
use serde_json::{json, Value};

/// UI 声明（数据，非代码）。静态 web/ui.json 与其保持逐字段一致（m38 断言）。
fn ui_declaration() -> Value {
    // D-225：单一事实源=web/ui.json（编译期嵌入；声明=数据，非代码）。
    serde_json::from_str(include_str!("../web/ui.json")).expect("ui.json must be valid JSON")
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

/// 一个 namespace 的 resolved value 拍平成行。
fn rows_for(ns: &str, value: &Value) -> Vec<Value> {
    match value.as_object() {
        Some(map) if !map.is_empty() => map
            .iter()
            .map(|(k, v)| json!({ "ns": ns, "field": k, "value": v }))
            .collect(),
        _ => vec![json!({ "ns": ns, "field": "—", "value": value })],
    }
}

fn list(_body: &Value) -> Vec<u8> {
    let bytes = host_services::get("settingsDescribe", b"{}");
    let proj: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return error("decode", "settingsDescribe projection unparseable"),
    };
    if proj.get("ok").and_then(|o| o.as_bool()) != Some(true) {
        let err = proj.get("error").cloned().unwrap_or_else(|| {
            json!({"code": "service", "message": "host service settingsDescribe failure"})
        });
        return serde_json::to_vec(&json!({ "ok": false, "error": err })).unwrap_or_default();
    }
    let namespaces = proj["value"]["namespaces"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut items: Vec<Value> = Vec::new();
    for ns_view in &namespaces {
        let ns = ns_view.get("ns").and_then(Value::as_str).unwrap_or("");
        let value = ns_view.get("value").cloned().unwrap_or(Value::Null);
        items.extend(rows_for(ns, &value));
    }
    serde_json::to_vec(&json!({ "ok": true, "value": { "items": items } })).unwrap_or_default()
}

struct PanelSettings;

impl Guest for PanelSettings {
    fn handle(namespace: String, method: String, body: Vec<u8>) -> Vec<u8> {
        let body_value: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        match (namespace.as_str(), method.as_str()) {
            ("panel-settings", "describeUI") => describe_ui(&body_value),
            ("panel-settings", "list") => list(&body_value),
            _ => error(
                "internal",
                &format!(
                    "panel-settings: endpoint {namespace}/{method} not provided by this plugin"
                ),
            ),
        }
    }
}

bindings::export!(PanelSettings with_types_in bindings);

//! panel-runtime-status 服务装配单元（面板改写 #2，D-187）：运行时状态卡。
//!
//! C4 改写型的第二次复制（零新型）：`describeUI` 与静态 web/ui.json 逐字段一致
//! （m34 断言）；`status` 端点跨服务聚合宿主投影（`loader` + `dynamicPlugins`），
//! **任一服务失败整体 fail-loud——不部分伪造**（缺一条腿的状态卡比诚实报错危险）。

#[allow(warnings)]
mod bindings;

use bindings::dsh::host_remote::host_services;
use bindings::exports::dsh::host_remote::remote::Guest;
use serde_json::{json, Value};

/// UI 声明（数据，非代码）。静态 web/ui.json 与其保持逐字段一致（m34 断言）。
fn ui_declaration() -> Value {
    json!({
        "$schema": "dsh/plugin-ui/v2",
        "kind": "card",
        "cardId": "panel-runtime-status.status",
        "type": "runtime",
        "title": "运行时状态",
        "description": "loader / 动态包实时聚合（只读）",
        "size": { "w": 2, "h": 2 },
        "view": {
            "kind": "status",
            "dataRpc": ["panel-runtime-status", "status"],
            "actions": []
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

/// 宿主服务只读投影：失败 → 透传 `{ok:false,error}`（返回 Err 携带原始错误体）。
fn get_service(service: &str) -> Result<Value, Vec<u8>> {
    let bytes = host_services::get(service, b"{}");
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(v) if v.get("ok").and_then(|o| o.as_bool()) == Some(true) => Ok(v),
        Ok(v) => {
            let err = v.get("error").cloned().unwrap_or_else(|| {
                json!({"code": "service", "message": format!("host service {service} failure")})
            });
            Err(serde_json::to_vec(&json!({ "ok": false, "error": err })).unwrap_or_default())
        }
        Err(_) => Err(error("decode", &format!("host service {service} unparseable"))),
    }
}

fn describe_ui(_body: &Value) -> Vec<u8> {
    serde_json::to_vec(&json!({ "ok": true, "value": ui_declaration() })).unwrap_or_default()
}

/// status：loader（group 过滤计数 + active/disabled）+ dynamicPlugins 计数。
/// tone 是**单元的诚实判断**：活跃>0 ok/idle；禁用>0 warn；动态包 idle。
fn status(_body: &Value) -> Vec<u8> {
    let loader = match get_service("loader") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let dynamic = match get_service("dynamicPlugins") {
        Ok(v) => v,
        Err(e) => return e,
    };
    let entries = loader
        .get("entries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let real: Vec<&Value> = entries
        .iter()
        .filter(|e| e.get("group").and_then(Value::as_bool) != Some(true))
        .collect();
    let active = real
        .iter()
        .filter(|e| e.get("fiber").map(|f| !f.is_null()).unwrap_or(false))
        .count();
    let disabled = real
        .iter()
        .filter(|e| e.get("disabled").and_then(Value::as_bool) == Some(true))
        .count();
    let dyn_count = dynamic
        .get("plugins")
        .and_then(Value::as_array)
        .map(|p| p.len())
        .unwrap_or(0);
    let items = json!([
        { "label": "loader 条目", "value": real.len(), "kind": "number" },
        { "label": "fiber 活跃", "value": active, "kind": "number",
          "tone": if active > 0 { "ok" } else { "idle" } },
        { "label": "禁用", "value": disabled, "kind": "number",
          "tone": if disabled > 0 { "warn" } else { "ok" } },
        { "label": "动态包", "value": dyn_count, "kind": "number", "tone": "idle" },
    ]);
    serde_json::to_vec(&json!({ "ok": true, "value": { "items": items } })).unwrap_or_default()
}

struct PanelRuntimeStatus;

impl Guest for PanelRuntimeStatus {
    fn handle(namespace: String, method: String, body: Vec<u8>) -> Vec<u8> {
        let body_value: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        match (namespace.as_str(), method.as_str()) {
            ("panel-runtime-status", "describeUI") => describe_ui(&body_value),
            ("panel-runtime-status", "status") => status(&body_value),
            _ => error(
                "internal",
                &format!(
                    "panel-runtime-status: endpoint {namespace}/{method} not provided by this plugin"
                ),
            ),
        }
    }
}

bindings::export!(PanelRuntimeStatus with_types_in bindings);

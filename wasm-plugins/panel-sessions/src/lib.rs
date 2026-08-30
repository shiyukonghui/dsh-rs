//! panel-sessions 服务装配单元（面板改写 #5，D-191）：会话清单卡（session 分类首卡）。
//!
//! `describeUI` 与静态 web/ui.json 逐字段一致（m37 一份契约）；`list` 端点经
//! host-services "sessionCandidates" 投影出真实会话候选，**行零加工直传**
//! （{sessionId,label,createdAt} 原样——时间格式化属渲染器演进，双权威禁令）；
//! 服务失败透传错误，**绝不伪造空表**。只读卡（打开/切换会话属未来交互形态）。

#[allow(warnings)]
mod bindings;

use bindings::dsh::host_remote::host_services;
use bindings::exports::dsh::host_remote::remote::Guest;
use serde_json::{json, Value};

/// UI 声明（数据，非代码）。静态 web/ui.json 与其保持逐字段一致（m37 断言）。
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

fn list(_body: &Value) -> Vec<u8> {
    let bytes = host_services::get("sessionCandidates", b"{}");
    let proj: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return error("decode", "sessionCandidates projection unparseable"),
    };
    if proj.get("ok").and_then(|o| o.as_bool()) != Some(true) {
        let err = proj.get("error").cloned().unwrap_or_else(|| {
            json!({"code": "service", "message": "host service sessionCandidates failure"})
        });
        return serde_json::to_vec(&json!({ "ok": false, "error": err })).unwrap_or_default();
    }
    let candidates = proj
        .get("candidates")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    // 行零加工：候选字段原样直传（sessionId/label/createdAt 语义归宿主投影）。
    let items: Vec<Value> = candidates
        .iter()
        .map(|c| {
            json!({
                "sessionId": c.get("sessionId").cloned().unwrap_or(Value::Null),
                "label": c.get("label").cloned().unwrap_or(Value::Null),
                "createdAt": c.get("createdAt").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();
    serde_json::to_vec(&json!({ "ok": true, "value": { "items": items } })).unwrap_or_default()
}

struct PanelSessions;

impl Guest for PanelSessions {
    fn handle(namespace: String, method: String, body: Vec<u8>) -> Vec<u8> {
        let body_value: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        match (namespace.as_str(), method.as_str()) {
            ("panel-sessions", "describeUI") => describe_ui(&body_value),
            ("panel-sessions", "list") => list(&body_value),
            _ => error(
                "internal",
                &format!(
                    "panel-sessions: endpoint {namespace}/{method} not provided by this plugin"
                ),
            ),
        }
    }
}

bindings::export!(PanelSessions with_types_in bindings);

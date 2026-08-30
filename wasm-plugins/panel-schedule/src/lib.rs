//! panel-schedule 服务装配单元（面板改写 #9 / D-195）：调度清单**声明单元**（读端）。
//!
//! D-193-B 定型复制：调度协议在宿主（`schedule/list` 薄臂，fold 事件日志权威）——
//! 本单元只拥有 v2 list 声明；**零自有数据端点**（create/delete 属写端，另立切片）。

#[allow(warnings)]
mod bindings;

use bindings::exports::dsh::host_remote::remote::Guest;
use serde_json::{json, Value};

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

struct PanelSchedule;

impl Guest for PanelSchedule {
    fn handle(namespace: String, method: String, _body: Vec<u8>) -> Vec<u8> {
        match (namespace.as_str(), method.as_str()) {
            ("panel-schedule", "describeUI") => {
                serde_json::to_vec(&json!({ "ok": true, "value": ui_declaration() }))
                    .unwrap_or_default()
            }
            _ => error(
                "internal",
                &format!(
                    "panel-schedule: endpoint {namespace}/{method} not provided by this plugin (声明单元：调度协议在宿主事件日志权威)"
                ),
            ),
        }
    }
}

bindings::export!(PanelSchedule with_types_in bindings);

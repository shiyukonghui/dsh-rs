//! panel-schedule 服务装配单元（面板改写 #9 / D-195）：调度清单**声明单元**（读端）。
//!
//! D-193-B 定型复制：调度协议在宿主（`schedule/list` 薄臂，fold 事件日志权威）——
//! 本单元只拥有 v2 list 声明；**零自有数据端点**（create/delete 属写端，另立切片）。

#[allow(warnings)]
mod bindings;

use bindings::exports::dsh::host_remote::remote::Guest;
use serde_json::{json, Value};

fn ui_declaration() -> Value {
    json!({
        "$schema": "dsh/plugin-ui/v2",
        "kind": "card",
        "cardId": "panel-schedule.list",
        "type": "runtime",
        "title": "调度任务",
        "description": "after/at/every 调度记录（只读；协议在宿主事件日志权威）",
        "size": { "w": 4, "h": 4 },
        "view": {
            "kind": "list",
            "dataRpc": ["schedule", "list"],
            "columns": [
                { "key": "id", "label": "ID" },
                { "key": "kind", "label": "类型" },
                { "key": "prompt", "label": "提示" },
                { "key": "scheduledAt", "label": "计划时间" }
            ],
            "rowsPath": "items",
            "actions": [],
            "emptyText": "没有调度记录"
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

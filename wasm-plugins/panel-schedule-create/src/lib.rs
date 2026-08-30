//! panel-schedule-create 服务装配单元（面板改写 #10 / D-197）：调度创建**表单声明单元**。
//!
//! 静态 form 契约（llm-deepseek 先例）+ 声明单元纪律（panel-chat 先例）：保存动作指
//! 宿主 `schedule/create` 薄臂（画布 {args:{values}} 形由 roundtrip 宿主测钉死）；
//! **零自有数据端点**。describeUI 与 web/ui.json 一份契约。

#[allow(warnings)]
mod bindings;

use bindings::exports::dsh::host_remote::remote::Guest;
use serde_json::{json, Value};

fn ui_declaration() -> Value {
    json!({
        "$schema": "dsh.panel-ui/v2",
        "kind": "card",
        "cardId": "panel-schedule-create.form",
        "type": "runtime",
        "title": "创建调度",
        "description": "after/at/every 调度创建（写端；动作走宿主 schedule/create 臂）",
        "size": { "w": 2, "h": 4 },
        "view": {
            "kind": "form",
            "fields": [
                { "name": "kind", "label": "类型", "type": "select",
                  "options": ["after", "at", "every"], "default": "after", "required": true },
                { "name": "prompt", "label": "提示", "type": "text", "required": true },
                { "name": "afterSeconds", "label": "延迟秒（after）", "type": "number", "default": 60 }
            ],
            "actions": [
                { "name": "create", "label": "创建", "rpc": ["schedule", "create"], "primary": true }
            ]
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

struct PanelScheduleCreate;

impl Guest for PanelScheduleCreate {
    fn handle(namespace: String, method: String, _body: Vec<u8>) -> Vec<u8> {
        match (namespace.as_str(), method.as_str()) {
            ("panel-schedule-create", "describeUI") => {
                serde_json::to_vec(&json!({ "ok": true, "value": ui_declaration() }))
                    .unwrap_or_default()
            }
            _ => error(
                "internal",
                &format!(
                    "panel-schedule-create: endpoint {namespace}/{method} not provided by this plugin (声明单元：创建协议在宿主 schedule/create 臂)"
                ),
            ),
        }
    }
}

bindings::export!(PanelScheduleCreate with_types_in bindings);

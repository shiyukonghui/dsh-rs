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

//! panel-settings-edit 服务装配单元（面板改写 #8 / S4，D-194）：设置编辑**声明单元**。
//!
//! D-193-B 裁决的复制：设置域是宿主域——本单元只拥有 v2 form 声明（`fieldsFrom`
//! 动态投影 + 保存动作指宿主 `settings/update` 既表面），**零自有数据端点**
//! （其余方法 fail-loud，m40 断言）。describeUI 与 web/ui.json 一份契约。

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

struct PanelSettingsEdit;

impl Guest for PanelSettingsEdit {
    fn handle(namespace: String, method: String, _body: Vec<u8>) -> Vec<u8> {
        match (namespace.as_str(), method.as_str()) {
            ("panel-settings-edit", "describeUI") => {
                serde_json::to_vec(&json!({ "ok": true, "value": ui_declaration() }))
                    .unwrap_or_default()
            }
            _ => error(
                "internal",
                &format!(
                    "panel-settings-edit: endpoint {namespace}/{method} not provided by this plugin (声明单元：设置协议在宿主既表面)"
                ),
            ),
        }
    }
}

bindings::export!(PanelSettingsEdit with_types_in bindings);

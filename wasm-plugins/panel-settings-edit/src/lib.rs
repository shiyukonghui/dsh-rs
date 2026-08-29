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
    json!({
        "$schema": "dsh/plugin-ui/v2",
        "kind": "card",
        "cardId": "panel-settings-edit.edit",
        "type": "config",
        "title": "设置编辑",
        "description": "命名空间下拉 + 动态 fields 投影（D-201 一卡通用；保存带乐观锁；secrets 不可编辑）",
        "size": { "w": 4, "h": 6 },
        "view": {
            "kind": "form",
            "fieldsFrom": { "rpc": ["settings", "describe"], "pick": "ui-theme", "nsSelect": true },
            "actions": [
                { "name": "save", "label": "保存", "rpc": ["settings", "update"], "primary": true }
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

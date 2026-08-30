//! panel-locale-edit 服务装配单元（面板改写 #12 / D-200）：locale 设置编辑**声明单元**。
//!
//! panel-settings-edit 定型的**机械复制**（E2E §2 预告兑现）：同一 `fieldsFrom` 契约，
//! 换 pick 即新 ns——schemaFields 投影与乐观锁保存全复用，本单元零新机制。

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

struct PanelLocaleEdit;

impl Guest for PanelLocaleEdit {
    fn handle(namespace: String, method: String, _body: Vec<u8>) -> Vec<u8> {
        match (namespace.as_str(), method.as_str()) {
            ("panel-locale-edit", "describeUI") => {
                serde_json::to_vec(&json!({ "ok": true, "value": ui_declaration() }))
                    .unwrap_or_default()
            }
            _ => error(
                "internal",
                &format!(
                    "panel-locale-edit: endpoint {namespace}/{method} not provided by this plugin (声明单元：设置协议在宿主既表面)"
                ),
            ),
        }
    }
}

bindings::export!(PanelLocaleEdit with_types_in bindings);

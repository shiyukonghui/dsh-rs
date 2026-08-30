//! panel-approval 服务装配单元（面板改写 #11 / D-199）：待审批清单**声明单元**（第十二卡）。
//!
//! 声明单元定型（D-193-B 系）：pending 数据面 = 宿主 `approval/pending` 薄臂
//! （wire.pending_requests 单一权威，D-198）；允许/拒绝 = 同一 decide 臂的
//! rowActions，`args.decision` 由声明字面量区分（C6/D-198 契约扩展）；
//! **零自有数据端点**。describeUI 与 web/ui.json 一份契约。

#[allow(warnings)]
mod bindings;

use bindings::exports::dsh::host_remote::remote::Guest;
use serde_json::{json, Value};

fn ui_declaration() -> Value {
    json!({
        "$schema": "dsh.panel-ui/v2",
        "kind": "card",
        "cardId": "panel-approval.pending",
        "type": "session",
        "title": "待审批",
        "description": "未决工具审批（决定走宿主 session.approval.decide；拒绝需确认）",
        "size": { "w": 4, "h": 4 },
        "view": {
            "kind": "list",
            "dataRpc": ["approval", "pending"],
            "columns": [
                { "key": "toolName", "label": "工具" },
                { "key": "sessionId", "label": "会话" },
                { "key": "reason", "label": "原因" }
            ],
            "rowsPath": "items",
            "actions": [],
            "rowActions": [
                { "name": "allow", "label": "允许", "rpc": ["session.approval", "decide"],
                  "scope": "row", "args": { "decision": "allowedOnce" } },
                { "name": "reject", "label": "拒绝", "rpc": ["session.approval", "decide"],
                  "scope": "row", "args": { "decision": "rejected" }, "confirm": true }
            ],
            "emptyText": "没有待审批项"
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

struct PanelApproval;

impl Guest for PanelApproval {
    fn handle(namespace: String, method: String, _body: Vec<u8>) -> Vec<u8> {
        match (namespace.as_str(), method.as_str()) {
            ("panel-approval", "describeUI") => {
                serde_json::to_vec(&json!({ "ok": true, "value": ui_declaration() }))
                    .unwrap_or_default()
            }
            _ => error(
                "internal",
                &format!(
                    "panel-approval: endpoint {namespace}/{method} not provided by this plugin (声明单元：审批协议在宿主臂)"
                ),
            ),
        }
    }
}

bindings::export!(PanelApproval with_types_in bindings);

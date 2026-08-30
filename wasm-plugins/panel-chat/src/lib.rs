//! panel-chat 服务装配单元（面板改写 #7 / C8-4，D-193）：聊天卡的**声明单元**。
//!
//! 架构裁决（D-193-B）：会话协议（list/history/prompt）归宿主原生臂——本单元只拥有
//! v2 chat 声明（`describeUI` 返回数据、渲染器在浏览器、Rust 不渲染三条不变量不动）。
//! **没有自有数据端点**：其余方法一律 fail-loud（单元不伪装能力，m39 断言）。
//! `describeUI` 与静态 web/ui.json 逐字段一致（一份契约）。

#[allow(warnings)]
mod bindings;

use bindings::exports::dsh::host_remote::remote::Guest;
use serde_json::{json, Value};

/// UI 声明（数据，非代码）。静态 web/ui.json 与其保持逐字段一致（m39 断言）。
fn ui_declaration() -> Value {
    json!({
        "$schema": "dsh.panel-ui/v2",
        "kind": "card",
        "cardId": "panel-chat.chat",
        "type": "session",
        "title": "聊天",
        "description": "会话聊天（选择/历史/发送/停止；会话协议在宿主，C8-4 声明单元）",
        "size": { "w": 4, "h": 6 },
        "view": {
            "kind": "chat",
            "sessionSource": ["session", "list"],
            "historyRpc": ["session", "history"],
            "sendRpc": ["session", "prompt"],
            "cancelRpc": ["session", "cancel"],
            "stream": "session-events"
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

struct PanelChat;

impl Guest for PanelChat {
    fn handle(namespace: String, method: String, _body: Vec<u8>) -> Vec<u8> {
        match (namespace.as_str(), method.as_str()) {
            ("panel-chat", "describeUI") => {
                serde_json::to_vec(&json!({ "ok": true, "value": ui_declaration() }))
                    .unwrap_or_default()
            }
            _ => error(
                "internal",
                &format!(
                    "panel-chat: endpoint {namespace}/{method} not provided by this plugin (声明单元：数据面在宿主原生臂)"
                ),
            ),
        }
    }
}

bindings::export!(PanelChat with_types_in bindings);

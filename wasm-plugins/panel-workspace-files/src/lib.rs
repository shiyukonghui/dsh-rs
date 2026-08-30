//! panel-workspace-files 服务装配单元（面板改写 #4，D-190）：工作区文件清单卡。
//!
//! 两段式服务探测（每段诚实失败）：先 `host_services.get("agentWorkspace")` 解析默认
//! 工作区（失败/缺 cwd → fail-loud，**绝不触达枚举、绝不猜目录**），再
//! `get("workspaceFiles", {cwd, query:""})` 列举出行 `{path}`。
//! `describeUI` 与静态 web/ui.json 逐字段一致（m36 一份契约断言）。只读卡。

#[allow(warnings)]
mod bindings;

use bindings::dsh::host_remote::host_services;
use bindings::exports::dsh::host_remote::remote::Guest;
use serde_json::{json, Value};

/// UI 声明（数据，非代码）。静态 web/ui.json 与其保持逐字段一致（m36 断言）。
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

/// 透传宿主错误（不裹成功、不夹带数据）。
fn passthrough_error(v: &Value) -> Vec<u8> {
    let err = v.get("error").cloned().unwrap_or_else(|| {
        json!({"code": "service", "message": "host service failure"})
    });
    serde_json::to_vec(&json!({ "ok": false, "error": err })).unwrap_or_default()
}

fn describe_ui(_body: &Value) -> Vec<u8> {
    serde_json::to_vec(&json!({ "ok": true, "value": ui_declaration() })).unwrap_or_default()
}

/// list：两段式探测。第一段解析工作区（**失败即止，不猜目录**）；第二段列举并投影。
fn list(_body: &Value) -> Vec<u8> {
    let ws_bytes = host_services::get("agentWorkspace", b"{}");
    let ws: Value = match serde_json::from_slice(&ws_bytes) {
        Ok(v) => v,
        Err(_) => return error("decode", "agentWorkspace projection unparseable"),
    };
    if ws.get("ok").and_then(|o| o.as_bool()) != Some(true) {
        return passthrough_error(&ws);
    }
    let cwd = ws.get("cwd").and_then(Value::as_str).unwrap_or("");
    if cwd.is_empty() {
        return error("no-workspace", "agentWorkspace returned empty cwd (not guessing)");
    }
    let payload = json!({ "cwd": cwd, "query": "" });
    let files_bytes =
        host_services::get("workspaceFiles", &serde_json::to_vec(&payload).unwrap_or_default());
    let files: Value = match serde_json::from_slice(&files_bytes) {
        Ok(v) => v,
        Err(_) => return error("decode", "workspaceFiles projection unparseable"),
    };
    if files.get("ok").and_then(|o| o.as_bool()) != Some(true) {
        return passthrough_error(&files);
    }
    let paths = files
        .get("paths")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let items: Vec<Value> = paths
        .iter()
        .map(|p| json!({ "path": p.clone() }))
        .collect();
    serde_json::to_vec(&json!({ "ok": true, "value": { "items": items } })).unwrap_or_default()
}

struct PanelWorkspaceFiles;

impl Guest for PanelWorkspaceFiles {
    fn handle(namespace: String, method: String, body: Vec<u8>) -> Vec<u8> {
        let body_value: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        match (namespace.as_str(), method.as_str()) {
            ("panel-workspace-files", "describeUI") => describe_ui(&body_value),
            ("panel-workspace-files", "list") => list(&body_value),
            _ => error(
                "internal",
                &format!(
                    "panel-workspace-files: endpoint {namespace}/{method} not provided by this plugin"
                ),
            ),
        }
    }
}

bindings::export!(PanelWorkspaceFiles with_types_in bindings);

//! host-remote 组件：Rust host 经 WASM 组件承载 remote 端点（D-115-Web D3）。
//!
//! **组件模型专用（用户裁定：禁止功能漂移到 C ABI）；target = wasm32-wasip1。**
//! 端点业务（真实实现，非占位）：宿主经 `remote.handle` 把 `/api` 端点请求交给本
//! 组件；组件经 `host-services` 反查宿主真实状态（持久 KV / session 消息 / 时钟 /
//! uuid），组装前端期望的 wire 结构返回。既有大面留在宿主原生，不经本组件。

#[allow(warnings)]
mod bindings;

use bindings::exports::dsh::host_remote::remote::Guest;
use bindings::dsh::host_remote::host_services;
use serde_json::{json, Value};

struct HostRemote;

/// 规范化错误字节（fail-loud：绝不伪造成功）。
fn error(code: &str, message: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "ok": false,
        "error": { "code": code, "message": message },
    }))
    .unwrap_or_default()
}

fn err_value(code: &str, message: &str) -> Value {
    json!({ "ok": false, "error": { "code": code, "message": message } })
}

/// 读宿主服务投影并解析为 JSON；宿主侧失败时以规范化错误字节响应。
fn get_service(service: &str, payload: &Value) -> Result<Value, Vec<u8>> {
    let bytes = host_services::get(service, &serde_json::to_vec(payload).unwrap_or_default());
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(v) if v.get("ok").and_then(|o| o.as_bool()) == Some(true) => Ok(v),
        Ok(v) => {
            let err = v.get("error").cloned().unwrap_or(err_value("service", "service failure"));
            Err(serde_json::to_vec(&json!({"ok": false, "error": err})).unwrap_or_default())
        }
        Err(_) => Err(error("decode", "host service projection unparseable")),
    }
}

/// 写宿主服务（真实持久）；失败 → 规范化错误字节。
fn set_service(service: &str, payload: &Value) -> Result<Value, Vec<u8>> {
    let bytes = host_services::set(service, &serde_json::to_vec(payload).unwrap_or_default());
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(v) if v.get("ok").and_then(|o| o.as_bool()) == Some(true) => Ok(v),
        Ok(v) => {
            let err = v.get("error").cloned().unwrap_or(err_value("service", "service failure"));
            Err(serde_json::to_vec(&json!({"ok": false, "error": err})).unwrap_or_default())
        }
        Err(_) => Err(error("decode", "host service write unparseable")),
    }
}

/// pluginInventory/list：从宿主 loader 投影真实条目，跳过 group 行，映射 wire。
fn plugin_inventory_list(_body: &Value) -> Vec<u8> {
    match get_service("loader", &json!({})) {
        Ok(proj) => {
            let mut entries: Vec<Value> = Vec::new();
            if let Some(list) = proj.get("entries").and_then(|e| e.as_array()) {
                for e in list {
                    if e.get("group").and_then(|g| g.as_bool()) == Some(true) {
                        continue; // 跳过 group 行（对齐 TS PluginInventoryGateway.list）
                    }
                    entries.push(json!({
                        "entryId": e.get("id").cloned().unwrap_or(Value::Null),
                        "moduleName": e.get("name").cloned().unwrap_or(Value::Null),
                        "enabled": !e.get("disabled").and_then(|d| d.as_bool()).unwrap_or(false),
                        "fiberPhase": e.get("fiber").and_then(|f| f.get("state")).cloned().unwrap_or(Value::Null),
                    }));
                }
            }
            serde_json::to_vec(&json!({"entries": entries})).unwrap_or_default()
        }
        Err(e) => e,
    }
}

// ---- messageFeedback：真实持久 + 校验/版本并发 ----

/// maxNoteBytes 部署上限（对齐 TS Config.maxNoteBytes；宿主可经配置覆盖）。
const MAX_NOTE_BYTES: usize = 8192;

/// messageFeedback/list：读宿主真实反馈 KV，无会话 → session-not-found。
fn message_feedback_list(body: &Value) -> Vec<u8> {
    let session_id = body.get("sessionId").and_then(|s| s.as_str()).unwrap_or("");
    if session_id.is_empty() {
        return serde_json::to_vec(&json!({"ok": false, "error": {"code": "session-not-found", "sessionId": session_id}})).unwrap_or_default();
    }
    match read_row(session_id) {
        Some(row) => serde_json::to_vec(&json!({"ok": true, "value": {"items": row.get("items").cloned().unwrap_or(Value::Array(vec![]))}})).unwrap_or_default(),
        None => serde_json::to_vec(&json!({"ok": false, "error": {"code": "session-not-found", "sessionId": session_id}})).unwrap_or_default(),
    }
}

/// messageFeedback/put：真实校验 + 版本并发 + 持久。
fn message_feedback_put(body: &Value) -> Vec<u8> {
    let session_id = body.get("sessionId").and_then(|s| s.as_str()).unwrap_or("");
    let message_id = body.get("messageId").and_then(|s| s.as_str()).unwrap_or("");
    let rating = body.get("rating").and_then(|s| s.as_str()).unwrap_or("");
    let note = body.get("note").and_then(|s| s.as_str());
    if session_id.is_empty() || message_id.is_empty() {
        return serde_json::to_vec(&json!({"ok": false, "error": {"code": "target-not-found", "sessionId": session_id, "messageId": message_id}})).unwrap_or_default();
    }
    // note 校验：blank / too-large。
    if let Some(note) = note {
        if note.trim().is_empty() {
            return serde_json::to_vec(&json!({"ok": false, "error": {"code": "note-blank"}})).unwrap_or_default();
        }
        let actual = note.as_bytes().len();
        if actual > MAX_NOTE_BYTES {
            return serde_json::to_vec(&json!({"ok": false, "error": {"code": "note-too-large", "maxBytes": MAX_NOTE_BYTES, "actualBytes": actual}})).unwrap_or_default();
        }
    }
    if rating != "positive" && rating != "negative" {
        return serde_json::to_vec(&json!({"ok": false, "error": {"code": "target-not-found", "sessionId": session_id, "messageId": message_id}})).unwrap_or_default();
    }
    // session 存在性（真实投影）。
    let row = match read_row(session_id) {
        Some(r) => r,
        None => {
            // 会话消息里必须真实存在该 messageId（target-not-found）。
            if !session_has_message(session_id, message_id) {
                return serde_json::to_vec(&json!({"ok": false, "error": {"code": "session-not-found", "sessionId": session_id}})).unwrap_or_default();
            }
            match empty_row(session_id) {
                Some(r) => r,
                None => {
                    return serde_json::to_vec(&json!({"ok": false, "error": {"code": "session-not-found", "sessionId": session_id}})).unwrap_or_default()
                }
            }
        }
    };
    // 读取现存条目（克隆，避免借用阻塞 items 的可变）
    let mut items: Vec<Value> = row
        .get("items")
        .and_then(|i| i.as_array())
        .cloned()
        .unwrap_or_default();
    let existing: Option<Value> = items
        .iter()
        .find(|i| i.get("messageId").and_then(|m| m.as_str()) == Some(message_id))
        .cloned();
    let current = existing.as_ref();
    // ifVersion 并发检测。
    let if_version = body.get("ifVersion").and_then(|s| s.as_str());
    if let Some(ifv) = if_version {
        let cur_version = current.and_then(|c| c.get("version")).and_then(|v| v.as_str());
        if cur_version != Some(ifv) {
            return serde_json::to_vec(&json!({"ok": false, "error": {"code": "version-conflict", "current": current.cloned().unwrap_or(Value::Null)}})).unwrap_or_default();
        }
    }
    let now = clock_now();
    let version = new_version();
    let item = json!({
        "messageId": message_id,
        "rating": rating,
        "note": note.map(|n| n.to_string()),
        "version": version,
        "createdAt": current.and_then(|c| c.get("createdAt")).cloned().unwrap_or(Value::from(now)),
        "updatedAt": now,
    });
    if let Some(idx) = items.iter().position(|i| i.get("messageId").and_then(|m| m.as_str()) == Some(message_id)) {
        items[idx] = item.clone();
    } else {
        items.push(item.clone());
    }
    let mut new_row = row.clone();
    new_row["items"] = Value::Array(items);
    if write_row(session_id, &new_row).is_err() {
        return error("internal", "feedback write failed");
    }
    let mut value = json!({
        "messageId": message_id,
        "rating": rating,
        "version": version,
        "createdAt": current.and_then(|c| c.get("createdAt")).cloned().unwrap_or(Value::from(now)),
        "updatedAt": now,
    });
    if let Some(n) = note {
        value["note"] = json!(n);
    }
    serde_json::to_vec(&json!({"ok": true, "value": value})).unwrap_or_default()
}

/// messageFeedback/delete：absent 幂等 + 版本并发 + 持久。
fn message_feedback_delete(body: &Value) -> Vec<u8> {
    let session_id = body.get("sessionId").and_then(|s| s.as_str()).unwrap_or("");
    let message_id = body.get("messageId").and_then(|s| s.as_str()).unwrap_or("");
    let if_version = body.get("ifVersion").and_then(|s| s.as_str());
    let Some(mut row) = read_row(session_id) else {
        return serde_json::to_vec(&json!({"ok": false, "error": {"code": "session-not-found", "sessionId": session_id}})).unwrap_or_default();
    };
    let mut items: Vec<Value> = row.get("items").cloned().unwrap_or(Value::Array(vec![])).as_array().cloned().unwrap_or_default();
    let idx = items.iter().position(|i| i.get("messageId").and_then(|m| m.as_str()) == Some(message_id));
    let Some(idx) = idx else {
        return serde_json::to_vec(&json!({"ok": true, "value": {"absent": true}})).unwrap_or_default();
    };
    if let Some(ifv) = if_version {
        let cur = items[idx].get("version").and_then(|v| v.as_str());
        if cur != Some(ifv) {
            return serde_json::to_vec(&json!({"ok": false, "error": {"code": "version-conflict", "current": items[idx].clone()}})).unwrap_or_default();
        }
    }
    items.remove(idx);
    row["items"] = Value::Array(items);
    if write_row(session_id, &row).is_err() {
        return error("internal", "feedback delete failed");
    }
    serde_json::to_vec(&json!({"ok": true, "value": {"absent": true}})).unwrap_or_default()
}

/// 读反馈 KV 行（无 → None）。
fn read_row(session_id: &str) -> Option<Value> {
    match get_service("kv", &json!({"namespace": "message_feedback", "key": session_id})) {
        Ok(v) => {
            let value = v.get("value").cloned();
            match value {
                Some(Value::Null) | None => None,
                Some(v) => Some(v),
            }
        }
        Err(_) => None,
    }
}

/// 建一个空的反馈行（继承会话 identity）。
fn empty_row(session_id: &str) -> Option<Value> {
    match get_service("sessionIdentity", &json!({"sessionId": session_id})) {
        Ok(v) => Some(json!({
            "session": v.get("identity").cloned().unwrap_or(Value::Null),
            "items": Value::Array(vec![]),
        })),
        Err(_) => None,
    }
}

/// 会话消息流里是否真实存在该 messageId（target-not-found 校验）。
fn session_has_message(session_id: &str, message_id: &str) -> bool {
    match get_service("sessionMessages", &json!({"sessionId": session_id})) {
        Ok(v) => v
            .get("messageIds")
            .and_then(|m| m.as_array())
            .map(|arr| arr.iter().any(|m| m.as_str() == Some(message_id)))
            .unwrap_or(false),
        Err(_) => false,
    }
}

/// 真实时钟（宿主 epoch ms）。
fn clock_now() -> u64 {
    match get_service("time", &json!({})) {
        Ok(v) => v.get("epochMs").and_then(|e| e.as_u64()).unwrap_or(0),
        Err(_) => 0,
    }
}

/// 真实 uuid v4（宿主生成，保证唯一）。
fn new_version() -> String {
    match get_service("newVersion", &json!({})) {
        Ok(v) => v
            .get("uuid")
            .and_then(|u| u.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "00000000-0000-0000-0000-000000000000".to_string()),
        Err(_) => "00000000-0000-0000-0000-000000000000".to_string(),
    }
}

/// 写反馈 KV 行。
fn write_row(session_id: &str, row: &Value) -> Result<(), Vec<u8>> {
    set_service("kv", &json!({"namespace": "message_feedback", "key": session_id, "value": row}))
        .map(|_| ())
}

impl Guest for HostRemote {
    fn handle(namespace: String, method: String, body: Vec<u8>) -> Vec<u8> {
        let body_value: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        match (namespace.as_str(), method.as_str()) {
            ("pluginInventory", "list") => plugin_inventory_list(&body_value),
            ("messageFeedback", "list") => message_feedback_list(&body_value),
            ("messageFeedback", "put") => message_feedback_put(&body_value),
            ("messageFeedback", "delete") => message_feedback_delete(&body_value),
            _ => error(
                "not-implemented",
                &format!("host-remote: no endpoint {namespace}/{method}"),
            ),
        }
    }
}

bindings::export!(HostRemote with_types_in bindings);

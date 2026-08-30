//! llm-deepseek 服务装配单元试点（P2：rust + ui 声明 + wasm，组件模型专用）。
//!
//! 由 deepseek-harness `packages/llm/llm-deepseek`（TS cordis 一行插件）转换而来：
//! - `name = 'llm-deepseek'` → 插件包文件夹名 + remote namespace `llm-deepseek`；
//! - `Config` schema → UI 声明字段子集（describeUI / web/ui.json，声明=数据，非代码）；
//! - settings namespace `llm-deepseek` → `values` 经 host-services 落宿主 kv
//!   （key `llm-deepseek/settings`；动作白名单 + fail-loud，不伪造成功）；
//! - `discoverModels` → 动作 RPC 返回默认模型目录（V4 Flash / V4 Pro / V4 Flash Vision Exp）。
//!
//! 复用 host-remote world 接口身份（export remote + import host-services），
//! 宿主 `WasmRemoteEndpointPlugin` 零改动即可加载。

#[allow(warnings)]
mod bindings;

use bindings::exports::dsh::host_remote::remote::Guest;
use bindings::dsh::host_remote::host_services;
use serde_json::{json, Value};

// ---- 默认容量常量（D-222：模型目录改真外呼，DEFAULT_MODELS 桩已退役）----

const DEFAULT_CONTEXT_WINDOW: u64 = 1_000_000;
const DEFAULT_MAX_TOKENS: u64 = 256_000;

// ---- UI 声明（数据，非代码）。静态 web/ui.json 与其保持逐字段一致（m32 断言）。----

/// 声明生产（P2：Rust 只生声明，不渲染）。
fn ui_declaration() -> Value {
    // D-225：单一事实源=web/ui.json（编译期嵌入；声明=数据，非代码）。
    serde_json::from_str(include_str!("../web/ui.json")).expect("ui.json must be valid JSON")
}
/// 规范化错误（fail-loud：绝不伪造成功；code 用前端 RpcError 联合内的 internal）。
fn error(code: &str, message: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "ok": false,
        "error": { "code": code, "message": message },
    }))
    .unwrap_or_default()
}

/// 宿主服务只读投影（host-services.get）：失败 → 规范化错误字节。
fn get_service(service: &str, payload: &Value) -> Result<Value, Vec<u8>> {
    let bytes = host_services::get(service, &serde_json::to_vec(payload).unwrap_or_default());
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(v) if v.get("ok").and_then(|o| o.as_bool()) == Some(true) => Ok(v),
        Ok(v) => {
            let err = v.get("error").cloned().unwrap_or_else(|| {
                json!({"code": "service", "message": format!("host service {service} failure")})
            });
            Err(serde_json::to_vec(&json!({"ok": false, "error": err})).unwrap_or_default())
        }
        Err(_) => Err(error("decode", "host service projection unparseable")),
    }
}

/// 宿主服务写入（host-services.set）：失败 → 规范化错误字节。
fn set_service(service: &str, payload: &Value) -> Result<Value, Vec<u8>> {
    let bytes = host_services::set(service, &serde_json::to_vec(payload).unwrap_or_default());
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(v) if v.get("ok").and_then(|o| o.as_bool()) == Some(true) => Ok(v),
        Ok(v) => {
            let err = v.get("error").cloned().unwrap_or_else(|| {
                json!({"code": "service", "message": format!("host service {service} write failure")})
            });
            Err(serde_json::to_vec(&json!({"ok": false, "error": err})).unwrap_or_default())
        }
        Err(_) => Err(error("decode", "host service write unparseable")),
    }
}

const KV_SETTINGS_KEY: &str = "llm-deepseek/settings";

/// describeUI：返回 UI 声明（与静态 web/ui.json 一致；声明=数据，非代码）。
fn describe_ui(_body: &Value) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "ok": true,
        "value": ui_declaration(),
    }))
    .unwrap_or_default()
}

/// currentValues：读宿主 kv 已保存的设置（无则空对象，诚实）。
fn current_values(_body: &Value) -> Vec<u8> {
    match get_service("kv", &json!({ "key": KV_SETTINGS_KEY })) {
        Ok(proj) => {
            let values = proj.get("value").cloned().unwrap_or(Value::Null);
            let values = if values.is_null() {
                json!({})
            } else {
                values
            };
            serde_json::to_vec(&json!({ "ok": true, "value": { "values": values } })).unwrap_or_default()
        }
        Err(e) => e,
    }
}

/// save：白名单校验 settings 字段 → 落宿主 kv。坏入参 / 未知字段 → fail-loud，不落盘。
fn save(body: &Value) -> Vec<u8> {
    let values = body.get("values").cloned().unwrap_or(Value::Null);
    let Some(values) = values.as_object() else {
        return error("internal", "llm-deepseek/save: body.values must be an object");
    };
    // 白名单：仅接受声明字段子集内的键（未知键 → fail-loud，不伪造成功）。
    let known = [
        "apiKeyEnv",
        "baseURL",
        "thinking",
        "reasoningEffort",
        "maxTokens",
        "defaultContextWindow",
        "models",
    ];
    for key in values.keys() {
        if !known.contains(&key.as_str()) {
            return error("internal", &format!("llm-deepseek/save: unknown field \"{key}\""));
        }
    }
    let cleaned: Value = values.into_iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    match set_service("kv", &json!({ "key": KV_SETTINGS_KEY, "value": cleaned })) {
        Ok(_) => serde_json::to_vec(&json!({ "ok": true, "value": { "saved": true } })).unwrap_or_default(),
        Err(e) => e,
    }
}

/// discoverModels：真外呼发现（D-222）——baseURL 优先当前表单值、缺省回落
/// 已存设置；真实 HTTP 由宿主 `llmDiscover` 臂承担（key 宿主 env-only，wasm 不接触）。
fn discover_models(body: &Value) -> Vec<u8> {
    let mut base = body
        .get("values")
        .or_else(|| body.get("baseURL").map(|_| body))
        .and_then(|v| v.get("baseURL"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if base.trim().is_empty() {
        if let Ok(proj) = get_service("kv", &json!({ "key": KV_SETTINGS_KEY })) {
            base = proj
                .get("value")
                .and_then(|v| v.get("baseURL"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
        }
    }
    if base.trim().is_empty() {
        return error(
            "internal",
            "llm-deepseek/discoverModels: baseURL not configured (fill form or save first)",
        );
    }
    match get_service("llmDiscover", &json!({ "baseURL": base })) {
        Ok(proj) => {
            let models = proj.get("models").cloned().unwrap_or_else(|| json!([]));
            serde_json::to_vec(&json!({ "ok": true, "value": { "models": models } })).unwrap_or_default()
        }
        Err(e) => e,
    }
}

struct LlmDeepSeek;

impl Guest for LlmDeepSeek {
    fn handle(namespace: String, method: String, body: Vec<u8>) -> Vec<u8> {
        let body_value: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        match (namespace.as_str(), method.as_str()) {
            ("llm-deepseek", "describeUI") => describe_ui(&body_value),
            ("llm-deepseek", "currentValues") => current_values(&body_value),
            ("llm-deepseek", "save") => save(&body_value),
            ("llm-deepseek", "discoverModels") => discover_models(&body_value),
            _ => error(
                "internal",
                &format!("llm-deepseek: endpoint {namespace}/{method} not provided by this plugin"),
            ),
        }
    }
}

bindings::export!(LlmDeepSeek with_types_in bindings);

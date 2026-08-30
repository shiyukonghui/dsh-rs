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

// ---- 默认模型目录（对齐 TS llm-deepseek 的 DEFAULT_MODELS）----

const DEFAULT_CONTEXT_WINDOW: u64 = 1_000_000;
const DEFAULT_MAX_TOKENS: u64 = 256_000;

fn default_models() -> Value {
    json!([
        { "id": "deepseek-v4-flash", "name": "DeepSeek-V4-Flash", "contextWindow": DEFAULT_CONTEXT_WINDOW },
        { "id": "deepseek-v4-pro", "name": "DeepSeek-V4-Pro", "contextWindow": DEFAULT_CONTEXT_WINDOW },
        {
            "id": "deepseek-v4-flash-vision-exp",
            "name": "DeepSeek-V4-Flash-Vision-Exp",
            "contextWindow": DEFAULT_CONTEXT_WINDOW,
            "inputModalities": ["text", "image"],
        },
    ])
}

// ---- UI 声明（数据，非代码）。静态 web/ui.json 与其保持逐字段一致（m32 断言）。----

/// 声明生产（P2：Rust 只生声明，不渲染）。
fn ui_declaration() -> Value {
    json!({
        "$schema": "dsh.panel-ui/v2",
        "kind": "card",
        "cardId": "llm-deepseek.settings",
        "type": "model",
        "title": "DeepSeek Provider",
        "description": "DeepSeek provider 连接与模型目录设置",
        "size": { "w": 2, "h": 3 },
        "view": {
            "kind": "form",
            "dataRpc": ["llm-deepseek", "currentValues"],
            "fields": [
            {
                "name": "apiKeyEnv",
                "label": "API Key 环境变量",
                "type": "text",
                "role": "credential-ref",
                "default": "DEEPSEEK_API_KEY",
                "required": true
            },
            {
                "name": "baseURL",
                "label": "Base URL",
                "type": "text",
                "default": "https://api.deepseek.com"
            },
            {
                "name": "thinking",
                "label": "Thinking",
                "type": "select",
                "options": ["enabled", "disabled"],
                "default": "enabled"
            },
            {
                "name": "reasoningEffort",
                "label": "Reasoning Effort",
                "type": "select",
                "options": ["off", "low", "high", "max"],
                "default": "high"
            },
            {
                "name": "maxTokens",
                "label": "Max Tokens",
                "type": "number",
                "default": DEFAULT_MAX_TOKENS,
                "min": 1
            },
            {
                "name": "defaultContextWindow",
                "label": "Default Context Window",
                "type": "number",
                "default": DEFAULT_CONTEXT_WINDOW,
                "min": 1
            },
            {
                "name": "models",
                "label": "Models（目录）",
                "type": "list",
                "item": {
                    "type": "object",
                    "fields": [
                        { "name": "id", "label": "Model ID", "type": "text", "required": true },
                        { "name": "name", "label": "显示名", "type": "text" },
                        { "name": "contextWindow", "label": "Context Window", "type": "number", "min": 1 }
                    ]
                }
            }
            ],
            "actions": [
                { "name": "save", "label": "保存", "rpc": ["llm-deepseek", "save"], "primary": true },
                { "name": "discoverModels", "label": "发现模型", "rpc": ["llm-deepseek", "discoverModels"] }
            ]
        }
    })
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

/// discoverModels：返回默认模型目录（试点阶段；真实网络探测留后续 genai 决策）。
fn discover_models(_body: &Value) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "ok": true,
        "value": { "models": default_models() },
    }))
    .unwrap_or_default()
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

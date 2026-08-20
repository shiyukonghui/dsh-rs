//! Remote 契约仓库（M0: `dsh-api:spec`）。
//!
//! 权威参考：RPC 契约的**生成物转译**（见 `spec/README.md` 的 `$provenance`）：
//! `spec/methods.json` / `spec/errors.json` / `spec/messages.json` /
//! `spec/schemas/session.json`。本模块以 `include_str!` 内嵌这些 JSON（单一权威），
//! 提供 typed 访问子——M1/M3 的 dispatch 按本仓库校验方法名、错误码、消息模型与
//! 各方法 request/value 模式。

use serde_json::{Map, Value};
use std::sync::OnceLock;

/// 内嵌的方法目录 JSON（52 个 client-request 方法）。
pub const METHODS_JSON: &str = include_str!("../spec/methods.json");
/// 内嵌的错误码目录 JSON（39 个错误码 + details 字段标注）。
pub const ERRORS_JSON: &str = include_str!("../spec/errors.json");
/// 内嵌的四象限消息模型 JSON。
pub const MESSAGES_JSON: &str = include_str!("../spec/messages.json");
/// 内嵌的 session 域 request/value JSON Schema。
pub const SESSION_SCHEMA_JSON: &str = include_str!("../spec/schemas/session.json");

/// 一个 Remote 方法条目（`spec/methods.json` 的行）。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct RpcMethod {
    /// wire 路径（`session.list`）。
    pub wire: String,
    /// Remote 命名空间。
    pub namespace: String,
    /// 导出的 Service 方法名。
    pub method: String,
    /// request schema 引用名（权威 zod/JSON Schema 名）。
    #[serde(rename = "requestSchema")]
    pub request_schema: String,
    /// value schema 引用名。
    #[serde(rename = "valueSchema")]
    pub value_schema: String,
}

/// 一个错误码条目（`spec/errors.json` 的行）。
#[derive(Debug, Clone)]
pub struct ErrorCode {
    /// 稳定错误码。
    pub code: String,
    /// details 字段 → required/optional 标注（有序，保持目录顺序）。
    pub details: Vec<(String, String)>,
}

impl ErrorCode {
    /// 字段在 details 层的必备标注（字段不在目录 → 空串）。
    pub fn detail_mark(&self, field: &str) -> &str {
        self.details
            .iter()
            .find(|(f, _)| f == field)
            .map(|(_, mark)| mark.as_str())
            .unwrap_or("")
    }
}

/// 一条消息形状的描述（`spec/messages.json` 的成员）。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct MessageShape {
    pub discriminant: String,
    pub fields: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carrier: Option<String>,
}

static METHODS: OnceLock<Vec<RpcMethod>> = OnceLock::new();
static ERROR_CODES: OnceLock<Vec<ErrorCode>> = OnceLock::new();
static MESSAGE_TYPES: OnceLock<Vec<String>> = OnceLock::new();
static MESSAGE_SHAPES: OnceLock<Vec<(String, MessageShape)>> = OnceLock::new();
static SESSION_SCHEMA: OnceLock<Value> = OnceLock::new();

fn parse_methods() -> Vec<RpcMethod> {
    #[derive(serde::Deserialize)]
    struct Root {
        methods: Vec<RpcMethod>,
    }
    let root: Root = serde_json::from_str(METHODS_JSON)
        .expect("spec/methods.json must parse (regenerate from rpc-map.ts)");
    root.methods
}

fn parse_error_codes() -> Vec<ErrorCode> {
    #[derive(serde::Deserialize)]
    struct Root {
        #[serde(rename = "errorCodes")]
        error_codes: Vec<ErrorCodeRaw>,
    }
    #[derive(serde::Deserialize)]
    struct ErrorCodeRaw {
        code: String,
        details: Map<String, Value>,
    }
    let root: Root =
        serde_json::from_str(ERRORS_JSON).expect("spec/errors.json must parse (rpc.ts)");
    root.error_codes
        .into_iter()
        .map(|e| ErrorCode {
            code: e.code,
            details: e
                .details
                .into_iter()
                .map(|(field, mark)| {
                    (field, mark.as_str().unwrap_or("required").to_string())
                })
                .collect(),
        })
        .collect()
}

fn parse_messages() -> (Vec<String>, Vec<(String, MessageShape)>) {
    let v: Value = serde_json::from_str(MESSAGES_JSON)
        .expect("spec/messages.json must parse (rpc.ts)");
    let messages = v
        .get("messages")
        .and_then(Value::as_object)
        .expect("messages.json: messages must be an object");
    let shapes: Vec<(String, MessageShape)> = messages
        .iter()
        .map(|(kind, shape)| {
            let parsed: MessageShape = serde_json::from_value(shape.clone())
                .expect("messages.json message shape must parse");
            (kind.clone(), parsed)
        })
        .collect();
    let types = shapes.iter().map(|(k, _)| k.clone()).collect();
    (types, shapes)
}

fn parse_session_schema() -> Value {
    serde_json::from_str(SESSION_SCHEMA_JSON)
        .expect("spec/schemas/session.json must parse (sessions.schema.ts)")
}

/// 全部 Remote 方法（52 项，目录顺序）。
pub fn methods() -> &'static [RpcMethod] {
    METHODS.get_or_init(parse_methods)
}

/// 方法是否存在（wire 路径精确匹配）。
pub fn has_method(wire: &str) -> bool {
    methods().iter().any(|m| m.wire == wire)
}

/// 按 wire 路径查方法。
pub fn find_method(wire: &str) -> Option<&'static RpcMethod> {
    methods().iter().find(|m| m.wire == wire)
}

/// 目录中出现的命名空间（去重、按首次出现顺序）。
pub fn namespaces() -> Vec<&'static str> {
    let mut seen = Vec::new();
    for m in methods() {
        if !seen.contains(&m.namespace.as_str()) {
            seen.push(m.namespace.as_str());
        }
    }
    seen
}

/// 全部错误码（39 项，目录顺序）。
pub fn error_codes() -> &'static [ErrorCode] {
    ERROR_CODES.get_or_init(parse_error_codes)
}

/// 错误码是否存在。
pub fn has_error_code(code: &str) -> bool {
    error_codes().iter().any(|e| e.code == code)
}

/// 按错误码查目录条目。
pub fn find_error_code(code: &str) -> Option<&'static ErrorCode> {
    error_codes().iter().find(|e| e.code == code)
}

/// 四象限消息类型名（`client-request`/`server-response`/`server-request`/`client-response`）。
pub fn message_types() -> &'static [String] {
    let (types, shapes) = parse_message_parts();
    let _ = shapes;
    types
}

fn parse_message_parts() -> (&'static Vec<String>, &'static [(String, MessageShape)]) {
    (
        MESSAGE_TYPES.get_or_init(|| parse_messages().0),
        MESSAGE_SHAPES.get_or_init(|| parse_messages().1),
    )
}

/// 一条消息类型的形状描述。
pub fn message_shape(kind: &str) -> Option<&'static MessageShape> {
    parse_message_parts().1.iter().find(|(k, _)| k == kind).map(|(_, s)| s)
}

/// RpcResult 判别（ok）与 RpcReceipt 是否在消息模型中声明。
pub fn has_rpc_result() -> bool {
    let v: Value = serde_json::from_str(MESSAGES_JSON).expect("messages.json");
    v.get("rpcResult").is_some()
}
pub fn has_rpc_receipt() -> bool {
    let v: Value = serde_json::from_str(MESSAGES_JSON).expect("messages.json");
    v.get("rpcReceipt").is_some()
}

fn session_schema() -> &'static Value {
    SESSION_SCHEMA.get_or_init(parse_session_schema)
}

/// 解析 session 域某方法的 request 模式（无 → None，M3 前的 fail-loud 由 dispatch 处理）。
pub fn session_request_schema(wire: &str) -> Option<&'static Value> {
    let method = find_method(wire)?;
    if method.namespace != "session" {
        return None;
    }
    session_schema().get("requests")?.get(wire)
}

/// 解析 session 域某方法的 value 模式。
pub fn session_value_schema(wire: &str) -> Option<&'static Value> {
    let method = find_method(wire)?;
    if method.namespace != "session" {
        return None;
    }
    session_schema().get("values")?.get(wire)
}

//! HTTP llm 适配器（M17/M31）：OpenAI 兼容 `/chat/completions` 客户端。
//!
//! 手写 HTTP/1.1（`std::net::TcpStream`；单线程纪律）；**M31：https 支持**
//! （native-tls 包裹——TLS 握手 + 加密传输）。
//! 第一性原理：真实模型接入的最小契约 = `POST {base}/chat/completions`，
//! body 为 `{model, messages, tools}`，响应 `choices[0].message`。
//! 非 2xx / 形状不符 → 错误 JSON（fail loud，不 panic）。

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use crate::types::Value;

/// OpenAI 兼容 chat/completions 路径（base 后拼接）。
const CHAT_PATH: &str = "/chat/completions";

/// 调用 OpenAI 兼容端点。`base` 形如 `http://host:port`（可带 /v1 前缀）。
/// 返回解析后的响应 JSON：`{content, tool_calls?}` 或 `{error, content:""}`。
///
/// M34：`messages` 为生产 `Message[]` 形状（`{id, role, content: ContentBlock[],
/// source}`）——先经 [`messages_to_wire`] 序列化为 OpenAI wire 形状再发送
/// （对齐 DSH `serializeMessages`）；扁平形状（`{role, content: string}`）亦
/// 兼容（原样透传，保持 M17 行为）。
pub fn chat_completions(
    base: &str,
    api_key: Option<&str>,
    model: &str,
    messages: &[Value],
    tools: &[Value],
) -> Value {
    let (scheme, host, port, path) = match parse_base(base) {
        Some(v) => v,
        None => return error_value("invalid base url"),
    };
    let wire = messages_to_wire(messages);
    let body = serde_json::json!({
        "model": model,
        "messages": wire,
        "tools": tools,
    });
    let body_text = match serde_json::to_string(&body) {
        Ok(t) => t,
        Err(e) => return error_value(&format!("request encode: {e}")),
    };

    let request = build_request(&path, api_key, &body_text);
    let response = match tcp_exchange(&scheme, &host, port, &request) {
        Ok(r) => r,
        Err(e) => return error_value(&e),
    };

    // 解析状态行 + 头 + body
    let (status, body_bytes) = match split_response(&response) {
        Some(v) => v,
        None => return error_value("malformed HTTP response"),
    };
    if !(200..300).contains(&status) {
        let detail = String::from_utf8_lossy(&body_bytes);
        return error_value(&format!("HTTP {status}: {detail}"));
    }
    let parsed: Value = match serde_json::from_slice(&body_bytes) {
        Ok(v) => v,
        Err(e) => return error_value(&format!("response parse: {e}")),
    };
    // choices[0].message → {content, tool_calls?}
    let message = parsed
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|c| c.first())
        .and_then(|c| c.get("message"))
        .cloned()
        .unwrap_or_else(|| error_value("no choices[0].message"));
    message
}

/// 生产 `Message[]` → OpenAI wire 消息序列（对齐 DSH `serializeMessages`，
/// `packages/llm/llm-deepseek/src/serialize.ts`）：
/// - `system` → `{role:"system", content}`（text blocks 拼接）；
/// - `assistant` → `{role:"assistant", content, tool_calls?}`（tool-call blocks →
///   `{id, type:"function", function:{name, arguments}}`；content 永不为 null——
///   纯 tool-call 轮 content 为 ""）；
/// - `user`（含 tool-result blocks）→ 文本先成一条 `{role:"user"}`，每个
///   tool-result block 展开为独立 `{role:"tool", tool_call_id, content}`；
/// - 扁平形状（`content` 为字符串、无 `id/source`）→ 原样透传（兼容旧契约）。
pub fn messages_to_wire(messages: &[Value]) -> Vec<Value> {
    let mut wire = Vec::new();
    for m in messages {
        let content = m.get("content").cloned().unwrap_or(Value::Null);
        // 扁平形状（content 是字符串）→ 原样透传
        if content.is_string() || content.is_null() {
            wire.push(m.clone());
            continue;
        }
        let Some(blocks) = content.as_array() else {
            wire.push(m.clone());
            continue;
        };
        let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("user");
        let flatten_text = |b: &Value| {
            b.as_array()
                .map(|arr| {
                    arr.iter()
                        .filter(|blk| blk.get("type").and_then(|t| t.as_str()) == Some("text"))
                        .filter_map(|blk| blk.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join("")
                })
                .unwrap_or_default()
        };
        match role {
            "system" => wire.push(serde_json::json!({
                "role": "system",
                "content": flatten_text(&content),
            })),
            "assistant" => {
                let text = flatten_text(&content);
                let tool_calls: Vec<Value> = blocks
                    .iter()
                    .filter(|blk| blk.get("type").and_then(|t| t.as_str()) == Some("tool-call"))
                    .map(|blk| {
                        serde_json::json!({
                            "id": blk.get("id").cloned().unwrap_or(Value::Null),
                            "type": "function",
                            "function": {
                                "name": blk.get("name").cloned().unwrap_or(Value::Null),
                                "arguments": blk.get("arguments").cloned().unwrap_or(Value::Null),
                            },
                        })
                    })
                    .collect();
                let mut out = serde_json::json!({
                    "role": "assistant",
                    // Text-less turns send "" — NEVER null（对齐生产：纯
                    // tool-call / reasoning 轮 content 为 ""）。
                    "content": text,
                });
                if !tool_calls.is_empty() {
                    out["tool_calls"] = serde_json::Value::Array(tool_calls);
                }
                wire.push(out);
            }
            _ => {
                // user role：文本 + tool-result 展开（生产规则：
                // text 非空或无 tool-result 时成 user 消息；每个 tool-result
                // block 独立成 {role:"tool", tool_call_id, content}）。
                let text = flatten_text(&content);
                let tool_results: Vec<Value> = blocks
                    .iter()
                    .filter(|blk| blk.get("type").and_then(|t| t.as_str()) == Some("tool-result"))
                    .cloned()
                    .collect();
                if !text.is_empty() || tool_results.is_empty() {
                    wire.push(serde_json::json!({ "role": "user", "content": text }));
                }
                for result in tool_results {
                    let tool_call_id = result
                        .get("toolCallId")
                        .cloned()
                        .unwrap_or(Value::Null);
                    let result_content = result.get("content").cloned().unwrap_or(Value::Null);
                    // 空工具输出仍需 wire 上有 content（对齐生产
                    // `flattenText(...) || '(no output)'`）。
                    let rtext = flatten_text(&result_content);
                    let rtext = if rtext.is_empty() {
                        "(no output)".to_string()
                    } else {
                        rtext
                    };
                    wire.push(serde_json::json!({
                        "role": "tool",
                        "tool_call_id": tool_call_id,
                        "content": rtext,
                    }));
                }
            }
        }
    }
    wire
}

/// 解析 base URL → (scheme, host, port, 拼接后的路径)。
/// 支持 `http://host:port[/prefix]` 与 `https://host[:port][/prefix]`；
/// 默认端口 80（http）/ 443（https）。
fn parse_base(base: &str) -> Option<(String, String, u16, String)> {
    let (scheme, rest) = if let Some(r) = base.strip_prefix("https://") {
        ("https".to_string(), r)
    } else if let Some(r) = base.strip_prefix("http://") {
        ("http".to_string(), r)
    } else {
        return None;
    };
    let (authority, prefix) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let (host, port) = match authority.rfind(':') {
        Some(i) => (
            authority[..i].to_string(),
            authority[i + 1..].parse::<u16>().ok(),
        ),
        None => (authority.to_string(), None),
    };
    if host.is_empty() {
        return None;
    }
    let port = port.unwrap_or(if scheme == "https" { 443 } else { 80 });
    let path = format!("{prefix}{CHAT_PATH}");
    Some((scheme, host, port, path))
}

/// 构造 HTTP/1.1 POST 请求（Connection: close；可选 Bearer 认证）。
fn build_request(path: &str, api_key: Option<&str>, body: &str) -> String {
    let mut req = format!(
        "POST {path} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
        "127.0.0.1",
        body.len()
    );
    if let Some(key) = api_key {
        req.push_str(&format!("Authorization: Bearer {key}\r\n"));
    }
    req.push_str("Connection: close\r\n\r\n");
    req.push_str(body);
    req
}

/// TCP 交换：发送请求、读取完整响应（按 Content-Length 或读到关闭）。
/// https 时经 native-tls 包裹（TLS 握手 + 加密传输）。
/// 可读写流（TcpStream 或 TLS 包裹）。
trait ReadWrite: Read + Write {}
impl<T: Read + Write> ReadWrite for T {}

fn tcp_exchange(
    scheme: &str,
    host: &str,
    port: u16,
    request: &str,
) -> Result<Vec<u8>, String> {
    let tcp = TcpStream::connect((host, port))
        .map_err(|e| format!("connect {host}:{port}: {e}"))?;
    tcp.set_read_timeout(Some(Duration::from_secs(30))).ok();
    let mut stream: Box<dyn ReadWrite> = if scheme == "https" {
        let connector = native_tls::TlsConnector::builder()
            .build()
            .map_err(|e| format!("tls connector: {e}"))?;
        let tls = connector
            .connect(host, tcp)
            .map_err(|e| format!("tls handshake {host}:{port}: {e}"))?;
        Box::new(tls)
    } else {
        Box::new(tcp)
    };
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("send: {e}"))?;
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    loop {
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                // Content-Length 足够则提前返回（避免依赖 close 时机）
                if let Some(total) = expected_length(&buf) {
                    if buf.len() >= total {
                        buf.truncate(total);
                        break;
                    }
                }
            }
            Err(_) => break,
        }
    }
    Ok(buf)
}

/// 若响应头声明 Content-Length，返回总字节数（头 + 空行 + body）。
fn expected_length(buf: &[u8]) -> Option<usize> {
    let end = buf.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&buf[..end]);
    let cl = head
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
        .and_then(|l| l.split(':').nth(1))
        .and_then(|v| v.trim().parse::<usize>().ok())?;
    Some(end + 4 + cl)
}

/// 拆分响应为 (状态码, body 字节)。
fn split_response(buf: &[u8]) -> Option<(u16, Vec<u8>)> {
    let end = buf.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&buf[..end]);
    let status = head
        .lines()
        .next()?
        .split_whitespace()
        .nth(1)?
        .parse::<u16>()
        .ok()?;
    Some((status, buf[end + 4..].to_vec()))
}

fn error_value(msg: &str) -> Value {
    serde_json::json!({
        "error": msg,
        "content": "",
    })
}

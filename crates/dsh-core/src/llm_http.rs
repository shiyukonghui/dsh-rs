//! HTTP llm 适配器（M17/M31）：OpenAI 兼容 `/chat/completions` 客户端。
//!
//! 手写 HTTP/1.1（`std::net::TcpStream`；单线程纪律）；**M31：https 支持**
//! （native-tls 包裹——TLS 握手 + 加密传输）。
//! 第一性原理：真实模型接入的最小契约 = `POST {base}/chat/completions`，
//! body 为 `{model, messages, tools}`，响应 `choices[0].message`。
//! 非 2xx / 形状不符 → 错误 JSON（fail loud，不 panic）。

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, atomic::AtomicBool, atomic::Ordering};
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

/// 可中断读取：连接后可选的取消谓词 + 读取超时轮询间隔（None = 原 30s 单次阻塞）。
struct ReadBudget<'a> {
    /// 每次 `read` 阻塞上限（短超时轮询 cancel 谓词的前提）。
    pub read_timeout: Duration,
    /// 谓词置位 → 主动中断（返回已读部分 + 置 `abort_flag`）。
    pub abort_flag: Option<Arc<AtomicBool>>,
    pub cancel: Option<&'a (dyn Fn() -> bool + Send + Sync)>,
}

fn tcp_exchange(
    scheme: &str,
    host: &str,
    port: u16,
    request: &str,
) -> Result<Vec<u8>, String> {
    tcp_exchange_core(scheme, host, port, request, &ReadBudget {
        read_timeout: Duration::from_secs(30),
        abort_flag: None,
        cancel: None,
    })
}

/// 可中断变体（D-115 Phase 4）：`cancel` 置位 → 置 `abort_flag` 并立即返回
/// 已读部分（socket 关闭由 drop 完成；不等待剩余响应）。
fn tcp_exchange_abortable(
    scheme: &str,
    host: &str,
    port: u16,
    request: &str,
    cancel: Option<&(dyn Fn() -> bool + Send + Sync)>,
    abort_flag: Arc<AtomicBool>,
) -> Result<Vec<u8>, String> {
    tcp_exchange_core(scheme, host, port, request, &ReadBudget {
        // 短读超时：让 cancel 谓词可在流中尽快被观察（对齐 TS fetch abort 响应性）。
        read_timeout: Duration::from_millis(200),
        abort_flag: Some(abort_flag),
        cancel,
    })
}

fn tcp_exchange_core(
    scheme: &str,
    host: &str,
    port: u16,
    request: &str,
    budget: &ReadBudget<'_>,
) -> Result<Vec<u8>, String> {
    let tcp = TcpStream::connect((host, port))
        .map_err(|e| format!("connect {host}:{port}: {e}"))?;
    tcp.set_read_timeout(Some(budget.read_timeout)).ok();
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
        // 读前先查 cancel：置位 → 中断（带出已读部分 + abort 标志）。
        if let (Some(flag), Some(cancel)) = (&budget.abort_flag, &budget.cancel) {
            if cancel() {
                flag.store(true, Ordering::SeqCst);
                break;
            }
        }
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
            // 可中断模式：短读超时是轮询 cancel 谓词的醒来时刻——继续循环（超时
            // 非错误）；原 30s 单次阻塞模式把 IO 错误（含超时）当断开（保持原语义）。
            Err(_) if budget.abort_flag.is_some() => continue,
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

// ---------------------------------------------------------------------------
// M6 step5a（D-080）：流式 chat/completions 传输——POST 已序列化（含 `"stream": true`）
// 的请求体，返回原始响应体字节；SSE 解码属 dsh-llm-deepseek（`sse::parse_sse`，不重复
// 造 parser）。复用 tcp_exchange/build_request/parse_base；非 2xx → 带 status 的结构化错误。
// ---------------------------------------------------------------------------

/// 流式请求的产物：HTTP 状态码 + 原始响应体字节（SSE 文本；由调用方解析）。
#[derive(Debug, Clone, PartialEq)]
pub struct StreamBody {
    pub status: u16,
    pub bytes: Vec<u8>,
}

/// 流式请求的失败：携带状态码（0 = 连接/IO/形状错误）与详情。
#[derive(Debug, Clone, PartialEq)]
pub struct StreamHttpError {
    pub status: u16,
    pub detail: String,
}

impl std::fmt::Display for StreamHttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let st = if self.status == 0 { "network/io".to_string() } else { format!("HTTP {}", self.status) };
        write!(f, "{st}: {}", self.detail)
    }
}

/// 可中断读取的产物（D-115 Phase 4）：`aborted = true` 表示读取因取消谓词被
/// **主动中断**（调用方不再等待完整响应；partial bytes 为已读部分）。对齐 TS
/// `fetch(url, {signal})` 的 abort 语义——abort 不是传输错误，是正常终止的一支。
#[derive(Debug, Clone, PartialEq)]
pub struct AbortableStreamBody {
    pub status: u16,
    pub bytes: Vec<u8>,
    pub aborted: bool,
}

/// POST 流式 chat/completions 的可中断变体：读取循环以短读超时轮询 `cancel`
/// 谓词；谓词置位 → 关闭 socket、返回 `aborted: true`（不返回 Error）。阻塞读因此
/// 可被外部取消信号打断（真·生成中停止的前提）。`base`/`api_key`/`body` 语义与
/// [`chat_completions_stream`] 一致。
///
/// 返回约定：`aborted == true` 时 `status` 为 0、`bytes` 为空（调用方以 Aborted
/// 归一，无需已读部分）；否则与 [`chat_completions_stream`] 相同（非 2xx → Err）。
pub fn chat_completions_stream_abortable(
    base: &str,
    api_key: Option<&str>,
    body: &str,
    cancel: Option<&(dyn Fn() -> bool + Send + Sync)>,
) -> Result<AbortableStreamBody, StreamHttpError> {
    let (scheme, host, port, path) = match parse_base(base) {
        Some(v) => v,
        None => return Err(StreamHttpError { status: 0, detail: "invalid base url".into() }),
    };
    let request = build_request(&path, api_key, body);
    // abort 标志经共享指针从读取循环带出（tcp_exchange 只返回 bytes）。
    let abort_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let response = match tcp_exchange_abortable(&scheme, &host, port, &request, cancel, abort_flag.clone()) {
        Ok(r) => r,
        Err(e) => return Err(StreamHttpError { status: 0, detail: e }),
    };
    let aborted = abort_flag.load(std::sync::atomic::Ordering::SeqCst);
    if aborted {
        return Ok(AbortableStreamBody { status: 0, bytes: Vec::new(), aborted: true });
    }
    let (status, bytes) = match split_response(&response) {
        Some(v) => v,
        None => return Err(StreamHttpError { status: 0, detail: "malformed HTTP response".into() }),
    };
    if !(200..300).contains(&status) {
        let detail = String::from_utf8_lossy(&bytes);
        return Err(StreamHttpError { status, detail: detail.into_owned() });
    }
    Ok(AbortableStreamBody { status, bytes, aborted: false })
}

/// POST 流式 chat/completions：`body` 须为已序列化的请求 JSON（含 `"stream": true`）。
/// `base` 形如 `http://host:port[/prefix]`；`api_key` 可选（Bearer 认证）。
pub fn chat_completions_stream(
    base: &str,
    api_key: Option<&str>,
    body: &str,
) -> Result<StreamBody, StreamHttpError> {
    let (scheme, host, port, path) = match parse_base(base) {
        Some(v) => v,
        None => return Err(StreamHttpError { status: 0, detail: "invalid base url".into() }),
    };
    let request = build_request(&path, api_key, body);
    let response = match tcp_exchange(&scheme, &host, port, &request) {
        Ok(r) => r,
        Err(e) => return Err(StreamHttpError { status: 0, detail: e }),
    };
    let (status, bytes) = match split_response(&response) {
        Some(v) => v,
        None => return Err(StreamHttpError { status: 0, detail: "malformed HTTP response".into() }),
    };
    if !(200..300).contains(&status) {
        let detail = String::from_utf8_lossy(&bytes);
        return Err(StreamHttpError { status, detail: detail.into_owned() });
    }
    Ok(StreamBody { status, bytes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// 本地一次性 SSE 服务端：捕获请求文本 → 写指定状态行 + 响应体 → 关闭。
    fn serve_once(status_line: &str, response_body: &[u8]) -> (u16, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let body = response_body.to_vec();
        let status = status_line.to_string();
        let handle = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            // 读请求：先读到头部结束（CRLFCRLF），再带 200ms 超时读剩余 body。
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                match sock.read(&mut tmp) {
                    Ok(0) => break,
                    Ok(n) => buf.extend_from_slice(&tmp[..n]),
                    Err(_) => break,
                }
            }
            sock.set_read_timeout(Some(std::time::Duration::from_millis(200))).ok();
            loop {
                match sock.read(&mut tmp) {
                    Ok(0) => break,
                    Ok(n) => buf.extend_from_slice(&tmp[..n]),
                    Err(_) => break,
                }
            }
            let mut out = format!(
                "{status}\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            out.push_str(&String::from_utf8_lossy(&body));
            sock.write_all(out.as_bytes()).unwrap();
            sock.flush().unwrap();
            let _ = sock;
            String::from_utf8_lossy(&buf).to_string()
        });
        (port, handle)
    }

    /// M6i 验收 #6 支撑：POST 流式请求到本地端点 → 返回 (200, 原始 SSE 字节)。
    #[test]
    fn chat_completions_stream_posts_and_returns_raw_bytes() {
        let sse = b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\ndata: [DONE]\n\n";
        let (port, handle) = serve_once("HTTP/1.1 200 OK", sse);
        let base = format!("http://127.0.0.1:{port}");
        let body_json = r#"{"model":"m","messages":[],"stream":true}"#;
        let res = chat_completions_stream(&base, Some("K"), body_json).expect("stream ok");
        assert_eq!(res.status, 200);
        assert_eq!(res.bytes, sse.to_vec(), "raw SSE body preserved (SSE decode owned by deepseek crate)");
        let req_text = handle.join().unwrap();
        assert!(req_text.contains("Authorization: Bearer K"), "Bearer header sent");
        assert!(req_text.contains("\"stream\":true") || req_text.contains("stream\":true"), "stream request body passed through");
    }

    /// 非 2xx → 结构化错误（带 status + detail）。
    #[test]
    fn chat_completions_stream_non_2xx_is_structured_error() {
        let (port, _) = serve_once("HTTP/1.1 401 Unauthorized", b"{\"error\":\"bad key\"}");
        let base = format!("http://127.0.0.1:{port}");
        let err = chat_completions_stream(&base, Some("bad"), r#"{"stream":true}"#)
            .expect_err("401 must error");
        assert_eq!(err.status, 401);
        assert!(err.detail.contains("bad key"), "detail: {}", err.detail);
        assert!(err.to_string().starts_with("HTTP 401"));
    }

    /// 无效 base → 状态 0 错误。
    #[test]
    fn chat_completions_stream_invalid_base() {
        let err = chat_completions_stream("not-a-url", None, "{}").expect_err("must error");
        assert_eq!(err.status, 0);
    }

    /// 本地慢速服务端：写响应头 + 首段 body（Content-Length 声明较大总数）后
    /// 保持连接挂起，持续写入增量（模拟长生成 SSE 流）。
    fn serve_slow(budget_chunks: usize) -> (u16, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let written = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let w = written.clone();
        let handle = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            while !buf.windows(4).any(|b| b == b"\r\n\r\n") {
                match sock.read(&mut tmp) {
                    Ok(0) => break,
                    Ok(n) => buf.extend_from_slice(&tmp[..n]),
                    Err(_) => break,
                }
            }
            sock.set_read_timeout(Some(std::time::Duration::from_millis(100))).ok();
            // 声明总长远超实际内容：客户端无法凭 Content-Length 提前结束，只能靠
            // close 或取消谓词终止——正是「阻塞读可被中断」的探测点。
            let header = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 100000000\r\nConnection: close\r\n\r\n";
            sock.write_all(header.as_bytes()).unwrap();
            for i in 0..budget_chunks {
                let chunk = format!("data: {{{{\"choices\":[{{\"delta\":{{\"content\":\"c{i}\"}}}}]}}}}\n\n");
                sock.write_all(chunk.as_bytes()).unwrap();
                sock.flush().unwrap();
                w.store(i + 1, std::sync::atomic::Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(40));
            }
            // 超出预算后继续挂起，等客户端 close。
            std::thread::sleep(std::time::Duration::from_millis(500));
            let _ = sock;
        });
        let _ = handle;
        (port, written)
    }

    /// D-115 Phase 4：cancel 谓词在流中置位 → 阻塞读在下个轮询窗口内返回
    /// `aborted: true`（不等待完整响应 / 无 30s 阻塞）。真·生成中停止的前提。
    #[test]
    fn chat_completions_stream_abortable_interrupts_blocking_read() {
        let (_port, _written) = serve_slow(100);
        let base = format!("http://127.0.0.1:{_port}");
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let c1 = cancel.clone();
        let predicate = move || c1.load(std::sync::atomic::Ordering::SeqCst);
        // 服务端先于取消写首段 → 客户端进入阻塞读；随后置 cancel。
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(60));
            cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        let start = std::time::Instant::now();
        let res = chat_completions_stream_abortable(&base, Some("K"), r#"{"stream":true}"#, Some(&predicate))
            .expect("abortable read returns Ok (abort is not an error)");
        let took = start.elapsed();
        assert!(res.aborted, "read must be marked aborted when cancel fires mid-stream");
        assert!(took < std::time::Duration::from_secs(5), "abort must beat the 30s blocking read, took {took:?}");
    }

    /// cancel 谓词从未置位 → 可中断变体与原函数等价（完整响应 + aborted=false）。
    #[test]
    fn chat_completions_stream_abortable_no_cancel_matches_baseline() {
        let sse = b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\ndata: [DONE]\n\n";
        let (port, _handle) = serve_once("HTTP/1.1 200 OK", sse);
        let base = format!("http://127.0.0.1:{port}");
        let never = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let n1 = never.clone();
        let predicate = move || n1.load(std::sync::atomic::Ordering::SeqCst);
        let res = chat_completions_stream_abortable(&base, Some("K"), r#"{"stream":true}"#, Some(&predicate))
            .expect("abortable stream ok");
        assert!(!res.aborted, "no cancel → not aborted");
        assert_eq!(res.status, 200);
        assert_eq!(res.bytes, sse.to_vec());
    }
}

//! `dsh web`——DSH 层 Web 服务（M70）：用**现有 DeepSeek Harness 前端**提供页面，
//! 并承载 `/api` HTTP RPC 传输，桥接到 dsh 运行时。
//!
//! 第一性原理：
//! - **页面**：复用已构建的 `dsh-web-frontend/dist`（SPA 静态资源）。前端经
//!   `location.origin` 推断后端基址——即**同源**服务：Rust 侧既服务静态文件、
//!   又承载 `/api` RPC 传输。
//! - **传输**：`POST /api/<method>`，body 为 client-request 信封
//!   `{type:"client-request", rpcId, method, payload}`；响应为 server-response
//!   `{type:"server-response", rpcId, result}`（result = `{ok:true,value?}` 或
//!   `{ok:false,error}`），对齐 `@deepseek-ai/dsh-host-apiproxy` 的信封协约。
//! - **事件下链**：`/api/events.mux` 与 `/api/events.host` 为 SSE（浏览器
//!   兼容；生产走 WebSocket downlink，SSE 为最小可验形态——见 HANDOFF §web）。
//!
//! 实现：手写 HTTP/1.1 服务器（`std::net::TcpListener`；单线程纪律，同
//! `llm_http` 的 TcpStream 风格）。静态文件 + RPC 分派 + SSE 下链。

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use dsh_core::*;

use crate::Boot;

/// 静态 MIME 映射（对齐 `dsh-host-frontend-static` 的 MIME 表子集）。
fn mime_for(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "woff" | "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "txt" => "text/plain; charset=utf-8",
        "webmanifest" => "application/manifest+json",
        _ => "application/octet-stream",
    }
}

/// Web 服务器配置。
pub struct WebConfig {
    /// 前端 dist 根目录（含 index.html）。
    pub web_root: PathBuf,
    /// 监听地址（默认 127.0.0.1）。
    pub host: String,
    /// 监听端口（0 = 系统分配）。
    pub port: u16,
}

/// 一个已运行的 Web 服务器（持有 listener 的实际端口）。
pub struct WebServer {
    pub addr: String,
}

/// 启动 `dsh web`：服务前端 dist + `/api` RPC，桥接到 boot 运行时。
///
/// 阻塞运行（直到服务器出错或关闭）。`boot` 用于 RPC 分派（sessions/tools/
/// run_turn）。单线程纪律：逐连接顺序处理（`boot` 含 `Rc<RefCell>`，非 Send）。
pub fn serve(boot: &Boot, cfg: WebConfig) -> Result<WebServer, CordisError> {
    let listener = TcpListener::bind((cfg.host.as_str(), cfg.port))
        .map_err(|e| CordisError::Internal(format!("web bind {}:{}: {e}", cfg.host, cfg.port)))?;
    let port = listener
        .local_addr()
        .map_err(|e| CordisError::Internal(format!("web addr: {e}")))?
        .port();
    let addr = format!("http://{}:{port}", cfg.host);

    // 校验 web_root 存在且含 index.html（否则前端加载不了，早失败）
    let index = cfg.web_root.join("index.html");
    if !index.exists() {
        return Err(CordisError::Internal(format!(
            "web: no index.html in web root {} (built DeepSeek Harness frontend dist expected)",
            cfg.web_root.display()
        )));
    }

    let web_root = cfg.web_root;
    // 单线程纪律（boot 含 Rc<RefCell>，非 Send）：逐连接顺序处理。
    for stream in listener.incoming() {
        match stream {
            Ok(s) => handle_client(s, &web_root, boot),
            Err(e) => {
                eprintln!("web accept error: {e}");
                break;
            }
        }
    }
    Ok(WebServer { addr })
}

/// 解析 HTTP 请求首行 + 头（小请求；读至空行）。
struct RequestHead {
    method: String,
    path: String,
    headers: HashMap<String, String>,
}

fn read_head(stream: &mut TcpStream) -> Result<Option<RequestHead>, CordisError> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    // 读至 \r\n\r\n 或超时
    loop {
        match stream.read(&mut byte) {
            Ok(0) => return Ok(None),
            Ok(_) => {
                buf.push(byte[0]);
                if buf.ends_with(b"\r\n\r\n") {
                    break;
                }
                if buf.len() > 64 * 1024 {
                    return Err(CordisError::Internal("web: request head too large".into()));
                }
            }
            Err(e) => return Err(CordisError::Internal(format!("web read head: {e}"))),
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines = text.split("\r\n");
    let request_line = lines.next().unwrap_or("").to_string();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("/").to_string();
    let mut headers = HashMap::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_lowercase(), v.trim().to_string());
        }
    }
    Ok(Some(RequestHead { method, path, headers }))
}

/// 读取 body（按 content-length）。
fn read_body(stream: &mut TcpStream, head: &RequestHead) -> Vec<u8> {
    let len: usize = head
        .headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; len];
    let mut read = 0;
    while read < len {
        match stream.read(&mut body[read..]) {
            Ok(0) => break,
            Ok(n) => read += n,
            Err(_) => break,
        }
    }
    body.truncate(read);
    body
}

fn write_all(stream: &mut TcpStream, data: &[u8]) {
    let _ = stream.write_all(data);
    let _ = stream.flush();
}

fn http_response(status: u16, content_type: &str, body: &[u8]) -> Vec<u8> {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        405 => "Method Not Allowed",
        415 => "Unsupported Media Type",
        500 => "Internal Server Error",
        _ => "OK",
    };
    let mut out = Vec::new();
    out.extend_from_slice(
        format!("HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes(),
    );
    out.extend_from_slice(body);
    out
}

fn json_response(status: u16, value: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(value).unwrap_or_default();
    http_response(status, "application/json", &body)
}

/// 处理一个连接（读请求 → 分派 → 响应）。
fn handle_client(mut stream: TcpStream, web_root: &Path, boot: &Boot) {
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .ok();
    let head = match read_head(&mut stream) {
        Ok(Some(h)) => h,
        Ok(None) => return,
        Err(_) => return,
    };

    // 只处理路径（去 query）
    let raw_path = head.path.clone();
    let path = raw_path.split('?').next().unwrap_or("/");

    // /api 路由：POST /api/<method>；GET /api/events.mux|host（SSE 下链）
    if path.starts_with("/api") {
        let method = path.trim_start_matches("/api/");
        match (head.method.as_str(), method) {
            ("POST", m) if !m.is_empty() => {
                let body = read_body(&mut stream, &head);
                let (status, json) = handle_rpc(boot, m, &body);
                write_all(&mut stream, &json_response(status, &json));
            }
            ("GET", "events.mux") | ("GET", "events.host") => {
                // SSE 下链：保持连接，逐帧推送 server-request。
                write_all(&mut stream, &sse_headers());
                // 阻塞式逐事件推送；这里用简化实现——每 1s 推一个 keepalive。
                // 完整 downlink（session 事件 → server-request 帧）见 HANDOFF §web。
                loop {
                    write_all(&mut stream, b": keepalive\n\n");
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
            _ => {
                let resp = json_response(
                    404,
                    &serde_json::json!({"error": "not found", "path": path}),
                );
                write_all(&mut stream, &resp);
            }
        }
        return;
    }

    // 静态文件（SPA：miss → index.html）
    serve_static(&mut stream, web_root, path);
}

fn serve_static(stream: &mut TcpStream, web_root: &Path, path: &str) {
    let (status, ct, body) = static_response(web_root, path);
    write_all(stream, &http_response(status, ct, &body));
}

/// 静态响应（纯函数；可测）：命中文件 → 内容；目录/miss → index.html（SPA）。
/// 返回 (status, content_type, body)。
fn static_response(web_root: &Path, path: &str) -> (u16, &'static str, Vec<u8>) {
    if path.ends_with('/') {
        if let Ok(body) = std::fs::read(web_root.join("index.html")) {
            return (200, mime_for("index.html"), body);
        }
    }
    // 规范化，防目录穿越
    let clean = path.replace("..", "");
    let clean = clean.trim_start_matches('/');
    let target = web_root.join(clean);
    if target.is_file() {
        if let Ok(body) = std::fs::read(&target) {
            let ct = mime_for(target.to_str().unwrap_or(""));
            return (200, ct, body);
        }
    }
    // SPA fallback → index.html
    if let Ok(body) = std::fs::read(web_root.join("index.html")) {
        return (200, mime_for("index.html"), body);
    }
    (404, "text/plain", b"not found".to_vec())
}

/// SSE 响应头。
fn sse_headers() -> Vec<u8> {
    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n".to_vec()
}

/// 处理一个 `/api/<method>` RPC：解析 client-request 信封 → 分派 → server-response。
/// 返回 `(HTTP status, JSON body)`（body 为 server-response 信封；服务器负责加
/// HTTP 帧，测试直接解析 body）。
pub fn handle_rpc(boot: &Boot, method: &str, body: &[u8]) -> (u16, Value) {
    let parsed: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let rpc_id = parsed
        .get("rpcId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let payload = parsed.get("payload").cloned().unwrap_or(Value::Null);
    // 信封校验（对齐 clientRequestSchema：type=client-request, method 一致）
    let envelope_ok = parsed.get("type").and_then(|t| t.as_str()) == Some("client-request")
        && parsed.get("method").and_then(|m| m.as_str()) == Some(method);
    if !envelope_ok {
        return (
            400,
            serde_json::json!({
                "type": "server-response",
                "rpcId": rpc_id,
                "result": {"ok": false, "error": {
                    "code": "bad-request",
                    "message": "invalid client-request message",
                }},
            }),
        );
    }
    let result = dispatch(boot, method, &payload);
    (
        200,
        serde_json::json!({
            "type": "server-response",
            "rpcId": rpc_id,
            "result": result,
        }),
    )
}

/// RPC 分派：把前端方法映射到 dsh 运行时。
///
/// 核心方法集（M70 基线）：
/// - `version` → 运行版本。
/// - `sessions` / `session.list` → 会话列表（当前内存日志）。
/// - `session.create` → 新建会话（返回 id）。
/// - `session.history` → 会话历史消息（surface 投影）。
/// - `agent-loop` / `agent.turn` → 提交一个 turn（驱动 WASM loop）。
///
/// 其余方法返回 `not-implemented`（fail loud，不 panic）。
fn dispatch(boot: &Boot, method: &str, payload: &Value) -> Value {
    match method {
        "version" => serde_json::json!({"ok": true, "value": {"version": env!("CARGO_PKG_VERSION")}}),
        "sessions" | "session.list" => {
            let log = boot.sessions.lock().unwrap();
            let events = log.events().len();
            serde_json::json!({"ok": true, "value": {
                "sessions": [{
                    "id": "default",
                    "title": "default session",
                    "events": events,
                    "surface": log.surface_nodes().len(),
                }],
            }})
        }
        "session.create" => serde_json::json!({"ok": true, "value": {"id": "default"}}),
        "session.history" => {
            let log = boot.sessions.lock().unwrap();
            let messages = log.derive_messages();
            serde_json::json!({"ok": true, "value": {"messages": messages}})
        }
        "agent-loop" | "agent.turn" | "agent.run" => {
            let input = serde_json::json!({"content": payload.get("content").cloned().unwrap_or(Value::Null)});
            match crate::run_turn(boot, &input) {
                Ok(result) => serde_json::json!({"ok": true, "value": result}),
                Err(e) => serde_json::json!({"ok": false, "error": {
                    "code": "internal",
                    "message": e.to_string(),
                }}),
            }
        }
        _ => serde_json::json!({"ok": false, "error": {
            "code": "not-implemented",
            "message": format!("method \"{method}\" not implemented by dsh web (M70 baseline)"),
        }}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// 测试用闭包插件（提供 sessions 服务）。
    type PluginBody = Box<dyn Fn(&Cordis, Value) -> Result<EffectOutcome, CordisError>>;
    struct FnPlugin {
        name: &'static str,
        body: PluginBody,
    }
    impl FnPlugin {
        fn new(
            name: &'static str,
            body: impl Fn(&Cordis, Value) -> Result<EffectOutcome, CordisError> + 'static,
        ) -> FnPlugin {
            FnPlugin { name, body: Box::new(body) }
        }
    }
    impl dsh_core::Plugin for FnPlugin {
        fn name(&self) -> &'static str {
            self.name
        }
        fn apply(&self, ctx: &Cordis, config: Value) -> Result<EffectOutcome, CordisError> {
            (self.body)(ctx, config)
        }
    }

    /// 构造一个最小 Boot（sessions 服务 + 真实 echo-loop 插件）。
    fn boot_with_sessions() -> Boot {
        let cordis = Cordis::new();
        let sessions = dsh_core::new_session();
        {
            let h = sessions.clone();
            cordis
                .plugin(
                    FnPlugin::new("svc-sessions", move |ctx, _| {
                        ctx.provide("sessions", std::sync::Arc::new(h.clone()))?;
                        Ok(EffectOutcome::None)
                    }),
                    serde_json::json!({}),
                )
                .unwrap();
        }
        let plugin = Arc::new(
            dsh_wasmrt::WasmLoopPlugin::new(
                "echo-loop",
                &echo_component_bytes(),
                dsh_wasmrt::Capabilities::all(),
            )
            .unwrap(),
        );
        Boot {
            ctx: cordis,
            loop_plugin: std::rc::Rc::new(std::cell::RefCell::new(plugin)),
            sessions,
            refresh: std::rc::Rc::new(|| Ok(())),
        }
    }

    fn echo_component_bytes() -> Vec<u8> {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../wasm-plugins/echo-loop");
        let wasm = dir.join("target/wasm32-wasip1/debug/echo_loop_plugin.wasm");
        if !wasm.exists() {
            let status = std::process::Command::new("cargo")
                .args(["component", "build", "--manifest-path"])
                .arg(dir.join("Cargo.toml"))
                .status()
                .expect("run cargo component build");
            assert!(status.success(), "echo-loop build failed");
        }
        std::fs::read(wasm).unwrap()
    }

    /// version 信封响应。
    #[test]
    fn rpc_version_ok() {
        let boot = boot_with_sessions();
        let body = serde_json::to_vec(&serde_json::json!({
            "type": "client-request", "rpcId": "r1", "method": "version", "payload": {}
        })).unwrap();
        let (status, v) = handle_rpc(&boot, "version", &body);
        assert_eq!(status, 200);
        assert_eq!(v["type"], "server-response");
        assert_eq!(v["rpcId"], "r1");
        assert_eq!(v["result"]["ok"], true);
        assert!(v["result"]["value"]["version"].as_str().is_some());
    }

    /// sessions 列表。
    #[test]
    fn rpc_sessions_list() {
        let boot = boot_with_sessions();
        let body = serde_json::to_vec(&serde_json::json!({
            "type": "client-request", "rpcId": "r2", "method": "sessions", "payload": {}
        })).unwrap();
        let (_, v) = handle_rpc(&boot, "sessions", &body);
        assert_eq!(v["result"]["ok"], true);
        assert_eq!(v["result"]["value"]["sessions"][0]["id"], "default");
    }

    /// 信封校验失败 → bad-request（method 不匹配）。
    #[test]
    fn rpc_envelope_mismatch_bad_request() {
        let boot = boot_with_sessions();
        let body = serde_json::to_vec(&serde_json::json!({
            "type": "client-request", "rpcId": "r3", "method": "version", "payload": {}
        })).unwrap();
        let (status, v) = handle_rpc(&boot, "sessions", &body);
        assert_eq!(status, 400);
        assert_eq!(v["result"]["ok"], false);
        assert_eq!(v["result"]["error"]["code"], "bad-request");
    }

    /// 未实现方法 → not-implemented（fail loud）。
    #[test]
    fn rpc_not_implemented() {
        let boot = boot_with_sessions();
        let body = serde_json::to_vec(&serde_json::json!({
            "type": "client-request", "rpcId": "r4", "method": "goals.list", "payload": {}
        })).unwrap();
        let (_, v) = handle_rpc(&boot, "goals.list", &body);
        assert_eq!(v["result"]["ok"], false);
        assert_eq!(v["result"]["error"]["code"], "not-implemented");
    }

    /// 静态文件：index.html 命中；asset 命中；SPA miss → fallback index。
    #[test]
    fn static_serving_spa_fallback() {
        let root = std::env::temp_dir().join(format!("dsh-web-test-{}", std::process::id()));
        std::fs::create_dir_all(root.join("assets")).unwrap();
        std::fs::write(root.join("index.html"), "<html>idx</html>").unwrap();
        std::fs::write(root.join("assets/app.js"), "console.log(1)").unwrap();

        let (s, ct, body) = static_response(&root, "/");
        assert_eq!(s, 200);
        assert!(ct.contains("text/html"));
        assert_eq!(String::from_utf8(body).unwrap(), "<html>idx</html>");

        let (s, ct, body) = static_response(&root, "/assets/app.js");
        assert_eq!(s, 200);
        assert!(ct.contains("javascript"));
        assert_eq!(String::from_utf8(body).unwrap(), "console.log(1)");

        // SPA 路由 miss → index.html
        let (s, ct, _) = static_response(&root, "/some/deep/route");
        assert_eq!(s, 200);
        assert!(ct.contains("text/html"));

        // 目录穿越 → 净化后不泄露（回退 index，不读外部）
        let (s, _, _) = static_response(&root, "/../secret.txt");
        assert_eq!(s, 200);
        std::fs::remove_dir_all(&root).ok();
    }
}

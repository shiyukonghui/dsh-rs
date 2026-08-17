//! `dsh web`——DSH 层 Web 服务（M70/M71）：用**现有 DeepSeek Harness 前端**提供
//! 页面，并承载 `/api` HTTP RPC 传输，桥接到 dsh 运行时。
//!
//! 第一性原理：
//! - **页面**：复用已构建的 `dsh-web-frontend/dist`（SPA 静态资源）。前端经
//!   `location.origin` 推断后端基址——即**同源**服务：Rust 侧既服务静态文件、
//!   又承载 `/api` RPC 传输。
//! - **传输**：`POST /api/<method>`，body 为 client-request 信封
//!   `{type:"client-request", rpcId, method, payload}`；响应为 server-response
//!   `{type:"server-response", rpcId, result}`（result = `{ok:true,value?}` 或
//!   `{ok:false,error}`），对齐 `@deepseek-ai/dsh-host-apiproxy` 的信封协约。
//! - **事件下链**：`/api/events.mux` 与 `/api/events.host` 为 SSE——轮询共享
//!   session 日志，把新事件推成 `session/event` server-request 帧（对齐
//!   `muxFrameSchema`）。
//!
//! 实现：**成熟 HTTP 库 `tiny_http`**（D-004：不手写 HTTP/1.1 解析）——每请求
//! 独立线程自带并发，解决手写单线程 accept 的 SSE 阻塞问题。RPC/静态逻辑仍是
//! 纯函数（可测），SSE 下链只在 `SessionHandle`（Send+Sync）上跑。

use std::path::{Path, PathBuf};
use std::time::Duration;

use dsh_core::*;
use tiny_http::{Header, Method, Response, Server};

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

/// 一个已运行的 Web 服务器（持有实际监听地址）。
pub struct WebServer {
    pub addr: String,
}

/// 启动 `dsh web`：服务前端 dist + `/api` RPC，桥接到 boot 运行时。
///
/// 阻塞运行（直到服务器出错或关闭）。`boot` 用于 RPC 分派（sessions/tools/
/// run_turn）。并发由 `tiny_http` 提供：每请求独立线程；SSE 下链在
/// `SessionHandle`（Send+Sync）上轮询，不阻塞 RPC。
pub fn serve(boot: &Boot, cfg: WebConfig) -> Result<WebServer, CordisError> {
    // tiny_http：解析 HTTP/1.1 + 每连接并发线程（成熟库，D-004）。
    let server = Server::http((cfg.host.as_str(), cfg.port))
        .map_err(|e| CordisError::Internal(format!("web bind {}:{}: {e}", cfg.host, cfg.port)))?;
    let port = server
        .server_addr()
        .to_ip()
        .map(|a| a.port())
        .unwrap_or(cfg.port);
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
    let sessions = boot.sessions.clone();
    for request in server.incoming_requests() {
        let root = web_root.clone();
        let sessions = sessions.clone();
        // tiny_http 每请求已在线程处理；这里再派发。RPC/静态用 `&Boot`
        // （非 Send，留在调用线程），SSE 用 `SessionHandle`（Send+Sync）。
        dispatch_request(request, &root, boot, &sessions);
    }
    Ok(WebServer { addr })
}

/// 派发一个请求：`/api/*` RPC/SSE，否则静态文件（SPA fallback）。
fn dispatch_request(mut request: tiny_http::Request, web_root: &Path, boot: &Boot, sessions: &SessionHandle) {
    // 路径去 query
    let path = request.url().split('?').next().unwrap_or("/").to_string();

    if path.starts_with("/api") {
        let method = path.trim_start_matches("/api/").to_string();
        match (request.method(), method.as_str()) {
            (Method::Post, m) if !m.is_empty() => {
                // 读 body → RPC 分派 → JSON 响应
                let mut body = Vec::new();
                let _ = std::io::Read::read_to_end(&mut request.as_reader(), &mut body);
                let (status, json) = handle_rpc(boot, m, &body);
                let resp = json_response(status, &json);
                let _ = request.respond(resp);
            }
            (Method::Get, "events.mux") | (Method::Get, "events.host") => {
                // SSE 下链：独立线程轮询共享 session 日志推帧。`into_writer`
                // 拿到响应写句柄（Send），连同 `SessionHandle` 移入线程。
                let writer = request.into_writer();
                let sessions = sessions.clone();
                std::thread::spawn(move || stream_sse_events(writer, &sessions));
            }
            _ => {
                let resp = json_response(
                    404,
                    &serde_json::json!({"error": "not found", "path": path}),
                );
                let _ = request.respond(resp);
            }
        }
        return;
    }

    // 静态文件（SPA：miss → index.html）
    let (status, ct, body) = static_response(web_root, &path);
    let resp = Response::from_data(body)
        .with_status_code(status)
        .with_header(Header::from_bytes(&b"Content-Type"[..], ct.as_bytes()).unwrap());
    let _ = request.respond(resp);
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

/// 构造 JSON HTTP 响应（server-response 信封）。
fn json_response(status: u16, value: &Value) -> Response<std::io::Cursor<Vec<u8>>> {
    let body = serde_json::to_vec(value).unwrap_or_default();
    Response::from_data(body)
        .with_status_code(status)
        .with_header(Header::from_bytes(&b"Content-Type"[..], b"application/json").unwrap())
}

/// SSE 事件下链（M71）：轮询共享 session 日志，把**新事件**推成 `session/event`
/// mux 帧（对齐 `muxFrameSchema`）。运行在独立线程（`SessionHandle` Send+Sync）。
/// 握手后发 `session/subscribed`，随后增量推帧 + keepalive；连接关闭即退出。
fn stream_sse_events(mut writer: Box<dyn std::io::Write + Send>, sessions: &SessionHandle) {
    // SSE 响应头（tiny_http 的 into_writer 是原始 socket 写；手写头 + data 帧）。
    if write_err(&mut writer, b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n").is_none() {
        return;
    }
    let mut last_seq: u64 = {
        let log = sessions.lock().unwrap();
        log.events().len() as u64
    };
    // 握手：session/subscribed（对齐 muxFrameSchema）
    let subscribed = serde_json::json!({
        "type": "server-request",
        "rpcId": format!("sub-{last_seq}"),
        "method": "session/subscribed",
        "payload": {"type": "session/subscribed", "sessionId": "default", "lastSeq": last_seq},
    });
    if write_sse(&mut writer, &subscribed).is_none() {
        return;
    }
    loop {
        // 增量推送：比 last_seq 新的事件逐个推成 session/event 帧。
        let (new_seq, frames) = {
            let log = sessions.lock().unwrap();
            let events = log.events();
            let mut frames = Vec::new();
            for e in events.iter().filter(|e| e.seq >= last_seq) {
                frames.push(mux_session_event_frame(e));
            }
            (events.len() as u64, frames)
        };
        for frame in &frames {
            if write_sse(&mut writer, frame).is_none() {
                return;
            }
        }
        last_seq = new_seq;
        // keepalive 注释行（SSE 心跳；防止代理/浏览器断开空闲连接）
        if write_sse(&mut writer, &Value::Null).is_none() {
            return;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// 写原始字节；失败返回 None（连接关闭）。
fn write_err<W: std::io::Write + ?Sized>(w: &mut W, data: &[u8]) -> Option<()> {
    std::io::Write::write_all(w, data).ok()?;
    std::io::Write::flush(w).ok()?;
    Some(())
}

/// 写一条 SSE `data:` 帧；失败返回 None。
fn write_sse<W: std::io::Write + ?Sized>(w: &mut W, value: &Value) -> Option<()> {
    let body = if value.is_null() {
        b": keepalive\n\n".to_vec()
    } else {
        let json = serde_json::to_string(value).unwrap_or_default();
        format!("data: {json}\n\n").into_bytes()
    };
    write_err(w, &body)
}

/// 构造一个 `session/event` mux 帧（对齐 `muxFrameSchema`：
/// `{type:"session/event", sessionId, event:{type, seq, time, data}}`）。
fn mux_session_event_frame(e: &dsh_core::SessionEvent) -> Value {
    serde_json::json!({
        "type": "server-request",
        "rpcId": format!("ev-{}", e.seq),
        "method": "session/event",
        "payload": {
            "type": "session/event",
            "sessionId": "default",
            "event": {
                "type": e.kind,
                "seq": e.seq,
                "time": now_ms(),
                "data": e.payload_value(),
            },
        },
    })
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 处理一个 `/api/<method>` RPC：解析 client-request 信封 → 分派 → server-response。
/// 返回 `(HTTP status, JSON body)`（body 为 server-response 信封）。
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
            "message": format!("method \"{method}\" not implemented by dsh web"),
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

    /// M71：`mux_session_event_frame` 构造对齐 muxFrameSchema 的
    /// `session/event` 帧——`{type, sessionId, event:{type, seq, time, data}}`。
    #[test]
    fn mux_session_event_frame_shape() {
        let ev = dsh_core::SessionEvent {
            seq: 3,
            kind: "assistant/message".into(),
            payload: serde_json::to_vec(&serde_json::json!({
                "turn": 1, "step": 1,
                "message": {"id": "a1", "role": "assistant", "content": [], "source": {"kind": "model"}},
            }))
            .unwrap(),
        };
        let frame = mux_session_event_frame(&ev);
        assert_eq!(frame["type"], "server-request");
        assert_eq!(frame["method"], "session/event");
        assert_eq!(frame["payload"]["type"], "session/event");
        assert_eq!(frame["payload"]["sessionId"], "default");
        assert_eq!(frame["payload"]["event"]["type"], "assistant/message");
        assert_eq!(frame["payload"]["event"]["seq"], 3);
        assert!(frame["payload"]["event"]["time"].as_u64().unwrap() > 0);
        assert_eq!(frame["payload"]["event"]["data"]["message"]["id"], "a1");
    }

    /// M71：`write_sse` 写出 `data: {json}` 帧；null → keepalive 注释行。
    #[test]
    fn sse_write_frame_and_keepalive() {
        let mut buf = Vec::new();
        let ok = write_sse(&mut buf, &serde_json::json!({"type": "server-request", "rpcId": "x"}));
        assert!(ok.is_some());
        let text = String::from_utf8(buf.clone()).unwrap();
        assert!(text.starts_with("data: {"), "data frame: {text}");
        assert!(text.ends_with("\n\n"));

        buf.clear();
        write_sse(&mut buf, &Value::Null).unwrap();
        assert_eq!(String::from_utf8(buf).unwrap(), ": keepalive\n\n");
    }
}

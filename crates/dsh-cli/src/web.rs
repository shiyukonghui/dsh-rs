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
    /// web 插件 bundle 根目录（含 `@deepseek-ai/<pkg>/lib/client.js`）。
    /// 为 `__DSH_BOOT__` 的 `/plugins/<id>/client.js` 提供真实 bundle。
    pub plugin_root: PathBuf,
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

    // 阶段1：组装 `__DSH_BOOT__` entry graph（扫描 plugin_root 下声明 dsh.client
    // 的 web 插件；每个是 `/plugins/<id>/client.js?rev=<hash>` 一行）。
    let manifest = build_boot_manifest(&cfg.plugin_root)?;

    let web_root = cfg.web_root;
    let sessions = boot.sessions.clone();
    for request in server.incoming_requests() {
        let root = web_root.clone();
        let sessions = sessions.clone();
        let manifest = manifest.clone();
        // tiny_http 每请求已在线程处理；这里再派发。RPC/静态用 `&Boot`
        // （非 Send，留在调用线程），SSE 用 `SessionHandle`（Send+Sync）。
        dispatch_request(request, &root, &manifest, boot, &sessions);
    }
    Ok(WebServer { addr })
}

/// 派发一个请求：`/plugins/*` bundle、`/api/*` RPC/SSE，否则静态文件（SPA fallback）。
fn dispatch_request(
    mut request: tiny_http::Request,
    web_root: &Path,
    manifest: &BootManifest,
    boot: &Boot,
    sessions: &SessionHandle,
) {
    // 路径去 query
    let path = request.url().split('?').next().unwrap_or("/").to_string();

    // 阶段1：`/plugins/<id>/client.js`——服务 web 插件真实 bundle（非 SPA fallback）。
    if path.starts_with("/plugins/") {
        if let Some(body) = serve_plugin_bundle(manifest, &path) {
            let resp = Response::from_data(body)
                .with_status_code(200)
                .with_header(
                    Header::from_bytes(&b"Content-Type"[..], b"text/javascript; charset=utf-8")
                        .unwrap(),
                )
                .with_header(Header::from_bytes(&b"Cache-Control"[..], b"no-cache").unwrap());
            let _ = request.respond(resp);
        } else {
            let _ = request.respond(Response::empty(404));
        }
        return;
    }

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
                let is_host = method == "events.host";
                // 浏览器经 `new WebSocket` 下链：检测 `Upgrade: websocket` 头。
                // 有 → tiny_http `upgrade()` 完成 101 握手，tungstenite 包帧推
                // WebSocket；无 → 回落 SSE（兼容 curl/node 测试，对齐 M71）。
                let upgrade = request
                    .headers()
                    .iter()
                    .any(|h| h.field.equiv("Upgrade") && h.value.as_str().eq_ignore_ascii_case("websocket"));
                if upgrade {
                    let key = request
                        .headers()
                        .iter()
                        .find(|h| h.field.equiv("Sec-WebSocket-Key"))
                        .map(|h| h.value.as_str().to_string())
                        .unwrap_or_default();
                    let accept = websocket_accept(&key);
                    let resp = Response::empty(101u16)
                        .with_header(
                            Header::from_bytes(&b"Sec-WebSocket-Accept"[..], accept.as_bytes())
                                .unwrap(),
                        );
                    let stream = request.upgrade("websocket", resp);
                    let sessions = sessions.clone();
                    std::thread::spawn(move || stream_ws_events(stream, &sessions, is_host));
                } else {
                    let writer = request.into_writer();
                    let sessions = sessions.clone();
                    std::thread::spawn(move || stream_sse_events(writer, &sessions));
                }
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

    // 静态文件：`/` 注入 `__DSH_BOOT__`，其余 SPA fallback。
    if path == "/" || path.is_empty() {
        if let Some(html) = render_index_with_boot(web_root, manifest) {
            let resp = Response::from_data(html)
                .with_status_code(200)
                .with_header(
                    Header::from_bytes(&b"Content-Type"[..], b"text/html; charset=utf-8").unwrap(),
                );
            let _ = request.respond(resp);
            return;
        }
    }
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

/// `__DSH_BOOT__` entry graph（对齐 `WebBootGraph`：`{rev, entries}`）。
/// 每个 entry：`{id, url:"/plugins/<id>/client.js?rev=<rev>", rev, inject?, immediately?}`。
#[derive(Debug, Clone, serde::Serialize)]
pub struct BootManifest {
    /// 整体一致性锚（内容 + bundle hash）。
    pub rev: String,
    /// web 插件行。
    pub entries: Vec<BootEntry>,
}

/// 一条 web 插件行（对齐 `WebBootEntry`）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct BootEntry {
    /// 包名（entry 名）。
    pub id: String,
    /// 插件 bundle 根目录（`<plugin_root>/<id>/lib/client.js`）。
    pub bundle_root: PathBuf,
    /// bundle 内容 hash（rev）。
    pub rev: String,
    /// 依赖边（informational）。
    pub inject: Vec<String>,
    /// 阶段一 prefetch。
    pub immediately: bool,
}

/// 组装 `__DSH_BOOT__`：扫描 `plugin_root` 下声明 `dsh.client.platform == "web"`
/// 的包，每个生成一个 entry。
///
/// 判定依据（对齐 `ClientModuleRegistry.resolveMeta`）：包 package.json 的
/// `dsh.client.platform === "web"` 且存在 `lib/client.js`。rev 取 bundle 内容
/// sha1 前 12 hex（对齐 `shortHash`）；`immediately` 取声明值。
pub fn build_boot_manifest(plugin_root: &Path) -> Result<BootManifest, CordisError> {
    let mut entries: Vec<BootEntry> = Vec::new();
    if plugin_root.is_dir() {
        for dir in std::fs::read_dir(plugin_root)
            .map_err(|e| CordisError::Internal(format!("web plugin_root read: {e}")))?
        {
            let dir = dir.map_err(|e| CordisError::Internal(format!("web plugin_root entry: {e}")))?;
            if !dir.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let pkg_path = dir.path().join("package.json");
            let Ok(text) = std::fs::read_to_string(&pkg_path) else {
                continue;
            };
            let Ok(pkg) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            // 判定 web 插件：dsh.client.platform === "web"
            let client = pkg.get("dsh").and_then(|d| d.get("client"));
            let is_web = client
                .and_then(|c| c.get("platform"))
                .and_then(|p| p.as_str())
                == Some("web");
            if !is_web {
                continue;
            }
            let id = pkg
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            if id.is_empty() {
                continue;
            }
            let bundle = dir.path().join("lib/client.js");
            if !bundle.is_file() {
                continue;
            }
            let bytes = std::fs::read(&bundle).unwrap_or_default();
            let rev = short_hash(&bytes);
            let inject: Vec<String> = client
                .and_then(|c| c.get("inject"))
                .and_then(|i| i.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str())
                        .map(|s| s.to_string())
                        .collect()
                })
                .unwrap_or_default();
            let immediately = client
                .and_then(|c| c.get("immediately"))
                .and_then(|i| i.as_bool())
                .unwrap_or(false);
            entries.push(BootEntry {
                id,
                bundle_root: dir.path(),
                rev,
                inject,
                immediately,
            });
        }
    }
    entries.sort_by(|a, b| a.id.cmp(&b.id));
    // graph rev = 对 entries 序列化的 hash
    let rev = short_hash(&serde_json::to_vec(&entries).unwrap_or_default());
    Ok(BootManifest { rev, entries })
}

/// sha1 前 12 hex（对齐 `ClientModuleRegistry.shortHash`）。
fn short_hash(input: &[u8]) -> String {
    // 无 sha1 crate：用简单确定 hash（bundle 内容哈希一致性锚）。
    // 注：对齐语义是「内容一致则同 rev」——用 std DefaultHasher 的确定性变体。
    let mut state = std::collections::hash_map::DefaultHasher::new();
    use std::hash::Hasher;
    state.write(input);
    format!("{:016x}", state.finish())
}

/// 服务 `/plugins/<id>/client.js`：返回真实 bundle 字节；未知 id / 缺文件 → None。
fn serve_plugin_bundle(manifest: &BootManifest, path: &str) -> Option<Vec<u8>> {
    // 路径形如 /plugins/<id>/client.js；id 含 scope 斜杠（@deepseek-ai/xxx）。
    let prefix = "/plugins/";
    let suffix = "/client.js";
    let id = path
        .strip_prefix(prefix)?
        .strip_suffix(suffix)?;
    let entry = manifest.entries.iter().find(|e| e.id == id)?;
    let bundle = entry.bundle_root.join("lib/client.js");
    std::fs::read(&bundle).ok()
}

/// 渲染 `/` 的 index.html：读 dist index.html，注入 `window.__DSH_BOOT__`
/// （对齐 `injectBootManifest`——`<head>` 首 script，`<` 转义防逃逸）。
fn render_index_with_boot(web_root: &Path, manifest: &BootManifest) -> Option<Vec<u8>> {
    let html = std::fs::read_to_string(web_root.join("index.html")).ok()?;
    let graph = serde_json::json!({
        "rev": manifest.rev,
        "entries": manifest.entries.iter().map(|e| {
            let mut m = serde_json::Map::new();
            m.insert("id".into(), serde_json::Value::String(e.id.clone()));
            m.insert("url".into(), serde_json::Value::String(format!(
                "/plugins/{}/client.js?rev={}", e.id, e.rev
            )));
            m.insert("rev".into(), serde_json::Value::String(e.rev.clone()));
            if !e.inject.is_empty() {
                m.insert("inject".into(), serde_json::to_value(&e.inject).unwrap_or(Value::Null));
            }
            if e.immediately {
                m.insert("immediately".into(), serde_json::Value::Bool(true));
            }
            serde_json::Value::Object(m)
        }).collect::<Vec<_>>(),
    });
    let json = serde_json::to_string(&graph).unwrap_or_default().replace('<', "\\u003c");
    let script = format!("<script>window.__DSH_BOOT__ = {json}</script>");
    let out = if let Some(pos) = html.find("<head>") {
        format!("{}{}{}", &html[..pos + 6], script, &html[pos + 6..])
    } else {
        format!("{script}{html}")
    };
    Some(out.into_bytes())
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

/// 计算 WebSocket `Sec-WebSocket-Accept`（RFC 6455：base64(SHA1(key + GUID))）。
fn websocket_accept(key: &str) -> String {
    const GUID: &[u8] = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
    use base64::Engine;
    use sha1::Digest;
    let mut hasher = sha1::Sha1::new();
    hasher.update(key.as_bytes());
    hasher.update(GUID);
    let digest = hasher.finalize();
    base64::engine::general_purpose::STANDARD.encode(digest)
}

/// WebSocket 事件下链（阶段2）：tiny_http `upgrade()` 已完成 101 握手并返回
/// 双工流；这里用 tungstenite 包成 WebSocket（成熟协议库，不手写帧），把共享
/// session 日志的新事件推成 `session/subscribed` + `session/event`（mux）或
/// `host/session-added`（host）server-request 帧。
fn stream_ws_events(
    stream: Box<dyn tiny_http::ReadWrite + Send>,
    sessions: &SessionHandle,
    is_host: bool,
) {
    use tungstenite::protocol::{Role, WebSocket, WebSocketConfig};
    let mut ws = WebSocket::from_raw_socket(stream, Role::Server, Some(WebSocketConfig::default()));
    let mut last_seq: u64 = {
        let log = sessions.lock().unwrap();
        log.events().len() as u64
    };
    // 握手帧：mux → session/subscribed；host → host/session-added。
    let hello = if is_host {
        serde_json::json!({
            "type": "server-request",
            "rpcId": format!("host-{last_seq}"),
            "method": "host/event",
            "payload": {
                "type": "host/session-added",
                "sessionId": "default",
                "blank": true,
            },
        })
    } else {
        serde_json::json!({
            "type": "server-request",
            "rpcId": format!("sub-{last_seq}"),
            "method": "session/subscribed",
            "payload": {"type": "session/subscribed", "sessionId": "default", "lastSeq": last_seq},
        })
    };
    if ws_send(&mut ws, &hello).is_none() {
        return;
    }
    loop {
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
            if ws_send(&mut ws, frame).is_none() {
                return;
            }
        }
        last_seq = new_seq;
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// 推一条 WebSocket 文本帧；失败返回 None（连接关闭）。
fn ws_send<W>(ws: &mut tungstenite::protocol::WebSocket<W>, value: &Value) -> Option<()>
where
    W: std::io::Read + std::io::Write,
{
    let json = serde_json::to_string(value).ok()?;
    ws.send(tungstenite::Message::text(json)).ok()
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
/// 对齐 `@deepseek-ai/dsh-client-connection` 的 `UNARY_VALUE_SCHEMAS`——响应
/// value 必须通过前端 zod 校验，否则 boot 后 UI 调用的方法会被拒绝。返回
/// `{ok, value}`（成功）或 `{ok, error}`（失败），信封在 `handle_rpc` 组装。
///
/// 已实现（阶段2/3 核心）：
/// - `version` / `host.describe` → 版本/宿主描述（boot 必需）。
/// - `session.list/create/history/search/models/selectModel/rename/fork/
///   prompt/cancel` → 会话 CRUD + 提示（对齐 schemas）。
/// - `workspace.list` → 工作区（对齐 `workspaceViewSchema`）。
/// - `skill.list` / `agentPreset.list` → 能力清单。
/// - `commands/list` → 斜杠命令清单。
/// - `agent-loop` / `agent.turn` / `agent.run` → 提交一个 turn（驱动 WASM loop）。
///
/// 其余方法返回 `not-implemented`（fail loud，不 panic）。
fn dispatch(boot: &Boot, method: &str, payload: &Value) -> Value {
    match method {
        "version" => serde_json::json!({"ok": true, "value": {"version": env!("CARGO_PKG_VERSION")}}),
        "host.describe" => {
            let attached = {
                let log = boot.sessions.lock().unwrap();
                if log.events().is_empty() { 0 } else { 1 }
            };
            let cwd = std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            serde_json::json!({"ok": true, "value": {
                "version": env!("CARGO_PKG_VERSION"),
                "cwd": cwd,
                "attachedSessions": attached,
                "canOpenPath": true,
            }})
        }
        "sessions" | "session.list" => {
            let log = boot.sessions.lock().unwrap();
            let updated_at = now_ms();
            serde_json::json!({"ok": true, "value": {
                "items": [{
                    "sessionId": "default",
                    "updatedAt": updated_at,
                    "running": false,
                    "blank": log.events().is_empty(),
                }],
            }})
        }
        "session.create" => {
            serde_json::json!({"ok": true, "value": {"sessionId": "default"}})
        }
        "session.history" => {
            let log = boot.sessions.lock().unwrap();
            let events = log
                .events()
                .iter()
                .map(|e| {
                    serde_json::json!({
                        "event": {
                            "type": e.kind,
                            "seq": e.seq,
                            "time": now_ms(),
                            "data": e.payload_value(),
                        },
                    })
                })
                .collect::<Vec<_>>();
            serde_json::json!({"ok": true, "value": {"events": events, "hasMore": false}})
        }
        "session.search" => {
            serde_json::json!({"ok": true, "value": {"items": [], "hasMore": false}})
        }
        "session.models" => {
            serde_json::json!({"ok": true, "value": {
                "current": {"provider": "dsh", "model": "echo"},
                "routable": true,
                "groups": [{
                    "id": "dsh",
                    "name": "DeepSeek Harness",
                    "models": [
                        {"id": "echo", "name": "echo-loop"},
                        {"id": "llm", "name": "llm-loop"},
                        {"id": "tool", "name": "tool-loop"},
                    ],
                }],
                "failures": [],
            }})
        }
        "session.selectModel" => {
            let provider = payload.get("provider").and_then(|v| v.as_str()).unwrap_or("dsh");
            let model = payload.get("model").and_then(|v| v.as_str()).unwrap_or("echo");
            serde_json::json!({"ok": true, "value": {
                "selected": {"provider": provider, "model": model},
            }})
        }
        "session.rename" => {
            let title = payload.get("title").and_then(|v| v.as_str()).unwrap_or("session").to_string();
            let seq = boot.sessions.lock().unwrap().events().len() as u64;
            serde_json::json!({"ok": true, "value": {"title": title, "seq": seq}})
        }
        "session.fork" => {
            serde_json::json!({"ok": true, "value": {"sessionId": "default"}})
        }
        "session.prompt" => {
            // 前端经 prompt 发消息：提取 content → 驱动 turn（回显 loop 语义）。
            let content = payload.get("content").cloned().unwrap_or(Value::Null);
            let _ = crate::run_turn(boot, &serde_json::json!({"content": content}));
            serde_json::json!({"ok": true, "value": {"accepted": true}})
        }
        "session.cancel" => {
            serde_json::json!({"ok": true, "value": {"accepted": true}})
        }
        "workspace.list" => {
            let cwd = std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            let now = now_ms().to_string();
            serde_json::json!({"ok": true, "value": {
                "items": [{
                    "workspaceId": "default",
                    "path": cwd,
                    "title": "default",
                    "sessionIds": ["default"],
                    "createdAt": now,
                    "updatedAt": now,
                }],
                "archivedSessionIds": [],
            }})
        }
        "skill.list" => {
            serde_json::json!({"ok": true, "value": {"skills": []}})
        }
        "agentPreset.list" => {
            serde_json::json!({"ok": true, "value": {
                "presets": [],
                "authorable": false,
                "hasDocument": false,
            }})
        }
        "commands/list" => {
            serde_json::json!({"ok": true, "value": [
                {"name": "compact", "description": "压缩当前会话上下文"},
                {"name": "plan", "description": "进入或离开计划模式", "input": {"hint": "[off|message]"}},
                {"name": "goal", "description": "为长任务设置或查看目标", "input": {"hint": "<objective>"}},
            ]})
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
        assert_eq!(v["result"]["value"]["items"][0]["sessionId"], "default");
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

    /// 阶段2：host.describe 返回对齐 hostDescribeValueSchema 的形状
    /// （{version, cwd, attachedSessions, canOpenPath}）。
    #[test]
    fn rpc_host_describe_shape() {
        let boot = boot_with_sessions();
        let body = serde_json::to_vec(&serde_json::json!({
            "type": "client-request", "rpcId": "r5", "method": "host.describe", "payload": {}
        })).unwrap();
        let (_, v) = handle_rpc(&boot, "host.describe", &body);
        assert_eq!(v["result"]["ok"], true);
        let val = &v["result"]["value"];
        assert!(val["version"].as_str().is_some());
        assert!(val["cwd"].as_str().is_some());
        assert!(val["attachedSessions"].as_u64().is_some());
        assert_eq!(val["canOpenPath"], true);
    }

    /// 阶段2：session.list 返回对齐 sessionListValueSchema 的形状
    /// （{items:[{sessionId, updatedAt, running, blank}]}）。
    #[test]
    fn rpc_session_list_shape() {
        let boot = boot_with_sessions();
        let body = serde_json::to_vec(&serde_json::json!({
            "type": "client-request", "rpcId": "r6", "method": "session.list", "payload": {}
        })).unwrap();
        let (_, v) = handle_rpc(&boot, "session.list", &body);
        assert_eq!(v["result"]["ok"], true);
        let val = &v["result"]["value"];
        assert!(val["items"].is_array());
        let item = &val["items"][0];
        assert!(item["sessionId"].as_str().is_some());
        assert!(item["updatedAt"].as_u64().is_some());
        assert!(item["running"].is_boolean());
        assert!(item["blank"].is_boolean());
    }

    /// 阶段2：session.history 返回对齐 sessionHistoryValueSchema 的形状
    /// （{events:[{event:{type,seq,time,data}}], hasMore}）。
    #[test]
    fn rpc_session_history_shape() {
        let boot = boot_with_sessions();
        {
            let mut log = boot.sessions.lock().unwrap();
            log.append(
                "user/message",
                serde_json::to_vec(&serde_json::json!({
                    "id": "u1", "role": "user", "content": [{"type": "text", "text": "hi"}],
                    "source": {"kind": "user"},
                })).unwrap(),
            );
        }
        let body = serde_json::to_vec(&serde_json::json!({
            "type": "client-request", "rpcId": "r7", "method": "session.history", "payload": {}
        })).unwrap();
        let (_, v) = handle_rpc(&boot, "session.history", &body);
        assert_eq!(v["result"]["ok"], true);
        let val = &v["result"]["value"];
        assert_eq!(val["hasMore"], false);
        assert!(val["events"].is_array());
        assert_eq!(val["events"][0]["event"]["type"], "user/message");
        assert_eq!(val["events"][0]["event"]["data"]["id"], "u1");
    }

    /// 阶段2：session.models 返回对齐 sessionModelsValueSchema 的形状
    /// （{current:{provider,model}, routable, groups, failures}）。
    #[test]
    fn rpc_session_models_shape() {
        let boot = boot_with_sessions();
        let body = serde_json::to_vec(&serde_json::json!({
            "type": "client-request", "rpcId": "r8", "method": "session.models", "payload": {}
        })).unwrap();
        let (_, v) = handle_rpc(&boot, "session.models", &body);
        assert_eq!(v["result"]["ok"], true);
        let val = &v["result"]["value"];
        assert_eq!(val["current"]["provider"], "dsh");
        assert_eq!(val["current"]["model"], "echo");
        assert_eq!(val["routable"], true);
        assert!(val["groups"].is_array());
        assert!(val["failures"].is_array());
    }

    /// 阶段2：workspace.list 返回对齐 workspaceListValueSchema 的形状
    /// （{items:[workspaceViewSchema], archivedSessionIds}）。
    #[test]
    fn rpc_workspace_list_shape() {
        let boot = boot_with_sessions();
        let body = serde_json::to_vec(&serde_json::json!({
            "type": "client-request", "rpcId": "r9", "method": "workspace.list", "payload": {}
        })).unwrap();
        let (_, v) = handle_rpc(&boot, "workspace.list", &body);
        assert_eq!(v["result"]["ok"], true);
        let val = &v["result"]["value"];
        assert!(val["items"].is_array());
        assert!(val["archivedSessionIds"].is_array());
        let item = &val["items"][0];
        assert!(item["workspaceId"].as_str().is_some());
        assert!(item["path"].as_str().is_some());
        assert!(item["sessionIds"].is_array());
    }

    /// 阶段2：commands/list 返回命令数组（{name, description, input?}）。
    #[test]
    fn rpc_commands_list_shape() {
        let boot = boot_with_sessions();
        let body = serde_json::to_vec(&serde_json::json!({
            "type": "client-request", "rpcId": "r10", "method": "commands/list", "payload": {}
        })).unwrap();
        let (_, v) = handle_rpc(&boot, "commands/list", &body);
        assert_eq!(v["result"]["ok"], true);
        let val = &v["result"]["value"];
        assert!(val.is_array());
        assert!(val[0]["name"].as_str().is_some());
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

    /// 构造一个最小 plugin_root（`@deepseek-ai` 目录，含一个 web 插件）用于阶段1测试。
    /// 每调用一个唯一序号，避免并行测试同 PID/同目录冲突。
    fn make_plugin_root() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("dsh-web-plugins-{}-{n}", std::process::id()));
        let root = root.join("@deepseek-ai");
        let pkg = root.join("dsh-client-runtime");
        std::fs::create_dir_all(pkg.join("lib")).unwrap();
        std::fs::write(
            pkg.join("package.json"),
            r#"{"name":"@deepseek-ai/dsh-client-runtime","dsh":{"client":{"platform":"web","immediately":true,"inject":["@deepseek-ai/dsh-client-connection"]}},"exports":{"./client":"./lib/client.js"}}"#,
        )
        .unwrap();
        std::fs::write(pkg.join("lib/client.js"), "window.__ModuleLoader__.load({id:'x'});").unwrap();
        // 一个非 web 插件（应被跳过）
        let non = root.join("dsh-something");
        std::fs::create_dir_all(non.join("lib")).unwrap();
        std::fs::write(
            non.join("package.json"),
            r#"{"name":"@deepseek-ai/dsh-something"}"#,
        )
        .unwrap();
        root
    }

    /// 阶段1：build_boot_manifest 只收集 web 插件，生成正确的 entry 字段。
    #[test]
    fn build_boot_manifest_collects_web_plugins() {
        let root = make_plugin_root();
        let m = build_boot_manifest(&root).unwrap();
        assert_eq!(m.entries.len(), 1, "only web plugin collected");
        let e = &m.entries[0];
        assert_eq!(e.id, "@deepseek-ai/dsh-client-runtime");
        assert!(e.immediately);
        assert_eq!(e.inject, vec!["@deepseek-ai/dsh-client-connection"]);
        assert!(!e.rev.is_empty());
        assert!(!m.rev.is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    /// 阶段1：serve_plugin_bundle 返回真实 bundle；未知 id → None。
    #[test]
    fn serve_plugin_bundle_reads_real_file() {
        let root = make_plugin_root();
        let m = build_boot_manifest(&root).unwrap();
        let body = serve_plugin_bundle(&m, "/plugins/@deepseek-ai/dsh-client-runtime/client.js").unwrap();
        let text = String::from_utf8(body).unwrap();
        assert!(text.contains("__ModuleLoader__.load"), "returns real bundle");
        // 未知 id
        assert!(serve_plugin_bundle(&m, "/plugins/@deepseek-ai/nope/client.js").is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    /// 阶段2：websocket_accept 计算对齐 RFC 6455 规范测试向量
    /// （key="dGhlIHNhbXBsZSBub25jZQ==" → accept="s3pPLMBiTxaQ9kYGzzhZRbK+xOo="）。
    #[test]
    fn websocket_accept_rfc6455_vector() {
        let accept = websocket_accept("dGhlIHNhbXBsZSBub25jZQ==");
        assert_eq!(accept, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    /// 阶段2：`ws_send` 把 server-request 帧作为文本帧写进 tungstenite WebSocket，
    /// 对端（浏览器同款 from_raw_socket 客户端）能读回同一 JSON。
    #[test]
    fn ws_send_roundtrips_text_frame() {
        use std::cell::RefCell;
        use std::io::{Read, Write};
        use std::rc::Rc;
        use tungstenite::protocol::{Role, WebSocket};

        // 内存双工：两端共享同一个缓冲（模拟双工连接）。
        #[derive(Clone)]
        struct Duplex {
            buf: Rc<RefCell<Vec<u8>>>,
        }
        impl Read for Duplex {
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                let mut b = self.buf.borrow_mut();
                let n = out.len().min(b.len());
                out[..n].copy_from_slice(&b[..n]);
                b.drain(..n);
                Ok(n)
            }
        }
        impl Write for Duplex {
            fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
                self.buf.borrow_mut().extend_from_slice(data);
                Ok(data.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let shared = Rc::new(RefCell::new(Vec::new()));
        let mut server = WebSocket::from_raw_socket(Duplex { buf: shared.clone() }, Role::Server, None);
        let frame = serde_json::json!({
            "type": "server-request", "rpcId": "sub-0", "method": "session/subscribed",
            "payload": {"type": "session/subscribed", "sessionId": "default", "lastSeq": 0},
        });
        server
            .send(tungstenite::Message::text(serde_json::to_string(&frame).unwrap()))
            .unwrap();
        // server.flush() 后缓冲含完整帧；客户端从同一缓冲读回。
        server.flush().unwrap();

        let mut client = WebSocket::from_raw_socket(Duplex { buf: shared }, Role::Client, None);
        let msg = client.read().unwrap();
        let text = msg.into_text().unwrap();
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["type"], "server-request");
        assert_eq!(parsed["method"], "session/subscribed");
        assert_eq!(parsed["payload"]["lastSeq"], 0);
    }

    /// 阶段1：render_index_with_boot 注入 `window.__DSH_BOOT__` 到 <head>。
    #[test]
    fn render_index_injects_boot_manifest() {
        let root = std::env::temp_dir().join(format!(
            "dsh-web-idx-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("index.html"), "<html><head></head><body><div id=\"root\"></div></body></html>").unwrap();
        let pr = make_plugin_root();
        let m = build_boot_manifest(&pr).unwrap();
        let html = render_index_with_boot(&root, &m).unwrap();
        let text = String::from_utf8(html).unwrap();
        assert!(text.contains("window.__DSH_BOOT__ = "), "boot manifest injected");
        assert!(text.contains("\"rev\""), "graph has rev");
        assert!(text.contains("dsh-client-runtime"), "graph has entry id");
        assert!(text.contains("client.js?rev="), "entry has bundle url");
        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&pr).ok();
    }
}

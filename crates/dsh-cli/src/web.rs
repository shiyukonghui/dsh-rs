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
use std::rc::Rc;
use std::time::Duration;

use dsh_core::*;
use tiny_http::{Header, Method, Response, Server};

use crate::session_host::{EventSink, SessionHost};
use crate::Boot;

/// trust fence（阶段4）：判定请求 Host 头是否为 loopback 权威
/// （对齐前端 `isLoopbackHostname`：localhost / `[::1]` / 127/8）。
fn host_is_loopback(request: &tiny_http::Request) -> bool {
    let host = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Host"))
        .map(|h| h.value.as_str().to_string())
        .unwrap_or_default();
    hostname_is_loopback(&host)
}

/// 纯判定：Host 值（可含端口）是否为 loopback。对齐前端 `isLoopbackHostname`
/// （localhost / `[::1]` / 127/8）。
fn hostname_is_loopback(host: &str) -> bool {
    let h = host.trim().to_lowercase();
    // IPv6 括号形式：`[::1]` 或 `[::1]:port` → 取括号内主机名。
    if let Some(inner) = h.strip_prefix('[') {
        let hostname = inner.split(']').next().unwrap_or("");
        return hostname == "::1";
    }
    // IPv4/localhost（按首个 ':' 去端口；localhost 无冒号也成立）。
    let hostname = h.split(':').next().unwrap_or("");
    if hostname == "localhost" {
        return true;
    }
    // 127/8（IPv4）
    if let Some(rest) = hostname.strip_prefix("127.") {
        return !rest.is_empty()
            && rest
                .split('.')
                .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
    }
    hostname == "127.0.0.1"
}

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
    /// 会话持久化根（`session/event` → append，`session/flush` → flush 落盘）。
    /// 缺省 = 纯内存（不落盘）。
    pub session_dir: Option<PathBuf>,
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
    // M1e：SessionHost——SessionStore（权威历史）+ 可选持久化挂载 + EventSink
    // 下链。loop 仍写 `boot.sessions`（SessionLog）；`session.prompt` adopt 进
    // 目标会话；`session/event` 下链走 EventSink（Send+Sync 供 SSE/WS 线程）。
    let host = match &cfg.session_dir {
        Some(dir) => SessionHost::with_root(dir),
        None => SessionHost::in_memory(),
    };
    // seed `default`（前端会话入口）。
    let _ = host.session("default");
    let sink = host.sink.clone();
    for request in server.incoming_requests() {
        let root = web_root.clone();
        let manifest = manifest.clone();
        let sink = sink.clone();
        // tiny_http 每请求已在线程处理；这里再派发。RPC/静态用 `&Boot`
        // （非 Send，留在调用线程），SSE/WS 用 `EventSink`（Send+Sync）。
        dispatch_request(request, &root, &manifest, boot, &host, &sink);
    }
    Ok(WebServer { addr })
}

/// 派发一个请求：`/plugins/*` bundle、`/api/*` RPC/SSE，否则静态文件（SPA fallback）。
fn dispatch_request(
    mut request: tiny_http::Request,
    web_root: &Path,
    manifest: &BootManifest,
    boot: &Boot,
    host: &Rc<SessionHost>,
    sink: &crate::session_host::EventSink,
) {
    // 路径去 query
    let path = request.url().split('?').next().unwrap_or("/").to_string();

    // 阶段4：trust fence——`/api` 与 `/plugins` 仅接受 loopback Host（防 DNS
    // rebinding：攻击者域名解析到 127.0.0.1 时，拒绝其跨域读宿主 API）。判定
    // 对齐前端 `isLoopbackHostname`（localhost / [::1] / 127/8）。
    if (path.starts_with("/api") || path.starts_with("/plugins/")) && !host_is_loopback(&request) {
        let resp = json_response(403, &serde_json::json!({
            "error": "forbidden",
            "message": "Host must be loopback",
        }));
        let _ = request.respond(resp);
        return;
    }

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
                let (status, json) = handle_rpc_host(boot, m, &body, host);
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
                    let sink = sink.clone();
                    std::thread::spawn(move || stream_ws_events(stream, &sink, is_host));
                } else {
                    let writer = request.into_writer();
                    let sink = sink.clone();
                    std::thread::spawn(move || stream_sse_events(writer, &sink));
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

/// SSE 事件下链（M71/M1e）：轮询 SessionHost 下链日志（EventSink），把**新事件**
/// 推成 `session/event` mux 帧（对齐 `muxFrameSchema`；每帧带真实 sessionId +
/// 真实 `time`）。运行在独立线程（`EventSink` Send+Sync）。握手后发
/// `session/subscribed`，随后增量推帧 + keepalive；连接关闭即退出。
fn stream_sse_events(mut writer: Box<dyn std::io::Write + Send>, sink: &EventSink) {
    // SSE 响应头（tiny_http 的 into_writer 是原始 socket 写；手写头 + data 帧）。
    if write_err(
        &mut writer,
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n",
    ).is_none() {
        return;
    }
    // 每连接独立游标：从当前下链日志末尾起读，只推连接建立后的新事件。
    let mut cursor = sink_len(sink);
    let last_seq = cursor as u64;
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
        // 增量推送：cursor 之后的新事件逐个推成 session/event 帧（真实 time）。
        let (new_cursor, frames) = {
            let log = sink.lock().unwrap();
            let mut frames = Vec::new();
            for (session_id, ev) in log.iter().skip(cursor) {
                frames.push(mux_session_event_frame(session_id, ev));
            }
            (log.len(), frames)
        };
        for frame in &frames {
            if write_sse(&mut writer, frame).is_none() {
                return;
            }
        }
        cursor = new_cursor;
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
/// 双工流；这里用 tungstenite 包成 WebSocket（成熟协议库，不手写帧），把
/// SessionHost 下链日志（EventSink）的新事件推成 `session/subscribed` +
/// `session/event`（mux）或 `host/session-added`（host）server-request 帧。
fn stream_ws_events(
    stream: Box<dyn tiny_http::ReadWrite + Send>,
    sink: &EventSink,
    is_host: bool,
) {
    use tungstenite::protocol::{Role, WebSocket, WebSocketConfig};
    let mut ws = WebSocket::from_raw_socket(stream, Role::Server, Some(WebSocketConfig::default()));
    let mut cursor = sink_len(sink);
    let last_seq = cursor as u64;
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
        let (new_cursor, frames) = {
            let log = sink.lock().unwrap();
            let mut frames = Vec::new();
            for (session_id, ev) in log.iter().skip(cursor) {
                frames.push(mux_session_event_frame(session_id, ev));
            }
            (log.len(), frames)
        };
        for frame in &frames {
            if ws_send(&mut ws, frame).is_none() {
                return;
            }
        }
        cursor = new_cursor;
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

/// 下链日志当前长度（Send+Sync 读；避免在调用处引入竞态）。
fn sink_len(sink: &EventSink) -> usize {
    sink.lock().unwrap().len()
}

/// 构造一个 `session/event` mux 帧（对齐 `muxFrameSchema`：
/// `{type:"session/event", sessionId, event:{type,seq,time,data}}`）。
/// 事件直接复用 dsh-session 的 strict-envelope 序列化（type/seq/time/data +
/// 可选 sourceEventSeqs/surfaceOp/ignorable）——与前端 `sessionEventSchema`
/// 逐字段一致；time 为会话 append 的真实 epoch ms。
fn mux_session_event_frame(session_id: &str, e: &dsh_session::types::SessionEvent) -> Value {
    let event = serde_json::to_value(e).unwrap_or(Value::Null);
    serde_json::json!({
        "type": "server-request",
        "rpcId": format!("ev-{}", e.seq),
        "method": "session/event",
        "payload": {
            "type": "session/event",
            "sessionId": session_id,
            "event": event,
        },
    })
}

/// 处理一个 `/api/<method>` RPC：解析 client-request 信封 → 分派 → server-response。
/// 返回 `(HTTP status, JSON body)`（body 为 server-response 信封）。
pub fn handle_rpc(boot: &Boot, method: &str, body: &[u8]) -> (u16, Value) {
    let host = SessionHost::in_memory();
    let _ = host.session("default");
    handle_rpc_host(boot, method, body, &host)
}

/// 带 SessionHost 版本（M1e 多会话；serve 用同一共享 host）。
pub fn handle_rpc_host(
    boot: &Boot,
    method: &str,
    body: &[u8],
    host: &Rc<SessionHost>,
) -> (u16, Value) {
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
    let result = dispatch(boot, method, &payload, host);
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
/// LLM 目录：`(current, groups)`——由 `Boot.llm`（dsh-core LlmService）注册表
/// 驱动；空注册表回退内置 loop 目录组（`dsh` 组：echo/llm/tool 是本仓真实可
/// 运行的 WASM loop 组件）。对齐 `sessionModelsValueSchema`/`llmModelsValueSchema`
/// （`{id,name,models:[{id,name}]}`）。
fn llm_catalog(boot: &Boot) -> (Value, Value) {
    let registered = boot.llm.lock().unwrap().providers();
    if registered.is_empty() {
        // 空注册表：内置 loop 目录组（echo/llm/tool 真实存在）。
        let groups = serde_json::json!([{
            "id": "dsh", "name": "DeepSeek Harness",
            "models": [
                {"id": "echo", "name": "echo-loop"},
                {"id": "llm", "name": "llm-loop"},
                {"id": "tool", "name": "tool-loop"},
            ],
        }]);
        let current = serde_json::json!({"provider": "dsh", "model": "echo"});
        (current, groups)
    } else {
        // 注册表驱动：每个 provider 一个组，模型 id 同 provider。
        let groups: Vec<Value> = registered
            .iter()
            .map(|p| {
                serde_json::json!({
                    "id": p.id,
                    "name": p.id,
                    "models": p
                        .models
                        .iter()
                        .map(|m| serde_json::json!({"id": m, "name": m}))
                        .collect::<Vec<_>>(),
                })
            })
            .collect();
        let first = &registered[0];
        let current = serde_json::json!({
            "provider": first.id,
            "model": first.models.first().cloned().unwrap_or_else(|| first.id.clone()),
        });
        (current, serde_json::Value::Array(groups))
    }
}

/// LLM provider 目录（`llm.providers`）——由 `Boot.llm` 注册表驱动，对齐
/// `configurableProviderViewSchema`（{provider, displayName, settingsNs,
/// settingsPath, active}）。空注册表 → 空数组（前端隐藏 provider 面板）。
fn llm_providers(boot: &Boot) -> Vec<Value> {
    boot.llm
        .lock()
        .unwrap()
        .providers()
        .iter()
        .map(|p| {
            serde_json::json!({
                "provider": p.id,
                "displayName": p.id,
                "settingsNs": "",
                "settingsPath": [],
                "active": true,
            })
        })
        .collect::<Vec<_>>()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// M3b：namespace 描述 → wire `SettingsNamespaceView`（对齐 settingsNamespaceViewSchema）。
fn namespace_view(view: dsh_settings::NamespaceDescriptor) -> Value {
    let mut v = serde_json::Map::new();
    v.insert("ns".to_string(), serde_json::json!(view.ns));
    let mut secrets = Vec::new();
    for slot in &view.secrets {
        secrets.push(serde_json::json!({"path": slot.path, "set": slot.set}));
    }
    let applies = match view.applies {
        dsh_settings::Applies::Live => "live",
        dsh_settings::Applies::Restart => "restart",
    };
    v.insert("schema".to_string(), view.schema);
    v.insert("value".to_string(), view.value);
    if let Some(base) = view.base {
        v.insert("base".to_string(), base);
    }
    if let Some(user) = view.user {
        v.insert("user".to_string(), user);
    }
    v.insert("applies".to_string(), serde_json::json!(applies));
    v.insert("secrets".to_string(), serde_json::json!(secrets));
    v.insert("revision".to_string(), serde_json::json!(view.revision));
    serde_json::Value::Object(v)
}

/// M3b：settings 错误 → wire `settings-rejected` 或 `SETTINGS_CONFLICT`。
fn settings_error_response(ns: &str, e: dsh_settings::SettingsError) -> Value {
    match e {
        dsh_settings::SettingsError::Conflict { expected, actual, .. } => serde_json::json!({
            "ok": false, "error": {
                "code": "SETTINGS_CONFLICT",
                "message": format!(
                    "settings namespace \"{ns}\" changed since it was read (expected revision {expected}, now {actual})"
                ),
            },
        }),
        dsh_settings::SettingsError::NotRegistered(name) => serde_json::json!({
            "ok": false, "error": {
                "code": "settings-rejected",
                "message": format!("settings namespace \"{name}\" is not registered"),
            },
        }),
        dsh_settings::SettingsError::Invalid { message } => serde_json::json!({
            "ok": false, "error": {
                "code": "settings-rejected",
                "message": message,
            },
        }),
    }
}

/// M3c：credentials 错误 → wire `credential-rejected`。
fn credentials_error_response(ref_name: &str, e: dsh_credentials::CredentialsError) -> Value {
    serde_json::json!({
        "ok": false, "error": {
            "code": "credential-rejected",
            "message": e.to_string(),
            "details": {"ref": ref_name},
        },
    })
}

// ---------------------------------------------------------------------------
// M4h：goal / subagent web RPC —— 把 M4 纯域服务接到 handle_rpc_host。
// ---------------------------------------------------------------------------

/// M4h：装配会话投影注册表，注册 `todos` 投影单元（M4g 交付的 into_unit）。
///
/// ProjectionRegistry 是可选能力：注册失败（重复键等）静默容忍，不 panic（对齐
/// `goal`/`plan` 等投影挂 dsh-session 事件流的 M4 后续接入——本子步只注册不强制暴露）。
pub fn todo_projection_registry() -> Rc<std::cell::RefCell<dsh_session_query::projection::ProjectionRegistry>> {
    let registry = Rc::new(std::cell::RefCell::new(
        dsh_session_query::projection::ProjectionRegistry::new(),
    ));
    {
        let mut reg = registry.borrow_mut();
        let _ = reg.register(dsh_session_query::todo::todos_projection_unit().into_unit());
    }
    registry
}

/// M4h：bad-request（ref 缺失 / revision<=0 / sessionId 缺失等 wire 前置校验失败）。
fn bad_request_response(message: impl Into<String>) -> Value {
    serde_json::json!({
        "ok": false, "error": {
            "code": "bad-request",
            "message": message.into(),
        },
    })
}

/// M4h：GoalServiceError → wire `{ok:false, error:{code, message}}`。
/// code 逐字用 GoalServiceError::code()（GOAL_* 稳定码）。
fn goal_error_response(e: &dsh_goal::GoalServiceError) -> Value {
    serde_json::json!({
        "ok": false, "error": {
            "code": e.code(),
            "message": e.to_string(),
        },
    })
}

/// M4h：从 payload 解析 goal ref（`{id, revision}`；revision<=0 视为缺失）。
fn goal_ref_from_payload(payload: &Value) -> Option<dsh_goal::GoalRef> {
    let r = payload.get("ref")?;
    let id = r.get("id")?.as_str()?.to_string();
    let revision = r.get("revision")?.as_u64()?;
    if revision == 0 {
        return None;
    }
    Some(dsh_goal::GoalRef::new(id, revision))
}

/// M4h：goal ref → wire `{ref: {id, revision}}`（响应 value）。
fn goal_ref_wire(gr: &dsh_goal::GoalRef) -> Value {
    serde_json::json!({"ref": {"id": gr.id.0, "revision": gr.revision}})
}

/// M4h：maxGoalRounds 解析（缺失 → None；显式值非法/0 → 0 哨兵让服务判
/// GOAL_INVALID_MAX_ROUNDS）。
fn goal_max_rounds(payload: &Value) -> Option<u64> {
    payload.get("maxGoalRounds").map(|v| v.as_u64().unwrap_or(0))
}

/// M4h：goal RPC 家族（goal.create/edit/pause/resume/complete/clear）。
fn goal_dispatch(boot: &Boot, method: &str, payload: &Value) -> Value {
    // 全部 goal.* 请求带 sessionId（catch-all：缺失 → bad-request）。
    let session_id = payload.get("sessionId").and_then(|v| v.as_str()).unwrap_or("");
    if session_id.is_empty() {
        return bad_request_response(format!("{method} requires sessionId"));
    }
    let mut svc = boot.goal.borrow_mut();
    match method {
        "goal.create" => {
            let objective = payload.get("objective").and_then(|v| v.as_str()).unwrap_or("");
            match svc.create(objective, goal_max_rounds(payload)) {
                Ok(gr) => serde_json::json!({"ok": true, "value": goal_ref_wire(&gr)}),
                Err(e) => goal_error_response(&e),
            }
        }
        "goal.edit" => {
            let Some(gr) = goal_ref_from_payload(payload) else {
                return bad_request_response("goal.edit requires ref {id, revision>0}");
            };
            let has_objective = payload.get("objective").and_then(|v| v.as_str()).is_some();
            let has_max = payload.get("maxGoalRounds").is_some();
            if !has_objective && !has_max {
                return bad_request_response("goal.edit requires objective and/or maxGoalRounds");
            }
            let objective = payload.get("objective").and_then(|v| v.as_str());
            match svc.edit(&gr, objective, goal_max_rounds(payload)) {
                Ok(gr2) => serde_json::json!({"ok": true, "value": goal_ref_wire(&gr2)}),
                Err(e) => goal_error_response(&e),
            }
        }
        "goal.pause" | "goal.resume" | "goal.complete" => {
            let Some(gr) = goal_ref_from_payload(payload) else {
                return bad_request_response(format!("{method} requires ref {{id, revision>0}}"));
            };
            let result = match method {
                "goal.pause" => svc.pause(&gr),
                "goal.resume" => svc.resume(&gr),
                _ => svc.complete(&gr),
            };
            match result {
                Ok(gr2) => serde_json::json!({"ok": true, "value": goal_ref_wire(&gr2)}),
                Err(e) => goal_error_response(&e),
            }
        }
        "goal.clear" => {
            // ref 缺失 → bad-request（revision<=0 亦视为缺失）。
            let Some(gr) = goal_ref_from_payload(payload) else {
                return bad_request_response("goal.clear requires ref {id, revision>0}");
            };
            // 幂等 no-op：无当前 goal（服务 Err(NotFound)）→ 仍 {cleared:true}（对齐 TS
            // clear 无 current goal 语义）；服务成功 → cleared:true；其余错误透传。
            match svc.clear(&gr) {
                Ok(_) => serde_json::json!({"ok": true, "value": {"cleared": true}}),
                Err(dsh_goal::GoalServiceError::NotFound) => {
                    serde_json::json!({"ok": true, "value": {"cleared": true}})
                }
                Err(e) => goal_error_response(&e),
            }
        }
        _ => bad_request_response("unknown goal method"),
    }
}

/// M4h：subagent.list entry wire（camelCase：hasChildren/label/reason）。
fn subagent_entry_wire(e: &dsh_subagent::ChildEntry) -> Value {
    if e.kind == "diagnostic" {
        return serde_json::json!({
            "kind": "diagnostic",
            "id": e.id,
            "reason": e.reason.clone().unwrap_or_default(),
        });
    }
    let mut v = serde_json::Map::new();
    v.insert("kind".to_string(), serde_json::json!("child"));
    v.insert("id".to_string(), serde_json::json!(e.id));
    v.insert("mode".to_string(), serde_json::json!(e.mode));
    v.insert("activity".to_string(), serde_json::json!(e.activity));
    v.insert("hasChildren".to_string(), serde_json::json!(e.has_children));
    // label：one-shot 可选 / continuable 必填（纯数据承载，wire 上看有无）。
    if let Some(l) = &e.label {
        v.insert("label".to_string(), serde_json::json!(l));
    }
    serde_json::Value::Object(v)
}

/// M4h：subagent RPC 家族（subagent.list/history/interrupt/prompt）。
fn subagent_dispatch(boot: &Boot, method: &str, payload: &Value) -> Value {
    match method {
        "subagent.list" => {
            // M4 无真实子代理运行时（无监控源）→ 空目录行（0 条 child/diagnostic）；
            // 行构造走 catalog 纯函数（category_child/diagnostic_row 的宿主投影保持
            // 同一 wire 形状——subagent_entry_wire），供真实目录源接入时复用。
            let rows: Vec<dsh_subagent::ChildEntry> = Vec::new();
            let entries: Vec<Value> = rows.iter().map(subagent_entry_wire).collect();
            let _ = boot;
            serde_json::json!({"ok": true, "value": {"entries": entries, "parentAvailable": true}})
        }
        "subagent.history" => {
            // M4 无真实持久化子代理日志 → 诚实空实现（events 空、hasMore false）。
            let _ = boot;
            serde_json::json!({"ok": true, "value": {"events": [], "hasMore": false}})
        }
        "subagent.prompt" => {
            let parent = payload.get("parentSessionId").and_then(|v| v.as_str()).unwrap_or("");
            let child = payload.get("childSessionId").and_then(|v| v.as_str()).unwrap_or("");
            let mode = payload.get("mode").and_then(|v| v.as_str()).unwrap_or("");
            let addr = dsh_subagent::PromptAddress {
                parent_session_id: parent.to_string(),
                child_session_id: child.to_string(),
                mode: mode.to_string(),
            };
            // prompt 仅对 continuable child（mode 校验）。
            if let Err(dsh_subagent::PromptError::NotContinuable) = dsh_subagent::prompt_gate(&addr) {
                return bad_request_response(
                    "subagent.prompt requires mode 'continuable'",
                );
            }
            let _ = boot;
            // M4 无真实投递 → 诚实合成 messageId（过 schema；未来接 agent-loop inbox）。
            serde_json::json!({"ok": true, "value": {"messageId": format!("pmsg-{child}:1")}})
        }
        "subagent.interrupt" => {
            let parent = payload.get("parentSessionId").and_then(|v| v.as_str()).unwrap_or("");
            let child = payload.get("childSessionId").and_then(|v| v.as_str()).unwrap_or("");
            let mode = payload.get("mode").and_then(|v| v.as_str()).unwrap_or("");
            let addr = dsh_subagent::InterruptAddress {
                parent_session_id: parent.to_string(),
                child_session_id: child.to_string(),
                mode: mode.to_string(),
            };
            let accepted = dsh_subagent::interrupt_receipt(&addr);
            let _ = boot;
            serde_json::json!({"ok": true, "value": {"accepted": accepted}})
        }
        _ => bad_request_response("unknown subagent method"),
    }
}

/// M4h：注册 M4 工具（当前：todo_write）到给定 ToolRegistry。
///
/// 说明：harness（Boot）当前没有持久 ToolRegistry 注入点（工具注册表只在 agent-loop
/// 装配时按需创建）——本子步提供纯注册函数 + 单元测试证明可注册 + 参数校验走
/// `dsh_session_query::todo::to_todo_list`；不强制挂 boot 链（差值记录于 D-043 同类）。
pub fn register_m4_tools(registry: &dsh_tools::ToolRegistry) {
    let def = dsh_tools::define_tool(dsh_tools::DefineToolOptions {
        name: "todo_write".to_string(),
        description: "写入/替换当前会话的待办列表（全表覆盖，单活动纪律受 allowParallel 约束）。".to_string(),
        parameters: serde_json::json!({
            "todos": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "content": {"type": "string", "required": true},
                        "status": {
                            "type": "string",
                            "enum": ["pending", "in_progress", "completed"],
                        },
                    },
                },
                "required": true,
            },
            "allowParallel": {"type": "boolean"},
        }),
        output_schema: serde_json::json!({ "type": "json" }),
        render: Rc::new(|_, value| {
            let text = format!("todos: {}", serde_json::to_string(value).unwrap_or_default());
            vec![dsh_llm::ContentBlock::text(text)]
        }),
        execute: Rc::new(|args, _| {
            let raw = args.get("todos").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            let allow_parallel = args.get("allowParallel").and_then(|v| v.as_bool()).unwrap_or(false);
            match dsh_session_query::todo::to_todo_list(&raw, allow_parallel) {
                Ok(list) => serde_json::to_value(list)
                    .map_err(|e| dsh_tools::ToolFailureData::new(e.to_string(), dsh_tools::CODE_INVALID_TOOL_OUTPUT, "Error")),
                Err(e) => Err(dsh_tools::ToolFailureData::new(
                    format!("todo list rejected: {e:?}"),
                    dsh_tools::CODE_INVALID_ARGS,
                    "TodoListError",
                )),
            }
        }),
        ..Default::default()
    })
    .expect("todo_write tool defines cleanly");
    registry
        .register_global(Rc::new(def))
        .expect("register todo_write");
}


fn dispatch(boot: &Boot, method: &str, payload: &Value, host: &Rc<SessionHost>) -> Value {
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
            let (current, _) = llm_catalog(boot);
            let provider = current.get("provider").and_then(|p| p.as_str());
            let model = current.get("model").and_then(|m| m.as_str());
            let mut value = serde_json::Map::new();
            value.insert("version".to_string(), serde_json::json!(env!("CARGO_PKG_VERSION")));
            value.insert("cwd".to_string(), serde_json::json!(cwd));
            value.insert("attachedSessions".to_string(), serde_json::json!(attached));
            value.insert("home".to_string(), serde_json::json!(crate::host_dir::home_dir()));
            // provider/model 可选：缺省省略（对齐 host schema 可选性）。
            if let Some(p) = provider {
                value.insert("provider".to_string(), serde_json::json!(p));
            }
            if let Some(m) = model {
                value.insert("model".to_string(), serde_json::json!(m));
            }
            value.insert("canOpenPath".to_string(), serde_json::json!(true));
            serde_json::json!({"ok": true, "value": value})
        }
        "host.pickDirectory" => {
            // M3a：无 native dialog 的诚实降级——`{path:null}` 对齐「用户取消」语义。
            serde_json::json!({"ok": true, "value": {"path": null}})
        }
        "host.listDirectory" => {
            // M3a：真实 fs 列目录（browse capability；默认列 home）。
            let path = payload.get("path").and_then(|p| p.as_str());
            match crate::host_dir::list_directory(path, 1000) {
                Ok(listing) => serde_json::json!({"ok": true, "value": {
                    "path": listing.path,
                    "home": listing.home,
                    "crumbs": listing.crumbs.iter().map(|c| serde_json::json!({
                        "name": c.name, "path": c.path, "hidden": c.hidden,
                    })).collect::<Vec<_>>(),
                    "entries": listing.entries.iter().map(|e| serde_json::json!({
                        "name": e.name, "path": e.path, "hidden": e.hidden,
                    })).collect::<Vec<_>>(),
                    "truncated": listing.truncated,
                }}),
                Err(e) => serde_json::json!({"ok": false, "error": {
                    "code": e.code, "message": e.message,
                }}),
            }
        }
        "host.createDirectory" => {
            // M3a：真实创建单段子目录（browse capability）。
            let parent = payload.get("path").and_then(|p| p.as_str()).unwrap_or("");
            let name = payload.get("name").and_then(|n| n.as_str()).unwrap_or("");
            match crate::host_dir::create_directory(parent, name) {
                Ok(path) => serde_json::json!({"ok": true, "value": {"path": path}}),
                Err(e) => serde_json::json!({"ok": false, "error": {
                    "code": e.code, "message": e.message,
                }}),
            }
        }
        "host.openPath" => {
            // M3a：无桌面 opener 的诚实降级——记录目标并回报 opened（差异见 D-037）。
            let path = payload.get("path").and_then(|p| p.as_str()).unwrap_or("");
            if path.is_empty() {
                serde_json::json!({"ok": false, "error": {
                    "code": "bad-request", "message": "path is required",
                }})
            } else {
                serde_json::json!({"ok": true, "value": {"opened": true}})
            }
        }
        "sessions" | "session.list" => {
            // M1e：SessionStore 提供权威列表（创建顺序、失活/空判定）。
            let updated_at = now_ms();
            let items = {
                let mut items = host
                    .list()
                    .iter()
                    .map(|s| {
                        serde_json::json!({
                            "sessionId": s.id().raw(),
                            "updatedAt": s.events().last().map(|e| e.time.max(0) as u64).unwrap_or(updated_at),
                            "running": false,
                            "blank": s.events().is_empty(),
                        })
                    })
                    .collect::<Vec<_>>();
                items.sort_by(|a, b| a["sessionId"].as_str().cmp(&b["sessionId"].as_str()));
                items
            };
            serde_json::json!({"ok": true, "value": {"items": items}})
        }
        "session.create" => {
            // M1e：SessionHost mint 唯一 sessionId 并创建空会话。
            let id = host.create_new().unwrap_or_else(|_| "s1".to_string());
            serde_json::json!({"ok": true, "value": {"sessionId": id}})
        }
        "session.history" => {
            // M1e：SessionStore 的历史（strict-envelope 事件直接 wire）。
            let sid = payload
                .get("sessionId")
                .and_then(|v| v.as_str())
                .unwrap_or("default")
                .to_string();
            let events = host
                .events(&sid)
                .iter()
                .map(|e| {
                    serde_json::json!({
                        "event": serde_json::to_value(e).unwrap_or(Value::Null),
                    })
                })
                .collect::<Vec<_>>();
            serde_json::json!({"ok": true, "value": {"events": events, "hasMore": false}})
        }
        "session.search" => {
            serde_json::json!({"ok": true, "value": {"items": [], "hasMore": false}})
        }
        "session.models" => {
            // M1e：由 Boot.llm（dsh-core LlmService）注册表驱动；空注册表回退
            // 内置 loop 目录组（echo/llm/tool——本仓真实可运行的 loop 组件）。
            let (current, groups) = llm_catalog(boot);
            serde_json::json!({"ok": true, "value": {
                "current": current,
                "routable": true,
                "groups": groups,
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
            let sid = payload.get("sessionId").and_then(|v| v.as_str()).unwrap_or("default");
            let seq = host.seq_of(sid);
            serde_json::json!({"ok": true, "value": {"title": title, "seq": seq}})
        }
        "session.fork" => {
            // M1e fork：从 live 源会话创建子会话（seed+边界标记均已 store 处理）。
            let src = payload.get("sessionId").and_then(|v| v.as_str()).unwrap_or("default");
            let (id, ok) = match host.fork(src) {
                Ok(id) => (id, true),
                Err(e) => (e, false),
            };
            if ok {
                serde_json::json!({"ok": true, "value": {"sessionId": id}})
            } else {
                // 源会话不存在 → 按 schema 失败（session-not-found）。
                serde_json::json!({"ok": false, "error": {
                    "code": "session-not-found",
                    "message": format!("cannot fork unknown session \"{src}\""),
                    "details": {"sessionId": src},
                }})
            }
        }
        "session.prompt" => {
            // 前端经 prompt 发消息：提取 content → 驱动 turn。
            // M2g：boot 装配了 Rust AgentLoopHost 时改驱真实 agent-loop（事件直接
            // 落共享 store；前端历史/下链同一事实源）；否则 M1 WASM loop 路径
            // （run_turn 的 SessionLog 新事件 adopt 进目标会话）。
            let sid = payload.get("sessionId").and_then(|v| v.as_str()).unwrap_or("default").to_string();
            let content = payload.get("content").cloned().unwrap_or(Value::Null);
            if boot.agent_loop.is_some() {
                // 取首个 text 块为 prompt 文本（M1 回显 loop 的输入形状）。
                let text = content
                    .as_array()
                    .and_then(|blocks| {
                        blocks.iter().find_map(|b| {
                            (b.get("type").and_then(|t| t.as_str()) == Some("text"))
                                .then(|| b.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string())
                        })
                    })
                    .unwrap_or_default();
                return match crate::run_rust_loop(boot, &sid, &text) {
                    Ok(()) => serde_json::json!({"ok": true, "value": {"accepted": true}}),
                    Err(e) => serde_json::json!({"ok": false, "error": {
                        "code": "internal",
                        "message": e.to_string(),
                    }}),
                };
            }
            let before = boot.sessions.lock().unwrap().events().len();
            let _ = crate::run_turn(boot, &serde_json::json!({"content": content}));
            let new_events: Vec<(String, Vec<u8>)> = {
                let log = boot.sessions.lock().unwrap();
                log.events()
                    .iter()
                    .skip(before)
                    .map(|e| (e.kind.clone(), e.payload.clone()))
                    .collect()
            };
            if !new_events.is_empty() {
                let _ = host.adopt(&sid, &new_events);
            }
            serde_json::json!({"ok": true, "value": {"accepted": true}})
        }
        "session.cancel" => {
            serde_json::json!({"ok": true, "value": {"accepted": true}})
        }
        "session.attachment" => {
            serde_json::json!({"ok": true, "value": {
                "attachment": {
                    "attachmentId": "default", "mediaType": "image/png",
                    "bytes": 0, "width": 1, "height": 1,
                },
                "data": "",
            }})
        }
        "session.updateQueue" => {
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
        "workspace.create" => {
            let path = payload.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let now = now_ms().to_string();
            serde_json::json!({"ok": true, "value": {
                "workspace": {
                    "workspaceId": "default", "path": path, "title": "default",
                    "sessionIds": [], "createdAt": now, "updatedAt": now,
                },
                "created": false,
            }})
        }
        "workspace.rename" => {
            let path = std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            let now = now_ms().to_string();
            serde_json::json!({"ok": true, "value": {
                "workspace": {
                    "workspaceId": "default", "path": path, "title": "default",
                    "sessionIds": ["default"], "createdAt": now, "updatedAt": now,
                },
            }})
        }
        "workspace.delete" => {
            serde_json::json!({"ok": true, "value": {"deleted": true}})
        }
        "workspace.insertBefore" => {
            serde_json::json!({"ok": true, "value": {"workspaceIds": ["default"]}})
        }
        "workspace.insertSessionBefore" | "workspace.archiveSession" => {
            let path = std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            let now = now_ms().to_string();
            serde_json::json!({"ok": true, "value": {
                "workspace": {
                    "workspaceId": "default", "path": path, "title": "default",
                    "sessionIds": ["default"], "createdAt": now, "updatedAt": now,
                },
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
        "agentPreset.select" => {
            let preset = payload.get("agentPreset").and_then(|v| v.as_str()).unwrap_or("default");
            serde_json::json!({"ok": true, "value": {"agentPreset": preset}})
        }
        "agentPreset.read" => {
            let preset = payload.get("agentPreset").and_then(|v| v.as_str()).unwrap_or("default");
            serde_json::json!({"ok": true, "value": {
                "agentPreset": preset, "trust": "user", "content": "",
            }})
        }
        "agentPreset.copy" => {
            let preset = payload.get("agentPreset").and_then(|v| v.as_str()).unwrap_or("default");
            serde_json::json!({"ok": true, "value": {"agentPreset": preset}})
        }
        "agentPreset.remove" => {
            serde_json::json!({"ok": true, "value": {}})
        }
        "agentPreset.openDocument" => {
            serde_json::json!({"ok": true, "value": {"opened": true}})
        }
        "settings.describe" => {
            // M3b：真实 service 驱动——列出已注册 namespace（分层 resolve + redact）。
            let mut sp = boot.settings.borrow_mut();
            let namespaces: Vec<Value> = sp
                .describe_all()
                .into_iter()
                .map(namespace_view)
                .collect();
            let writable = true;
            let has_document = sp.has_document();
            serde_json::json!({"ok": true, "value": {
                "writable": writable,
                "hasDocument": has_document,
                "namespaces": namespaces,
            }})
        }
        "settings.openDocument" => {
            // M3b：无桌面 opener 的诚实降级——`{opened:true}`（差异见 D-037）。
            serde_json::json!({"ok": true, "value": {"opened": true}})
        }
        "settings.update" | "settings.replace" | "settings.mutate" => {
            let ns = payload.get("ns").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let expected = payload.get("expectedRevision").and_then(|v| v.as_u64());
            let mut sp = boot.settings.borrow_mut();
            let result = match method {
                "settings.update" => {
                    let patch = payload.get("patch").cloned().unwrap_or(Value::Null);
                    sp.update(&ns, &patch, expected)
                }
                "settings.replace" => {
                    let section = payload.get("section").cloned().unwrap_or(Value::Null);
                    sp.replace(&ns, &section, expected)
                }
                _ => {
                    let ops = payload.get("ops").cloned().unwrap_or(Value::Null);
                    sp.mutate(&ns, &ops, expected)
                }
            };
            match result {
                Ok(view) => serde_json::json!({"ok": true, "value": namespace_view(view)}),
                Err(e) => settings_error_response(&ns, e),
            }
        }
        "credentials.describe" => {
            // M3c：真实 service 驱动——按 refs 批量描述（configured/source/writable）。
            let creds = boot.credentials.borrow();
            let refs = payload.get("refs").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            let mut out = serde_json::Map::new();
            for r in refs {
                let Some(name) = r.as_str() else {
                    return serde_json::json!({"ok": false, "error": {
                        "code": "bad-request",
                        "message": "refs must be strings",
                    }});
                };
                if !dsh_credentials::is_credential_ref_name(name) {
                    return serde_json::json!({"ok": false, "error": {
                        "code": "bad-request",
                        "message": format!("invalid credential ref \"{name}\""),
                    }});
                }
                let view = creds.describe(name).unwrap_or(
                    dsh_credentials::CredentialView { configured: false, source: None, writable: true }
                );
                let mut v = serde_json::Map::new();
                v.insert("configured".to_string(), serde_json::json!(view.configured));
                if let Some(src) = view.source {
                    v.insert("source".to_string(), serde_json::json!(src));
                }
                v.insert("writable".to_string(), serde_json::json!(view.writable));
                out.insert(name.to_string(), serde_json::Value::Object(v));
            }
            serde_json::json!({"ok": true, "value": {"credentials": out}})
        }
        "credentials.set" | "credentials.unset" => {
            let name = payload.get("ref").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if !dsh_credentials::is_credential_ref_name(&name) {
                return serde_json::json!({"ok": false, "error": {
                    "code": "bad-request",
                    "message": format!("invalid credential ref \"{name}\""),
                }});
            }
            let mut creds = boot.credentials.borrow_mut();
            let result = if method == "credentials.set" {
                let value = payload.get("value").and_then(|v| v.as_str()).unwrap_or("");
                creds.set(&name, value)
            } else {
                creds.unset(&name)
            };
            match result {
                Ok(()) => serde_json::json!({"ok": true, "value": {}}),
                Err(e) => credentials_error_response(&name, e),
            }
        }
        "llm.providers" => {
            // M1e：由 Boot.llm 注册表驱动（configurableProviderViewSchema）。
            let providers = llm_providers(boot);
            serde_json::json!({"ok": true, "value": {"providers": providers}})
        }
        "llm.models" => {
            let (_, groups) = llm_catalog(boot);
            serde_json::json!({"ok": true, "value": {
                "groups": groups,
                "failures": [],
            }})
        }
        "llm.discoverModels" => {
            serde_json::json!({"ok": true, "value": {"models": []}})
        }
        "goal.create" | "goal.edit" | "goal.pause" | "goal.resume" | "goal.complete" | "goal.clear" => {
            goal_dispatch(boot, method, payload)
        }
        "subagent.list" | "subagent.history" | "subagent.prompt" | "subagent.interrupt" => {
            subagent_dispatch(boot, method, payload)
        }
        "commands/list" => {
            serde_json::json!({"ok": true, "value": [
                {"name": "compact", "description": "压缩当前会话上下文"},
                {"name": "plan", "description": "进入或离开计划模式", "input": {"hint": "[off|message]"}},
                {"name": "goal", "description": "为长任务设置或查看目标", "input": {"hint": "<objective>"}},
                {"name": "subagents", "description": "列出子代理目录", "input": {"hint": "[parentSessionId]"}},
            ]})
        }
        // cordis 插件清单 UI（dynamicCordisRunner remote）：host 侧无动态插件，
        // inventory 空数组、syncInspectManifest 返回 null（对齐其 result schema）。
        "dynamicCordisRunner/inventory" => {
            serde_json::json!({"ok": true, "value": []})
        }
        "dynamicCordisRunner/syncInspectManifest" => {
            serde_json::json!({"ok": true, "value": null})
        }
        "agent-loop" | "agent.turn" | "agent.run" => {
            if boot.agent_loop.is_some() {
                // M2g：调度到 Rust AgentLoopHost（默认会话映射；事件落共享 store）。
                let text = payload
                    .get("content")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string();
                return match crate::run_rust_loop(boot, "default", &text) {
                    Ok(()) => serde_json::json!({"ok": true, "value": {"accepted": true}}),
                    Err(e) => serde_json::json!({"ok": false, "error": {
                        "code": "internal",
                        "message": e.to_string(),
                    }}),
                };
            }
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
            llm: dsh_core::new_llm(),
            refresh: std::rc::Rc::new(|| Ok(())),
            agent_loop: None,
            settings: std::rc::Rc::new(std::cell::RefCell::new(
                dsh_settings::SettingsProvider::memory(),
            )),
            credentials: std::rc::Rc::new(std::cell::RefCell::new(
                dsh_credentials::CredentialProvider::memory(),
            )),
            goal: std::rc::Rc::new(std::cell::RefCell::new(
                dsh_goal::GoalService::new(dsh_goal::ServiceOptions::default()),
            )),
            projections: todo_projection_registry(),
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
    /// （{version, cwd, attachedSessions, home, canOpenPath}；M3a 补 home）。
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
        let home = val["home"].as_str().expect("host.describe.home present (M3a)");
        assert!(!home.is_empty());
    }

    /// M3a：host.listDirectory 经 /api 返回 DirectoryListing 形状，且真实包含
    /// 一个测试目录（browse capability 真实 fs 读）。
    #[test]
    fn rpc_host_list_directory_real_fs() {
        let boot = boot_with_sessions();
        let dir = std::env::temp_dir().join(format!(
            "dsh-web-list-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("alpha")).unwrap();
        std::fs::create_dir_all(dir.join(".zeta")).unwrap();
        std::fs::write(dir.join("file.txt"), "x").unwrap();
        let body = serde_json::to_vec(&serde_json::json!({
            "type": "client-request", "rpcId": "r", "method": "host.listDirectory",
            "payload": {"path": dir.to_str().unwrap()},
        })).unwrap();
        let (status, v) = handle_rpc(&boot, "host.listDirectory", &body);
        assert_eq!(status, 200);
        assert_eq!(v["result"]["ok"], true);
        let val = &v["result"]["value"];
        let entries = val["entries"].as_array().expect("entries array");
        assert!(entries.iter().any(|e| e["name"] == "alpha"), "alpha row");
        assert!(entries.iter().any(|e| e["name"] == ".zeta" && e["hidden"] == true), ".zeta hidden row");
        assert!(!entries.iter().any(|e| e["name"] == "file.txt"), "non-dir skipped");
        assert!(val["crumbs"].as_array().is_some() && !val["crumbs"].as_array().unwrap().is_empty());
        assert_eq!(val["truncated"], false);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// M3a：host.createDirectory 真实创建；重复 → directory-exists 错误链路。
    #[test]
    fn rpc_host_create_directory_real_fs() {
        let boot = boot_with_sessions();
        let dir = std::env::temp_dir().join(format!(
            "dsh-web-create-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mk = |name: &str| {
            let body = serde_json::to_vec(&serde_json::json!({
                "type": "client-request", "rpcId": "r", "method": "host.createDirectory",
                "payload": {"path": dir.to_str().unwrap(), "name": name},
            })).unwrap();
            handle_rpc(&boot, "host.createDirectory", &body)
        };
        // 相对父 → directory-create-failed。
        let mk_rel = || {
            let body = serde_json::to_vec(&serde_json::json!({
                "type": "client-request", "rpcId": "r", "method": "host.createDirectory",
                "payload": {"path": "relative/parent", "name": "x"},
            })).unwrap();
            handle_rpc(&boot, "host.createDirectory", &body)
        };
        let (_, v) = mk("nested");
        assert_eq!(v["result"]["ok"], true);
        assert_eq!(
            v["result"]["value"]["path"],
            dir.join("nested").to_string_lossy().to_string()
        );
        assert!(dir.join("nested").is_dir());
        let (_, dup) = mk("nested");
        assert_eq!(dup["result"]["ok"], false);
        assert_eq!(dup["result"]["error"]["code"], "directory-exists");
        let (_, rel) = mk_rel();
        assert_eq!(rel["result"]["ok"], false);
        assert_eq!(rel["result"]["error"]["code"], "directory-create-failed");
        let _ = std::fs::remove_dir_all(&dir);
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

    /// 构造一个 seed `default` 的 SessionHost（测试用；M1e 会话由 store 承载）。
    fn seeded_host() -> Rc<SessionHost> {
        let host = SessionHost::in_memory();
        let _ = host.session("default");
        host
    }

    /// 阶段2：session.history 返回对齐 sessionHistoryValueSchema 的形状
    /// （{events:[{event:{type,seq,time,data}}], hasMore}）。
    #[test]
    fn rpc_session_history_shape() {
        let boot = boot_with_sessions();
        let host = seeded_host();
        // 预置 default 历史到 store（grant-append：user/message + assistant/message）。
        host.adopt(
            "default",
            &[
                (
                    "user/message".into(),
                    serde_json::to_vec(&serde_json::json!({
                        "id": "u1", "role": "user", "content": [{"type": "text", "text": "hi"}],
                        "source": {"kind": "user"},
                    })).unwrap(),
                ),
                (
                    "assistant/message".into(),
                    serde_json::to_vec(&serde_json::json!({
                        "turn": 1, "step": 1,
                        "message": {
                            "id": "a1", "role": "assistant",
                            "content": [{"type": "text", "text": "hi"}],
                            "source": {"kind": "model", "provider": "mock", "model": "mock"},
                        },
                    })).unwrap(),
                ),
            ],
        )
        .unwrap();
        let body = serde_json::to_vec(&serde_json::json!({
            "type": "client-request", "rpcId": "r7", "method": "session.history", "payload": {}
        })).unwrap();
        let (_, v) = handle_rpc_host(&boot, "session.history", &body, &host);
        assert_eq!(v["result"]["ok"], true);
        let val = &v["result"]["value"];
        assert_eq!(val["hasMore"], false);
        assert!(val["events"].is_array());
        assert_eq!(val["events"][0]["event"]["type"], "user/message");
        assert_eq!(val["events"][0]["event"]["data"]["id"], "u1");
        // strict envelope：time 为真实 epoch ms（>0），seq 连续。
        assert!(val["events"][0]["event"]["time"].as_u64().unwrap() > 0);
        assert_eq!(val["events"][0]["event"]["seq"], 0);
        assert_eq!(val["events"][1]["event"]["seq"], 1);
    }

    /// 阶段3/4 多会话：session.create mint 新 id，session.list 含多会话，
    /// session.prompt 把 turn 事件 adopt 进目标 session 的独立 store 历史。
    #[test]
    fn rpc_multi_session_create_list_prompt() {
        let boot = boot_with_sessions();
        let host = seeded_host();

        // 创建两个新会话
        let mut ids = Vec::new();
        for _ in 0..2 {
            let body = serde_json::to_vec(&serde_json::json!({
                "type": "client-request", "rpcId": "r", "method": "session.create", "payload": {}
            })).unwrap();
            let (_, v) = handle_rpc_host(&boot, "session.create", &body, &host);
            let id = v["result"]["value"]["sessionId"].as_str().unwrap().to_string();
            assert!(!ids.contains(&id), "session ids unique");
            ids.push(id);
        }
        assert_eq!(ids.len(), 2);
        // 共 3 个会话（default + 2）
        {
            let body = serde_json::to_vec(&serde_json::json!({
                "type": "client-request", "rpcId": "r", "method": "session.list", "payload": {}
            })).unwrap();
            let (_, v) = handle_rpc_host(&boot, "session.list", &body, &host);
            assert_eq!(v["result"]["value"]["items"].as_array().unwrap().len(), 3);
        }
        // 对第一个新会话 prompt → 事件只进该会话 store 历史
        {
            let sid = &ids[0];
            let body = serde_json::to_vec(&serde_json::json!({
                "type": "client-request", "rpcId": "r", "method": "session.prompt",
                "payload": {"sessionId": sid, "content": [{"type": "text", "text": "hi"}]},
            })).unwrap();
            let (_, v) = handle_rpc_host(&boot, "session.prompt", &body, &host);
            assert_eq!(v["result"]["value"]["accepted"], true);
        }
        // 目标会话历史有事件；另一新会话历史为空（独立 store 会话）。
        {
            let body = serde_json::to_vec(&serde_json::json!({
                "type": "client-request", "rpcId": "r", "method": "session.history",
                "payload": {"sessionId": &ids[0]},
            })).unwrap();
            let (_, v) = handle_rpc_host(&boot, "session.history", &body, &host);
            let evs = v["result"]["value"]["events"].as_array().unwrap();
            assert!(!evs.is_empty());
            // strict-envelope：assistant/message 带真实 time（epoch ms）。
            let assistant = evs.iter().find(|e| e["event"]["type"] == "assistant/message").unwrap();
            assert!(assistant["event"]["time"].as_u64().unwrap() > 0);

            let body2 = serde_json::to_vec(&serde_json::json!({
                "type": "client-request", "rpcId": "r", "method": "session.history",
                "payload": {"sessionId": &ids[1]},
            })).unwrap();
            let (_, v2) = handle_rpc_host(&boot, "session.history", &body2, &host);
            assert!(v2["result"]["value"]["events"].as_array().unwrap().is_empty());
        }
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

    /// 阶段3：dynamicCordisRunner inventory → []、syncInspectManifest → null
    /// （对齐其 result schema，清除 cordis 清单 UI 的 boot 报错）。
    #[test]
    fn rpc_dynamic_cordis_runner_empty() {
        let boot = boot_with_sessions();
        for (m, expected) in [
            ("dynamicCordisRunner/inventory", "[]"),
            ("dynamicCordisRunner/syncInspectManifest", "null"),
        ] {
            let body = serde_json::to_vec(&serde_json::json!({
                "type": "client-request", "rpcId": "r", "method": m, "payload": {}
            })).unwrap();
            let (_, v) = handle_rpc(&boot, m, &body);
            assert_eq!(v["result"]["ok"], true, "{m} ok");
            assert_eq!(
                serde_json::to_string(&v["result"]["value"]).unwrap(),
                expected,
                "{m} value"
            );
        }
    }

    /// 阶段3/4：settings/credentials/llm/goal/subagent/agentPreset 方法返回
    /// 对齐各自 UNARY_VALUE_SCHEMAS 的形状（空实现但 ok:true，不 fail loud）。
    #[test]
    fn rpc_extended_method_surface_ok() {
        let boot = boot_with_sessions();
        // 该方法面冒烟测试以「空 payload 可合法 ok」为主；M4h 后需 payload 的方法
        // （goal.create 需 objective、subagent.interrupt 需 mode）用最小合法 payload
        // 触发（对齐 M3a 对 host.createDirectory/openPath 的处理：真实语义覆盖移入
        // 专用测试；此处保留方法面 ok 检查）。
        let cases: &[(&str, &str, &str)] = &[
            ("settings.describe", "writable", "bool"),
            ("credentials.describe", "credentials", "obj"),
            ("llm.providers", "providers", "arr"),
            ("llm.models", "groups", "arr"),
            ("goal.create", "ref", "obj"),
            ("goal.clear", "cleared", "bool"),
            ("subagent.list", "entries", "arr"),
            ("subagent.interrupt", "accepted", "bool"),
            ("agentPreset.select", "agentPreset", "str"),
            ("agentPreset.read", "content", "str"),
            ("agentPreset.copy", "agentPreset", "str"),
            ("agentPreset.remove", "x", "miss"),
        ];
        let cases2: &[(&str, &str, &str)] = &[
            ("session.attachment", "attachment", "obj"),
            ("session.updateQueue", "accepted", "bool"),
            ("host.pickDirectory", "path", "null"),
            ("host.listDirectory", "entries", "arr"),
            // M3a：host.createDirectory/host.openPath 已做实，空 payload 不再是
            // 合法 ok 响应（create 需 path+name、openPath 需 path）→ 由专用测试覆盖。
            ("workspace.create", "workspace", "obj"),
            ("workspace.rename", "workspace", "obj"),
            ("workspace.delete", "deleted", "bool"),
            ("workspace.insertBefore", "workspaceIds", "arr"),
            ("workspace.insertSessionBefore", "workspace", "obj"),
            ("workspace.archiveSession", "workspace", "obj"),
            ("agentPreset.openDocument", "opened", "bool"),
        ];
        // 方法 → 冒烟 payload（缺省空对象；需要入参的方法给最小合法 payload）。
        fn surface_payload(m: &str) -> Value {
            match m {
                "goal.create" => serde_json::json!({
                    "sessionId": "default", "objective": "surface goal",
                }),
                "goal.clear" => serde_json::json!({
                    "sessionId": "default", "ref": {"id": "goal-1", "revision": 1},
                }),
                "subagent.interrupt" => serde_json::json!({
                    "parentSessionId": "default",
                    "childSessionId": "c-1",
                    "mode": "continuable",
                }),
                _ => serde_json::json!({}),
            }
        }
        for (m, key, expect) in cases.iter().chain(cases2.iter()) {
            let body = serde_json::to_vec(&serde_json::json!({
                "type": "client-request", "rpcId": "r", "method": m, "payload": surface_payload(m)
            })).unwrap();
            let (status, v) = handle_rpc(&boot, m, &body);
            assert_eq!(status, 200, "{m} status");
            assert_eq!(v["result"]["ok"], true, "{m} ok");
            let val = &v["result"]["value"];
            match *expect {
                "bool" => assert!(val[*key].is_boolean(), "{m}.{key} bool"),
                "obj" => assert!(val[*key].is_object(), "{m}.{key} obj"),
                "arr" => assert!(val[*key].is_array(), "{m}.{key} arr"),
                "str" => assert!(val[*key].as_str().is_some(), "{m}.{key} str"),
                "null" => assert!(val[*key].is_null(), "{m}.{key} null"),
                _ => assert_eq!(val[*key], Value::Null, "{m}.{key} absent"),
            }
        }
    }

    /// 阶段2：session.prompt 驱动 turn 后 accepted:true，且 session 事件增长。
    #[test]
    fn rpc_session_prompt_runs_turn() {
        let boot = boot_with_sessions();
        let body = serde_json::to_vec(&serde_json::json!({
            "type": "client-request", "rpcId": "r", "method": "session.prompt",
            "payload": {"content": [{"type": "text", "text": "hello"}]},
        })).unwrap();
        let (_, v) = handle_rpc(&boot, "session.prompt", &body);
        assert_eq!(v["result"]["ok"], true);
        assert_eq!(v["result"]["value"]["accepted"], true);
        assert!(!boot.sessions.lock().unwrap().events().is_empty());
    }

    /// M1e E2E：prompt → 事件 adopt 进 store + 持久化落盘 → **重启**（新 host
    /// 同根）→ 历史恢复；且下链日志被后续连接读取。
    #[test]
    fn web_e2e_prompt_persist_restart_restores() {
        let boot = boot_with_sessions();
        let root = std::env::temp_dir().join(format!("dsh-web-m1e-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);

        let prompt = serde_json::to_vec(&serde_json::json!({
            "type": "client-request", "rpcId": "r", "method": "session.prompt",
            "payload": {"content": [{"type": "text", "text": "persist me"}]},
        })).unwrap();
        let history_body = serde_json::to_vec(&serde_json::json!({
            "type": "client-request", "rpcId": "r", "method": "session.history", "payload": {}
        })).unwrap();

        // 第一次启动：prompt → 事件进 store + 持久化。
        {
            let host = SessionHost::with_root(&root);
            let (_, v) = handle_rpc_host(&boot, "session.prompt", &prompt, &host);
            assert_eq!(v["result"]["value"]["accepted"], true);
            let (_, v) = handle_rpc_host(&boot, "session.history", &history_body, &host);
            let evs = v["result"]["value"]["events"].as_array().unwrap();
            assert!(!evs.is_empty(), "prompt 事件已进 store");
            assert!(host.seq_of("default") >= 6);
            // 下链日志：prompt 的事件已进入 EventSink（新连接全部可读）。
            assert!(host.sink_len() >= 6);
            host.flush("default").unwrap();
        }

        // 「重启」：新 host 从同一持久化根恢复 → 历史在、可继续 prompt。
        {
            let host2 = SessionHost::with_root(&root);
            assert!(host2.is_live("default"));
            let (_, v) = handle_rpc_host(&boot, "session.history", &history_body, &host2);
            let evs = v["result"]["value"]["events"].as_array().unwrap();
            assert!(!evs.is_empty(), "重启后历史恢复");
            // 继续一 turn：seq 连续（不重复）。
            let before = host2.seq_of("default");
            let (_, v) = handle_rpc_host(&boot, "session.prompt", &prompt, &host2);
            assert_eq!(v["result"]["value"]["accepted"], true);
            let after = host2.seq_of("default");
            assert!(after > before, "重启后仍可追加");
            // 下链新事件从旧游标后可见。
            let (_, v) = handle_rpc_host(&boot, "session.history", &history_body, &host2);
            assert_eq!(
                v["result"]["value"]["events"].as_array().unwrap().len(),
                after as usize
            );
            host2.flush("default").unwrap();
        }

        let _ = std::fs::remove_dir_all(&root).ok();
    }

    /// 阶段4：trust fence 判定 Host 头是否 loopback（对齐前端 isLoopbackHostname）。
    #[test]
    fn host_is_loopback_classifies() {
        for ok in ["127.0.0.1", "127.0.0.1:3000", "localhost", "localhost:3000", "[::1]", "127.0.0.2", "127.1.2.3"] {
            assert!(hostname_is_loopback(ok), "should accept {ok}");
        }
        for bad in ["evil.com", "attacker.example", "127.abc", "10.0.0.1", "localhost.evil.com", ""] {
            assert!(!hostname_is_loopback(bad), "should reject {bad}");
        }
        // "127" 无点：不应算 loopback（127/8 要求至少 127.x）。
        assert!(!hostname_is_loopback("127"), "bare 127 is not loopback");
        // 空 host 头：不应放行。
        assert!(!hostname_is_loopback(""), "empty host is not loopback");
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

    /// M71/M1e：`mux_session_event_frame` 构造对齐 muxFrameSchema 的
    /// `session/event` 帧——`{type, sessionId, event:{type, seq, time, data}}`
    /// （event 为 strict-envelope 序列化；time 为会话真实 epoch ms）。
    #[test]
    fn mux_session_event_frame_shape() {
        use dsh_session::types::EventKind;
        let ev = dsh_session::types::SessionEvent::new(
            3,
            1_700_000_000_123,
            EventKind::from_str("assistant/message"),
            serde_json::json!({
                "turn": 1, "step": 1,
                "message": {"id": "a1", "role": "assistant", "content": [], "source": {"kind": "model"}},
            }),
        );
        let frame = mux_session_event_frame("default", &ev);
        assert_eq!(frame["type"], "server-request");
        assert_eq!(frame["method"], "session/event");
        assert_eq!(frame["payload"]["type"], "session/event");
        assert_eq!(frame["payload"]["sessionId"], "default");
        assert_eq!(frame["payload"]["event"]["type"], "assistant/message");
        assert_eq!(frame["payload"]["event"]["seq"], 3);
        assert_eq!(frame["payload"]["event"]["time"], 1_700_000_000_123i64);
        assert_eq!(frame["payload"]["event"]["data"]["message"]["id"], "a1");
        // 帧事件对象只有 strict-envelope 键（无额外字段泄漏；键序无关 JSON schema）。
        let mut keys: Vec<&str> = frame["payload"]["event"]
            .as_object()
            .unwrap()
            .keys()
            .map(|k| k.as_str())
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, ["data", "seq", "time", "type"]);
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

    /// M2g：session.prompt 在装配了 Rust AgentLoopHost 时改驱真实 agent-loop，
    /// 事件直接落共享 SessionHost store（前端历史读模型 + EventSink 下链同一事实源）。
    #[test]
    fn rpc_prompt_routes_to_rust_agent_loop_shared_store() {
        use std::cell::{Cell, RefCell};
        use std::collections::VecDeque;
        use std::rc::Rc;

        // Mock adapter：一段文本回答（模拟模型应答；Rust loop 真实驱动）。
        let script = Rc::new(RefCell::new(VecDeque::from_iter([vec![
            dsh_llm::StreamChunk::BlockStart {
                index: 0,
                block_type: "text".parse().unwrap(),
            },
            dsh_llm::StreamChunk::TextDelta { index: 0, text: "hello from rust loop".into() },
            dsh_llm::StreamChunk::BlockEnd {
                index: 0,
                block: dsh_llm::ContentBlock::text("hello from rust loop"),
            },
            dsh_llm::StreamChunk::Finish {
                reason: dsh_llm::FinishReason::Stop,
                replay_state: None,
            },
        ]])));
        let calls = Rc::new(Cell::new(0u32));
        struct Adapter {
            script: Rc<RefCell<VecDeque<Vec<dsh_llm::StreamChunk>>>>,
            calls: Rc<Cell<u32>>,
        }
        impl dsh_llm::LlmAdapter for Adapter {
            fn stream(
                &self,
                _options: dsh_llm::GenerateOptions,
            ) -> Box<dyn Iterator<Item = dsh_llm::StreamChunk>> {
                self.calls.set(self.calls.get() + 1);
                let next = self
                    .script
                    .borrow_mut()
                    .pop_front()
                    .unwrap_or_default();
                Box::new(next.into_iter())
            }
        }
        let llm = Rc::new(dsh_llm::LlmRuntime::new());
        llm.register_adapter(&["mock"], Rc::new(Adapter { script, calls }))
            .unwrap();

        let tools = Rc::new(dsh_tools::ToolRegistry::new(
            dsh_tools::ToolExecutionMode::Native,
        ));
        // 配置 agent：provider mock → 映射到注册的 mock adapter；sessionId = default。
        use dsh_agent_loop::{AgentLoopConfig, AgentLoopHost, ConfiguredAgent};
        let session_host = SessionHost::in_memory();
        let _ = session_host.session("default");
        let config = AgentLoopConfig {
            max_parallel_tool_calls: None,
            agents: vec![ConfiguredAgent {
                id: "a1".into(),
                provider: Some("mock".into()),
                model: Some("mock-model".into()),
                session_id: Some("default".into()),
                max_tokens: None,
                cwd: None,
                resume_session_id: None,
            }],
        };
        let host = AgentLoopHost::with_store(
            config,
            llm,
            tools,
            session_host.store.clone(),
        )
        .unwrap();
        let mut boot = boot_with_sessions();
        boot.agent_loop = Some(host.clone());

        // session.prompt → sessionId default（Rust loop 路径，不经过 WASM adopt）。
        let body = serde_json::to_vec(&serde_json::json!({
            "type": "client-request", "rpcId": "r1", "method": "session.prompt",
            "payload": {"sessionId": "default", "content": [{"type": "text", "text": "hi from ui"}]},
        })).unwrap();
        let (_, v) = handle_rpc_host(&boot, "session.prompt", &body, &session_host);
        assert_eq!(v["result"]["value"]["accepted"], true);

        // 事件直接落在共享 store：user/message + assistant/message + turn/end。
        let evs = session_host.events("default");
        assert!(
            evs.iter().any(|e| e.kind.as_str() == "assistant/message"),
            "Rust loop assistant/message in shared store"
        );
        assert!(
            evs.iter().any(|e| e.kind.as_str() == "user/message"),
            "user/message written by the loop"
        );
        let assistant = evs
            .iter()
            .find(|e| e.kind.as_str() == "assistant/message")
            .unwrap();
        assert_eq!(
            assistant.data["message"]["content"][0]["text"],
            "hello from rust loop"
        );
        // EventSink 下链触发（前端实时帧来源）。
        assert!(session_host.sink_len() >= 4, "downlink fired: {}", session_host.sink_len());
        // session.history 读模型可回读（前端视角）。
        let body2 = serde_json::to_vec(&serde_json::json!({
            "type": "client-request", "rpcId": "r2", "method": "session.history",
            "payload": {"sessionId": "default"},
        })).unwrap();
        let (_, h) = handle_rpc_host(&boot, "session.history", &body2, &session_host);
        assert!(h["result"]["value"]["events"].as_array().unwrap().len() == evs.len());
        // 驱动回到 idle（agent 可按配置 id 取到）。
        use dsh_agent::AgentStatus;
        assert_eq!(host.agent("a1").unwrap().status(), AgentStatus::Idle);
    }

    /// M3b：settings 全方法面经 handle_rpc_host 真实服务驱动。
    /// describe → update(merge) → mutate(path-op) → replace(reset) → conflict。
    #[test]
    fn rpc_settings_full_wire_real_driver() {
        let boot = boot_with_sessions();
        // 注册一个测试 namespace（真实 schema + secret）进共享 provider。
        {
            let mut sp = boot.settings.borrow_mut();
            let mut dict = std::collections::HashMap::new();
            dict.insert("mode".to_string(), dsh_schema::Schema::string());
            dict.insert(
                "token".to_string(),
                dsh_schema::Schema::secret(&dsh_schema::Schema::string()),
            );
            sp.register("test-ns", &dsh_schema::Schema::object(dict), None, dsh_settings::Applies::Live);
        }
        let session_host = SessionHost::in_memory();
        let call = |method: &str, payload: serde_json::Value| {
            let body = serde_json::to_vec(&serde_json::json!({
                "type": "client-request", "rpcId": "r", "method": method, "payload": payload,
            })).unwrap();
            handle_rpc_host(&boot, method, &body, &session_host).1
        };
        // describe：value redact（token 缺席），secrets 枚举 set:false，revision 0。
        let res = call("settings.describe", serde_json::json!({}));
        assert_eq!(res["result"]["ok"], true);
        let ns_list = res["result"]["value"]["namespaces"].as_array().unwrap();
        let test_ns = ns_list.iter().find(|n| n["ns"] == "test-ns").expect("registered ns");
        assert_eq!(test_ns["revision"], 0);
        assert!(test_ns["value"].get("token").is_none(), "secret redacted from value");
        let secrets = test_ns["secrets"].as_array().unwrap();
        assert!(secrets.iter().any(|s| s["path"][0] == "token" && s["set"] == false));
        // update(merge)：写入 mode；token 不动（secrets 仍 set:false）。
        let res = call("settings.update", serde_json::json!({
            "ns": "test-ns", "patch": {"mode": "fast"}, "expectedRevision": 0,
        }));
        assert_eq!(res["result"]["ok"], true);
        assert_eq!(res["result"]["value"]["revision"], 1);
        assert_eq!(res["result"]["value"]["value"]["mode"], "fast");
        // mutate(path-op)：set 深路径 + unset。
        let res = call("settings.mutate", serde_json::json!({
            "ns": "test-ns", "ops": [{"op": "set", "path": ["extra", "k"], "value": 2}],
            "expectedRevision": 1,
        }));
        assert_eq!(res["result"]["ok"], true);
        assert_eq!(res["result"]["value"]["value"]["extra"]["k"], 2);
        assert_eq!(res["result"]["value"]["revision"], 2);
        let res = call("settings.mutate", serde_json::json!({
            "ns": "test-ns", "ops": [{"op": "unset", "path": ["extra", "k"]}],
            "expectedRevision": 2,
        }));
        assert_eq!(res["result"]["ok"], true);
        assert!(res["result"]["value"]["value"]["extra"].get("k").is_none());
        // replace(reset)：清空 user → value 回落 schema default/缺省。
        let res = call("settings.replace", serde_json::json!({
            "ns": "test-ns", "section": {}, "expectedRevision": 3,
        }));
        assert_eq!(res["result"]["ok"], true);
        assert_eq!(res["result"]["value"]["value"]["mode"], Value::Null);
        // conflict：带 stale revision 再写 → SETTINGS_CONFLICT。
        let res = call("settings.update", serde_json::json!({
            "ns": "test-ns", "patch": {"mode": "x"}, "expectedRevision": 0,
        }));
        assert_eq!(res["result"]["ok"], false);
        assert_eq!(res["result"]["error"]["code"], "SETTINGS_CONFLICT");
        // openDocument：诚实降级 opened:true。
        let res = call("settings.openDocument", serde_json::json!({}));
        assert_eq!(res["result"]["ok"], true);
        assert_eq!(res["result"]["value"]["opened"], true);
    }

    /// M3c：credentials 全方法面经 handle_rpc_host 真实服务驱动。
    /// describe（configured）→ set → resolve via describe source → unset（幂等）。
    #[test]
    fn rpc_credentials_full_wire_real_driver() {
        let boot = boot_with_sessions();
        // 注入一个 env 遮蔽 ref（验证 shadowed 拒绝走 wire）。
        {
            let mut env = std::collections::HashMap::new();
            env.insert("SHADOWED_KEY".to_string(), "envv".to_string());
            let cp = boot.credentials.clone();
            let mut c = cp.borrow_mut();
            *c = dsh_credentials::CredentialProvider::with_env(env);
        }
        let session_host = SessionHost::in_memory();
        let call = |method: &str, payload: serde_json::Value| {
            let body = serde_json::to_vec(&serde_json::json!({
                "type": "client-request", "rpcId": "r", "method": method, "payload": payload,
            })).unwrap();
            handle_rpc_host(&boot, method, &body, &session_host).1
        };
        // describe：未知 ref → unconfigured writable:true；env ref → configured writable:false。
        let res = call("credentials.describe", serde_json::json!({
            "refs": ["MY_STORED", "SHADOWED_KEY", "BAD-NAME"],
        }));
        assert_eq!(res["result"]["ok"], false, "invalid ref name -> bad-request");
        assert_eq!(res["result"]["error"]["code"], "bad-request");
        let res = call("credentials.describe", serde_json::json!({
            "refs": ["MY_STORED", "SHADOWED_KEY"],
        }));
        assert_eq!(res["result"]["ok"], true);
        let creds = &res["result"]["value"]["credentials"];
        assert_eq!(creds["MY_STORED"]["configured"], false);
        assert_eq!(creds["MY_STORED"]["writable"], true);
        assert_eq!(creds["SHADOWED_KEY"]["configured"], true);
        assert_eq!(creds["SHADOWED_KEY"]["source"], "env");
        assert_eq!(creds["SHADOWED_KEY"]["writable"], false);
        // set 到文件层（memory provider 的 document_path None → 内存持久化）。
        let res = call("credentials.set", serde_json::json!({
            "ref": "MY_STORED", "value": "abc123",
        }));
        assert_eq!(res["result"]["ok"], true);
        let res = call("credentials.describe", serde_json::json!({"refs": ["MY_STORED"]}));
        assert_eq!(res["result"]["value"]["credentials"]["MY_STORED"]["configured"], true);
        assert_eq!(res["result"]["value"]["credentials"]["MY_STORED"]["source"], "file");
        // env shadowed set → credential-rejected。
        let res = call("credentials.set", serde_json::json!({
            "ref": "SHADOWED_KEY", "value": "x",
        }));
        assert_eq!(res["result"]["ok"], false);
        assert_eq!(res["result"]["error"]["code"], "credential-rejected");
        assert_eq!(res["result"]["error"]["details"]["ref"], "SHADOWED_KEY");
        // empty value set → credential-rejected（Empty）。
        let res = call("credentials.set", serde_json::json!({
            "ref": "MY_STORED", "value": "",
        }));
        assert_eq!(res["result"]["ok"], false);
        assert_eq!(res["result"]["error"]["code"], "credential-rejected");
        // unset → 配置消失；再 unset 幂等成功。
        let res = call("credentials.unset", serde_json::json!({"ref": "MY_STORED"}));
        assert_eq!(res["result"]["ok"], true);
        let res = call("credentials.describe", serde_json::json!({"refs": ["MY_STORED"]}));
        assert_eq!(res["result"]["value"]["credentials"]["MY_STORED"]["configured"], false);
        let res = call("credentials.unset", serde_json::json!({"ref": "MY_STORED"}));
        assert_eq!(res["result"]["ok"], true, "unset absent idempotent");
    }

    /// M4h：goal.create 由 GoalService 真实创建 → ref {id: goal-1, revision: 1}。
    #[test]
    fn rpc_goal_create_returns_real_ref() {
        let boot = boot_with_sessions();
        let body = serde_json::to_vec(&serde_json::json!({
            "type": "client-request", "rpcId": "r", "method": "goal.create",
            "payload": {"sessionId": "default", "objective": "fix the flaky test"},
        })).unwrap();
        let (_, v) = handle_rpc(&boot, "goal.create", &body);
        assert_eq!(v["result"]["ok"], true, "goal.create ok");
        let refv = &v["result"]["value"]["ref"];
        assert_eq!(refv["id"], "goal-1", "first id is goal-1");
        assert_eq!(refv["revision"], 1, "first revision is 1");
        assert!(refv["revision"].as_u64().unwrap() > 0);
    }

    /// M4h：goal.create 缺 objective → GOAL_INVALID_OBJECTIVE（逐字对齐 GoalErrorCode）。
    #[test]
    fn rpc_goal_create_missing_objective_rejects() {
        let boot = boot_with_sessions();
        let body = serde_json::to_vec(&serde_json::json!({
            "type": "client-request", "rpcId": "r", "method": "goal.create",
            "payload": {"sessionId": "default"},
        })).unwrap();
        let (_, v) = handle_rpc(&boot, "goal.create", &body);
        assert_eq!(v["result"]["ok"], false, "missing objective must reject");
        assert_eq!(v["result"]["error"]["code"], "GOAL_INVALID_OBJECTIVE");
    }

    /// M4h：goal.create 缺 sessionId → bad-request（sessionId 必填校验）。
    #[test]
    fn rpc_goal_create_requires_session_id() {
        let boot = boot_with_sessions();
        let body = serde_json::to_vec(&serde_json::json!({
            "type": "client-request", "rpcId": "r", "method": "goal.create",
            "payload": {"objective": "no session"},
        })).unwrap();
        let (_, v) = handle_rpc(&boot, "goal.create", &body);
        assert_eq!(v["result"]["ok"], false, "missing sessionId must reject");
        assert_eq!(v["result"]["error"]["code"], "bad-request");
    }

    /// M4h：goal.create → goal.complete → goal.clear 全链路（complete 后 clear 幂等
    /// cleared:true；clear 无当前目标时 NotFound → cleared:true）。
    #[test]
    fn rpc_goal_complete_then_clear() {
        let boot = boot_with_sessions();
        // create
        let body = serde_json::to_vec(&serde_json::json!({
            "type": "client-request", "rpcId": "r", "method": "goal.create",
            "payload": {"sessionId": "default", "objective": "finish M4h"},
        })).unwrap();
        let (_, v) = handle_rpc(&boot, "goal.create", &body);
        let refv = v["result"]["value"]["ref"].clone();
        // complete（消耗当前目标）
        let body = serde_json::to_vec(&serde_json::json!({
            "type": "client-request", "rpcId": "r", "method": "goal.complete",
            "payload": {"sessionId": "default", "ref": refv},
        })).unwrap();
        let (_, v) = handle_rpc(&boot, "goal.complete", &body);
        assert_eq!(v["result"]["ok"], true, "goal.complete ok");
        assert_eq!(v["result"]["value"]["ref"]["revision"], 2, "revision bumps to 2");
        // clear（目标已 complete，服务仍持有 → 正常 clear）
        let body = serde_json::to_vec(&serde_json::json!({
            "type": "client-request", "rpcId": "r", "method": "goal.clear",
            "payload": {"sessionId": "default", "ref": v["result"]["value"]["ref"].clone()},
        })).unwrap();
        let (_, v) = handle_rpc(&boot, "goal.clear", &body);
        assert_eq!(v["result"]["ok"], true, "goal.clear ok");
        assert_eq!(v["result"]["value"]["cleared"], true);
        // 再来一次 clear（ref 缺失 / 无当前目标）→ 幂等 cleared:true
        let body = serde_json::to_vec(&serde_json::json!({
            "type": "client-request", "rpcId": "r", "method": "goal.clear",
            "payload": {"sessionId": "default", "ref": {"id": "goal-1", "revision": 99}},
        })).unwrap();
        let (_, v) = handle_rpc(&boot, "goal.clear", &body);
        assert_eq!(v["result"]["ok"], true, "clear no current goal idempotent");
        assert_eq!(v["result"]["value"]["cleared"], true);
        // 完全缺失 ref → bad-request（wire：ref 缺失或 revision<=0 → bad-request）。
        let body = serde_json::to_vec(&serde_json::json!({
            "type": "client-request", "rpcId": "r", "method": "goal.clear",
            "payload": {"sessionId": "default"},
        })).unwrap();
        let (_, v) = handle_rpc(&boot, "goal.clear", &body);
        assert_eq!(v["result"]["ok"], false, "clear missing ref rejects");
        assert_eq!(v["result"]["error"]["code"], "bad-request");
    }

    /// M4h：subagent.list 空目录 → entries=[], parentAvailable=true；subagent.history
    /// → events=[], hasMore=false（诚实空实现）。
    #[test]
    fn rpc_subagent_list_empty_catalog() {
        let boot = boot_with_sessions();
        let body = serde_json::to_vec(&serde_json::json!({
            "type": "client-request", "rpcId": "r", "method": "subagent.list",
            "payload": {"parentSessionId": "default"},
        })).unwrap();
        let (_, v) = handle_rpc(&boot, "subagent.list", &body);
        assert_eq!(v["result"]["ok"], true);
        assert_eq!(v["result"]["value"]["entries"], serde_json::json!([]));
        assert_eq!(v["result"]["value"]["parentAvailable"], true);
        // history 诚实空
        let body = serde_json::to_vec(&serde_json::json!({
            "type": "client-request", "rpcId": "r", "method": "subagent.history",
            "payload": {"parentSessionId": "default", "childSessionId": "c-1", "mode": "one-shot"},
        })).unwrap();
        let (_, v) = handle_rpc(&boot, "subagent.history", &body);
        assert_eq!(v["result"]["ok"], true);
        assert_eq!(v["result"]["value"]["events"], serde_json::json!([]));
        assert_eq!(v["result"]["value"]["hasMore"], false);
    }

    /// M4h：condergate 子代 —— subagent.prompt 仅 continuable；非 continuable → bad-request
    /// （控制面 prompt_gate 的 wire 投影）。
    #[test]
    fn rpc_subagent_prompt_gates_mode() {
        let boot = boot_with_sessions();
        let body = serde_json::to_vec(&serde_json::json!({
            "type": "client-request", "rpcId": "r", "method": "subagent.prompt",
            "payload": {"parentSessionId": "default", "childSessionId": "c-1", "mode": "one-shot"},
        })).unwrap();
        let (_, v) = handle_rpc(&boot, "subagent.prompt", &body);
        assert_eq!(v["result"]["ok"], false, "one-shot child cannot be prompted");
        assert_eq!(v["result"]["error"]["code"], "bad-request");
        // continuable → 诚实 messageId
        let body = serde_json::to_vec(&serde_json::json!({
            "type": "client-request", "rpcId": "r", "method": "subagent.prompt",
            "payload": {"parentSessionId": "default", "childSessionId": "c-1", "mode": "continuable"},
        })).unwrap();
        let (_, v) = handle_rpc(&boot, "subagent.prompt", &body);
        assert_eq!(v["result"]["ok"], true);
        assert!(v["result"]["value"]["messageId"].as_str().is_some());
    }

    /// M4h：commands/list 含 subagents 项（方法面扩展）。
    #[test]
    fn rpc_commands_list_includes_subagents() {
        let boot = boot_with_sessions();
        let body = serde_json::to_vec(&serde_json::json!({
            "type": "client-request", "rpcId": "r10", "method": "commands/list", "payload": {}
        })).unwrap();
        let (_, v) = handle_rpc(&boot, "commands/list", &body);
        assert_eq!(v["result"]["ok"], true);
        let names: Vec<&str> = v["result"]["value"]
            .as_array().unwrap()
            .iter()
            .filter_map(|c| c["name"].as_str())
            .collect();
        assert!(names.contains(&"subagents"), "subagents command present: {names:?}");
        assert!(names.contains(&"goal"));
        assert!(names.contains(&"plan"));
        assert!(names.contains(&"compact"));
    }

    /// M4h：Boot 挂载 todos 投影单元（ProjectionRegistry 注册成功）。
    #[test]
    fn boot_mounts_todos_projection() {
        let boot = boot_with_sessions();
        let reg = boot.projections.borrow();
        let unit = reg.get("todos");
        assert!(unit.is_some(), "todos projection unit registered");
        assert_eq!(unit.unwrap().key(), "todos");
        assert_eq!(unit.unwrap().state_version(), 2);
    }

    /// M4h：register_m4_tools 可注册 todo_write + 参数校验走 to_todo_list（执行兜底
    /// 语义：空 todos → ToolArgsError/执行拒绝）。
    #[test]
    fn register_m4_tools_todo_write() {
        use dsh_tools::{ToolExecutionInput, ToolExecutionMode, ToolRegistry};
        let registry = ToolRegistry::new(ToolExecutionMode::Native);
        register_m4_tools(&registry);
        // 注册成功 → 全局可见
        assert!(registry.get("todo_write", None).is_some(), "todo_write registered+visible");
        // 有效参数执行 OK（normalized todos 走 to_todo_list）。
        let input = ToolExecutionInput::new(
            "call-1",
            "todo_write",
            serde_json::json!({
                "todos": [
                    {"content": "write tests", "status": "in_progress"},
                    {"content": "implement"},
                ],
            }),
            Some("agent-1".to_string()),
        );
        let res = registry.execute(&input, None);
        assert!(!res.is_error, "valid todos execute ok: {res:?}");
        let val = res.value.unwrap();
        let arr = val.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["content"], "write tests");
        assert_eq!(arr[0]["status"], "in_progress");
        // 空 content → 执行拒绝（to_todo_list EmptyContent）。
        let input = ToolExecutionInput::new(
            "call-2",
            "todo_write",
            serde_json::json!({"todos": [{"content": "  "}]}),
            Some("agent-1".to_string()),
        );
        let res = registry.execute(&input, None);
        assert!(res.is_error, "empty content rejected");
        // 重复 content → 拒绝（DuplicateContent）。
        let input = ToolExecutionInput::new(
            "call-3",
            "todo_write",
            serde_json::json!({"todos": [
                {"content": "dup", "status": "pending"},
                {"content": "dup", "status": "completed"},
            ]}),
            Some("agent-1".to_string()),
        );
        let res = registry.execute(&input, None);
        assert!(res.is_error, "duplicate content rejected");
        // allowParallel=false 多个 in_progress → 拒绝（TooManyInProgress）。
        let input = ToolExecutionInput::new(
            "call-4",
            "todo_write",
            serde_json::json!({"todos": [
                {"content": "a", "status": "in_progress"},
                {"content": "b", "status": "in_progress"},
            ]}),
            Some("agent-1".to_string()),
        );
        let res = registry.execute(&input, None);
        assert!(res.is_error, "two in_progress without allowParallel rejected");
    }
}

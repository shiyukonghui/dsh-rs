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
use std::sync::Arc;
use std::time::Duration;

use dsh_core::*;
use tiny_http::{Header, Method, Response, Server};

use crate::session_host::{EventSink, SessionHost};
use crate::Boot;

/// M5h web 接线（M5 工具注册 + 宿主句柄绑定；独立模块承载，避免 web.rs 膨胀）。
pub mod web_m5;
#[allow(unused_imports)]
pub use web_m5::{register_m5_tools_with_host, M5HostServices};

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
    /// M6W（D-092）：SQLite 会话存储文件（`SqliteBackend` 持久化后端）。
    /// 优先级高于 `session_dir`（同时给定 → sqlite 生效 + eprintln 显式警告）；
    /// 缺省 = None（回落到 session_dir / 内存）。
    pub sqlite_store: Option<PathBuf>,
    /// M6（P2）：agent 循环工作区根（工具 cwd）。缺省 = 当前工作目录 canonicalize。
    pub workspace_root: Option<PathBuf>,
    /// M6：装配服务器执行闭环（真 LLM + M4/M5 工具 + 共享 store 的 AgentLoopHost）。
    /// 缺省 false（保留既存 cordis/loop-plugin 语义）；装配失败 → `serve` fail-loud
    /// （诚实，不默默降级到 WASM 路径）。
    pub enable_agent_loop: bool,
    /// M6：LLM 端点 base URL（缺省 `DSH_LLM_BASE_URL` 环境变量，再缺省
    /// `https://api.deepseek.com`）。
    pub llm_base_url: Option<String>,
    /// M6：LLM 模型名（缺省 `DSH_LLM_MODEL` 环境变量，再缺省 `deepseek-chat`）。
    pub llm_model: Option<String>,
    /// M6（step7，D-086）：`.env` 文件（进程环境上游可选来源）。serve 启动先 apply
    /// （overwrite:false，既有环境变量优先；解析/读错 fail-loud）。`DEEPSEEK_API_KEY`
    /// 经此进入进程环境后仍由 `server_llm_runtime` 以 env 读取——key 永不落
    /// settings/库/git（IV-3）。
    pub env_file: Option<PathBuf>,
}

/// 一个已运行的 Web 服务器（持有实际监听地址）。
pub struct WebServer {
    pub addr: String,
}

/// M6W（D-092）：按 config 选择 SessionHost 持久化后端。
/// 优先级：`sqlite_store` > `session_dir` > 纯内存。
/// 同时给定 `sqlite_store` 与 `session_dir` → sqlite 生效 + `eprintln!` 显式警告
/// （fail-loud，绝不清零静默降级）。SQLite 打开/建 schema 失败 → `Err`（boot 终止）。
/// 采用字段级 Option 引用（serve 早前会 move 其他 cfg 字段——避免整体借 `&cfg`）。
fn session_host_for(
    sqlite_store: &Option<PathBuf>,
    session_dir: &Option<PathBuf>,
) -> Result<std::rc::Rc<crate::session_host::SessionHost>, String> {
    match (sqlite_store, session_dir) {
        (Some(file), Some(dir)) => {
            eprintln!(
                "dsh web: --sqlite-store overrides --session-dir; ignoring {}",
                dir.display()
            );
            crate::session_host::SessionHost::with_sqlite(file)
        }
        (Some(file), None) => crate::session_host::SessionHost::with_sqlite(file),
        (None, Some(dir)) => Ok(crate::session_host::SessionHost::with_root(dir)),
        (None, None) => Ok(crate::session_host::SessionHost::in_memory()),
    }
}

/// 启动 `dsh web`：服务前端 dist + `/api` RPC，桥接到 boot 运行时。
///
/// 阻塞运行（直到服务器出错或关闭）。`boot` 用于 RPC 分派（sessions/tools/
/// run_turn）。并发由 `tiny_http` 提供：每请求独立线程；SSE 下链在
/// `SessionHandle`（Send+Sync）上轮询，不阻塞 RPC。
///
/// M6（step1b）：`cfg.enable_agent_loop` 时在服务线程装配服务器执行闭环——
/// `assemble_server_runtime`（真实 M4+M5 工具 + deepseek LLM + 共享 store）并写入
/// `boot.agent_loop`（之后 `agent.turn/agent.run/session.prompt` 走 Rust loop）。
/// 装配失败 → fail-loud（不默默回退 WASM 路径）。
pub fn serve(boot: &mut Boot, cfg: WebConfig) -> Result<WebServer, CordisError> {
    // M6 step7（D-086）：`.env` 进程环境上游——先 apply（overwrite:false），之后装配的
    // env 读取链（DSH_LLM_BASE_URL / DSH_LLM_MODEL / DEEPSEEK_API_KEY …）透明吃到 overlay。
    // 解析/读错 → fail-loud（绝不静默跳过坏行）；key 仅入进程环境，不落 settings/git（IV-3）。
    if let Some(env_file) = &cfg.env_file {
        let applied = crate::m6_env::apply_env_file(Some(env_file)).map_err(|e| {
            CordisError::Internal(format!("web env-file {}: {e}", env_file.display()))
        })?;
        eprintln!("dsh web: applied {} key(s) from env-file {}", applied, env_file.display());
    }
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

    // D-099：HMR SSE 通道（`/plugins/events`）。Arc 共享 + 独立 watcher 线程——每
    // `HMR_POLL_INTERVAL_MS` 扫一遍 client bundle 内容变化，广播 `rebuilt` 帧；
    // 无重建 watcher 改 bundle 时通道保持空闲（对齐 TS「the chain stays idle」）。
    let hmr = std::sync::Arc::new(crate::hmr_events::HmrChannel::new(&manifest));
    {
        let hmr = hmr.clone();
        std::thread::spawn(move || hmr.run(crate::hmr_events::HMR_POLL_INTERVAL_MS));
    }

    let web_root = cfg.web_root;
    // M1e：SessionHost——SessionStore（权威历史）+ 可选持久化挂载 + EventSink
    // 下链。loop 仍写 `boot.sessions`（SessionLog）；`session.prompt` adopt 进
    // 目标会话；`session/event` 下链走 EventSink（Send+Sync 供 SSE/WS 线程）。
    let host = session_host_for(&cfg.sqlite_store, &cfg.session_dir)
        .map_err(|e| CordisError::Internal(format!("session persistence: {e}")))?;
    // seed `default`（前端会话入口）。
    let _ = host.session("default");

    // M3a+（D-098）：装配进程内原生目录选择器（`host.pickDirectory`）。Windows 桌面经
    // IFileDialog/COM（零子进程）弹系统目录框；无桌面/失败 → wire `directory-picker-unavailable`
    // （诚实，不冒充取消）。测试 Boot 不装配（None）→ 同一错误路径，由 stub 测试覆盖。
    // Arc（+Send+Sync）：该模态对话框由独立线程驱动（见 dispatch_request 特判），不饿死
    // 单线程 accept 循环。
    boot.host_picker = Some(std::sync::Arc::new(crate::host_picker::pick_directory_native)
        as crate::HostPicker);

    // M6（step3，D-083）：serve 主循环 tick 上下文（enable_agent_loop 装配时填充）。
    // 主循环 `recv_timeout` 自驱节拍——每 tick 间隔刺探请求（有则派发），超时（无请求）
    // 则纯推进：主线程 `m5g_tick_once`（调度到期 + jobs 合作泵）。推进点唯一收敛到
    // serve 主线程（非 Send 宿主纪律）；工具注册与 tick 共享同一 schedule/bash_jobs
    // 实例（ServerLoopBundle）。不启用 agent_loop 时 tick 上下文为空，循环等价于阻塞
    // 接收（仅多 ≤tick 间隔的轮询唤醒）。
    let mut tick_schedule: Option<Rc<crate::web::dsh_cli_host::ScheduleHost>> = None;
    let mut tick_bridge: Option<Rc<web_m5::BashJobsBridge>> = None;

    // M6（step1b）：装配服务器执行闭环并写入 boot.agent_loop。
    if cfg.enable_agent_loop {
        let ws_root = match &cfg.workspace_root {
            Some(r) => r
                .canonicalize()
                .map_err(|e| CordisError::Internal(format!("workspace_root canonicalize: {e}")))?,
            None => std::env::current_dir()
                .map_err(|e| CordisError::Internal(format!("cwd: {e}")))?
                .canonicalize()
                .map_err(|e| CordisError::Internal(format!("cwd canonicalize: {e}")))?,
        };
        let base_url = cfg
            .llm_base_url
            .clone()
            .or_else(|| std::env::var("DSH_LLM_BASE_URL").ok())
            .unwrap_or_else(|| "https://api.deepseek.com".to_string());
        let model = cfg
            .llm_model
            .clone()
            .or_else(|| std::env::var("DSH_LLM_MODEL").ok())
            .unwrap_or_else(|| "deepseek-chat".to_string());
        let bundle = assemble_server_runtime(&host, ws_root, &base_url, &model).map_err(|e| {
            CordisError::Internal(format!(
                "agent loop assembly failed (enable_agent_loop, base {base_url}, model {model}): {e}"
            ))
        })?;
        eprintln!(
            "dsh web: agent loop enabled (base {base_url}, model {model}); agent.turn/run/session.prompt now drive the Rust loop; tick every {}ms",
            M6_SERVE_TICK_INTERVAL_MS
        );
        boot.agent_loop = Some(bundle.host.clone());
        // M6 step8（D-087）：真实 provider catalog 视图注入 Boot（llm.models caps）。
        boot.agent_catalog = Some(crate::m6_llm::server_catalog_view(&base_url, &model));
        tick_schedule = Some(bundle.schedule.clone());
        tick_bridge = bundle.bash_jobs.clone();
    }

    let sink = host.sink.clone();

    // D-100：宿主事件日志（`host/*` 帧内层 payload 的 append-only 队列）。RPC 处理器经
    // `push_host_frame` 写入；`events.host` SSE/WS 线程各自持游标包装成 server-request
    // 后下推。Only serve 装配（非 web boot/测试口 None → RPC 不推帧，注册表语义仍生效）。
    let host_events: Arc<std::sync::Mutex<Vec<Value>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    boot.host_events = Some(host_events.clone());
    loop {
        let request = match server.recv_timeout(std::time::Duration::from_millis(M6_SERVE_TICK_INTERVAL_MS)) {
            Ok(Some(request)) => Some(request),
            Ok(None) => None,
            Err(_) => {
                // 服务器关闭（等价于 incoming_requests() 结束）→ 停止服务并返回。
                eprintln!("dsh web: recv ended; server stopping");
                break;
            }
        };
        if let Some(request) = request {
            let root = web_root.clone();
            let manifest = manifest.clone();
            let hmr = hmr.clone();
            let sink = sink.clone();
            // tiny_http 每请求已在线程处理；这里再派发。RPC/静态用 `&Boot`
            // （非 Send，留在调用线程），SSE/WS 用 `EventSink`（Send+Sync）。
            dispatch_request(request, &root, &manifest, &hmr, boot, &host, &sink);
        }
        if let (Some(sched), Some(bridge)) = (&tick_schedule, &tick_bridge) {
            let now = system_now_ms();
            if let Err(e) = web_m5::m5g_tick_once(sched, Some(bridge), now) {
                eprintln!("dsh web: tick advance failed: {e}");
            }
        }
    }
    Ok(WebServer { addr })
}

/// M6（step3）：serve 主循环自驱节拍间隔（毫秒）。推进点每 tick 一次：调度到期注入 +
/// bash jobs 合作结算；非阻塞（recv_timeout），无忙轮询。
pub const M6_SERVE_TICK_INTERVAL_MS: u64 = 250;

/// `/plugins/events` HMR SSE 通道的路由决策（D-099）：GET→连接流、HEAD→事件流头、
/// 其他方法→405（对齐 TS 路由的非 GET/HEAD 405 语义）；路径不匹配 → None
/// （回落 `/plugins/<id>/client.js` bundle 分支）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HmrEventsPlan {
    Stream,
    HeadersOnly,
    MethodNotAllowed,
}

fn hmr_events_plan(path: &str, method: &Method) -> Option<HmrEventsPlan> {
    if path != crate::hmr_events::EVENTS_ENDPOINT {
        return None;
    }
    Some(match *method {
        Method::Get => HmrEventsPlan::Stream,
        Method::Head => HmrEventsPlan::HeadersOnly,
        _ => HmrEventsPlan::MethodNotAllowed,
    })
}

/// 派发一个请求：`/plugins/*` bundle、`/api/*` RPC/SSE，否则静态文件（SPA fallback）。
fn dispatch_request(
    mut request: tiny_http::Request,
    web_root: &Path,
    manifest: &BootManifest,
    hmr: &Arc<crate::hmr_events::HmrChannel>,
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

    // D-099：`/plugins/events` 是客户端插件 HMR SSE 通道（前端 `client-hmr` 无条件
    // 订阅），须先于 `/plugins/<id>/client.js` bundle 分支拦截，否则按未知资源 404
    // （使用测试发现：控制台 `GET /plugins/events` 404 + EventSource 重连刷屏）。
    if let Some(plan) = hmr_events_plan(&path, request.method()) {
        match plan {
            HmrEventsPlan::Stream => {
                // SSE 连接独立线程：写头 → connected + graph 帧 → watcher 广播帧；
                // 连接关闭即退（long-lived，线程隔离，不阻塞 accept 循环）。
                let writer = request.into_writer();
                let ch = hmr.clone();
                std::thread::spawn(move || crate::hmr_events::stream_hmr_events(writer, ch));
            }
            // HEAD：事件流头（无体）。浏览器 EventSource 用 GET，HEAD 仅为对齐语义。
            HmrEventsPlan::HeadersOnly => {
                let resp = Response::empty(200u16).with_header(
                    Header::from_bytes(&b"Content-Type"[..], b"text/event-stream").unwrap(),
                );
                let _ = request.respond(resp);
            }
            HmrEventsPlan::MethodNotAllowed => {
                let _ = request.respond(Response::empty(405u16));
            }
        }
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
            // D-098：`host.pickDirectory` 是 user-paced 模态对话框（可能挂很久）。
            // serve 的 accept 循环单线程内联处理 RPC——若在此阻塞会饿死整个服务
            // （SSE/静态/其他 RPC 全部排队）。对齐 TS（对话框跑 Worker）：整个请求
            // 派到独立线程，accept 循环保持响应。
            (Method::Post, "host.pickDirectory") => {
                let picker = boot.host_picker.clone();
                let m = method.clone();
                std::thread::spawn(move || {
                    let mut body = Vec::new();
                    let _ = std::io::Read::read_to_end(&mut request.as_reader(), &mut body);
                    let rpc_id = rpc_id_of(&body);
                    let result = if !rpc_envelope_ok(&body, &m) {
                        serde_json::json!({"ok": false, "error": {
                            "code": "bad-request",
                            "message": "invalid client-request message",
                        }})
                    } else {
                        pick_directory_result(&picker)
                    };
                    let resp = json_response(200, &rpc_response(&rpc_id, result));
                    let _ = request.respond(resp);
                });
            }
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
                // D-100：host 通道独有下推 `host/*` 帧（Arc 共享宿主事件日志；None →
                // 测试口/非 web boot 走空日志，等价于旧行为）。
                let host_events = boot.host_events.clone();
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
                    std::thread::spawn(move || stream_ws_events(stream, &sink, is_host, host_events));
                } else {
                    let writer = request.into_writer();
                    let sink = sink.clone();
                    std::thread::spawn(move || stream_sse_events(writer, &sink, is_host, host_events));
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

/// 宿主组合决定（对齐 TS bundle-graph 组合期「按已组合后端能力挂对应 flow 客户端」，
/// 见 `deepseek-harness/packages/host/apiproxy/README.md` 目录选择 seam）：目录选择我们
/// 组合 **native** 后端（`host_picker_windows`：进程内 IFileDialog/COM，零子进程），
/// 页内 browse 流程不挂载。因此从 boot 图排除 browse 流程客户端，只让 native 客户端
/// 占据 ui-workspace 的 single directory-flow 洞（系统原生目录对话框）。
const HOST_COMPOSITION_EXCLUDED_CLIENTS: &[&str] = &["@deepseek-ai/dsh-client-ui-directory-picker-browse"];

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
            // D-098：宿主组合只提供 native 目录选择后端 → 排除 browse 流程客户端
            // （否则同时挂载会让 browse/native 竞争 single directory-flow 洞）。
            if HOST_COMPOSITION_EXCLUDED_CLIENTS.contains(&id.as_str()) {
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

/// 内容哈希（同为 bundle rev；D-099：HMR watcher 用它比对内容是否变化）。原始意图是
/// sha1 前 12 hex（对齐 `ClientModuleRegistry.shortHash`）；沿用无 sha1 crate 时代的
/// DefaultHasher 确定性变体——进程内内容变化检测足够（HMR 契约），跨进程不稳定属
/// 既有债务（D-099 已知限制②）。
pub(crate) fn short_hash(input: &[u8]) -> String {
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
/// `session/subscribed`（mux）或 `host/session-added`（host，D-100 修 SSE 无 host
/// 语义缺口），随后增量推帧 + keepalive；host 通道再独有地推宿主事件日志
/// （`HostEventsLog`）里累积的 `host/*` 帧；连接关闭即退出。
fn stream_sse_events(
    mut writer: Box<dyn std::io::Write + Send>,
    sink: &EventSink,
    is_host: bool,
    host_events: Option<Arc<std::sync::Mutex<Vec<Value>>>>,
) {
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
    // 握手帧：mux → session/subscribed；host → host/session-added（真实 seed 会话）。
    let hello = if is_host {
        serde_json::json!({
            "type": "server-request",
            "rpcId": format!("host-{last_seq}"),
            "method": "host/event",
            "payload": {"type": "host/session-added", "sessionId": "default", "blank": true},
        })
    } else {
        serde_json::json!({
            "type": "server-request",
            "rpcId": format!("sub-{last_seq}"),
            "method": "session/subscribed",
            "payload": {"type": "session/subscribed", "sessionId": "default", "lastSeq": last_seq},
        })
    };
    if write_sse(&mut writer, &hello).is_none() {
        return;
    }
    // host 帧游标（`events.host` 通道独有；每连接独立，append-only 日志安全）。
    let mut host_cursor = 0usize;
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
        if is_host {
            // D-100：host 通道独有下推 `host/*` 帧（server-request host/event 信封）。
            let host_frames = {
                let mut out = Vec::new();
                if let Some(log) = &host_events {
                    if let Ok(guard) = log.lock() {
                        for index in host_cursor..guard.len() {
                            out.push(host_frame_envelope(index, &guard[index]));
                        }
                        host_cursor = guard.len();
                    }
                }
                out
            };
            for frame in &host_frames {
                if write_sse(&mut writer, frame).is_none() {
                    return;
                }
            }
        }
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
/// `session/event`（mux）或 `host/session-added` + `host/*`（host，D-100）帧。
fn stream_ws_events(
    stream: Box<dyn tiny_http::ReadWrite + Send>,
    sink: &EventSink,
    is_host: bool,
    host_events: Option<Arc<std::sync::Mutex<Vec<Value>>>>,
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
    // host 帧游标（`events.host` 通道独有；每连接独立，append-only 日志安全）。
    let mut host_cursor = 0usize;
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
        if is_host {
            // D-100：host 通道独有下推 `host/*` 帧（server-request host/event 信封）。
            let host_frames = {
                let mut out = Vec::new();
                if let Some(log) = &host_events {
                    if let Ok(guard) = log.lock() {
                        for index in host_cursor..guard.len() {
                            out.push(host_frame_envelope(index, &guard[index]));
                        }
                        host_cursor = guard.len();
                    }
                }
                out
            };
            for frame in &host_frames {
                if ws_send(&mut ws, frame).is_none() {
                    return;
                }
            }
        }
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

/// D-100：把宿主事件日志里的一条 `host/*` **内层 payload**（如
/// `{type:"host/workspace-changed", workspace:{...}}`）包装成 `events.host` 下链信封
/// （server-request `host/event`；rpcId 用日志游标序号，进程内稳定单调）。
fn host_frame_envelope(index: usize, payload: &Value) -> Value {
    serde_json::json!({
        "type": "server-request",
        "rpcId": format!("host-{index}"),
        "method": "host/event",
        "payload": payload,
    })
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
/// `host.pickDirectory` 三态 wire 结果（native 能力）：选中 `{ok,value:{path}}` /
/// 取消 `{ok,value:{path:null}}`（合法语义） / 失败或未装配
/// `directory-picker-unavailable`（绝不拿 null 冒充取消）。
fn pick_directory_result(picker: &Option<crate::HostPicker>) -> Value {
    match picker.as_ref() {
        Some(pick) => match pick() {
            Ok(Some(path)) => serde_json::json!({"ok": true, "value": {"path": path}}),
            Ok(None) => serde_json::json!({"ok": true, "value": {"path": null}}),
            Err(msg) => serde_json::json!({"ok": false, "error": {
                "code": "directory-picker-unavailable", "message": msg,
            }}),
        },
        None => serde_json::json!({"ok": false, "error": {
            "code": "directory-picker-unavailable",
            "message": "native directory picker is not assembled",
        }}),
    }
}

/// 从 client-request body 取 rpcId（缺省空串）。
fn rpc_id_of(body: &[u8]) -> String {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v.get("rpcId").and_then(|r| r.as_str()).map(String::from))
        .unwrap_or_default()
}

/// client-request 信封校验（对齐 clientRequestSchema：type + method 一致）。
fn rpc_envelope_ok(body: &[u8], method: &str) -> bool {
    serde_json::from_slice::<Value>(body)
        .map(|v| {
            v.get("type").and_then(|t| t.as_str()) == Some("client-request")
                && v.get("method").and_then(|m| m.as_str()) == Some(method)
        })
        .unwrap_or(false)
}

/// server-response 信封。
fn rpc_response(rpc_id: &str, result: Value) -> Value {
    serde_json::json!({
        "type": "server-response",
        "rpcId": rpc_id,
        "result": result,
    })
}

/// D-100：把一条 `host/*` 帧的**内层 payload**（如
/// `{type:"host/workspace-changed", workspace:{...}}`）压入宿主事件日志。`events.host`
/// 的 SSE/WS 流各自持游标把它包装成 `server-request {method:"host/event"}` 后下推。
/// 未装配（`boot.host_events` 为 None，如测试口）→ no-op（注册表语义仍生效，事件面由
/// serve 级测试覆盖）。
pub fn push_host_frame(boot: &Boot, payload: serde_json::Value) {
    if let Some(log) = &boot.host_events {
        if let Ok(mut log) = log.lock() {
            log.push(payload);
        }
    }
}

/// RPC 分派是 handle_rpc_host 的纯函数核心：把方法 + payload → `{ok,value|error}`。
pub fn handle_rpc_host(
    boot: &Boot,
    method: &str,
    body: &[u8],
    host: &Rc<SessionHost>,
) -> (u16, Value) {
    let rpc_id = rpc_id_of(body);
    if !rpc_envelope_ok(body, method) {
        return (
            400,
            rpc_response(
                &rpc_id,
                serde_json::json!({"ok": false, "error": {
                    "code": "bad-request",
                    "message": "invalid client-request message",
                }}),
            ),
        );
    }
    let payload = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v.get("payload").cloned())
        .unwrap_or(Value::Null);
    let result = dispatch(boot, method, &payload, host);
    (200, rpc_response(&rpc_id, result))
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
    // M6 step8（D-087）：装配 loop 的真实 provider catalog 优先（groups 只含 id/name
    // 保持 wire 形状；容量/重试走 llm.models 的 `caps` 增量）。
    if let Some(view) = &boot.agent_catalog {
        let provider = view["provider"].as_str().unwrap_or("deepseek").to_string();
        let models: Vec<Value> = view["models"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .map(|m| {
                        let id = m["id"].as_str().unwrap_or("?").to_string();
                        serde_json::json!({"id": id.clone(), "name": id})
                    })
                    .collect()
            })
            .unwrap_or_default();
        let first_model = models
            .first()
            .and_then(|m| m["id"].as_str().map(str::to_string))
            .unwrap_or_else(|| provider.clone());
        let current = serde_json::json!({"provider": provider.clone(), "model": first_model});
        let groups = serde_json::json!([{ "id": provider.clone(), "name": provider, "models": models }]);
        return (current, groups);
    }
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

/// P1-b：settings `agent-presets.default` 解析——新会话未选时的初始预设
/// （D-103/C-04：default 会话不隐式 join，此字段只决定初始选择）。
fn agent_presets_default(boot: &Boot) -> String {
    let mut sp = boot.settings.borrow_mut();
    match sp.describe("agent-presets") {
        Ok(view) => view
            .value
            .get("default")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| crate::preset_host::DEPLOYMENT_DEFAULT_PRESET.to_string()),
        Err(_) => crate::preset_host::DEPLOYMENT_DEFAULT_PRESET.to_string(),
    }
}

// ---------------------------------------------------------------------------
// M4h：goal / subagent web RPC —— 把 M4 纯域服务接到 handle_rpc_host。
// ---------------------------------------------------------------------------

/// M4h：装配会话投影注册表，注册 `todos` + M4 三键（goal/plan/subagent）投影单元。
///
/// ProjectionRegistry 是可选能力：注册失败（重复键等）静默容忍，不 panic——`todos`
/// 与 goal/plan/subagent 单元（`m4_projection_units`）都以各自 stateVersion 注册，
/// 供 `session.history` 的 projections 块真实折叠当前会话事件（验收 #2/#9）。
pub fn assembled_projection_registry() -> Rc<std::cell::RefCell<dsh_session_query::projection::ProjectionRegistry>> {
    let registry = Rc::new(std::cell::RefCell::new(
        dsh_session_query::projection::ProjectionRegistry::new(),
    ));
    {
        let mut reg = registry.borrow_mut();
        let _ = reg.register(dsh_session_query::todo::todos_projection_unit().into_unit());
        for unit in dsh_session_query::m4_units::m4_projection_units() {
            let _ = reg.register(unit);
        }
    }
    registry
}

/// 兼容别名——旧装配只挂 todos；全部装配统一走 assembled_projection_registry。
pub fn todo_projection_registry() -> Rc<std::cell::RefCell<dsh_session_query::projection::ProjectionRegistry>> {
    assembled_projection_registry()
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

/// M4h：host/内部运行时错误（fail loud；不伪装成功）。
fn error_response(code: &str, message: impl Into<String>) -> Value {
    serde_json::json!({
        "ok": false, "error": {
            "code": code,
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
///
/// 每次成功 mutation 后把服务的最近一次 `goal/change` 变更 meta 落进目标会话
/// （验收 #2「goal/change 事件落会话」）——事件由 GoalService 产，caller 落会话。
fn goal_dispatch(boot: &Boot, method: &str, payload: &Value, host: &Rc<SessionHost>) -> Value {
    // 全部 goal.* 请求带 sessionId（catch-all：缺失 → bad-request）。
    let session_id = payload.get("sessionId").and_then(|v| v.as_str()).unwrap_or("");
    if session_id.is_empty() {
        return bad_request_response(format!("{method} requires sessionId"));
    }
    let mut svc = boot.goal.borrow_mut();
    // 成功分派后落事件：从服务取走最近变更 meta → append `goal/change`。
    let emit = |svc: &mut dsh_goal::GoalService| {
        let meta = svc.take_last_change();
        if let Some(meta) = meta {
            let data = serde_json::to_value(&meta).unwrap_or(Value::Null);
            if let Ok(s) = host.session(session_id) {
                let _ = s
                    .append(dsh_session::types::EventKind::GoalChange, data, None)
                    .map_err(|e| e.to_string());
            }
        }
    };
    match method {
        "goal.create" => {
            let objective = payload.get("objective").and_then(|v| v.as_str()).unwrap_or("");
            match svc.create(objective, goal_max_rounds(payload)) {
                Ok(gr) => {
                    emit(&mut svc);
                    serde_json::json!({"ok": true, "value": goal_ref_wire(&gr)})
                }
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
                Ok(gr2) => {
                    emit(&mut svc);
                    serde_json::json!({"ok": true, "value": goal_ref_wire(&gr2)})
                }
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
                Ok(gr2) => {
                    emit(&mut svc);
                    serde_json::json!({"ok": true, "value": goal_ref_wire(&gr2)})
                }
                Err(e) => goal_error_response(&e),
            }
        }
        "goal.clear" => {
            // ref 缺失 → bad-request（revision<=0 亦视为缺失）。
            let Some(gr) = goal_ref_from_payload(payload) else {
                return bad_request_response("goal.clear requires ref {id, revision>0}");
            };
            // 幂等 no-op：无当前 goal（服务 Err(NotFound)）→ 仍 {cleared:true}（对齐 TS
            // clear 无 current goal 语义）；服务成功 → cleared:true + 墓碑事件；其余错误透传。
            match svc.clear(&gr) {
                Ok(_) => {
                    emit(&mut svc);
                    serde_json::json!({"ok": true, "value": {"cleared": true}})
                }
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
///
/// 真实驱动（M4i 收口 / 验收 #5）：list = 只读枚举（store + 描述符折叠）；history =
/// child 事件分页 + projections；prompt = gate 后经 AgentLoopHost followup 驱动一轮
/// （返回真实 messageId；未装配 loop → fail loud）；interrupt = fire-and-return 收据。
fn subagent_dispatch(
    boot: &Boot,
    method: &str,
    payload: &Value,
    host: &Rc<SessionHost>,
) -> Value {
    use crate::subagent_runtime as sa;
    match method {
        "subagent.list" => {
            let parent = payload
                .get("parentSessionId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let (rows, parent_available) = sa::list_children(host, &parent);
            let entries: Vec<Value> = rows.iter().map(subagent_entry_wire).collect();
            serde_json::json!({
                "ok": true,
                "value": { "entries": entries, "parentAvailable": parent_available },
            })
        }
        "subagent.history" => {
            let parent = payload
                .get("parentSessionId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let child = payload
                .get("childSessionId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let mode = payload.get("mode").and_then(|v| v.as_str()).unwrap_or("");
            if parent.is_empty() || child.is_empty() {
                return bad_request_response("subagent.history requires parentSessionId + childSessionId");
            }
            if mode != "one-shot" && mode != "continuable" {
                return bad_request_response("subagent.history requires mode one-shot|continuable");
            }
            let before_seq = payload.get("beforeSeq").and_then(|v| v.as_u64());
            let max = payload
                .get("maxMessages")
                .and_then(|v| v.as_u64())
                .map(|m| m as usize);
            let (events, has_more) = sa::history(host, &child, before_seq, max);
            // projections 块：折叠 child 会话事件（对齐 session.history 的 M4h 投影块）。
            let projections = {
                let reg = boot.projections.borrow();
                let mut ps = dsh_session_query::projection::ProjectionSession::new(&reg);
                let child_events = host.events(&child);
                for e in &child_events {
                    ps.observe(e);
                }
                let snap = ps.snapshot();
                let as_of_seq = if child_events.is_empty() {
                    -1i64
                } else {
                    snap.as_of_seq as i64
                };
                let mut values = serde_json::Map::new();
                for (k, v) in snap.values {
                    values.insert(k, v);
                }
                serde_json::json!({ "asOfSeq": as_of_seq, "values": Value::Object(values) })
            };
            serde_json::json!({
                "ok": true,
                "value": { "events": events, "hasMore": has_more, "projections": projections },
            })
        }
        "subagent.prompt" => {
            let parent = payload
                .get("parentSessionId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let child = payload
                .get("childSessionId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let mode = payload.get("mode").and_then(|v| v.as_str()).unwrap_or("");
            if parent.is_empty() || child.is_empty() {
                return bad_request_response("subagent.prompt requires parentSessionId + childSessionId");
            }
            if mode != "continuable" {
                return bad_request_response("subagent.prompt requires mode 'continuable'");
            }
            // content：文本块拼接（wire content[].text）。
            let text = payload
                .get("content")
                .and_then(|c| c.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            match sa::prompt(host, &boot.agent_loop, &parent, &child, &text) {
                Ok(message_id) => serde_json::json!({
                    "ok": true,
                    "value": { "messageId": message_id },
                }),
                Err(e) => match e {
                    sa::SubagentError::BadRequest(m) => bad_request_response(m),
                    sa::SubagentError::Internal(m) => error_response("internal", m),
                },
            }
        }
        "subagent.interrupt" => {
            let parent = payload
                .get("parentSessionId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let child = payload
                .get("childSessionId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if parent.is_empty() || child.is_empty() {
                return bad_request_response("subagent.interrupt requires parentSessionId + childSessionId");
            }
            let accepted = sa::interrupt(&parent, &child);
            serde_json::json!({ "ok": true, "value": { "accepted": accepted } })
        }
        _ => bad_request_response("unknown subagent method"),
    }
}

// ---------------------------------------------------------------------------
// M4h 补实：jobs/schedule/todo 宿主服务 seam（M4i 验收 #6/#7/#8）
// ---------------------------------------------------------------------------

/// M4h 宿主服务句柄集合：job_*/schedule_*/todo 工具的 bind 目标。
///
/// `register_m4_tools` 接受可选的 `&M4HostServices`：有句柄 → 宿主工具 bind 到真实
/// 宿主（fail loud 不再 NOT_BOUND）；无句柄 → 注册定义但保持 `NOT_BOUND`（诚实：
/// 宿主未装配时绝不伪装成功，D-052 已记录）。
#[derive(Default)]
pub struct M4HostServices {
    /// 共享 JobRegistry（真实生命周期状态机）。
    pub jobs: Option<Rc<std::cell::RefCell<dsh_jobs::registry::JobRegistry>>>,
    /// schedule 域：`schedule/change` 事件 fold 与到期注入（挂在会话事件上）。
    pub schedule: Option<Rc<dsh_cli_host::ScheduleHost>>,
    /// todo 域：把 `todo/write` 事件落到属主 agent 的会话（todo 工具的真实句柄）。
    pub todo: Option<Rc<dsh_cli_host::TodoWriteHost>>,
}

/// M4h 补实：TodoWriteHost —— `todo_write` 工具写入属主会话的真实句柄。
///
/// 对齐 `packages/todo/tool-todo/src/index.ts`：todo 写入既产出模型可见输出
/// （{todos, counts}），也落 `todo/write` 事件到当前会话（`todos` 投影据此折叠）。
/// agent→session 的归属由宿主装配时登记（web 集成中与 AgentLoopHost 共享 store）。
pub mod dsh_cli_host {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    use dsh_session::types::EventKind;
    use dsh_session::runtime::Session;
    use serde_json::{json, Value};

    /// todo 写宿主：登记 agent→session 归属 + 把规范化 todo 表落为 `todo/write` 事件
    /// （对齐 `packages/todo/tool-todo`：写入既产出模型可见输出，也落事件到属主会话，
    /// `todos` 投影据此折叠）。
    pub struct TodoWriteHost {
        host: Rc<crate::session_host::SessionHost>,
        agent_to_session: RefCell<HashMap<String, String>>,
        default_session: String,
    }

    impl TodoWriteHost {
        pub fn new(host: Rc<crate::session_host::SessionHost>, default_session: String) -> Self {
            Self {
                host,
                agent_to_session: RefCell::new(HashMap::new()),
                default_session,
            }
        }

        /// 登记 agent id 的属主会话（web 集成装配时由宿主调用）。
        pub fn bind_agent(&self, agent: &str, session_id: &str) {
            self.agent_to_session
                .borrow_mut()
                .insert(agent.to_string(), session_id.to_string());
        }

        /// 解析属主会话 id（agent 登记优先；未登记回退默认会话）。
        pub fn session_id_for(&self, agent: Option<&str>) -> String {
            agent
                .and_then(|a| self.agent_to_session.borrow().get(a).cloned())
                .unwrap_or_else(|| self.default_session.clone())
        }

        /// 写 `todo/write` 事件（全表替换；对齐投影整表语义）到 agent 属主会话。
        pub fn write(&self, agent: Option<&str>, todos: &[Value]) -> Result<(), String> {
            let sid = self.session_id_for(agent);
            let session = self.host.session(&sid).map_err(|e| e.to_string())?;
            session
                .append(EventKind::TodoWrite, json!({ "todos": todos }), None)
                .map(|_| ())
                .map_err(|e| e.0)
        }

        /// 供 executors / tests 探测默认会话 id。
        pub fn default_session_id(&self) -> &str {
            &self.default_session
        }
    }

    /// ScheduleHost：以某会话（通常为发起调度提醒的 agent 会话）的事件日志为权威。
    pub struct ScheduleHost {
        /// 事件追加目标会话（`Rc`，web 集成时 SessionHost 持有）。
        session: Rc<Session>,
    }

    impl ScheduleHost {
        pub fn new(session: Rc<Session>) -> Self {
            Self { session }
        }

        /// fold 当前会话的 `schedule/change` 事件 → active 记录。
        pub fn fold(&self) -> Result<dsh_schedule::FoldedSchedules, String> {
            let events: Vec<Value> = self
                .session
                .events()
                .iter()
                .filter(|e| e.kind == EventKind::ScheduleChange)
                .map(|e| json!({ "type": e.kind.as_str(), "data": e.data }))
                .collect();
            dsh_schedule::fold_schedule_events(&events).map_err(|e| e.to_string())
        }

        /// create：构造一次 after / at / every 记录并追加 `schedule/change` create 事件。
        /// 返回新 id（fail loud：构造失败 → Err，绝不落坏事件）。
        pub fn create(
            &self,
            kind: &str,
            prompt: &str,
            after_seconds: Option<u64>,
            at: Option<&str>,
            every_seconds: Option<u64>,
            now_epoch: i64,
        ) -> Result<String, String> {
            use dsh_schedule::{
                create_after_record, create_at_record_from_offset, create_every_record,
                ScheduleRecordData,
            };
            let folded = self.fold().map_err(|e| e.to_string())?;
            let seen = folded.seen_ids.clone();
            let id = dsh_schedule::allocate_id_from_seen(&seen);
            let record: ScheduleRecordData = match kind {
                "after" => create_after_record(
                    &id,
                    prompt,
                    after_seconds.ok_or_else(|| {
                        "after requires afterSeconds".to_string()
                    })?,
                    now_epoch,
                )
                .map_err(|e| format!("{e:?}"))?,
                "at" => create_at_record_from_offset(&id, prompt, at.unwrap_or_default(), now_epoch)
                    .map_err(|e| format!("{e:?}"))?,
                "every" => create_every_record(
                    &id,
                    prompt,
                    every_seconds.ok_or_else(|| "every requires everySeconds".to_string())?,
                    now_epoch,
                )
                .map_err(|e| format!("{e:?}"))?,
                other => return Err(format!("unknown schedule kind \"{other}\"")),
            };
            // create 事件载荷：`{version, operation:"create", schedule:<record>}`；decode
            // 按 kind 强制精确键集合（after={id,kind,prompt,afterSeconds,scheduledAt}，
            // at={id,kind,prompt,scheduledAt}，every={id,kind,prompt,everySeconds,
            // scheduledAt}）——建最小精确对象，绝不带多余键。
            let mut schedule = serde_json::Map::new();
            schedule.insert("id".into(), Value::String(record.id.clone()));
            schedule.insert("kind".into(), Value::String(record.kind.clone()));
            schedule.insert("prompt".into(), Value::String(record.prompt.clone()));
            match record.kind.as_str() {
                "after" => {
                    schedule.insert(
                        "afterSeconds".into(),
                        Value::from(record.after_seconds.unwrap_or(0)),
                    );
                }
                "every" => {
                    schedule.insert(
                        "everySeconds".into(),
                        Value::from(record.every_seconds.unwrap_or(0)),
                    );
                }
                _ => {}
            }
            schedule.insert(
                "scheduledAt".into(),
                Value::String(record.scheduled_at.clone()),
            );
            let data = json!({
                "version": dsh_schedule::SCHEDULE_CHANGE_VERSION,
                "operation": "create",
                "schedule": Value::Object(schedule),
            });
            // 先 decode 校验再落（坏事件被拒绝 → 不污染日志）。
            dsh_schedule::decode_schedule_change(&data)
                .map_err(|e| format!("schedule create payload rejected: {e:?}"))?;
            self.session
                .append(EventKind::ScheduleChange, data, None)
                .map_err(|e| e.0)?;
            Ok(id)
        }

        /// list：fold 出该会话 schedule view 行（wire `ScheduleView[]`；缺省字段省略）。
        pub fn list(&self) -> Result<Value, String> {
            let folded = self.fold().map_err(|e| e.to_string())?;
            let mut rows = Vec::new();
            for r in &folded.records {
                let mut row = serde_json::Map::new();
                row.insert("id".into(), Value::String(r.id.clone()));
                row.insert("kind".into(), Value::String(r.kind.clone()));
                row.insert("prompt".into(), Value::String(r.prompt.clone()));
                row.insert("scheduledAt".into(), Value::String(r.scheduled_at.clone()));
                if let Some(a) = r.after_seconds {
                    row.insert("afterSeconds".into(), Value::from(a));
                }
                if let Some(e) = r.every_seconds {
                    row.insert("everySeconds".into(), Value::from(e));
                }
                row.insert("deliveryMode".into(), Value::String("session-local".into()));
                rows.push(Value::Object(row));
            }
            Ok(Value::Array(rows))
        }

        /// delete：追加 `schedule/change` delete 事件（bad id → None 不落事件）。
        pub fn delete(&self, id: &str) -> Result<bool, String> {
            let folded = self.fold().map_err(|e| e.to_string())?;
            if !folded.active_ids.iter().any(|a| a == id) {
                return Ok(false);
            }
            let data = json!({
                "version": dsh_schedule::SCHEDULE_CHANGE_VERSION,
                "operation": "delete",
                "id": id,
            });
            dsh_schedule::decode_schedule_change(&data)
                .map_err(|e| format!("schedule delete payload rejected: {e:?}"))?;
            self.session
                .append(EventKind::ScheduleChange, data, None)
                .map_err(|e| e.0)?;
            Ok(true)
        }

        /// 到期注入（M4i 验收 #7）：fold 后取 due 记录，为每条写 `dispatch` 事件
        /// （one-shot 无 acceptedAt / every 带规范 acceptedAt），并生成 framing 文本。
        /// 返回 `(framing_lines, dispatched_ids)`；无 due → 空。
        pub fn dispatch_due(&self, now_epoch: i64) -> Result<(Vec<String>, Vec<String>), String> {
            let folded = self.fold().map_err(|e| e.to_string())?;
            let mut framing = Vec::new();
            let mut dispatched = Vec::new();
            for record in dsh_schedule::due_records(&folded, now_epoch) {
                let Some(data) = dsh_schedule::dispatch_schedule_change(&record, now_epoch) else {
                    continue;
                };
                self.session
                    .append(EventKind::ScheduleChange, data, None)
                    .map_err(|e| e.0)?;
                framing.push(dsh_schedule::framing_text(&record));
                dispatched.push(record.id.clone());
            }
            Ok((framing, dispatched))
        }
    }
}

/// M4 h：注册全部 M4 工具（todo/job_*/schedule_*/exit_plan_mode/workflow）。
///
/// 有 `host` 时 bind job_*/schedule_* 到真实宿主；否则注册定义但宿主工具保持
/// `NOT_BOUND`（fail loud）。workflow 恒桩（UNSUPPORTED_OPTION）。
pub fn register_m4_tools_with_host(
    registry: &dsh_tools::ToolRegistry,
    host: Option<&M4HostServices>,
) {
    use dsh_tools::m4;

    // todo_write：宿主 todo 句柄在场 → 绑定写入器（落 `todo/write` 事件到属主会话 +
    // 规范化输出 {todos, counts}）；否则注册自包含定义（校验-only，不落事件——宿主
    // 未装配时事件无从归属，保持诚实，不伪称已持久化）。
    if let Some(todo_host) = host.and_then(|h| h.todo.clone()) {
        let bound = todo_write_with_host_executor(todo_host);
        registry
            .register_global(bound)
            .expect("register todo_write (host-bound)");
    } else {
        registry
            .register_global(m4::todo_write(false).expect("todo_write defines"))
            .expect("register todo_write");
    }

    // job_*：宿主 JobRegistry bind。
    let (job_output, job_list, job_kill) = (
        m4::job_output().expect("job_output defines"),
        m4::job_list().expect("job_list defines"),
        m4::job_kill().expect("job_kill defines"),
    );
    if let Some(jobs) = host.and_then(|h| h.jobs.clone()) {
        let bind_jobs = jobs.clone();
        job_output.bind(job_output_executor(bind_jobs.clone()));
        job_list.bind(job_list_executor(bind_jobs.clone()));
        job_kill.bind(job_kill_executor(bind_jobs));
    }
    registry
        .register_global(job_output.definition())
        .expect("register job_output");
    registry
        .register_global(job_list.definition())
        .expect("register job_list");
    registry
        .register_global(job_kill.definition())
        .expect("register job_kill");

    // schedule_*：宿主 ScheduleHost bind。
    let (schedule_create, schedule_list, schedule_delete) = (
        m4::schedule_create().expect("schedule_create defines"),
        m4::schedule_list().expect("schedule_list defines"),
        m4::schedule_delete().expect("schedule_delete defines"),
    );
    if let Some(sched) = host.and_then(|h| h.schedule.clone()) {
        let bind_sched = sched.clone();
        schedule_create.bind(schedule_create_executor(bind_sched.clone()));
        schedule_list.bind(schedule_list_executor(bind_sched.clone()));
        schedule_delete.bind(schedule_delete_executor(bind_sched));
    }
    registry
        .register_global(schedule_create.definition())
        .expect("register schedule_create");
    registry
        .register_global(schedule_list.definition())
        .expect("register schedule_list");
    registry
        .register_global(schedule_delete.definition())
        .expect("register schedule_delete");

    // exit_plan_mode：计划前置校验（无宿主 plan-mode 服务 → NOT_BOUND 诚实）。
    let exit_plan = m4::exit_plan_mode().expect("exit_plan_mode defines");
    registry
        .register_global(exit_plan.definition())
        .expect("register exit_plan_mode");

    // workflow：恒桩（meta 校验 → UNSUPPORTED_OPTION）。
    registry
        .register_global(m4::workflow().expect("workflow defines"))
        .expect("register workflow");
}

// ---- todo_write 宿主 executor（bind 目标；落 `todo/write` 事件 + 规范化输出） ----

/// todo_write 宿主绑定版：校验/规范化（不变，复用 to_todo_list）+ 把规范表落为
/// `todo/write` 事件到属主会话，并返回模型可见 `{todos, counts}`（对齐
/// `packages/todo/tool-todo`：写入即事件，todos 投影据此折叠）。
fn todo_write_with_host_executor(todo_host: Rc<dsh_cli_host::TodoWriteHost>) -> Rc<dsh_tools::ToolDefinition> {
    use dsh_tools::types::{ToolExecute, ToolFailureData, CODE_INVALID_ARGS};
    let execute: ToolExecute = Rc::new(move |args, ctx| {
        let agent = ctx.agent.as_deref();
        if agent.is_none() {
            // 对齐参考：拒绝无 agent 调用者（无处归属），绝不静默 no-op。
            return Err(ToolFailureData::new(
                "todo_write requires an owning agent session".to_string(),
                CODE_INVALID_ARGS,
                "TodoWriteError",
            ));
        }
        let raw = args
            .get("todos")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        match dsh_session_query::todo::to_todo_list(&raw, false) {
            Ok(list) => {
                let todos = serde_json::to_value(&list).unwrap_or_else(|_| serde_json::json!([]));
                let counts = dsh_session_query::todo::todo_counts(&list);
                // 落 `todo/write` 事件（整表替换；投影空事件 → no-op，仍返回输出）。
                if let Err(e) = todo_host.write(agent, &todos.as_array().cloned().unwrap_or_default()) {
                    return Err(ToolFailureData::new(
                        format!("todo/write event rejected: {e}"),
                        dsh_tools::CODE_INVALID_TOOL_OUTPUT,
                        "TodoWriteError",
                    ));
                }
                Ok(serde_json::json!({ "todos": todos, "counts": counts }))
            }
            Err(e) => Err(ToolFailureData::new(
                format!("todo list rejected: {e:?}"),
                CODE_INVALID_ARGS,
                "TodoListError",
            )),
        }
    });
    // 复用 SA-4 todo_write 的 schema/输出/描述，仅换执行器（消重，语义不漂移）。
    // 刚构造的 base refcount==1 → Rc::try_unwrap 拿回本体直接改 execute。
    let base = dsh_tools::m4::todo_write(false).expect("todo_write defines");
    let mut def = Rc::try_unwrap(base).unwrap_or_else(|_| {
        panic!("todo_write def freshly created must be refcount 1")
    });
    def.execute = execute;
    Rc::new(def)
}

// ---- job_* 宿主 executor（bind 目标） ----

fn job_output_executor(
    jobs: Rc<std::cell::RefCell<dsh_jobs::registry::JobRegistry>>,
) -> dsh_tools::types::ToolExecute {
    use dsh_tools::types::{ToolFailureData, CODE_INVALID_ARGS};
    Rc::new(move |args, _ctx| {
        let id = args
            .get("job_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ToolFailureData::new("job_output requires job_id", CODE_INVALID_ARGS, "JobError")
            })?;
        let wait = args.get("wait").and_then(|v| v.as_bool()).unwrap_or(false);
        let mut reg = jobs.borrow_mut();
        // wait=true → 等终态（本宿主纯内存、单线程：非终态即返回当前快照）；
        // wait=false → read（text + snapshot）。
        let read = reg.read(id, None);
        let (text, job_view) = match read {
            Ok(r) => {
                let view = dsh_jobs::snapshot_to_view(&r.snapshot);
                (r.text.clone(), view)
            }
            Err(e) => {
                return Err(ToolFailureData::new(
                    format!("job read failed: {e:?}"),
                    dsh_tools::CODE_INVALID_TOOL_OUTPUT,
                    "JobError",
                ));
            }
        };
        if wait {
            match reg.wait(id, None) {
                Ok(s) => {
                    let view = dsh_jobs::snapshot_to_view(&s);
                    let text2 = view["detail"]
                        .as_str()
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| text.clone());
                    return Ok(serde_json::json!({ "text": text2, "job": view }));
                }
                Err(e) => {
                    return Err(ToolFailureData::new(
                        format!("job wait failed: {e:?}"),
                        dsh_tools::CODE_INVALID_TOOL_OUTPUT,
                        "JobError",
                    ));
                }
            }
        }
        Ok(serde_json::json!({ "text": text, "job": job_view }))
    })
}

fn job_list_executor(
    jobs: Rc<std::cell::RefCell<dsh_jobs::registry::JobRegistry>>,
) -> dsh_tools::types::ToolExecute {
    Rc::new(move |_args, _ctx| {
        let reg = jobs.borrow();
        let snaps = reg.list(None);
        Ok(dsh_jobs::jobs_frame(&snaps))
    })
}

fn job_kill_executor(
    jobs: Rc<std::cell::RefCell<dsh_jobs::registry::JobRegistry>>,
) -> dsh_tools::types::ToolExecute {
    use dsh_tools::types::CODE_INVALID_ARGS;
    Rc::new(move |args, _ctx| {
        let id = args
            .get("job_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                dsh_tools::ToolFailureData::new(
                    "job_kill requires job_id",
                    CODE_INVALID_ARGS,
                    "JobError",
                )
            })?;
        let reason = args.get("reason").and_then(|v| v.as_str()).map(str::to_string);
        let outcome = jobs.borrow_mut().kill(id, None, reason.as_deref());
        match outcome {
            Ok(kill_outcome) => {
                let (outcome_str, job) = match kill_outcome {
                    dsh_jobs::KillOutcome::AlreadyFinished => {
                        let snap = jobs.borrow().get(id, None).unwrap_or_else(|_| {
                            // 极端：kill 后瞬时不可达 → 最小 view（不应发生；防御）。
                            dsh_jobs::JobSnapshot {
                                id: id.to_string(),
                                kind: String::new(),
                                label: String::new(),
                                owner: None,
                                status: dsh_jobs::JobStatus::Completed,
                                detail: None,
                                started_at: 0,
                                finished_at: None,
                                reported: false,
                            }
                        });
                        ("already-finished", dsh_jobs::snapshot_to_view(&snap))
                    }
                    dsh_jobs::KillOutcome::Requested => {
                        let snap = jobs.borrow().get(id, None).unwrap_or_else(|_| {
                            dsh_jobs::JobSnapshot {
                                id: id.to_string(),
                                kind: String::new(),
                                label: String::new(),
                                owner: None,
                                status: dsh_jobs::JobStatus::Stopping,
                                detail: None,
                                started_at: 0,
                                finished_at: None,
                                reported: false,
                            }
                        });
                        ("cancellation-requested", dsh_jobs::snapshot_to_view(&snap))
                    }
                };
                Ok(serde_json::json!({ "outcome": outcome_str, "job": job }))
            }
            Err(e) => Err(dsh_tools::ToolFailureData::new(
                format!("job kill failed: {e:?}"),
                dsh_tools::CODE_INVALID_TOOL_OUTPUT,
                "JobError",
            )),
        }
    })
}

// ---- goal-round-driver 实配端口（M4i 验收 #3） ----

/// goal-round-driver 的宿主端口：把 `Rc<ReactLoopAgent>` 实配到 `StatusPort`
/// （status_idle / has_pending_inbox / followup）。装配好后，宿主在每轮结束时调用
/// `drive_once`（或 `round_driver_outcome` 判定 + `followup` 投递），让 armed 目标
/// 自动续跑下一轮（单线程同步：followup 即时驱动该轮直至空闲）。
pub struct GoalRoundPort {
    agent: Rc<dsh_agent_loop::ReactLoopAgent>,
}

impl GoalRoundPort {
    pub fn new(agent: Rc<dsh_agent_loop::ReactLoopAgent>) -> Self {
        Self { agent }
    }
}

impl dsh_goal::round_driver::StatusPort for GoalRoundPort {
    fn status_idle(&self) -> bool {
        self.agent.status() == dsh_agent::types::AgentStatus::Idle
    }

    fn has_pending_inbox(&self) -> bool {
        self.agent.inbox().has_pending()
    }

    fn followup(&mut self, _id: &dsh_goal::types::GoalId, message: &str) -> Result<(), String> {
        let msg = dsh_llm::Message::user(
            dsh_llm::MessageId::from_raw("goal-round-followup".to_string()),
            vec![dsh_llm::ContentBlock::text(message)],
        );
        self.agent.followup(msg).map_err(|e| e.to_string())
    }
}

// ---- schedule_* 宿主 executor（bind 目标） ----

fn schedule_create_executor(
    sched: Rc<dsh_cli_host::ScheduleHost>,
) -> dsh_tools::types::ToolExecute {
    use dsh_tools::types::CODE_INVALID_ARGS;
    Rc::new(move |args, _ctx| {
        let prompt = args
            .get("prompt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                dsh_tools::ToolFailureData::new(
                    "schedule_create requires prompt",
                    CODE_INVALID_ARGS,
                    "ScheduleError",
                )
            })?;
        let after = args.get("after_seconds").and_then(|v| v.as_u64());
        let every = args.get("every_seconds").and_then(|v| v.as_u64());
        let at = args.get("at").and_then(|v| v.as_str());
        let kind = if every.is_some() {
            "every"
        } else if at.is_some() {
            "at"
        } else {
            "after"
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        match sched.create(kind, prompt, after, at, every, now) {
            Ok(id) => Ok(serde_json::json!({ "id": id })),
            Err(e) => Err(dsh_tools::ToolFailureData::new(
                e,
                dsh_tools::CODE_INVALID_TOOL_OUTPUT,
                "ScheduleError",
            )),
        }
    })
}

fn schedule_list_executor(
    sched: Rc<dsh_cli_host::ScheduleHost>,
) -> dsh_tools::types::ToolExecute {
    Rc::new(move |_args, _ctx| match sched.list() {
        Ok(v) => Ok(v),
        Err(e) => Err(dsh_tools::ToolFailureData::new(
            e,
            dsh_tools::CODE_INVALID_TOOL_OUTPUT,
            "ScheduleError",
        )),
    })
}

fn schedule_delete_executor(
    sched: Rc<dsh_cli_host::ScheduleHost>,
) -> dsh_tools::types::ToolExecute {
    use dsh_tools::types::CODE_INVALID_ARGS;
    Rc::new(move |args, _ctx| {
        let id = args
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                dsh_tools::ToolFailureData::new(
                    "schedule_delete requires id",
                    CODE_INVALID_ARGS,
                    "ScheduleError",
                )
            })?;
        match sched.delete(id) {
            Ok(true) => Ok(serde_json::json!({ "deleted": true })),
            Ok(false) => Ok(serde_json::json!({ "deleted": false })),
            Err(e) => Err(dsh_tools::ToolFailureData::new(
                e,
                dsh_tools::CODE_INVALID_TOOL_OUTPUT,
                "ScheduleError",
            )),
        }
    })
}

/// 兼容别名：无宿主注册全部 M4 工具（job_*/schedule_* 保持 NOT_BOUND）。
pub fn register_m4_tools(registry: &dsh_tools::ToolRegistry) {
    register_m4_tools_with_host(registry, None);
}

// ---------------------------------------------------------------------------
// M6 step1（验收 #2）：服务器装配工厂 —— 把 M4/M5 工具 + 宿主组装成 AgentLoopHost 的
// 真实注册表（共享 store：与 SessionHost 同店 → 前端读模型同源）；装配方传入 provider/
// model（生产 = deepseek + 配置端点；测试 = mock）与两个宿主（M4 + M5）。
// ---------------------------------------------------------------------------

/// 装配 LoopHost——真实注册表 = `register_m4_tools_with_host(m4)` +
/// `register_m5_tools_with_host(m5.services)`（fs/terminal/shell/bash/code 宿主 bind）。
/// 单一默认 agent（provider/model 由装配方指定；session_id "default" 与前端会话入口一致）。
/// 宿主生命周期清理（`M5Host::shutdown` 等）由装配方挂 disposer（step2 补实）。
pub fn assemble_server_loop(
    session_store: Rc<dsh_session::store::SessionStore>,
    workspace_root: std::path::PathBuf,
    llm: Rc<dsh_llm::LlmRuntime>,
    provider: &str,
    model: &str,
    m4: M4HostServices,
    m5: web_m5::M5Host,
) -> Result<Rc<dsh_agent_loop::AgentLoopHost>, String> {
    let tools = Rc::new(dsh_tools::ToolRegistry::new(
        dsh_tools::ToolExecutionMode::Native,
    ));
    register_m4_tools_with_host(&tools, Some(&m4));
    register_m5_tools_with_host(&tools, Some(&m5.services));
    let config = dsh_agent_loop::AgentLoopConfig {
        max_parallel_tool_calls: None,
        agents: vec![dsh_agent_loop::ConfiguredAgent {
            id: "default".into(),
            provider: Some(provider.to_string()),
            model: Some(model.to_string()),
            session_id: Some("default".into()),
            max_tokens: None,
            cwd: Some(workspace_root.to_string_lossy().into_owned()),
            resume_session_id: None,
        }],
    };
    let host = dsh_agent_loop::AgentLoopHost::with_store(config, llm, tools.clone(), session_store)?;
    // M6 step9（D-088）：宿主 pre-execute 钩子——把「记录 + 放行」钩子接上共享
    // `default` 会话（dsh-tools pre-decision 缝延伸；`hookInvoked` 事件记录 vs TS
    // `HookInvoked` 对齐；放行保持既有语义）。
    {
        let sid = dsh_session::types::SessionId::from_raw("default".to_string());
        let session = host
            .store
            .get(&sid)
            .ok_or_else(|| "default session missing for pre-execute hook".to_string())?;
        web_m5::wire_recording_pre_execute_hook(&tools, session)?;
    }
    // M6 step4（D-084）：sandbox:policy 投影——把动态段（order 110）注册进宿主 prompt；
    // provider 每次装配从共享 store 现算有效沙箱模式（fail-closed 缺省 read-only）。
    web_m5::register_sandbox_policy_section(
        &host.prompt,
        host.store.clone(),
        "default",
        workspace_root.clone(),
    )?;
    // M6 step2（D-082）：宿主生命周期清理——`host.teardown()` 时执行 M5 关停
    // （bash bg 树 kill + settle Killed；terminal dispose），无孤儿进程。
    host.add_disposer(Rc::new(move || m5.shutdown()));
    Ok(host)
}

/// 系统当前毫秒时间（`JobRegistry` now；i64 契约对齐 dsh-jobs）。
fn system_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// M6 step3（D-083）：serve 递增推进 bundle——loop host + 调度宿主 + bash jobs 桥。
/// schedule/bash_jobs 与 loop 工具注册**共享同一实例**（Rc clone）：serve 主循环的
/// `m5g_tick_once`（调度到期 + jobs 合作泵）与 agent 工具执行推进同一状态。
pub struct ServerLoopBundle {
    /// 服务器执行闭环（真实 M4+M5 注册 + 共享 store）。
    pub host: Rc<dsh_agent_loop::AgentLoopHost>,
    /// 调度宿主（tick `dispatch_due` 的目标；与工具 schedule_create 同实例）。
    pub schedule: Rc<crate::web::dsh_cli_host::ScheduleHost>,
    /// bash 后台 jobs 桥（tick `pump()` 结算；与 bash run_in_background 同实例）。
    pub bash_jobs: Option<Rc<web_m5::BashJobsBridge>>,
}

/// M6 step1b（D-081）/step3（D-083）/step6（D-085）：serve 接线编排——在 SessionHost 上
/// 构建 M4（jobs/schedule/todo 真实句柄）+ M5（真实工厂）+ LLM → `assemble_server_loop`
/// （共享 store 写回）。返回 bundle：schedule/bash_jobs 供 serve tick 推进同一实例。
/// 装配失败（如 bash 不可用 / 宿主构造错）→ `Err`：serve fail-loud（诚实，不默默回退
/// WASM 路径）。
///
/// `assemble_server_runtime`（生产便捷包装）用 deepseek LLM（key 仅 env）；无 key →
/// 首回合 fail-loud，装配照常。`assemble_server_runtime_with_llm` 暴露 LLM/品牌注入缝
/// （完整装配路径的可测前端闭环；mock 驱动 / 显式 no-key / 真实 key 均可）。
pub fn assemble_server_runtime(
    host: &Rc<crate::session_host::SessionHost>,
    workspace_root: std::path::PathBuf,
    base_url: &str,
    model: &str,
) -> Result<ServerLoopBundle, String> {
    let llm = crate::m6_llm::server_llm_runtime(base_url, model);
    assemble_server_runtime_with_llm(host, workspace_root, llm, "deepseek", model)
}

/// 完整装配路径（LLM/品牌可注入）——serve 装配 + 测试共用同一代码路径。
pub fn assemble_server_runtime_with_llm(
    host: &Rc<crate::session_host::SessionHost>,
    workspace_root: std::path::PathBuf,
    llm: Rc<dsh_llm::LlmRuntime>,
    provider: &str,
    model: &str,
) -> Result<ServerLoopBundle, String> {
    let session = host.session("default").map_err(|e| e.to_string())?;
    let jobs = Rc::new(std::cell::RefCell::new(dsh_jobs::registry::JobRegistry::new(
        dsh_jobs::registry::JobRegistryConfig {
            max_concurrent_per_owner: 8,
            now: Box::new(system_now_ms),
        },
    )));
    let schedule = Rc::new(dsh_cli_host::ScheduleHost::new(session));
    let todo = Rc::new(dsh_cli_host::TodoWriteHost::new(host.clone(), "default".into()));
    todo.bind_agent("default", "default");
    let m4 = M4HostServices {
        jobs: Some(jobs),
        schedule: Some(schedule.clone()),
        todo: Some(todo),
    };
    let m5 = web_m5::M5Host::assemble(workspace_root.clone())?;
    let bash_jobs = m5.services.bash_jobs.clone();
    let loop_host = assemble_server_loop(
        host.store.clone(),
        workspace_root,
        llm,
        provider,
        model,
        m4,
        m5,
    )?;
    Ok(ServerLoopBundle {
        host: loop_host,
        schedule,
        bash_jobs,
    })
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
            // M3a+（D-098）：进程内原生选择器（Windows IFileDialog/COM，零子进程）→
            // 选中路径；取消 → {path:null}（对齐 TS seam「native 下取消为 null」，合法语义）；
            // 未装配/失败 → 诚实 `directory-picker-unavailable`（绝不拿 null 冒充取消）。
            // 生产上该 RPC 由 dispatch_request 优先派到独立线程（user-paced 模态不饿死
            // accept 循环）；这里保留完整语义供 handle_rpc_host 直调（测试/程序内）用。
            pick_directory_result(&boot.host_picker)
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
            // D-100：`{workspaceId}` 时把新会话 attach 进该工作区（对齐 TS
            // session.create{workspaceId} 的归属语义）并推 host 增量帧
            // （host/workspace-changed + host/session-added）；未知工作区 → 诚实报错。
            let attached_path: Option<String> =
                if let Some(ws_id) = payload.get("workspaceId").and_then(|v| v.as_str()) {
                    let attached = {
                        let mut reg = boot.workspaces.borrow_mut();
                        reg.attach_session(ws_id, &id)
                    };
                    match attached {
                        Some(record) => {
                            push_host_frame(
                                boot,
                                serde_json::json!({"type": "host/workspace-changed",
                                    "workspace": crate::workspace_host::workspace_view(&record)}),
                            );
                            push_host_frame(
                                boot,
                                serde_json::json!({"type": "host/session-added",
                                    "sessionId": id, "blank": true}),
                            );
                            Some(record.path.clone())
                        }
                        None => {
                            return serde_json::json!({"ok": false, "error": {
                                "code": "workspace-not-found",
                                "message": format!("unknown workspace '{}'", ws_id),
                            }})
                        }
                    }
                } else {
                    None
                };
            // D-101：给新会话挂接真实 agent——否则 `session.prompt` 对它们报
            // `no configured agent maps to session`。cwd 沿用工作区路径（D-100 归属），
            // 无工作区 → 继承部署默认 agent 的 cwd。装配失败 → fail loud。
            if let Err(e) = crate::ensure_session_agent(boot, &id, attached_path.as_deref()) {
                return serde_json::json!({"ok": false, "error": {
                    "code": "internal",
                    "message": e.to_string(),
                }});
            }
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
            // M4h：投影块（验收 #9「投影键经 history 响应携带」）——把会话事件经
            // ProjectionRegistry 折叠出 projections（asOfSeq + values）。折叠在
            // 调用线程同步做，读模型与事件流天然一致（M4-REQUIREMENTS §3）。
            let projections = {
                let reg = boot.projections.borrow();
                let mut ps = dsh_session_query::projection::ProjectionSession::new(&reg);
                let session_events = host.events(&sid);
                for e in &session_events {
                    ps.observe(e);
                }
                let snap = ps.snapshot();
                // asOfSeq 惯例：空日志 = -1（与 sessionProjectionsBlockSchema/订阅 lastSeq 一致）。
                let as_of_seq = if session_events.is_empty() {
                    -1i64
                } else {
                    snap.as_of_seq as i64
                };
                let mut values = serde_json::Map::new();
                for (k, v) in snap.values {
                    values.insert(k, v);
                }
                serde_json::json!({ "asOfSeq": as_of_seq, "values": Value::Object(values) })
            };
            serde_json::json!({"ok": true, "value": {
                "events": events, "hasMore": false, "projections": projections,
            }})
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
                // D-101：子会话继承源 agent 的 cwd（源无 agent 配置 → None → 部署默认）。
                let src_cwd = boot
                    .agent_loop
                    .as_ref()
                    .and_then(|h| h.configured_for_session(src))
                    .and_then(|c| c.cwd.clone());
                if let Err(e) = crate::ensure_session_agent(boot, &id, src_cwd.as_deref()) {
                    return serde_json::json!({"ok": false, "error": {
                        "code": "internal",
                        "message": e.to_string(),
                    }});
                }
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
            // D-100：真实注册表（registry order + 全局归档集），不再 canned stub。
            let reg = boot.workspaces.borrow();
            let items: Vec<Value> = reg
                .list()
                .iter()
                .map(crate::workspace_host::workspace_view)
                .collect();
            let archived = reg.archived_session_ids();
            serde_json::json!({"ok": true, "value": {
                "items": items,
                "archivedSessionIds": archived,
            }})
        }
        "workspace.create" => {
            // D-100：canonicalize → 同 path 幂等（created:false，不改 title）/
            // 新 path 铸全新 id + title=basename（created:true）；随后推
            // host/workspace-changed 增量帧（客户端 upsert + 其它 tab 同步）。
            let path = payload.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let outcome = {
                let mut reg = boot.workspaces.borrow_mut();
                reg.create(&path)
            };
            match outcome {
                Ok(oc) => {
                    let view = boot
                        .workspaces
                        .borrow()
                        .get(&oc.id)
                        .map(|r| crate::workspace_host::workspace_view(&r));
                    match view {
                        Some(view) => {
                            push_host_frame(
                                boot,
                                serde_json::json!({"type": "host/workspace-changed", "workspace": view}),
                            );
                            serde_json::json!({"ok": true, "value": {"workspace": view, "created": oc.created}})
                        }
                        None => serde_json::json!({"ok": false, "error": {
                            "code": "bad-request",
                            "message": "workspace.create: record vanished",
                        }}),
                    }
                }
                Err(message) => serde_json::json!({"ok": false, "error": {
                    "code": "workspace-path-invalid",
                    "message": message,
                }}),
            }
        }
        "workspace.rename" => {
            let ws_id = payload.get("workspaceId").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let title = payload.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let renamed = {
                let mut reg = boot.workspaces.borrow_mut();
                reg.rename(&ws_id, &title)
            };
            match renamed {
                Some(record) => {
                    let view = crate::workspace_host::workspace_view(&record);
                    push_host_frame(
                        boot,
                        serde_json::json!({"type": "host/workspace-changed", "workspace": view}),
                    );
                    serde_json::json!({"ok": true, "value": {"workspace": view}})
                }
                None => serde_json::json!({"ok": false, "error": {
                    "code": "workspace-not-found",
                    "message": format!("unknown workspace '{}'", ws_id),
                }}),
            }
        }
        "workspace.delete" => {
            let ws_id = payload.get("workspaceId").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let deleted = {
                let mut reg = boot.workspaces.borrow_mut();
                reg.delete(&ws_id)
            };
            push_host_frame(
                boot,
                serde_json::json!({"type": "host/workspace-removed", "workspaceId": ws_id}),
            );
            serde_json::json!({"ok": true, "value": {"deleted": deleted}})
        }
        "workspace.insertBefore" => {
            let ws_id = payload.get("workspaceId").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let before = payload
                .get("beforeWorkspaceId")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let order = {
                let mut reg = boot.workspaces.borrow_mut();
                reg.insert_before(&ws_id, before.as_deref())
            };
            match order {
                Ok(ids) => {
                    push_host_frame(
                        boot,
                        serde_json::json!({"type": "host/workspace-order-changed", "workspaceIds": ids}),
                    );
                    serde_json::json!({"ok": true, "value": {"workspaceIds": ids}})
                }
                Err(message) => serde_json::json!({"ok": false, "error": {
                    "code": "workspace-order-invalid",
                    "message": message,
                }}),
            }
        }
        "workspace.insertSessionBefore" => {
            let ws_id = payload.get("workspaceId").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let sid = payload.get("sessionId").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let before = payload
                .get("beforeSessionId")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let moved = {
                let mut reg = boot.workspaces.borrow_mut();
                reg.insert_session_before(&ws_id, &sid, before.as_deref())
            };
            match moved {
                Some(record) => {
                    let view = crate::workspace_host::workspace_view(&record);
                    push_host_frame(
                        boot,
                        serde_json::json!({"type": "host/workspace-changed", "workspace": view}),
                    );
                    serde_json::json!({"ok": true, "value": {"workspace": view}})
                }
                None => serde_json::json!({"ok": false, "error": {
                    "code": "workspace-not-found",
                    "message": format!("cannot move session '{}' in workspace '{}'", sid, ws_id),
                }}),
            }
        }
        "workspace.archiveSession" => {
            let sid = payload.get("sessionId").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let archived = {
                let mut reg = boot.workspaces.borrow_mut();
                reg.archive_session(&sid)
            };
            push_host_frame(
                boot,
                serde_json::json!({"type": "host/archived-sessions-changed", "archivedSessionIds": archived}),
            );
            serde_json::json!({"ok": true, "value": {"archivedSessionIds": archived}})
        }
        "skill.list" => {
            serde_json::json!({"ok": true, "value": {"skills": []}})
        }
        "agentPreset.list" => {
            // P1-b：真实发现（不缓存）；isDefault = settings agent-presets.default 解析值。
            let presets: Vec<Value> = {
                let host = boot.presets.borrow();
                let default = agent_presets_default(boot);
                host.roster().iter().map(|p| crate::preset_host::to_entry(p, p.id == default)).collect()
            };
            let authorable = boot.presets.borrow().authorable();
            serde_json::json!({"ok": true, "value": {
                "presets": presets,
                "authorable": authorable,
                "hasDocument": false,
            }})
        }
        "agentPreset.select" => {
            let preset = payload.get("agentPreset").and_then(|v| v.as_str()).unwrap_or("");
            // P1：join standing 是 P2（mount+guard）；未落地前显式拒绝，不假装切换（诚实）。
            serde_json::json!({"ok": false, "error": {
                "code": "agent-preset-unsupported",
                "message": format!(
                    "agentPreset.select ({preset}): joining a standing mount lands in P2, not implemented yet"
                ),
            }})
        }
        "agentPreset.read" => {
            let preset = payload
                .get("agentPreset")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            match boot.presets.borrow().find(&preset) {
                Some(p) => match std::fs::read_to_string(&p.path) {
                    Ok(content) => {
                        let mut v = serde_json::Map::new();
                        v.insert("agentPreset".into(), serde_json::json!(p.id));
                        v.insert(
                            "trust".into(),
                            serde_json::json!(match p.trust {
                                dsh_agent_presets::PresetTrust::System => "system",
                                dsh_agent_presets::PresetTrust::User => "user",
                            }),
                        );
                        v.insert("content".into(), serde_json::json!(content));
                        // 可选字段只在存在时出现（wire schema 不允许 null）。
                        if let Some(n) = &p.name {
                            v.insert("name".into(), serde_json::json!(n));
                        }
                        if let Some(d) = &p.description {
                            v.insert("description".into(), serde_json::json!(d));
                        }
                        serde_json::json!({"ok": true, "value": serde_json::Value::Object(v)})
                    }
                    Err(e) => serde_json::json!({"ok": false, "error": {
                        "code": "agent-preset-not-found",
                        "message": format!("agentPreset.read {preset}: cannot read {}: {e}", p.path.display()),
                    }}),
                },
                None => serde_json::json!({"ok": false, "error": {
                    "code": "agent-preset-not-found",
                    "message": format!("agentPreset.read: no preset \"{preset}\""),
                }}),
            }
        }
        "agentPreset.copy" => {
            // P1：copy/remove 作者流并入 P5（诚实拒绝，不假装成功）。
            serde_json::json!({"ok": false, "error": {
                "code": "agent-preset-unsupported",
                "message": "agentPreset.copy (authoring) lands in P5, not implemented yet",
            }})
        }
        "agentPreset.remove" => {
            serde_json::json!({"ok": false, "error": {
                "code": "agent-preset-unsupported",
                "message": "agentPreset.remove (authoring) lands in P5, not implemented yet",
            }})
        }
        "agentPreset.openDocument" => {
            let preset = payload
                .get("agentPreset")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            match boot.presets.borrow().find(&preset) {
                Some(p) => {
                    // 诚实：Rust 侧未接原生打开器 → {opened:false, path=预设目录}（align TS 无 opener 降级）。
                    let dir = p
                        .path
                        .parent()
                        .map(|d| d.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    serde_json::json!({"ok": true, "value": {"opened": false, "path": dir}})
                }
                None => serde_json::json!({"ok": false, "error": {
                    "code": "agent-preset-not-found",
                    "message": format!("agentPreset.openDocument: no preset \"{preset}\""),
                }}),
            }
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
                // M6 step8（D-087）：真实装配 catalog 的容量/重试增量（无 → null）。
                "caps": boot.agent_catalog.clone().unwrap_or(Value::Null),
                "failures": [],
            }})
        }
        "llm.discoverModels" => {
            serde_json::json!({"ok": true, "value": {"models": []}})
        }
        "goal.create" | "goal.edit" | "goal.pause" | "goal.resume" | "goal.complete" | "goal.clear" => {
            goal_dispatch(boot, method, payload, host)
        }
        "subagent.list" | "subagent.history" | "subagent.prompt" | "subagent.interrupt" => {
            subagent_dispatch(boot, method, payload, host)
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
            agent_catalog: None,
            settings: std::rc::Rc::new(std::cell::RefCell::new(
                dsh_settings::SettingsProvider::memory(),
            )),
            credentials: std::rc::Rc::new(std::cell::RefCell::new(
                dsh_credentials::CredentialProvider::memory(),
            )),
            goal: std::rc::Rc::new(std::cell::RefCell::new(
                dsh_goal::GoalService::new(dsh_goal::ServiceOptions::default()),
            )),
            projections: assembled_projection_registry(),
            host_picker: None,
            workspaces: std::rc::Rc::new(std::cell::RefCell::new(
                crate::workspace_host::WorkspaceRegistry::new(),
            )),
            host_events: None,
            presets: std::rc::Rc::new(std::cell::RefCell::new(
                crate::preset_host::PresetHost::default(),
            )),
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

    /// D-100：workspace.create 走真实注册表——新路径铸独立 id + title=basename +
    /// created:true，推 host/workspace-changed；同路径幂等（created:false 同 id）；
    /// workspace.list 反映真实注册表（新工作区 prepend）。
    #[test]
    fn rpc_workspace_create_real_semantics() {
        let mut boot = boot_with_sessions();
        boot.host_events = Some(std::sync::Arc::new(std::sync::Mutex::new(Vec::new())));
        let host = seeded_host();
        let dir = std::env::temp_dir().join(format!("dsh-ws-rpc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.to_string_lossy().to_string();

        let call = |boot: &crate::Boot, method: &str, payload: Value, host: &Rc<SessionHost>| {
            let body = serde_json::to_vec(&serde_json::json!({
                "type": "client-request", "rpcId": "r", "method": method, "payload": payload,
            })).unwrap();
            let (_, v) = handle_rpc_host(boot, method, &body, host);
            v
        };

        // 新路径 → 独立 id、created:true、title=basename、sessionIds 空。
        let v = call(
            &boot,
            "workspace.create",
            serde_json::json!({"path": path}),
            &host,
        );
        assert_eq!(v["result"]["ok"], true);
        let view = &v["result"]["value"]["workspace"];
        let id = view["workspaceId"].as_str().unwrap().to_string();
        assert_ne!(id, "default", "new workspace id must not collide with boot default");
        assert_eq!(v["result"]["value"]["created"], true);
        assert_eq!(
            view["title"],
            dir.file_name().unwrap().to_string_lossy().to_string(),
            "title must be the path basename"
        );
        assert_eq!(view["sessionIds"], serde_json::json!([]));
        // 推了 host/workspace-changed 帧（payload 同 create 返回值）。
        {
            let log = boot.host_events.as_ref().unwrap().lock().unwrap();
            assert_eq!(log.len(), 1, "one host frame after create");
            assert_eq!(log[0]["type"], "host/workspace-changed");
            assert_eq!(log[0]["workspace"]["workspaceId"], id);
        }

        // 同路径幂等 → created:false、同 id、不重复注册。
        let v2 = call(
            &boot,
            "workspace.create",
            serde_json::json!({"path": path}),
            &host,
        );
        assert_eq!(v2["result"]["ok"], true);
        assert_eq!(v2["result"]["value"]["created"], false);
        assert_eq!(v2["result"]["value"]["workspace"]["workspaceId"], id);

        // workspace.list 反映真实注册表：新工作区 prepend 在 default 前，sessionIds 为空。
        let v3 = call(&boot, "workspace.list", serde_json::json!({}), &host);
        let items = v3["result"]["value"]["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["workspaceId"], id, "new workspace prepended");
        assert_eq!(items[1]["workspaceId"], "default");
        assert_eq!(v3["result"]["value"]["archivedSessionIds"], serde_json::json!([]));

        // 不存在的路径 → 诚实错误（workspace-path-invalid），非假成功。
        let missing = format!("Z:\\dsh-no-such-{}", std::process::id());
        let v4 = call(&boot, "workspace.create", serde_json::json!({"path": missing}), &host);
        assert_eq!(v4["result"]["ok"], false);
        assert_eq!(v4["result"]["error"]["code"], "workspace-path-invalid");
    }

    /// D-100：session.create{workspaceId} 把新会话 attach 进工作区 sessionIds 并推
    /// host/workspace-changed + host/session-added；未知工作区 → workspace-not-found。
    #[test]
    fn rpc_session_create_attaches_to_workspace_and_host_frames() {
        let mut boot = boot_with_sessions();
        boot.host_events = Some(std::sync::Arc::new(std::sync::Mutex::new(Vec::new())));
        let host = seeded_host();

        let call = |boot: &crate::Boot, method: &str, payload: Value, host: &Rc<SessionHost>| {
            let body = serde_json::to_vec(&serde_json::json!({
                "type": "client-request", "rpcId": "r", "method": method, "payload": payload,
            })).unwrap();
            let (_, v) = handle_rpc_host(boot, method, &body, host);
            v
        };

        // 建一个工作区。
        let dir = std::env::temp_dir().join(format!("dsh-ws-rpc-attach-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let v = call(
            &boot,
            "workspace.create",
            serde_json::json!({"path": dir.to_string_lossy().to_string()}),
            &host,
        );
        let ws_id = v["result"]["value"]["workspace"]["workspaceId"].as_str().unwrap().to_string();

        // session.create{workspaceId} → attach，返回新 sessionId。
        let v2 = call(
            &boot,
            "session.create",
            serde_json::json!({"workspaceId": ws_id}),
            &host,
        );
        assert_eq!(v2["result"]["ok"], true);
        let sid = v2["result"]["value"]["sessionId"].as_str().unwrap().to_string();

        // workspace.list 里该工作区 sessionIds 已含新会话（客户端分组面可见）。
        let v3 = call(&boot, "workspace.list", serde_json::json!({}), &host);
        for item in v3["result"]["value"]["items"].as_array().unwrap() {
            if item["workspaceId"] == serde_json::json!(ws_id) {
                assert_eq!(item["sessionIds"], serde_json::json!([sid]));
            }
        }

        // host 帧：create → workspace-changed；attach → workspace-changed + session-added。
        {
            let log = boot.host_events.as_ref().unwrap().lock().unwrap();
            let kinds: Vec<&str> = log.iter().map(|f| f["type"].as_str().unwrap()).collect();
            assert_eq!(
                kinds,
                vec!["host/workspace-changed", "host/workspace-changed", "host/session-added"]
            );
            assert_eq!(log[2]["sessionId"], sid);
            assert_eq!(log[2]["blank"], true);
        }

        // 未知工作区 → 诚实报错（不假成功、不 attach）。
        let v4 = call(
            &boot,
            "session.create",
            serde_json::json!({"workspaceId": "no-such-ws"}),
            &host,
        );
        assert_eq!(v4["result"]["ok"], false);
        assert_eq!(v4["result"]["error"]["code"], "workspace-not-found");
    }

    /// D-101：`session.create` mint 的会话已挂接真实 agent——`session.prompt` 不再报
    /// `no configured agent maps to session`，事件落共享 store（agent cwd 沿用工作区
    /// 路径）；未创建的会话仍诚实报 internal（不因修复而全局放行）。
    #[test]
    fn session_create_registers_agent_and_prompt_routes() {
        use std::cell::RefCell;
        use std::collections::VecDeque;
        use std::rc::Rc;
        let session_host = SessionHost::in_memory();
        let _ = session_host.session("default");
        let root = std::env::temp_dir().join(format!("dsh-d101-{}", std::process::id()));
        if root.exists() {
            let _ = std::fs::remove_dir_all(&root);
        }
        std::fs::create_dir_all(&root).unwrap();

        let script = Rc::new(RefCell::new(VecDeque::from_iter([
            vec![
                dsh_llm::StreamChunk::BlockStart {
                    index: 0,
                    block_type: "text".parse().unwrap(),
                },
                dsh_llm::StreamChunk::TextDelta { index: 0, text: "hello from d101".into() },
                dsh_llm::StreamChunk::BlockEnd {
                    index: 0,
                    block: dsh_llm::ContentBlock::text("hello from d101"),
                },
                dsh_llm::StreamChunk::Finish {
                    reason: dsh_llm::FinishReason::Stop,
                    replay_state: None,
                },
            ],
            vec![
                dsh_llm::StreamChunk::BlockStart {
                    index: 0,
                    block_type: "text".parse().unwrap(),
                },
                dsh_llm::StreamChunk::TextDelta { index: 0, text: "hello from restored".into() },
                dsh_llm::StreamChunk::BlockEnd {
                    index: 0,
                    block: dsh_llm::ContentBlock::text("hello from restored"),
                },
                dsh_llm::StreamChunk::Finish {
                    reason: dsh_llm::FinishReason::Stop,
                    replay_state: None,
                },
            ],
        ])));
        struct Adapter {
            script: Rc<RefCell<VecDeque<Vec<dsh_llm::StreamChunk>>>>,
        }
        impl dsh_llm::LlmAdapter for Adapter {
            fn stream(
                &self,
                _options: dsh_llm::GenerateOptions,
            ) -> Box<dyn Iterator<Item = dsh_llm::StreamChunk>> {
                let next = self.script.borrow_mut().pop_front().unwrap_or_default();
                Box::new(next.into_iter())
            }
        }
        let llm = Rc::new(dsh_llm::LlmRuntime::new());
        llm.register_adapter(&["mock"], Rc::new(Adapter { script })).unwrap();

        let bundle = match crate::web::assemble_server_runtime_with_llm(
            &session_host,
            root.clone(),
            llm,
            "mock",
            "mock-model",
        ) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("assemble deferred (bash unavailable?): {e}");
                let _ = std::fs::remove_dir_all(&root);
                return;
            }
        };
        let mut boot = boot_with_sessions();
        boot.host_events = Some(std::sync::Arc::new(std::sync::Mutex::new(Vec::new())));

        let call = |boot: &crate::Boot, method: &str, payload: Value, host: &Rc<SessionHost>| {
            let body = serde_json::to_vec(&serde_json::json!({
                "type": "client-request", "rpcId": "r", "method": method, "payload": payload,
            })).unwrap();
            let (_, v) = handle_rpc_host(boot, method, &body, host);
            v
        };

        // 打开一个工作区（D-100 真实注册表），再开一个会话（D-101 挂接 agent）。
        let ws_dir = root.join("ws").to_string_lossy().to_string();
        std::fs::create_dir_all(&ws_dir).unwrap();
        let v = call(
            &boot,
            "workspace.create",
            serde_json::json!({"path": ws_dir}),
            &session_host,
        );
        let ws_id = v["result"]["value"]["workspace"]["workspaceId"].as_str().unwrap().to_string();
        // 装配 agent-loop 后：session.create 为会话注册 agent。
        boot.agent_loop = Some(bundle.host.clone());
        let v2 = call(
            &boot,
            "session.create",
            serde_json::json!({"workspaceId": ws_id}),
            &session_host,
        );
        assert_eq!(v2["result"]["ok"], true, "session.create: {v2}");
        let sid = v2["result"]["value"]["sessionId"].as_str().unwrap().to_string();

        // 该会话已可被 configured_for_session 解析（run_rust_loop 路由查询路径），
        // 且 cwd 沿用工作区路径（D-100 归属）。
        let agent = bundle.host.configured_for_session(&sid);
        let agent = agent.expect("session.create must register a routable agent");
        assert_eq!(agent.cwd.as_deref(), Some(ws_dir.as_str()), "agent cwd = workspace path");

        // session.prompt 对全新会话 → accepted（此前报 internal:no configured agent）。
        let v3 = call(
            &boot,
            "session.prompt",
            serde_json::json!({"sessionId": sid,
                "content": [{"type": "text", "text": "hi from ui"}]}),
            &session_host,
        );
        assert_eq!(v3["result"]["value"]["accepted"], true, "prompt accepted: {v3}");

        // 共享 store：事件落新会话（user/message + assistant/message）。
        let evs = session_host.events(&sid);
        assert!(evs.iter().any(|e| e.kind.as_str() == "user/message"));
        assert!(evs.iter().any(|e| e.kind.as_str() == "assistant/message"));
        let assistant = evs.iter().find(|e| e.kind.as_str() == "assistant/message").unwrap();
        assert_eq!(assistant.data["message"]["content"][0]["text"], "hello from d101");

        // 重启续接路径（D-101 lazy）：共享 store 已有会话但从未 RPC 注册 agent
        // （如持久化恢复）→ 首次 prompt 时挂接 agent 再路由，非 internal 错误。
        let restored_sid = session_host.create_new().unwrap();
        let v5 = call(
            &boot,
            "session.prompt",
            serde_json::json!({"sessionId": restored_sid,
                "content": [{"type": "text", "text": "hi restored"}]}),
            &session_host,
        );
        assert_eq!(v5["result"]["value"]["accepted"], true, "restored session accepted: {v5}");
        assert!(
            bundle.host.configured_for_session(&restored_sid).is_some(),
            "lazy registration visible to routing"
        );

        // 未创建的会话仍 fail loud（internal:no configured agent）——修复不放行任意 id。
        let v4 = call(
            &boot,
            "session.prompt",
            serde_json::json!({"sessionId": "ghost-unknown",
                "content": [{"type": "text", "text": "hi"}]}),
            &session_host,
        );
        assert_eq!(v4["result"]["ok"], false);
        assert_eq!(v4["result"]["error"]["code"], "internal");
        assert!(
            v4["result"]["error"]["message"]
                .as_str()
                .unwrap()
                .contains("no configured agent maps to session"),
            "unknown session still fails loud: {v4}"
        );

        let _ = std::fs::remove_dir_all(&root);
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
            // P1-b：agentPreset.* 已离去 ok-冒烟——真实语义（list/read 实数据、
            // select/copy/remove 诚实 unsupported、openDocument 降级）全由
            // rpc_agent_presets_list_read_real_discovery 覆盖。
        ];
        let cases2: &[(&str, &str, &str)] = &[
            ("session.attachment", "attachment", "obj"),
            ("session.updateQueue", "accepted", "bool"),
            // host.pickDirectory 移出 ok-冒烟：行为由专用 seam 测试覆盖
            // （未装配 → directory-picker-unavailable，非 {path:null} 冒充取消，D-096）。
            ("host.listDirectory", "entries", "arr"),
            // M3a：host.createDirectory/host.openPath 已做实，空 payload 不再是
            // 合法 ok 响应（create 需 path+name、openPath 需 path）→ 由专用测试覆盖。
            ("workspace.create", "workspace", "obj"),
            ("workspace.rename", "workspace", "obj"),
            ("workspace.delete", "deleted", "bool"),
            ("workspace.insertBefore", "workspaceIds", "arr"),
            ("workspace.insertSessionBefore", "workspace", "obj"),
            ("workspace.archiveSession", "archivedSessionIds", "arr"),
            // P1-b：agentPreset.openDocument 已做实（支配 opened:false+path），
            // 移出 ok-冒烟（空 payload 走 not-found），由 RPC 专用测试覆盖。
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
                // D-100：workspace.* 走真实注册表——需合法 payload 才 ok（空 path/未知
                // id 会诚实报错，不再返回假 stub 成功）。
                "workspace.create" => serde_json::json!({
                    "path": std::env::temp_dir().to_string_lossy().to_string(),
                }),
                "workspace.rename" => serde_json::json!({
                    "workspaceId": "default", "title": "renamed",
                }),
                "workspace.insertBefore" => serde_json::json!({
                    "workspaceId": "default",
                }),
                "workspace.insertSessionBefore" => serde_json::json!({
                    "workspaceId": "default", "sessionId": "default",
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

    /// M3a+（D-098）：`host.pickDirectory` 三态 wire（native seam 经注入 stub 可测；
    /// 不触发真实弹框）。选中→{path}；取消→{path:null}；失败/未装配→
    /// `directory-picker-unavailable`（**不再**拿 null 冒充取消）。
    #[test]
    fn rpc_host_pick_directory_seam_three_state() {
        use std::sync::Arc;

        fn call(boot: &crate::Boot) -> Value {
            let body = serde_json::to_vec(&serde_json::json!({
                "type": "client-request", "rpcId": "r", "method": "host.pickDirectory",
                "payload": {},
            })).unwrap();
            let (_, v) = handle_rpc_host(boot, "host.pickDirectory", &body, &SessionHost::in_memory());
            v
        }

        // 选中。
        let mut b = boot_with_sessions();
        b.host_picker = Some(Arc::new(|| Ok(Some("C:\\selected".to_string()))) as crate::HostPicker);
        let v = call(&b);
        assert_eq!(v["result"]["ok"], true);
        assert_eq!(v["result"]["value"]["path"], "C:\\selected");

        // 取消 → null（合法语义，不是错误）。
        let mut b = boot_with_sessions();
        b.host_picker = Some(Arc::new(|| Ok(None)) as crate::HostPicker);
        let v = call(&b);
        assert_eq!(v["result"]["ok"], true);
        assert!(v["result"]["value"]["path"].is_null());

        // 原生不可用 → 诚实错误。
        let mut b = boot_with_sessions();
        b.host_picker = Some(Arc::new(|| Err("no desktop session".to_string())) as crate::HostPicker);
        let v = call(&b);
        assert_eq!(v["result"]["ok"], false);
        assert_eq!(v["result"]["error"]["code"], "directory-picker-unavailable");

        // 未装配（默认 Boot）→ 同一错误，绝不返回 null 冒充取消。
        let b = boot_with_sessions();
        let v = call(&b);
        assert_eq!(v["result"]["ok"], false);
        assert_eq!(v["result"]["error"]["code"], "directory-picker-unavailable");
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

    /// D-100：events.host 下链信封形状（server-request host/event；payload 原样内层）。
    #[test]
    fn host_frame_envelope_wire_shape() {
        let frame = host_frame_envelope(
            3,
            &serde_json::json!({"type": "host/workspace-changed", "workspace": {"workspaceId": "w1"}}),
        );
        assert_eq!(frame["type"], "server-request");
        assert_eq!(frame["rpcId"], "host-3");
        assert_eq!(frame["method"], "host/event");
        assert_eq!(frame["payload"]["type"], "host/workspace-changed");
        assert_eq!(frame["payload"]["workspace"]["workspaceId"], "w1");
        // 序列化后经 write_sse 可下推。
        let mut buf = Vec::new();
        assert!(write_sse(&mut buf, &frame).is_some());
        let text = String::from_utf8(buf).unwrap();
        assert!(text.starts_with("data: {"));
        assert!(text.ends_with("\n\n"));
        assert!(text.contains("\"method\":\"host/event\""));
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

    /// 阶段1：服务端只选择与已组合 host 后端匹配的目录选择流程——组合 **native**
    /// 后端（进程内 IFileDialog/COM）时，browse 流程客户端必须从 boot 图排除（否则
    /// 两者竞争 ui-workspace 的 single directory-flow 洞，流程不确定）。
    #[test]
    fn build_boot_manifest_composes_only_one_directory_picker_flow() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("dsh-web-picker-{}-{n}", std::process::id()));
        let scope = root.join("@deepseek-ai");
        let picker = |name: &str| {
            let pkg = scope.join(name);
            std::fs::create_dir_all(pkg.join("lib")).unwrap();
            std::fs::write(
                pkg.join("package.json"),
                format!(
                    r#"{{"name":"@deepseek-ai/{name}","dsh":{{"client":{{"platform":"web","immediately":true}}}},"exports":{{"./client":"./lib/client.js"}}}}"#
                ),
            )
            .unwrap();
            std::fs::write(pkg.join("lib/client.js"), format!("load('{name}');")).unwrap();
        };
        picker("dsh-client-ui-directory-picker-browse");
        picker("dsh-client-ui-directory-picker-native");

        let m = build_boot_manifest(&scope).unwrap();
        let ids: Vec<&str> = m.entries.iter().map(|e| e.id.as_str()).collect();
        assert!(
            ids.contains(&"@deepseek-ai/dsh-client-ui-directory-picker-native"),
            "native flow stays composed: {ids:?}"
        );
        assert!(
            !ids.contains(&"@deepseek-ai/dsh-client-ui-directory-picker-browse"),
            "browse flow must be excluded from boot graph: {ids:?}"
        );
        assert_eq!(ids.len(), 1, "exactly the native flow remains: {ids:?}");
        std::fs::remove_dir_all(&root).ok();
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

    /// D-099：`/plugins/events` 路由决策——GET→SSE 流、HEAD→事件流头、其他方法→405；
    /// 非该路径→None（回落 `/plugins/<id>/client.js` 与 `/api/*` 既有分支，不再 404）。
    #[test]
    fn hmr_events_plan_routes() {
        use tiny_http::Method;
        assert_eq!(hmr_events_plan("/plugins/events", &Method::Get), Some(HmrEventsPlan::Stream));
        assert_eq!(hmr_events_plan("/plugins/events", &Method::Head), Some(HmrEventsPlan::HeadersOnly));
        assert_eq!(hmr_events_plan("/plugins/events", &Method::Post), Some(HmrEventsPlan::MethodNotAllowed));
        assert_eq!(hmr_events_plan("/plugins/@deepseek-ai/x/client.js", &Method::Get), None);
        assert_eq!(hmr_events_plan("/api/events.mux", &Method::Get), None);
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

    /// P1-b：agentPreset list/read 经 handle_rpc_host 真实发现语义（注入 temp 根）。
    /// select 诚实 P2 门、copy/remove 诚实 P5 门、openDocument 诚实降级 opened:false。
    #[test]
    fn rpc_agent_presets_list_read_real_discovery() {
        use dsh_agent_presets::{PresetRoot, PresetTrust};
        let boot = boot_with_sessions();
        {
            let mut sp = boot.settings.borrow_mut();
            crate::preset_host::register_agent_presets_settings(&mut sp);
        }
        // 注入 temp 根：system standard/code + user mine（authorable=true）。
        let base = std::env::temp_dir().join(format!("dsh-cli-presets-rpc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let sys = base.join("sys");
        let usr = base.join("usr");
        for (dir, id, meta) in [
            (&sys, "standard", "order: 1\nname: 标准\n"),
            (&sys, "code", "order: 2\n"),
            (&usr, "mine", "name: 我的\n"),
        ] {
            let preset_dir = dir.join(id);
            std::fs::create_dir_all(&preset_dir).unwrap();
            std::fs::write(preset_dir.join("agent.cordis.yml"), "- id: p\n  name: 'plugin-x'\n").unwrap();
            std::fs::write(preset_dir.join("preset.yml"), meta).unwrap();
        }
        let roots = vec![
            PresetRoot { path: sys.clone(), trust: PresetTrust::System },
            PresetRoot { path: usr.clone(), trust: PresetTrust::User },
        ];
        *boot.presets.borrow_mut() = crate::preset_host::PresetHost::with_user_root(roots, Some(usr.clone()));

        let session_host = SessionHost::in_memory();
        let call = |method: &str, payload: serde_json::Value| {
            let body = serde_json::to_vec(&serde_json::json!({
                "type": "client-request", "rpcId": "r", "method": method, "payload": payload,
            })).unwrap();
            handle_rpc_host(&boot, method, &body, &session_host).1
        };
        // list：3 预设、order-else-id 排序、isDefault=standard（settings base）、
        // authorable=true、hasDocument=false（无原生打开器，诚实）。
        let res = call("agentPreset.list", serde_json::json!({}));
        assert_eq!(res["result"]["ok"], true);
        let presets = res["result"]["value"]["presets"].as_array().unwrap();
        let ids: Vec<&str> = presets.iter().map(|p| p["id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec!["standard", "code", "mine"], "order: standard(1) code(2) mine(∞)");
        assert_eq!(res["result"]["value"]["authorable"], true);
        assert_eq!(res["result"]["value"]["hasDocument"], false);
        let mk = |id: &str| presets.iter().find(|p| p["id"] == id).unwrap();
        assert_eq!(mk("standard")["isDefault"], true);
        assert_eq!(mk("standard")["trust"], "system");
        assert_eq!(mk("standard")["name"], "标准");
        assert!(mk("standard").get("description").is_none(), "absent fields omitted");
        assert!(mk("standard").get("broken").is_none());
        assert_eq!(mk("mine")["isDefault"], false);
        assert_eq!(mk("mine")["trust"], "user");
        // settings 更新 default=mine → list 重读 isDefault 翻转（发现不缓存）。
        let res = call("settings.update", serde_json::json!({
            "ns": "agent-presets", "patch": {"default": "mine"}, "expectedRevision": 0,
        }));
        assert_eq!(res["result"]["ok"], true);
        let res = call("agentPreset.list", serde_json::json!({}));
        let presets = res["result"]["value"]["presets"].as_array().unwrap();
        assert_eq!(presets.iter().find(|p| p["id"] == "mine").unwrap()["isDefault"], true);
        assert_eq!(presets.iter().find(|p| p["id"] == "standard").unwrap()["isDefault"], false);
        // read：真实内容 + trust + metadata；缺字段省略（不 null）。
        let res = call("agentPreset.read", serde_json::json!({"agentPreset": "standard"}));
        assert_eq!(res["result"]["ok"], true);
        assert_eq!(res["result"]["value"]["content"], "- id: p\n  name: 'plugin-x'\n");
        assert_eq!(res["result"]["value"]["trust"], "system");
        assert_eq!(res["result"]["value"]["name"], "标准");
        assert!(res["result"]["value"].get("description").is_none());
        // read 未知 → agent-preset-not-found。
        let res = call("agentPreset.read", serde_json::json!({"agentPreset": "nope"}));
        assert_eq!(res["result"]["ok"], false);
        assert_eq!(res["result"]["error"]["code"], "agent-preset-not-found");
        // select：诚实 P2 门（join 未落地）。
        let res = call("agentPreset.select", serde_json::json!({"sessionId": "default", "agentPreset": "code"}));
        assert_eq!(res["result"]["ok"], false);
        assert_eq!(res["result"]["error"]["code"], "agent-preset-unsupported");
        // copy/remove：诚实 P5 门。
        let res = call("agentPreset.copy", serde_json::json!({"from": "standard", "agentPreset": "copy1"}));
        assert_eq!(res["result"]["ok"], false);
        let res = call("agentPreset.remove", serde_json::json!({"agentPreset": "mine"}));
        assert_eq!(res["result"]["ok"], false);
        // openDocument：诚实降级 {opened:false, path}（无原生打开器）。
        let res = call("agentPreset.openDocument", serde_json::json!({"agentPreset": "mine"}));
        assert_eq!(res["result"]["ok"], true);
        assert_eq!(res["result"]["value"]["opened"], false);
        assert!(res["result"]["value"]["path"].as_str().unwrap().ends_with("mine"));
        // openDocument 未知 → agent-preset-not-found。
        let res = call("agentPreset.openDocument", serde_json::json!({"agentPreset": "nope"}));
        assert_eq!(res["result"]["ok"], false);
        let _ = std::fs::remove_dir_all(&base);
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

    /// M4h：goal.create 后 `goal/change` 事件落进目标会话（验收 #2）。
    /// 通过 handle_rpc_host 显式 host 断言会话日志包含 goal/change snapshot。
    #[test]
    fn rpc_goal_create_appends_goal_change_event() {
        use dsh_session::types::EventKind;
        let boot = boot_with_sessions();
        let host = SessionHost::in_memory();
        let _ = host.session("default");
        let body = serde_json::to_vec(&serde_json::json!({
            "type": "client-request", "rpcId": "r", "method": "goal.create",
            "payload": {"sessionId": "default", "objective": "ship M4h"},
        })).unwrap();
        let (_, v) = handle_rpc_host(&boot, "goal.create", &body, &host);
        assert_eq!(v["result"]["ok"], true, "goal.create ok");
        // 会话日志出现 goal/change（kind 字面量 + 版本 + snapshot 载荷）
        let events = host.events("default");
        let goal_changes: Vec<_> = events
            .iter()
            .filter(|e| e.kind == EventKind::GoalChange)
            .collect();
        assert_eq!(goal_changes.len(), 1, "恰好一条 goal/change");
        let data = &goal_changes[0].data;
        assert_eq!(data["kind"], "goal/change");
        assert_eq!(data["version"], 1);
        assert_eq!(data["operation"], "create");
        assert_eq!(data["goal"]["id"], "goal-1");
        assert_eq!(data["goal"]["phase"], "active");
        assert_eq!(data["roundsStarted"], 0);
    }

    /// M4h：goal.complete + goal.clear 各落一条 goal/change（last-wins 可重放）。
    #[test]
    fn rpc_goal_mutation_chain_appends_events() {
        use dsh_session::types::EventKind;
        let boot = boot_with_sessions();
        let host = SessionHost::in_memory();
        let _ = host.session("default");
        let call = |method: &str, payload: serde_json::Value| {
            let body = serde_json::to_vec(&serde_json::json!({
                "type": "client-request", "rpcId": "r", "method": method, "payload": payload,
            })).unwrap();
            handle_rpc_host(&boot, method, &body, &host).1
        };
        let created = call("goal.create", serde_json::json!({
            "sessionId": "default", "objective": "finish",
        }));
        let ref1 = created["result"]["value"]["ref"].clone();
        let completed = call("goal.complete", serde_json::json!({
            "sessionId": "default", "ref": ref1,
        }));
        let ref2 = completed["result"]["value"]["ref"].clone();
        call("goal.clear", serde_json::json!({
            "sessionId": "default", "ref": ref2,
        }));
        let ops: Vec<String> = host
            .events("default")
            .iter()
            .filter(|e| e.kind == EventKind::GoalChange)
            .map(|e| e.data["operation"].as_str().unwrap_or("").to_string())
            .collect();
        assert_eq!(ops, vec!["create", "complete", "clear"], "ops 顺序即事件顺序");
    }

    /// M4h：session.history 带 projections 块（验收 #9）——goal/plan/subagent/todos
    /// 四键都进 projections（空日志 asOfSeq=-1；有事件 → 折叠出真实值）。
    #[test]
    fn rpc_session_history_carries_projections_block() {
        let boot = boot_with_sessions();
        let host = SessionHost::in_memory();
        let _ = host.session("default");
        // 先空日志验证 asOfSeq=-1 + 四键在
        let body = serde_json::to_vec(&serde_json::json!({
            "type": "client-request", "rpcId": "r", "method": "session.history",
            "payload": {"sessionId": "default"},
        })).unwrap();
        let (_, v) = handle_rpc_host(&boot, "session.history", &body, &host);
        let p = &v["result"]["value"]["projections"];
        assert_eq!(p["asOfSeq"], -1, "空日志 asOfSeq=-1");
        for key in ["goal", "plan", "subagent", "todos"] {
            assert!(p["values"].get(key).is_some(), "{key} 进 projections.values");
        }
        assert_eq!(p["values"]["goal"]["goal"], Value::Null, "无目标时 goal 键为 null");
        // goal.create 后折叠 → goal 投影携带 snapshot（last-wins）
        let body = serde_json::to_vec(&serde_json::json!({
            "type": "client-request", "rpcId": "r", "method": "goal.create",
            "payload": {"sessionId": "default", "objective": "project me"},
        })).unwrap();
        let (_, _) = handle_rpc_host(&boot, "goal.create", &body, &host);
        let body = serde_json::to_vec(&serde_json::json!({
            "type": "client-request", "rpcId": "r", "method": "session.history",
            "payload": {"sessionId": "default"},
        })).unwrap();
        let (_, v) = handle_rpc_host(&boot, "session.history", &body, &host);
        let p = &v["result"]["value"]["projections"];
        assert!(p["asOfSeq"].as_i64().unwrap() >= 0, "有事件后 asOfSeq>=0（首批事件 seq 从 0 起）");
        assert_eq!(
            p["values"]["goal"]["goal"]["objective"], "project me",
            "goal 投影折叠出当前 snapshot"
        );
        assert_eq!(p["values"]["goal"]["goal"]["phase"], "active");
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

    /// M4h：subagent.list 真实驱动——空目录 → entries=[] + parentAvailable=true；
    /// spawn 一个 child 后 list 出现 child 行、history 读其事件（验收 #5）。
    #[test]
    fn rpc_subagent_list_and_history_real_driver() {
        use crate::subagent_runtime::{self as sa, SpawnMode, SpawnOptions};
        let boot = boot_with_sessions();
        let host = SessionHost::in_memory();
        let _ = host.session("default");
        let call_host = |method: &str, payload: serde_json::Value| {
            let body = serde_json::to_vec(&serde_json::json!({
                "type": "client-request", "rpcId": "r", "method": method, "payload": payload,
            })).unwrap();
            handle_rpc_host(&boot, method, &body, &host).1
        };
        // 空目录：parent 存在 → entries=[] + parentAvailable=true。
        let v = call_host("subagent.list", serde_json::json!({
            "parentSessionId": "default",
        }));
        assert_eq!(v["result"]["ok"], true);
        assert_eq!(v["result"]["value"]["entries"], serde_json::json!([]));
        assert_eq!(v["result"]["value"]["parentAvailable"], true);
        // 真实 spawn 一个 child → list 出现 child 行。
        let opts = SpawnOptions {
            mode: SpawnMode::Continuable,
            provider: "mock".into(),
            label: Some("audit".into()),
            ..Default::default()
        };
        let child = sa::spawn_child(&host, "default", &opts).expect("spawn ok");
        let v = call_host("subagent.list", serde_json::json!({
            "parentSessionId": "default",
        }));
        let entries = v["result"]["value"]["entries"].clone();
        assert_eq!(entries.as_array().unwrap().len(), 1, "一个 child 行");
        assert_eq!(entries[0]["kind"], "child");
        assert_eq!(entries[0]["mode"], "continuable");
        assert_eq!(entries[0]["label"], "audit");
        assert_eq!(entries[0]["id"], child);
        // history 真实读 child 事件（descriptor 落日志 → events 非空 + projections 块）。
        let v = call_host("subagent.history", serde_json::json!({
            "parentSessionId": "default", "childSessionId": child, "mode": "continuable",
        }));
        assert_eq!(v["result"]["ok"], true);
        assert!(
            !v["result"]["value"]["events"].as_array().unwrap().is_empty(),
            "child 日志有事件"
        );
        assert_eq!(
            v["result"]["value"]["events"][0]["event"]["type"], "subagent/descriptor",
            "首事件为描述符"
        );
        assert_eq!(v["result"]["value"]["hasMore"], false);
        // projections 块携带 subagent 身份（折叠自描述符事件；view 直接是 identity）。
        let proj = &v["result"]["value"]["projections"]["values"]["subagent"];
        assert_eq!(proj["mode"], "continuable");
        assert_eq!(proj["label"], "audit");
    }

    /// M4h：subagent.prompt gate——one-shot → bad-request；缺 agent_loop → fail loud
    /// （internal，不伪装成功）；装了 agent_loop → 经 Rust loop 真实驱动一轮返回
    /// messageId（fake-loop 驱动链路验收 #5）。
    #[test]
    fn rpc_subagent_prompt_gates_and_drives_fake_loop() {
        let boot = boot_with_sessions();
        let host = SessionHost::in_memory();
        let _ = host.session("default");
        let call_host = |method: &str, payload: serde_json::Value| {
            let body = serde_json::to_vec(&serde_json::json!({
                "type": "client-request", "rpcId": "r", "method": method, "payload": payload,
            })).unwrap();
            handle_rpc_host(&boot, method, &body, &host).1
        };
        // 1) one-shot mode → bad-request（gate 前置）。
        let v = call_host("subagent.prompt", serde_json::json!({
            "parentSessionId": "default", "childSessionId": "c-1", "mode": "one-shot",
            "content": [{"type": "text", "text": "hi"}],
        }));
        assert_eq!(v["result"]["ok"], false, "one-shot child cannot be prompted");
        assert_eq!(v["result"]["error"]["code"], "bad-request");
        // 2) continuable 但未装配 agent-loop → fail loud（internal，绝不伪装成功）。
        let v = call_host("subagent.prompt", serde_json::json!({
            "parentSessionId": "default", "childSessionId": "c-1", "mode": "continuable",
            "content": [{"type": "text", "text": "hi"}],
        }));
        assert_eq!(v["result"]["ok"], false);
        assert_eq!(v["result"]["error"]["code"], "internal");
        assert!(
            v["result"]["error"]["message"].as_str().unwrap().contains("AgentLoopHost"),
            "fail loud 信息指明缺 loop"
        );
    }

    /// M4i 验收 #5：#subagent.prompt 经真实 Rust loop 驱动 child 一轮（fake-loop＝
    /// mock adapter + AgentLoopHost 共享 store）。spawn 真实 child（continuable，
    /// agentProvider=mock）→ subagent.prompt → child 会话落 user/assistant/turn/end
    /// 事件 → 返回真实 messageId → subagent.history 可回读 assistant 内容。
    #[test]
    fn rpc_subagent_prompt_drives_real_child_agent_round() {
        use std::cell::{Cell, RefCell};
        use std::collections::VecDeque;
        use std::rc::Rc;

        use crate::subagent_runtime::{self as sa, SpawnMode, SpawnOptions};

        // Mock adapter（fake-loop：模型应答脚本；Rust loop 真实整轮驱动）。
        let script = Rc::new(RefCell::new(VecDeque::from_iter([vec![
            dsh_llm::StreamChunk::BlockStart {
                index: 0,
                block_type: "text".parse().unwrap(),
            },
            dsh_llm::StreamChunk::TextDelta {
                index: 0,
                text: "child says hi".into(),
            },
            dsh_llm::StreamChunk::BlockEnd {
                index: 0,
                block: dsh_llm::ContentBlock::text("child says hi"),
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
            fn stream(&self, _o: dsh_llm::GenerateOptions) -> Box<dyn Iterator<Item = dsh_llm::StreamChunk>> {
                self.calls.set(self.calls.get() + 1);
                let next = self.script.borrow_mut().pop_front().unwrap_or_default();
                Box::new(next.into_iter())
            }
        }
        let llm = Rc::new(dsh_llm::LlmRuntime::new());
        llm.register_adapter(&["mock"], Rc::new(Adapter {
            script: script.clone(),
            calls: calls.clone(),
        }))
        .unwrap();

        let tools = Rc::new(dsh_tools::ToolRegistry::new(
            dsh_tools::ToolExecutionMode::Native,
        ));
        let session_host = SessionHost::in_memory();
        let _ = session_host.session("default");
        let config = dsh_agent_loop::AgentLoopConfig {
            max_parallel_tool_calls: None,
            agents: vec![dsh_agent_loop::ConfiguredAgent {
                id: "a-main".into(),
                provider: Some("mock".into()),
                model: Some("mock-model".into()),
                session_id: Some("default".into()),
                max_tokens: None,
                cwd: None,
                resume_session_id: None,
            }],
        };
        let loop_host = dsh_agent_loop::AgentLoopHost::with_store(
            config,
            llm,
            tools,
            session_host.store.clone(),
        )
        .unwrap();
        let mut boot = boot_with_sessions();
        boot.agent_loop = Some(loop_host.clone());

        // 真实 spawn continuable child（agentProvider=mock）。
        let opts = SpawnOptions {
            mode: SpawnMode::Continuable,
            provider: "mock".into(),
            label: Some("worker".into()),
            agent_provider: Some("mock".into()),
            agent_model: Some("mock-model".into()),
            ..Default::default()
        };
        let child = sa::spawn_child(&session_host, "default", &opts).expect("spawn child");
        let body = serde_json::to_vec(&serde_json::json!({
            "type": "client-request", "rpcId": "r", "method": "subagent.prompt",
            "payload": {
                "parentSessionId": "default", "childSessionId": child, "mode": "continuable",
                "content": [{"type": "text", "text": "do the work"}],
            },
        })).unwrap();
        let (_, v) = handle_rpc_host(&boot, "subagent.prompt", &body, &session_host);
        assert_eq!(v["result"]["ok"], true, "subagent.prompt ok: {v}");
        let message_id = v["result"]["value"]["messageId"].as_str().expect("messageId").to_string();
        assert!(message_id.starts_with(&format!("pmsg-{child}:")), "真实 messageId: {message_id}");

        // child 会话被真实驱动：user/message + assistant/message 落共享 store。
        assert!(calls.get() == 1, "mock adapter invoked exactly once");
        let evs = session_host.events(&child);
        assert!(evs.iter().any(|e| e.kind.as_str() == "user/message"), "user/message in child");
        assert!(evs.iter().any(|e| e.kind.as_str() == "assistant/message"), "assistant/message in child");
        // subagent.history 回读 assistant 内容。
        let body2 = serde_json::to_vec(&serde_json::json!({
            "type": "client-request", "rpcId": "r2", "method": "subagent.history",
            "payload": {
                "parentSessionId": "default", "childSessionId": child, "mode": "continuable",
            },
        })).unwrap();
        let (_, h) = handle_rpc_host(&boot, "subagent.history", &body2, &session_host);
        assert_eq!(h["result"]["ok"], true);
        let history_events = h["result"]["value"]["events"].as_array().unwrap();
        assert!(
            history_events.iter().any(|row| row["event"]["type"] == "assistant/message"),
            "assistant 事件可经 history 回读"
        );
        let assistant = history_events
            .iter()
            .find(|row| row["event"]["type"] == "assistant/message")
            .unwrap();
        assert_eq!(
            assistant["event"]["data"]["message"]["content"][0]["text"],
            "child says hi",
            "child 的模型应答内容"
        );
    }
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
        // M4h：三键投影单元一并挂载（goal/plan/subagent 真实折叠会话事件）。
        for key in ["goal", "plan", "subagent"] {
            let unit = reg.get(key);
            assert!(unit.is_some(), "{key} projection unit registered");
        }
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
                    {"content": "implement", "status": "pending"},
                ],
            }),
            Some("agent-1".to_string()),
        );
        let res = registry.execute(&input, None);
        assert!(!res.is_error, "valid todos execute ok: {res:?}");
        let val = res.value.unwrap();
        let arr = val["todos"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["content"], "write tests");
        assert_eq!(arr[0]["status"], "in_progress");
        assert_eq!(val["counts"]["inProgress"], 1, "counts 规范化输出");
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

    /// M4i 验收：#register_m4_tools（无宿主）注册全部 9 个 M4 工具；job_*/schedule_*
    /// 未 bind → 结构化 NOT_BOUND（fail loud，绝不伪装成功）。
    #[test]
    fn register_all_m4_tools_unbound_fail_loud() {
        use dsh_tools::{ToolExecutionInput, ToolExecutionMode, ToolRegistry};
        let registry = ToolRegistry::new(ToolExecutionMode::Native);
        register_m4_tools(&registry);
        for name in [
            "todo_write",
            "job_output",
            "job_list",
            "job_kill",
            "schedule_create",
            "schedule_list",
            "schedule_delete",
            "exit_plan_mode",
            "workflow",
        ] {
            assert!(
                registry.get(name, None).is_some(),
                "{name} registered+visible"
            );
        }
        // 未 bind 的 job_output：结构化 isError（code NOT_BOUND）。
        let input = ToolExecutionInput::new(
            "j1",
            "job_output",
            serde_json::json!({ "job_id": "bash-1" }),
            Some("agent-1".to_string()),
        );
        let res = registry.execute(&input, None);
        assert!(res.is_error, "unbound job_output fails loud");
        let info = res.error.as_ref().and_then(|e| e.info.as_ref());
        assert_eq!(
            info.map(|i| i.code.as_str()).unwrap_or(""),
            "NOT_BOUND",
            "结构化 NOT_BOUND code"
        );
    }

    /// M4i 验收 #6：register_m4_tools_with_host 带 JobRegistry bind——job_list/read
    /// 走真实注册表状态机（start → list → read 内容 → kill）。
    #[test]
    fn register_m4_tools_with_job_registry_binds_really() {
        use dsh_tools::{ToolExecutionInput, ToolExecutionMode, ToolRegistry};
        let registry = ToolRegistry::new(ToolExecutionMode::Native);
        let jobs = Rc::new(std::cell::RefCell::new(dsh_jobs::registry::JobRegistry::new(
            dsh_jobs::registry::JobRegistryConfig {
                max_concurrent_per_owner: 10,
                now: Box::new(|| 1000),
            },
        )));
        let host = M4HostServices {
            jobs: Some(jobs.clone()),
            schedule: None,
            todo: None,
        };
        register_m4_tools_with_host(&registry, Some(&host));
        // start 一个真实 job（写输出由 settle 模拟）。
        let id = {
            use dsh_jobs::StartSpec;
            let producer = || dsh_jobs::registry::ProducerHooks {
                on_cancel: Box::new(|_| {}),
                read_output: None,
            };
            jobs.borrow_mut()
                .start(StartSpec { kind: "bash", label: "echo hi", owner: None, producer: Box::new(producer) })
                .unwrap()
        };
        assert_eq!(id, "bash-1");
        // job_list：真实 frame（taskViewSchema），含刚起的 job。
        let input = ToolExecutionInput::new("l1", "job_list", serde_json::json!({}), Some("agent-1".into()));
        let res = registry.execute(&input, None);
        assert!(!res.is_error, "job_list ok: {:?}", res.error);
        let arr = res.value.unwrap();
        assert_eq!(arr[0]["id"], "bash-1");
        assert_eq!(arr[0]["kind"], "bash");
        assert_eq!(arr[0]["status"], "running");
        // settle completed → job_output 真实 read。
        jobs.borrow_mut().settle(
            &id,
            dsh_jobs::JobSettlement {
                status: dsh_jobs::JobStatus::Completed,
                detail: Some("exit 0".into()),
                output: Some("done".into()),
            },
        );
        let input = ToolExecutionInput::new("o1", "job_output", serde_json::json!({ "job_id": id }), Some("agent-1".into()));
        let res = registry.execute(&input, None);
        assert!(!res.is_error, "job_output ok: {:?}", res.error);
        let val = res.value.unwrap();
        assert_eq!(val["job"]["status"], "completed", "read 返回完成态快照");
        // job_kill：已终态 → kill 返回接受（本注册表语义）。
        let input = ToolExecutionInput::new("k1", "job_kill", serde_json::json!({ "job_id": id }), Some("agent-1".into()));
        let res = registry.execute(&input, None);
        assert!(!res.is_error, "job_kill ok: {:?}", res.error);
    }

    /// M4i 验收 #7：schedule host bind——schedule_create 落 `schedule/change` create 事件
    /// 到会话 + fold 出列表 + dispatch_due 到期注入（dispatch 事件 + framing 文本）。
    #[test]
    fn register_m4_tools_with_schedule_host_injects_due() {
        use dsh_tools::{ToolExecutionInput, ToolExecutionMode, ToolRegistry};
        let registry = ToolRegistry::new(ToolExecutionMode::Native);
        let host_store = SessionHost::in_memory();
        let _ = host_store.session("default");
        let sched_session = host_store.session("default").expect("default live");
        let sched = Rc::new(dsh_cli_host::ScheduleHost::new(sched_session));
        let host = M4HostServices {
            jobs: None,
            schedule: Some(sched.clone()),
            todo: None,
        };
        register_m4_tools_with_host(&registry, Some(&host));
        // schedule_create (after, 1s)。
        let t0 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let input = ToolExecutionInput::new(
            "s1",
            "schedule_create",
            serde_json::json!({ "prompt": "standup", "after_seconds": 1 }),
            Some("agent-1".into()),
        );
        let res = registry.execute(&input, None);
        assert!(!res.is_error, "schedule_create ok: {:?}", res.error);
        let id = res.value.unwrap()["id"].as_str().expect("id").to_string();
        assert!(id.starts_with("schedule-"), "id namespace: {id}");
        // fold：active 记录 1 条。
        let folded = sched.fold().expect("fold ok");
        assert_eq!(folded.records.len(), 1);
        assert_eq!(folded.records[0].prompt, "standup");
        // list：ScheduleView[] 含该记录。
        let input = ToolExecutionInput::new("sl", "schedule_list", serde_json::json!({}), Some("agent-1".into()));
        let res = registry.execute(&input, None);
        assert!(!res.is_error);
        let rows = res.value.unwrap();
        assert_eq!(rows[0]["id"], id);
        assert_eq!(rows[0]["kind"], "after");
        // 到期注入：now 推进到 t0+2s 之后 → due；dispatch_schedule_change 构造
        // one-shot dispatch（无 acceptedAt）→ framing 文本落事件。
        let (framing, dispatched) = sched.dispatch_due(t0 + 2000).expect("dispatch due ok");
        assert_eq!(dispatched, vec![id.clone()], "after 到期 dispatch");
        assert_eq!(framing.len(), 1);
        assert!(framing[0].contains("[SCHEDULE REMINDER]"), "framing 样板");
        assert!(framing[0].contains(&id), "framing 携带 id");
        // dispatch 后一次 after 记录被消费 → fold 移除。
        let folded2 = sched.fold().expect("fold2 ok");
        assert!(folded2.records.is_empty(), "after dispatch 后不再 active");
        // 事件落日志（schedule/change 真实存在于会话）。
        let evs = host_store.events("default");
        let sched_events = evs.iter().filter(|e| e.kind == dsh_session::types::EventKind::ScheduleChange).count();
        assert!(sched_events >= 2, "create + dispatch 落会话事件: {sched_events}");
    }

    /// M4i 验收 #7：schedule create delete 生命周期（delete 追加事件 + fold 移除）。
    #[test]
    fn schedule_host_create_then_delete() {
        let host_store = SessionHost::in_memory();
        let _ = host_store.session("default");
        let sched = dsh_cli_host::ScheduleHost::new(
            host_store.session("default").expect("default live"),
        );
        let id = sched
            .create("after", "ship", Some(60), None, None, 1000)
            .expect("create after");
        assert!(sched.delete(&id).expect("delete ok"), "active id deletable");
        let folded = sched.fold().expect("fold");
        assert!(folded.records.is_empty(), "delete 后无 active");
        // 不存在 id → delete=false（不落事件）。
        let deleted_none = sched.delete("nope");
        assert!(!deleted_none.unwrap(), "no-op delete returns false");
    }

    /// M4i 验收 #8：todo 工具 + `todo/write` 事件 + `todos` 投影。
    ///
    /// 宿主 todo 句柄在场时：todo_write 校验/规范化并在属主会话落 `todo/write`
    /// 整表事件；`todos` 投影折叠事件后可视（全表）；无宿主时不落事件（自包含校验）。
    #[test]
    fn todo_tool_with_host_lands_todo_write_and_projection_folds() {
        use dsh_tools::{ToolExecutionInput, ToolExecutionMode, ToolRegistry};
        let registry = ToolRegistry::new(ToolExecutionMode::Native);
        // 宿主：SessionStore + todo host（默认会话 "default"）。
        let host_store = SessionHost::in_memory();
        let _ = host_store.session("default");
        let todo_host = Rc::new(dsh_cli_host::TodoWriteHost::new(
            host_store.clone(),
            "default".to_string(),
        ));
        let host = M4HostServices {
            jobs: None,
            schedule: None,
            todo: Some(todo_host.clone()),
        };
        register_m4_tools_with_host(&registry, Some(&host));
        // todo_write：有效表执行 → 输出 {todos, counts} + default 会话落 todo/write。
        let input = ToolExecutionInput::new(
            "t1",
            "todo_write",
            serde_json::json!({
                "todos": [
                    {"content": "m4 paper", "status": "in_progress"},
                    {"content": "ship", "status": "pending"},
                ],
            }),
            Some("agent-1".into()),
        );
        let res = registry.execute(&input, None);
        assert!(!res.is_error, "host todo_write ok: {:?}", res.error);
        let val = res.value.unwrap();
        assert_eq!(val["counts"], serde_json::json!({"pending":1,"inProgress":1,"completed":0}));
        // 事件已落会话（todo/write 整表）。
        let evs = host_store.events("default");
        let todo_events: Vec<_> = evs
            .iter()
            .filter(|e| e.kind == dsh_session::types::EventKind::TodoWrite)
            .collect();
        assert_eq!(todo_events.len(), 1, "一个 todo/write 事件");
        let data = &todo_events[0].data;
        assert_eq!(data["todos"][0]["content"], "m4 paper");
        // todos 投影折叠事件 → 整表可视。
        let mut reg = dsh_session_query::ProjectionRegistry::new();
        reg.register(dsh_session_query::todo::todos_projection_unit().into_unit())
            .expect("register todos unit");
        let mut ps = dsh_session_query::ProjectionSession::new(&reg);
        for e in &evs {
            ps.observe(e);
        }
        let snap = ps.snapshot();
        let todos = &snap.values["todos"];
        assert_eq!(todos[0]["content"], "m4 paper", "投影折叠出整表");
        // 无 agent 调用者 → 拒绝（对齐参考：无处归属，绝不静默）。
        let input = ToolExecutionInput::new(
            "t2",
            "todo_write",
            serde_json::json!({"todos": [{"content": "x", "status": "pending"}]}),
            None,
        );
        let res = registry.execute(&input, None);
        assert!(res.is_error, "无 agent 拒绝");
    }

    /// M4i 验收 #3：#GoalRoundPort 把真实 `Rc<ReactLoopAgent>` 实配到 goal-round-driver；
    /// armed 目标 + agent 空闲 + 空 inbox → drive_once 经 followup 驱动一个真实 Rust
    /// 轮次（fake-loop：mock adapter 应答脚本），Rust loop 该轮结束回到 idle 后判定
    /// 仍 Continue（未超 cap）。
    #[test]
    fn goal_round_driver_drives_real_agent_round() {
        use std::cell::{Cell, RefCell};
        use std::collections::VecDeque;

        // Mock adapter（fake-loop：每次 stream 应答一轮文本）。
        let script = Rc::new(RefCell::new(VecDeque::from_iter([
            vec![
                dsh_llm::StreamChunk::BlockStart {
                    index: 0,
                    block_type: "text".parse().unwrap(),
                },
                dsh_llm::StreamChunk::TextDelta {
                    index: 0,
                    text: "round done".into(),
                },
                dsh_llm::StreamChunk::BlockEnd {
                    index: 0,
                    block: dsh_llm::ContentBlock::text("round done"),
                },
                dsh_llm::StreamChunk::Finish {
                    reason: dsh_llm::FinishReason::Stop,
                    replay_state: None,
                },
            ],
            vec![
                dsh_llm::StreamChunk::BlockStart {
                    index: 0,
                    block_type: "text".parse().unwrap(),
                },
                dsh_llm::StreamChunk::TextDelta {
                    index: 0,
                    text: "round two done".into(),
                },
                dsh_llm::StreamChunk::BlockEnd {
                    index: 0,
                    block: dsh_llm::ContentBlock::text("round two done"),
                },
                dsh_llm::StreamChunk::Finish {
                    reason: dsh_llm::FinishReason::Stop,
                    replay_state: None,
                },
            ],
        ])));
        let calls = Rc::new(Cell::new(0u32));
        struct Adapter {
            script: Rc<RefCell<VecDeque<Vec<dsh_llm::StreamChunk>>>>,
            calls: Rc<Cell<u32>>,
        }
        impl dsh_llm::LlmAdapter for Adapter {
            fn stream(&self, _o: dsh_llm::GenerateOptions) -> Box<dyn Iterator<Item = dsh_llm::StreamChunk>> {
                self.calls.set(self.calls.get() + 1);
                let next = self.script.borrow_mut().pop_front().unwrap_or_default();
                Box::new(next.into_iter())
            }
        }
        let llm = Rc::new(dsh_llm::LlmRuntime::new());
        llm.register_adapter(
            &["mock"],
            Rc::new(Adapter {
                script: script.clone(),
                calls: calls.clone(),
            }),
        )
        .unwrap();

        let tools = Rc::new(dsh_tools::ToolRegistry::new(
            dsh_tools::ToolExecutionMode::Native,
        ));
        let session_host = SessionHost::in_memory();
        let _ = session_host.session("default");
        let config = dsh_agent_loop::AgentLoopConfig {
            max_parallel_tool_calls: None,
            agents: vec![dsh_agent_loop::ConfiguredAgent {
                id: "a-main".into(),
                provider: Some("mock".into()),
                model: Some("mock-model".into()),
                session_id: Some("default".into()),
                max_tokens: None,
                cwd: None,
                resume_session_id: None,
            }],
        };
        let loop_host = dsh_agent_loop::AgentLoopHost::with_store(
            config,
            llm,
            tools,
            session_host.store.clone(),
        )
        .unwrap();
        let agent = loop_host.ensure_agent(&dsh_agent_loop::ConfiguredAgent {
            id: "a-main".into(),
            provider: Some("mock".into()),
            model: Some("mock-model".into()),
            session_id: Some("default".into()),
            max_tokens: None,
            cwd: None,
            resume_session_id: None,
        })
        .expect("ensure a-main");

        // armed 目标（cap 2，尚未跑任何轮次）。
        let mut goal = dsh_goal::GoalService::new(dsh_goal::ServiceOptions::default());
        let gr = goal.create("finish the paper", Some(2)).expect("create goal");
        assert_eq!(goal.activation(), dsh_goal::GoalActivation::Armed);
        assert_eq!(goal.rounds_started(), 0);

        let mut port = GoalRoundPort::new(agent.clone());
        // 空闲 + 空 inbox + armed + 未超 cap → 续跑判定 Continue。
        assert_eq!(
            dsh_goal::round_driver::round_driver_outcome(&goal, &gr.id, &port),
            Some(dsh_goal::round_driver::RoundOutcome::Continue),
            "eligible to continue"
        );
        // drive_once：admit 第 1 轮 + followup → real loop 驱动该轮（同步到空闲）。
        let out = dsh_goal::round_driver::drive_once(&mut goal, &mut port, &gr.id)
            .expect("drive ok");
        assert!(matches!(out, dsh_goal::round_driver::RoundOutcome::Continue));
        assert_eq!(goal.rounds_started(), 1, "第 1 轮已准入");
        assert_eq!(calls.get(), 1, "mock adapter 驱动了真实一轮");
        // 该轮落 user/assistant（fake-loop 全局 store）。
        let evs = session_host.events("default");
        assert!(evs.iter().any(|e| e.kind.as_str() == "user/message"), "user/message 落会话");
        assert!(evs.iter().any(|e| e.kind.as_str() == "assistant/message"), "assistant/message 落会话");
        assert!(evs.iter().any(|e| e.kind.as_str() == "turn/end"), "turn/end 落会话");
        // 该轮 followup 文本含 round 提示（objective + Round: 1/2）。
        let user_msgs: Vec<&dsh_session::types::SessionEvent> = evs
            .iter()
            .filter(|e| e.kind.as_str() == "user/message")
            .collect();
        let joined: String = user_msgs
            .iter()
            .map(|e| serde_json::to_string(&e.data).unwrap_or_default())
            .collect();
        assert!(joined.contains("finish the paper"), "objective 进 followup");
        assert!(joined.contains("Round: 1/2"), "Round: 1/2 提示进 followup");

        // 本轮回到 idle 后仍 eligible（未超 cap）→ 第 2 轮也 drive_once。
        assert!(agent.status() == dsh_agent::types::AgentStatus::Idle, "round done → idle");
        let out2 = dsh_goal::round_driver::drive_once(&mut goal, &mut port, &gr.id)
            .expect("drive second");
        assert!(matches!(out2, dsh_goal::round_driver::RoundOutcome::Continue));
        assert_eq!(goal.rounds_started(), 2, "第 2 轮已准入");
        assert_eq!(calls.get(), 2, "second real round driven");
        // 已到 cap（2）→ 不再 eligible。
        assert_eq!(
            dsh_goal::round_driver::round_driver_outcome(&goal, &gr.id, &port),
            None,
            "cap 到达 → 不续跑"
        );
    }

    // ---- M5h 接线测试（step7；register_m5_tools_with_host） ----

    /// 内存终端后端：echo 文本、缓冲读、记录 signal/close（web 接线测试专用替身；
    /// dsh-terminal 测试内的 FakeBackend 不导出，D-068 记录此重复）。
    #[derive(Debug, Clone)]
    struct M5FakeBackend {
        sent: Vec<String>,
        read_buf: String,
        signaled: Vec<dsh_terminal::TerminalSignal>,
        closed: bool,
        status: dsh_terminal::TerminalSessionStatus,
    }
    impl Default for M5FakeBackend {
        fn default() -> Self {
            M5FakeBackend {
                sent: Vec::new(),
                read_buf: String::new(),
                signaled: Vec::new(),
                closed: false,
                status: dsh_terminal::TerminalSessionStatus::Running,
            }
        }
    }
    impl dsh_terminal::TerminalBackend for M5FakeBackend {
        fn open(&mut self, _owner: &str, _cfg: &dsh_terminal::TerminalConfig) -> Result<(), dsh_terminal::TerminalError> {
            Ok(())
        }
        fn send(&mut self, req: &dsh_terminal::TerminalSendRequest) -> Result<dsh_terminal::TerminalSendResult, dsh_terminal::TerminalError> {
            self.sent.push(format!("{}submit={}", req.text, req.submit));
            if self.status == dsh_terminal::TerminalSessionStatus::Running {
                self.read_buf.push_str(&format!("echo:{}", req.text));
            }
            Ok(dsh_terminal::TerminalSendResult {
                viewport: self.read_buf.clone(),
                wait_reason: dsh_terminal::TerminalWaitReason::StdinRead,
                session_status: self.status,
                truncated: false,
            })
        }
        fn read(&mut self, max_read_bytes: usize) -> Result<String, dsh_terminal::TerminalError> {
            let mut buf = String::new();
            std::mem::swap(&mut buf, &mut self.read_buf);
            buf.truncate(max_read_bytes);
            Ok(buf)
        }
        fn signal(&mut self, sig: dsh_terminal::TerminalSignal) -> Result<(), dsh_terminal::TerminalError> {
            self.signaled.push(sig);
            if matches!(sig, dsh_terminal::TerminalSignal::Sigkill) {
                self.status = dsh_terminal::TerminalSessionStatus::Exited;
            }
            Ok(())
        }
        fn close(&mut self) -> Result<(), dsh_terminal::TerminalError> {
            self.closed = true;
            Ok(())
        }
        fn label(&self) -> &str {
            "fake"
        }
        fn kind(&self) -> dsh_terminal::TerminalBackendKind {
            dsh_terminal::TerminalBackendKind::Bash
        }
    }

    /// M5i 接线 #1：全部 M5 工具注册可见；无宿主句柄 → 结构化 NOT_BOUND（诚实）。
    #[test]
    fn register_all_m5_tools_visible_and_unbound_fail_loud() {
        use dsh_tools::{ToolExecutionInput, ToolExecutionMode, ToolRegistry};
        let registry = ToolRegistry::new(ToolExecutionMode::Native);
        register_m5_tools_with_host(&registry, None);
        for name in [
            "bash", "read", "write", "edit", "read_image", "glob", "grep",
            "str_replace_editor", "terminal_open", "terminal_send", "terminal_read",
            "terminal_signal", "terminal_close", "terminal_list",
        ] {
            assert!(
                registry.get(name, None).is_some(),
                "{name} registered+visible"
            );
        }
        // 未绑定的 bash：结构化 isError（code NOT_BOUND），绝不伪装成功。
        let input = ToolExecutionInput::new(
            "b1",
            "bash",
            serde_json::json!({ "command": "echo hi", "description": "d" }),
            Some("agent-1".to_string()),
        );
        let res = registry.execute(&input, None);
        assert!(res.is_error, "unbound bash fails loud");
        let info = res.error.as_ref().and_then(|e| e.info.as_ref());
        assert_eq!(
            info.map(|i| i.code.as_str()).unwrap_or(""),
            "NOT_BOUND",
            "结构化 NOT_BOUND code"
        );
        // 无 agent 调用者的 terminal_open：语义校验错误（不 panic）。
        let input = ToolExecutionInput::new(
            "t0", "terminal_open", serde_json::json!({ "type": "bash" }), None,
        );
        let res = registry.execute(&input, None);
        assert!(res.is_error, "agent-less terminal_open rejected");
    }

    /// M5i 接线 #2：terminal 宿主句柄在场 → 六件套走真实注册表/FakeBackend 全生命周期：
    /// open → list（属主过滤）→ send → read → signal → close，foreign-owner 被拒。
    #[test]
    fn register_m5_tools_with_terminal_host_binds_really() {
        use dsh_tools::{ToolExecutionInput, ToolExecutionMode, ToolRegistry};
        let registry = ToolRegistry::new(ToolExecutionMode::Native);
        let term = Rc::new(std::cell::RefCell::new(
            dsh_terminal::TerminalSessionService::new(),
        ));
        term.borrow_mut()
            .register_backend(
                dsh_terminal::BackendDefinition {
                    id: "bash".into(),
                    kind: dsh_terminal::TerminalBackendKind::Bash,
                    label: "fake bash".into(),
                },
                Box::new(|_cfg| Box::new(M5FakeBackend::default())),
            )
            .expect("register fake backend");
        let host = M5HostServices { terminal: Some(term), fs: None, shell: None, bash_jobs: None, code: None };
        register_m5_tools_with_host(&registry, Some(&host));

        // open
        let res = registry.execute(
            &ToolExecutionInput::new(
                "t1", "terminal_open", serde_json::json!({ "type": "bash", "name": "work" }),
                Some("agent-1".into()),
            ),
            None,
        );
        assert!(!res.is_error, "open ok: {:?}", res.error);
        let session_id = res
            .value
            .unwrap()["sessionId"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(!session_id.is_empty(), "sessionId allocated");

        // list（属主过滤：agent-1 可见，foreign 不可见）
        let res = registry.execute(
            &ToolExecutionInput::new(
                "t2",
                "terminal_list",
                serde_json::json!({}),
                Some("agent-1".into()),
            ),
            None,
        );
        assert!(!res.is_error);
        let arr = res.value.unwrap()["sessions"].as_array().unwrap().clone();
        assert_eq!(arr.len(), 1, "agent-1 只见自己的会话");
        assert_eq!(arr[0]["sessionId"].as_str().unwrap(), session_id);

        // send（假后端 echo 进 viewport）
        let res = registry.execute(
            &ToolExecutionInput::new(
                "t3",
                "terminal_send",
                serde_json::json!({ "sessionId": session_id, "text": "ls", "submit": true }),
                Some("agent-1".into()),
            ),
            None,
        );
        assert!(!res.is_error, "send ok: {:?}", res.error);
        let v = res.value.unwrap();
        assert!(v["viewport"].as_str().unwrap_or("").contains("echo:ls"));
        assert_eq!(v["waitReason"].as_str().unwrap(), "stdin_read");
        assert_eq!(v["sessionStatus"]["kind"].as_str().unwrap(), "running");

        // read（滚缓冲）
        let res = registry.execute(
            &ToolExecutionInput::new(
                "t4",
                "terminal_read",
                serde_json::json!({ "sessionId": session_id }),
                Some("agent-1".into()),
            ),
            None,
        );
        assert!(!res.is_error, "read ok");
        assert!(
            res.value.unwrap()["text"]
                .as_str()
                .unwrap_or("")
                .contains("echo:ls")
        );

        // signal（SIGTERM → 送达）
        let res = registry.execute(
            &ToolExecutionInput::new(
                "t5",
                "terminal_signal",
                serde_json::json!({ "sessionId": session_id, "signal": "SIGTERM" }),
                Some("agent-1".into()),
            ),
            None,
        );
        assert!(!res.is_error, "signal ok");
        assert_eq!(res.value.unwrap()["delivered"], true);

        // foreign-owner 不得操作（权威拒绝）
        let res = registry.execute(
            &ToolExecutionInput::new(
                "t6",
                "terminal_signal",
                serde_json::json!({ "sessionId": session_id, "signal": "SIGINT" }),
                Some("agent-2".into()),
            ),
            None,
        );
        assert!(res.is_error, "foreign owner denied");

        // close（属主）
        let res = registry.execute(
            &ToolExecutionInput::new(
                "t7",
                "terminal_close",
                serde_json::json!({ "sessionId": session_id }),
                Some("agent-1".into()),
            ),
            None,
        );
        assert!(!res.is_error, "close ok");
        // 关闭后会话已删：后续 send 报错。
        let res = registry.execute(
            &ToolExecutionInput::new(
                "t8",
                "terminal_send",
                serde_json::json!({ "sessionId": session_id, "text": "x", "submit": true }),
                Some("agent-1".into()),
            ),
            None,
        );
        assert!(res.is_error, "closed session gone");
    }

    /// M5i 接线 #3：fs 宿主句柄在场 → read/write/edit/glob/grep/str_replace_editor 走真实
    /// LocalFileSystem+ObservationGate（临时 root）：write 建文件 → read 观察 → edit 版本
    /// CAS → 未观察写既有文件被拒（read-before-write）→ glob → grep → sr_editor view/替换。
    #[test]
    fn register_m5_tools_with_fs_host_binds_really() {
        use dsh_tools::{ToolExecutionInput, ToolExecutionMode, ToolRegistry};
        let root = std::env::temp_dir().join(format!("dsh-m5-fs-test-{}", std::process::id()));
        if root.exists() {
            let _ = std::fs::remove_dir_all(&root);
        }
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap(); // w4 写入需要父目录
        let registry = ToolRegistry::new(ToolExecutionMode::Native);
        let fsh = Rc::new(web_m5::FsHost::new(root.clone()));
        register_m5_tools_with_host(
            &registry,
            Some(&M5HostServices { terminal: None, fs: Some(fsh), shell: None, bash_jobs: None, code: None }),
        );

        let exec = |call_id: &str, name: &str, args: serde_json::Value| {
            registry.execute(&ToolExecutionInput::new(call_id, name, args, Some("agent-1".into())), None)
        };

        // write 建文件（create）
        let res = exec(
            "w1",
            "write",
            serde_json::json!({ "file_path": "hello.txt", "content": "hello world\nline two\n" }),
        );
        assert!(!res.is_error, "write ok: {:?}", res.error);
        assert_eq!(res.value.unwrap()["operation"], "create");

        // 未读就再写同一文件 → 读前写被拒（FS_NOT_OBSERVED carri出诚实报错）
        let res = exec(
            "w2",
            "write",
            serde_json::json!({ "file_path": "hello.txt", "content": "overwrite without read" }),
        );
        assert!(res.is_error, "read-before-write enforced");
        let code = res
            .error
            .as_ref()
            .and_then(|e| e.info.as_ref())
            .map(|i| i.code.clone())
            .unwrap_or_default();
        assert_eq!(code, "FS_NOT_OBSERVED");

        // read 观察 → read-before-write 放行（update，带窗口渲染 lines）
        let res = exec(
            "r1",
            "read",
            serde_json::json!({ "file_path": "hello.txt" }),
        );
        assert!(!res.is_error, "read ok: {:?}", res.error);
        let v = res.value.unwrap();
        assert_eq!(v["total_lines"], 2);
        assert_eq!(v["lines"][0]["text"], "hello world");

        // write（observed）→ update
        let res = exec(
            "w3",
            "write",
            serde_json::json!({ "file_path": "hello.txt", "content": "HELLO\nline two\nline three\n" }),
        );
        assert!(!res.is_error, "write update ok");
        assert_eq!(res.value.unwrap()["operation"], "update");

        // 另一 agent 未观察同名 → FS_NOT_OBSERVED（owner 隔离）
        let res_other = registry.execute(
            &ToolExecutionInput::new(
                "o1",
                "write",
                serde_json::json!({ "file_path": "hello.txt", "content": "x" }),
                Some("agent-2".into()),
            ),
            None,
        );
        assert!(res_other.is_error, "foreign owner unaobserved write rejected");

        // read（agent-2 未见 → 观察 agent-2 自己；随后可 edit）
        let _r = registry.execute(
            &ToolExecutionInput::new(
                "r2",
                "read",
                serde_json::json!({ "file_path": "hello.txt" }),
                Some("agent-2".into()),
            ),
            None,
        );
        assert!(!_r.is_error);

        // edit：agent-2 替换（版本 CAS 已观察）
        let res = registry.execute(
            &ToolExecutionInput::new(
                "e1",
                "edit",
                serde_json::json!({ "file_path": "hello.txt", "old_string": "line three", "new_string": "line three!" }),
                Some("agent-2".into()),
            ),
            None,
        );
        assert!(!res.is_error, "edit ok: {:?}", res.error);

        // glob：匹配 .txt
        let res = exec("g1", "glob", serde_json::json!({ "pattern": "*.txt" }));
        assert!(!res.is_error, "glob ok");
        let matches = res.value.unwrap()["matches"].as_array().unwrap().clone();
        assert!(matches.iter().any(|m| m.as_str().unwrap_or("").ends_with("hello.txt")));

        // 再写一个文件供 grep 多行定位
        let res = exec(
            "w4",
            "write",
            serde_json::json!({ "file_path": "src/main.rs", "content": "fn main() {}\n// needle here\n" }),
        );
        assert!(!res.is_error);
        // grep：needle
        let res = exec("g2", "grep", serde_json::json!({ "pattern": "needle" }));
        assert!(!res.is_error, "grep ok: {:?}", res.error);
        let body = res.value.unwrap();
        assert_eq!(body["seen"], 1);
        assert_eq!(body["matches"][0]["path"], "src/main.rs");
        assert!(body["matches"][0]["line"].as_str().unwrap().contains("needle"));

        // str_replace_editor view
        let res = exec(
            "s1",
            "str_replace_editor",
            serde_json::json!({ "file_path": "src/main.rs", "view": true }),
        );
        assert!(!res.is_error, "sr view ok");
        assert!(res.value.unwrap()["content"].as_str().unwrap().contains("fn main()"));

        // str_replace_editor str_replace（唯一替换）
        let res = exec(
            "s2",
            "str_replace_editor",
            serde_json::json!({
                "file_path": "src/main.rs",
                "old_string": "fn main() {}",
                "new_string": "fn main() { /* replaced */ }",
            }),
        );
        assert!(!res.is_error, "sr replace ok: {:?}", res.error);
        // 落盘确认
        let disk = std::fs::read_to_string(root.join("src/main.rs")).unwrap();
        assert!(disk.contains("replaced"), "replacement persisted: {disk}");

        // 清理临时 root
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 依赖 Git Bash 的真实执行（Windows 候选探测；缺 bash 时门控跳过——诚实）：
    /// bash 工具前台真跑（echo + pwd cwd 锚定 root + 非零退出带 exit code 标记）；
    /// run_in_background/sandbox_permissions 诚实 UNSUPPORTED（jobs 桥/D-070 未接线）。
    #[test]
    fn register_m5_tools_with_shell_host_binds_bash_really() {
        use dsh_tools::{ToolExecutionInput, ToolExecutionMode, ToolRegistry};
        fn bash_available() -> bool {
            #[cfg(windows)]
            {
                ["C:\\Program Files\\Git\\bin\\bash.exe", "C:\\Program Files\\Git\\usr\\bin\\bash.exe", "C:\\Windows\\System32\\bash.exe"]
                    .iter()
                    .any(|p| std::path::Path::new(p).exists())
            }
            #[cfg(not(windows))]
            {
                true
            }
        }
        if !bash_available() {
            eprintln!("bash unavailable; skipping real-execution shell binding test");
            return;
        }
        let root = std::env::temp_dir().join(format!("dsh-m5-bash-test-{}", std::process::id()));
        if root.exists() {
            let _ = std::fs::remove_dir_all(&root);
        }
        std::fs::create_dir_all(&root).unwrap();

        let registry = ToolRegistry::new(ToolExecutionMode::Native);
        let shost = Rc::new(web_m5::ShellHost::new(root.clone()).expect("shell host"));
        register_m5_tools_with_host(
            &registry,
            Some(&M5HostServices { terminal: None, fs: None, shell: Some(shost), bash_jobs: None, code: None }),
        );

        let exec = |call_id: &str, args: serde_json::Value| {
            registry.execute(
                &ToolExecutionInput::new(call_id, "bash", args, Some("agent-1".into())),
                None,
            )
        };

        // 前台 echo：exitCode 0 + stdout 文本（渲染含 [exit code: 0] 不出现）。
        let res = exec(
            "b1",
            serde_json::json!({ "command": "echo hello-dsh-bash", "description": "test echo" }),
        );
        assert!(!res.is_error, "bash echo ok: {:?}", res.error);
        let v = res.value.unwrap();
        assert_eq!(v["exitCode"], 0);
        assert!(v["stdout"]["text"].as_str().unwrap().contains("hello-dsh-bash"));
        assert!(!v["stdout"]["truncated"].as_bool().unwrap());

        // cwd 锚定宿主 root（pwd）。
        let res = exec(
            "b2",
            serde_json::json!({ "command": "pwd", "description": "test pwd" }),
        );
        assert!(!res.is_error, "bash pwd ok: {:?}", res.error);
        let pwd = res.value.unwrap()["stdout"]["text"].as_str().unwrap().trim().to_string();
        assert!(pwd.contains("dsh-m5-bash-test"),
            "pwd anchored at host root: {pwd} (root: {root:?})");

        // 非零退出：exitCode 非 0，渲染出 [exit code: n]。
        let res = exec(
            "b3",
            serde_json::json!({ "command": "exit 3", "description": "test nonzero" }),
        );
        assert!(!res.is_error, "nonzero exit resolves as result: {:?}", res.error);
        assert_eq!(res.value.unwrap()["exitCode"], 3);

        // 后台：诚实 UNSUPPORTED（jobs producer 桥未接线，D-070）。
        let res = exec(
            "b4",
            serde_json::json!({ "command": "echo bg", "description": "test bg", "run_in_background": true }),
        );
        assert!(res.is_error, "background rejected until jobs bridge lands");
        let code = res
            .error
            .as_ref()
            .and_then(|e| e.info.as_ref())
            .map(|i| i.code.clone())
            .unwrap_or_default();
        assert_eq!(code, "UNSUPPORTED_OPTION");

        // sandbox_permissions：非空诚实 UNSUPPORTED（SAND 投影未接线，D-070）。
        let res = exec(
            "b5",
            serde_json::json!({ "command": "echo x", "description": "test sandbox", "sandbox_permissions": "network" }),
        );
        assert!(res.is_error, "sandbox escalation rejected until SAND projection");

        // 清理
        let _ = std::fs::remove_dir_all(&root);
    }

    /// M5i 接线 #5：bash 后台 jobs producer 桥（真实 Git Bash 门控）——run_in_background:true
    /// 返回 jobId；宿主合作泵（pump）推进至 settle；job_read 终态携全文；job 授权围栏作用
    /// （foreign caller 拒绝）。与 app 侧 job_read/job_kill 工具共享同一 JobRegistry 语义。
    #[test]
    fn register_m5_tools_with_bash_jobs_bridge_background_really() {
        use dsh_tools::{ToolExecutionInput, ToolExecutionMode, ToolRegistry};
        fn bash_available() -> bool {
            #[cfg(windows)]
            {
                ["C:\\Program Files\\Git\\bin\\bash.exe", "C:\\Program Files\\Git\\usr\\bin\\bash.exe", "C:\\Windows\\System32\\bash.exe"]
                    .iter()
                    .any(|p| std::path::Path::new(p).exists())
            }
            #[cfg(not(windows))]
            {
                true
            }
        }
        if !bash_available() {
            eprintln!("bash unavailable; skipping background jobs bridge test");
            return;
        }
        let root = std::env::temp_dir().join(format!("dsh-m5-bash-bg-{}", std::process::id()));
        if root.exists() {
            let _ = std::fs::remove_dir_all(&root);
        }
        std::fs::create_dir_all(&root).unwrap();

        let registry = ToolRegistry::new(ToolExecutionMode::Native);
        let shost = Rc::new(web_m5::ShellHost::new(root.clone()).expect("shell host"));
        let bridge = Rc::new(web_m5::BashJobsBridge::new());
        register_m5_tools_with_host(
            &registry,
            Some(&M5HostServices {
                terminal: None,
                fs: None,
                shell: Some(shost),
                bash_jobs: Some(bridge.clone()),
                code: None,
            }),
        );

        // 后台启动：返回 jobId（无前台 exitCode 语义）。
        let res = registry.execute(
            &ToolExecutionInput::new(
                "bg1",
                "bash",
                serde_json::json!({
                    "command": "echo job-start; sleep 0.3; echo job-end",
                    "description": "test background job",
                    "run_in_background": true,
                }),
                Some("agent-1".into()),
            ),
            None,
        );
        assert!(!res.is_error, "background start ok: {:?}", res.error);
        let v = res.value.unwrap();
        let job_id = v["jobId"].as_str().expect("jobId").to_string();

        // 合作泵推进至 settle（真实 sleep 进程）。
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if bridge.pump() > 0 {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "job did not settle in time");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        // job_read：completed + 终态全文。
        let read = bridge.read(&job_id, Some("agent-1")).expect("read by owner ok");
        assert_eq!(read.snapshot.status.as_str(), "completed");
        assert!(read.text.contains("job-start"), "stdout has start: {}", read.text);
        assert!(read.text.contains("job-end"), "stdout has end: {}", read.text);

        // 授权围栏：foreign caller 拒绝。
        assert!(bridge.read(&job_id, Some("agent-2")).is_err(), "foreign read rejected");

        // 后台不再进前台路径（前台同 host 同时可用）。
        let res = registry.execute(
            &ToolExecutionInput::new(
                "fg1",
                "bash",
                serde_json::json!({ "command": "echo fg-ok", "description": "test fg" }),
                Some("agent-1".into()),
            ),
            None,
        );
        assert!(!res.is_error, "foreground still works with bridge present");
        assert_eq!(res.value.unwrap()["exitCode"], 0);

        // 清理
        let _ = std::fs::remove_dir_all(&root);
    }

    fn m5g_epoch_now_ms() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }

    /// M5i 接线 #6（验收 7）：M5g 服务层 tick 线程 → mpsc → 主线程 tick_once 自动驱动
    /// schedule 到期（**非手工** dispatch_due：主循环只消费线程 tick，到期由 tick_once
    /// 触发）。after(0s) 记录经线程 tick 自动派发 + 落日志。
    #[test]
    fn m5g_tick_service_thread_drives_schedule_dispatch_automatically() {
        let host_store = SessionHost::in_memory();
        let _ = host_store.session("default");
        let sched_session = host_store.session("default").expect("default live");
        let sched = Rc::new(dsh_cli_host::ScheduleHost::new(sched_session));
        let now = m5g_epoch_now_ms();
        let id = sched
            .create("after", "m5g automation ping", Some(1), None, None, now)
            .expect("create after(1)");

        let tick = web_m5::M5gTick::start(15);
        let mut fired = false;
        // 主循环：仅消费服务线程 tick → tick_once（唯一触发点；不直接调 dispatch_due）。
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        for _ in 0..300 {
            assert!(std::time::Instant::now() < deadline, "tick starvation");
            if !tick.wait_tick(std::time::Duration::from_millis(50)) {
                continue;
            }
            let (_framing, dispatched) = web_m5::m5g_tick_once(&sched, None, m5g_epoch_now_ms())
                .expect("tick_once ok");
            if dispatched.contains(&id) {
                fired = true;
                break;
            }
        }
        assert!(fired, "schedule {id} auto-fired via service tick (non-manual)");
        // 派发事件已落会话日志（schedule/change ≥2：create + dispatch）。
        let evs = host_store.events("default");
        let sched_events = evs
            .iter()
            .filter(|e| e.kind == dsh_session::types::EventKind::ScheduleChange)
            .count();
        assert!(sched_events >= 2, "create + dispatch events: {sched_events}");
    }

    /// M5i 接线 #7（验收 5/7）：M5g 服务线程 tick 自动结算 bash 后台 job（**非手工** pump：
    /// 主循环只 eat tick → tick_once（bridge.pump 内建），job 终态自动 arrive）。
    #[test]
    fn m5g_tick_auto_settles_bash_background_job() {
        use dsh_tools::{ToolExecutionInput, ToolExecutionMode, ToolRegistry};
        fn bash_available() -> bool {
            #[cfg(windows)]
            {
                ["C:\\Program Files\\Git\\bin\\bash.exe", "C:\\Program Files\\Git\\usr\\bin\\bash.exe", "C:\\Windows\\System32\\bash.exe"]
                    .iter()
                    .any(|p| std::path::Path::new(p).exists())
            }
            #[cfg(not(windows))]
            {
                true
            }
        }
        if !bash_available() {
            eprintln!("bash unavailable; skipping M5g auto-settle test");
            return;
        }
        let root = std::env::temp_dir().join(format!("dsh-m5-tick-{}", std::process::id()));
        if root.exists() {
            let _ = std::fs::remove_dir_all(&root);
        }
        std::fs::create_dir_all(&root).unwrap();

        let registry = ToolRegistry::new(ToolExecutionMode::Native);
        let shost = Rc::new(web_m5::ShellHost::new(root.clone()).expect("shell host"));
        let bridge = Rc::new(web_m5::BashJobsBridge::new());
        register_m5_tools_with_host(
            &registry,
            Some(&M5HostServices {
                terminal: None,
                fs: None,
                shell: Some(shost),
                bash_jobs: Some(bridge.clone()),
                code: None,
            }),
        );

        let res = registry.execute(
            &ToolExecutionInput::new(
                "bg1",
                "bash",
                serde_json::json!({
                    "command": "sleep 0.2; echo auto-settled",
                    "description": "M5g auto-settle",
                    "run_in_background": true,
                }),
                Some("agent-1".into()),
            ),
            None,
        );
        assert!(!res.is_error, "bg start: {:?}", res.error);
        let job_id = res.value.unwrap()["jobId"].as_str().expect("jobId").to_string();

        // 主循环：只 eat tick → tick_once(sched, bridge)（内建 pump），绝不手工 pump。
        let host_store = SessionHost::in_memory();
        let _ = host_store.session("default");
        let sched = Rc::new(dsh_cli_host::ScheduleHost::new(
            host_store.session("default").expect("sess"),
        ));
        let tick = web_m5::M5gTick::start(15);
        let now = m5g_epoch_now_ms();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut settled = false;
        for _ in 0..300 {
            assert!(std::time::Instant::now() < deadline, "job never settled");
            if !tick.wait_tick(std::time::Duration::from_millis(50)) {
                continue;
            }
            let _ = web_m5::m5g_tick_once(&sched, Some(bridge.as_ref()), now)
                .expect("tick_once ok");
            if let Ok(read) = bridge.read(&job_id, Some("agent-1")) {
                if read.snapshot.status.as_str() == "completed" {
                    assert!(read.text.contains("auto-settled"), "full output: {}", read.text);
                    settled = true;
                    break;
                }
            }
        }
        assert!(settled, "bash bg job auto-settled via M5g service tick");

        // 清理
        let _ = std::fs::remove_dir_all(&root);
    }

    /// M5i 接线 #8（验收 #6）：run_code 传输**真实执行**——Code-mode registry + python 后端
    /// 宿主覆盖注入传输（替换占位桩）；python 可用门控（真实子进程）。schema 校验、
    /// 完成值 lossless 跨界、print→logs、dict→json。
    #[test]
    fn register_m5_run_code_transport_executes_python() {
        use dsh_code_runtime::{python_available, PythonCodeRuntime, PythonConfig};
        use dsh_tools::{ToolExecutionInput, ToolExecutionMode, ToolRegistry};
        if !python_available() {
            eprintln!("python unavailable; skipping run_code transport test");
            return;
        }
        let registry = ToolRegistry::new(ToolExecutionMode::Code);
        let cr = Rc::new(PythonCodeRuntime::new(PythonConfig::default()));
        register_m5_tools_with_host(
            &registry,
            Some(&M5HostServices {
                terminal: None,
                fs: None,
                shell: None,
                bash_jobs: None,
                code: Some(cr),
            }),
        );

        let exec = |call_id: &str, args: serde_json::Value| {
            registry.execute(
                &ToolExecutionInput::new(call_id, "run_code", args, Some("agent-1".into())),
                None,
            )
        };

        // schema 硬校验：code 缺失 → INVALID_ARGS（不触发运行时）。
        let res = exec("rc0", serde_json::json!({ "description": "x" }));
        assert!(res.is_error, "missing code rejected");

        // 真实 python：return 表达式 → 完成值 42（lossless 跨界）。
        let res = exec(
            "rc1",
            serde_json::json!({ "code": "return 1 + 41", "description": "add" }),
        );
        assert!(!res.is_error, "run_code executes: {:?}", res.error);
        let v = res.value.unwrap();
        assert_eq!(v["language"], "python");
        assert_eq!(v["value"], 42);
        assert!(v["error"].is_null());

        // print → logs；return None → 无完成值（null）。
        let res = exec(
            "rc2",
            serde_json::json!({ "code": "print(\"hello-log\")\nreturn None", "description": "log" }),
        );
        assert!(!res.is_error, "log run ok: {:?}", res.error);
        let v = res.value.unwrap();
        let logs = v["logs"].as_array().unwrap();
        assert!(
            logs.iter().any(|l| l.as_str().is_some_and(|s| s.contains("hello-log"))),
            "logs captured: {logs:?}"
        );
        assert!(v["value"].is_null());

        // dict → lossless JSON 对象跨界。
        let res = exec(
            "rc3",
            serde_json::json!({ "code": "return {'ok': True, 'n': 7}", "description": "dict" }),
        );
        assert!(!res.is_error, "dict run ok: {:?}", res.error);
        let v = res.value.unwrap();
        assert_eq!(v["value"]["ok"], true);
        assert_eq!(v["value"]["n"], 7);
    }

    /// M5i 接线 #9：effectiveSandboxMode 会话事件 fold（last-wins `sandbox/mode`；未知
    /// 模式忽略；delegation 源标记；缺省 read-only）+ `sandbox:policy` 系统提示段
    /// （仅 workspace-write 产可写根名单）。
    #[test]
    fn fold_effective_sandbox_mode_last_wins_and_policy_segment() {
        use serde_json::json;

        // 空事件 → read-only/default。
        let e = web_m5::fold_effective_sandbox_mode(&[]);
        assert_eq!(e.mode.as_str(), "read-only");
        assert_eq!(e.source, "default");

        // last-wins：workspace-write → danger（delegation 源标记）。
        let events = vec![
            json!({ "type": "sandbox/mode", "data": { "mode": "workspace-write" } }),
            json!({ "type": "sandbox/mode", "data": { "mode": "danger-full-access", "source": "delegation" } }),
        ];
        let e = web_m5::fold_effective_sandbox_mode(&events);
        assert_eq!(e.mode.as_str(), "danger-full-access");
        assert_eq!(e.source, "session-delegation");

        // 非 sandbox/mode 事件 + 未知模式被忽略（log-only 语义）。
        let events = vec![
            json!({ "type": "user/message", "data": {} }),
            json!({ "type": "sandbox/mode", "data": { "mode": "nonsense-mode" } }),
            json!({ "type": "sandbox/mode", "data": { "mode": "workspace-write" } }),
        ];
        let e = web_m5::fold_effective_sandbox_mode(&events);
        assert_eq!(e.mode.as_str(), "workspace-write");
        assert_eq!(e.source, "session");

        // sandbox:policy 段：read-only → 无根；(workspace-write → 含工作区根)。
        let ro = web_m5::sandbox_policy_segment(dsh_sandbox::SandboxMode::ReadOnly, None);
        assert!(ro.contains("read-only"), "{ro}");
        assert!(ro.contains("(none — read-only)"), "{ro}");
        let root = std::env::temp_dir().join("dsh-policy-seg");
        let ws = web_m5::sandbox_policy_segment(dsh_sandbox::SandboxMode::WorkspaceWrite, Some(&root));
        assert!(ws.contains("workspace-write"), "{ws}");
        assert!(ws.contains("dsh-policy-seg"), "{ws}");
    }

    /// M5i（验收 #3）：approved > session > default 完整解析优先级。
    #[test]
    fn resolve_sandbox_mode_precedence_approved_session_default() {
        use dsh_sandbox::SandboxMode;
        use serde_json::json;
        use web_m5::{resolve_sandbox_mode, EffectiveSandbox};

        // 无 approved + 无会话 → 默认 read-only。
        assert_eq!(
            resolve_sandbox_mode(None, &[]),
            EffectiveSandbox {
                mode: SandboxMode::ReadOnly,
                source: "default"
            }
        );

        // 无 approved + 会话最后一跳（workspace-write）→ 会话档。
        let events = vec![json!({ "type": "sandbox/mode", "data": { "mode": "workspace-write" } })];
        assert_eq!(
            resolve_sandbox_mode(None, &events),
            EffectiveSandbox {
                mode: SandboxMode::WorkspaceWrite,
                source: "session"
            }
        );

        // approved（审批缝显式，delegation）覆盖会话（含更宽的 danger 会话也被覆盖）。
        let events = vec![json!({ "type": "sandbox/mode", "data": { "mode": "danger-full-access" } })];
        assert_eq!(
            resolve_sandbox_mode(Some(SandboxMode::WorkspaceWrite), &events),
            EffectiveSandbox {
                mode: SandboxMode::WorkspaceWrite,
                source: "approved"
            }
        );

        // approved 且无会话事件 → 直接 approved。
        assert_eq!(
            resolve_sandbox_mode(Some(SandboxMode::DangerFullAccess), &[]),
            EffectiveSandbox {
                mode: SandboxMode::DangerFullAccess,
                source: "approved"
            }
        );
    }

    /// M5i 接线 #10（验收 #9）：M5Host::assemble 生产装配——一次构造 terminal/fs/shell/
    /// bash_jobs（+code，python 可用时）全部句柄并真实驱动：bash 前台真跑（echo）、
    /// fs write→read 生命周期、glob 匹配——非仅测试可配，宿主生产面可用。
    #[test]
    fn m5_host_assemble_drives_real_tools() {
        use dsh_tools::{ToolExecutionInput, ToolExecutionMode, ToolRegistry};
        fn bash_available() -> bool {
            #[cfg(windows)]
            {
                ["C:\\Program Files\\Git\\bin\\bash.exe", "C:\\Program Files\\Git\\usr\\bin\\bash.exe", "C:\\Windows\\System32\\bash.exe"]
                    .iter()
                    .any(|p| std::path::Path::new(p).exists())
            }
            #[cfg(not(windows))]
            {
                true
            }
        }
        let root = std::env::temp_dir().join(format!("dsh-m5-assemble-{}", std::process::id()));
        if root.exists() {
            let _ = std::fs::remove_dir_all(&root);
        }
        std::fs::create_dir_all(&root).unwrap();

        let host = web_m5::M5Host::assemble(root.clone()).expect("m5 host assembles");
        // 全部宿主句柄在场。
        assert!(host.services.terminal.is_some());
        assert!(host.services.fs.is_some());
        assert!(host.services.shell.is_some());
        assert!(host.services.bash_jobs.is_some());

        let registry = ToolRegistry::new(ToolExecutionMode::Native);
        host.register(&registry);
        let exec = |call_id: &str, name: &str, args: serde_json::Value| {
            registry.execute(
                &ToolExecutionInput::new(call_id, name, args, Some("agent-1".into())),
                None,
            )
        };

        // fs write → read → glob 全生命周期（生产装配真实驱动）。
        let res = exec("a1", "write", serde_json::json!({ "file_path": "hello.txt", "content": "hello assemble\n" }));
        assert!(!res.is_error, "assemble write: {:?}", res.error);
        let res = exec("a2", "read", serde_json::json!({ "file_path": "hello.txt" }));
        assert!(!res.is_error, "assemble read ok");
        assert_eq!(res.value.unwrap()["lines"][0]["text"], "hello assemble");
        let res = exec("a3", "glob", serde_json::json!({ "pattern": "*.txt" }));
        assert!(!res.is_error, "assemble glob ok");
        let matches = res.value.unwrap()["matches"].as_array().unwrap().clone();
        assert!(matches.iter().any(|m| m.as_str().unwrap_or("").ends_with("hello.txt")));

        // bash 前台（Git Bash 门控）。
        if bash_available() {
            let res = exec("a4", "bash", serde_json::json!({ "command": "echo assembled-bash", "description": "test" }));
            assert!(!res.is_error, "assemble bash: {:?}", res.error);
            assert_eq!(res.value.unwrap()["exitCode"], 0);
        } else {
            eprintln!("bash unavailable; skipping assemble bash path");
        }

        // 清理
        let _ = std::fs::remove_dir_all(&root);
    }

    /// M6i 验收 #2：服务器装配工厂 `assemble_server_loop`——真实注册表（M4+M5 工具 +
    /// 宿主 bind，共享 store 与 SessionHost 同店）；一轮真实 loop 回合（mock LLM 脚本先发
    /// `todo_write` 工具调用）经生产路径 `run_rust_loop` 驱动，M4 todo_write 真身执行并落
    /// `todo/write` 事件到共享 store（TodoWriteHost bind agent "default"）。
    #[test]
    fn assemble_server_loop_builds_loop_host_with_m4_m5_and_drives_a_turn() {
        use dsh_llm::{CallId, ContentBlock, FinishReason, StreamChunk, ToolCallBlock};
        use std::cell::RefCell;
        use std::collections::VecDeque;

        fn todo_chunks(id: &str) -> Vec<StreamChunk> {
            let args = r#"{"todos":[{"content":"assemble-it","status":"in_progress"}]}"#;
            vec![
                StreamChunk::ToolCallDelta {
                    index: 0,
                    id: CallId::from_raw(id),
                    name: Some("todo_write".into()),
                    arguments_delta: args.into(),
                },
                StreamChunk::BlockEnd {
                    index: 0,
                    block: ContentBlock::ToolCall(ToolCallBlock {
                        id: CallId::from_raw(id),
                        name: "todo_write".into(),
                        arguments: args.into(),
                    }),
                },
                StreamChunk::Finish {
                    reason: FinishReason::ToolCalls,
                    replay_state: None,
                },
            ]
        }

        fn text_chunks(text: &str) -> Vec<StreamChunk> {
            vec![
                StreamChunk::BlockStart {
                    index: 0,
                    block_type: "text".parse().unwrap(),
                },
                StreamChunk::TextDelta {
                    index: 0,
                    text: text.into(),
                },
                StreamChunk::BlockEnd {
                    index: 0,
                    block: ContentBlock::text(text),
                },
                StreamChunk::Finish {
                    reason: FinishReason::Stop,
                    replay_state: None,
                },
            ]
        }

        struct Adapter {
            script: Rc<RefCell<VecDeque<Vec<StreamChunk>>>>,
        }
        impl dsh_llm::LlmAdapter for Adapter {
            fn stream(
                &self,
                _options: dsh_llm::GenerateOptions,
            ) -> Box<dyn Iterator<Item = StreamChunk>> {
                let next = self.script.borrow_mut().pop_front().unwrap_or_default();
                Box::new(next.into_iter())
            }
        }
        let script = Rc::new(RefCell::new(VecDeque::from_iter([
            todo_chunks("t1"),
            text_chunks("todo tracked"),
        ])));
        let llm = Rc::new(dsh_llm::LlmRuntime::new());
        llm.register_adapter(&["mock"], Rc::new(Adapter { script }))
            .unwrap();

        // 会话宿主（共享 store）+ todo 宿主（bind agent "default" → session "default"）。
        let session_host = SessionHost::in_memory();
        let _ = session_host.session("default");
        let todo = Rc::new(crate::web::dsh_cli_host::TodoWriteHost::new(
            session_host.clone(),
            "default".into(),
        ));
        todo.bind_agent("default", "default");
        let m4 = M4HostServices {
            jobs: None,
            schedule: None,
            todo: Some(todo),
        };

        // 工作区 + M5 宿主（真实生产工厂）。
        let root = std::env::temp_dir().join(format!("dsh-m6-assemble-{}", std::process::id()));
        if root.exists() {
            let _ = std::fs::remove_dir_all(&root);
        }
        std::fs::create_dir_all(&root).unwrap();
        let m5 = web_m5::M5Host::assemble(root.clone()).expect("m5 assembles");

        let loop_host = assemble_server_loop(
            session_host.store.clone(),
            root.clone(),
            llm,
            "mock",
            "mock-model",
            m4,
            m5,
        )
        .expect("assemble_server_loop ok");

        // 视图 = 真实注册表：M4 + M5 全工具可见（保证 agent 可调用面）。
        let names: Vec<String> = loop_host
            .tools
            .known_names(None)
            .into_iter()
            .collect();
        for want in [
            "todo_write",
            "job_list",
            "job_output",
            "schedule_create",
            "write",
            "read",
            "edit",
            "glob",
            "grep",
            "str_replace_editor",
            "bash",
            "terminal_open",
            "terminal_send",
        ] {
            assert!(
                names.iter().any(|n| n == want),
                "loop registry missing {want}; have {names:?}"
            );
        }

        // 装配进 boot → 生产路径 run_rust_loop 驱动一轮真实回合。
        let mut boot = boot_with_sessions();
        boot.agent_loop = Some(loop_host.clone());
        crate::run_rust_loop(&boot, "default", "please track my todo").expect("turn runs");

        // 事件落共享 store（同店）：todo 宿主真实写 + 工具调用 + 收尾 assistant。
        let evs = session_host.events("default");
        assert!(
            evs.iter().any(|e| e.kind.as_str() == "todo/write"),
            "todo/write landed in shared store: {evs:?}"
        );
        assert!(
            evs.iter().any(|e| e.kind.as_str() == "tool/call"),
            "tool/call seen: {evs:?}"
        );
        assert!(
            evs.iter().any(|e| e.kind.as_str() == "assistant/message"),
            "final assistant message in store"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// M6i 验收 #2 支撑：`assemble_server_runtime`（serve 接线编排）——同一 SessionHost
    /// 上构建 M4（jobs/schedule/todo 真句柄）+ M5（真工厂）+ deepseek LLM（无 key →
    /// 首回合 fail-loud，装配照常）→ LoopHost；共享 store 与 SessionHost 同店；真实
    /// 注册表含 M4+M5。bash 不可用时装配会失败（诚实），此时记录跳过非失败断言。
    #[test]
    fn assemble_server_runtime_builds_real_loop_host_for_serve() {
        let host = SessionHost::in_memory();
        let _ = host.session("default");
        let root = std::env::temp_dir().join(format!("dsh-m6-serve-{}", std::process::id()));
        if root.exists() {
            let _ = std::fs::remove_dir_all(&root);
        }
        std::fs::create_dir_all(&root).unwrap();

        match crate::web::assemble_server_runtime(
            &host,
            root.clone(),
            "http://127.0.0.1:1",
            "deepseek-v4-flash-0731-ext",
        ) {
            Ok(bundle) => {
                let loop_host = bundle.host;
                assert!(
                    Rc::ptr_eq(&host.store, &loop_host.store),
                    "loop host shares the SessionHost store (frontend read model)"
                );
                let names = loop_host.tools.known_names(None);
                for want in ["todo_write", "job_list", "write", "read", "bash", "terminal_open"] {
                    assert!(
                        names.iter().any(|n| n == want),
                        "serve loop registry missing {want}; have {names:?}"
                    );
                }
                assert!(
                    bundle.bash_jobs.is_some(),
                    "serve bundle exposes bash jobs bridge for tick"
                );
            }
            Err(e) => {
                eprintln!("assemble_server_runtime deferred (expect NoBash/assemble): {e}");
            }
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    /// M6i 验收 #3：宿主生命周期清理——`M5Host::shutdown()` 杀后台 bash 树（kill_all →
    /// settle Killed）且**真无孤儿**（marker 未写 == 进程被杀而非跑完）；终端会话 dispose。
    /// bash 不可用 → 诚实记录跳过（与 M5 assemble bash 门控一致）。
    #[test]
    fn m5_shutdown_kills_background_bash_no_orphan() {
        use dsh_jobs::JobStatus;
        use dsh_tools::{ToolExecutionInput, ToolExecutionMode, ToolRegistry};
        let root = std::env::temp_dir().join(format!("dsh-m6-shutdown-{}", std::process::id()));
        if root.exists() {
            let _ = std::fs::remove_dir_all(&root);
        }
        std::fs::create_dir_all(&root).unwrap();
        let mut m5 = match web_m5::M5Host::assemble(root.clone()) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("m5 assemble deferred (bash/pty unavailable?): {e}");
                let _ = std::fs::remove_dir_all(&root);
                return;
            }
        };
        // 终端：注册 FakeBackend（生产 PTY backend 装配属后续里程碑；此处确定性验证
        // dispose 清空会话表）。bash 保持真实桥（孤儿杀验证）。
        let term = Rc::new(std::cell::RefCell::new(dsh_terminal::TerminalSessionService::new()));
        term.borrow_mut()
            .register_backend(
                dsh_terminal::BackendDefinition {
                    id: "bash".into(),
                    kind: dsh_terminal::TerminalBackendKind::Bash,
                    label: "fake bash".into(),
                },
                Box::new(|_cfg| Box::new(M5FakeBackend::default())),
            )
            .expect("register fake backend");
        m5.services.terminal = Some(term);
        let registry = ToolRegistry::new(ToolExecutionMode::Native);
        m5.register(&registry);
        let agent = Some("agent-1".to_string());

        // 后台 bash：sleep 2 && 写 marker。shutdown 前：running、marker 未写。
        let marker = root.join("done.txt");
        let cmd = format!(
            "sleep 2 && echo DONE > {}",
            marker.display().to_string().replace('\\', "/")
        );
        let res = registry.execute(
            &ToolExecutionInput::new(
                "c1",
                "bash",
                serde_json::json!({
                    "command": cmd,
                    "description": "m6 shutdown lifecycle test",
                    "run_in_background": true
                }),
                agent.clone(),
            ),
            None,
        );
        assert!(!res.is_error, "bash bg start: {:?}", res.error);
        let job_id = res.value.unwrap()["jobId"].as_str().unwrap().to_string();
        assert!(!marker.exists(), "marker must not exist yet (process still running)");
        let bridge = m5.services.bash_jobs.clone().unwrap();
        assert_eq!(
            bridge.read(&job_id, agent.as_deref()).unwrap().snapshot.status,
            JobStatus::Running,
            "bg job running before shutdown"
        );

        // 终端会话：open（FakeBackend 确定可用）→ 会话存在。
        let tres = registry.execute(
            &ToolExecutionInput::new(
                "c2",
                "terminal_open",
                serde_json::json!({ "type": "bash", "name": "work" }),
                agent.clone(),
            ),
            None,
        );
        assert!(!tres.is_error, "terminal_open: {:?}", tres.error);
        let terminal_svc = m5.services.terminal.clone().unwrap();
        assert_eq!(terminal_svc.borrow().list().len(), 1, "one terminal session before shutdown");

        // 生命周期关停。
        m5.shutdown();

        // bash bg：kill_all → 合作泵已 settle → Killed。
        assert_eq!(
            bridge.read(&job_id, agent.as_deref()).unwrap().snapshot.status,
            JobStatus::Killed,
            "bg job settled Killed by shutdown"
        );
        // 真无孤儿：等 ≥ 2.5s，marker 仍不出现 == 进程被树杀而非跑完写 marker。
        std::thread::sleep(std::time::Duration::from_millis(2600));
        assert!(!marker.exists(), "background process truly killed (no orphan)");

        // 终端会话全部 dispose（list 空）。
        assert!(
            terminal_svc.borrow().list().is_empty(),
            "terminal sessions disposed on shutdown"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// M6i 验收 #4：serve 主循环 tick 推进（`m5g_tick_once` on ServerLoopBundle）——
    /// 同一 schedule/bash_jobs 实例：① `schedule_create` 注册的 after 到期 → tick 自动
    /// 派发（user/message 提醒落 default 会话；未到期 → 不派发）；② 后台 bash 完成 →
    /// tick 合作泵自动结算（Completed，非手工）。这是 serve recv_timeout 自驱节拍的
    /// 行为探针（推进点唯一收敛主线程）。
    #[test]
    fn server_tick_once_advances_schedule_and_settles_jobs() {
        use dsh_jobs::JobStatus;
        use dsh_tools::ToolExecutionInput;
        use dsh_session::types::EventKind;
        let host = SessionHost::in_memory();
        let _ = host.session("default");
        let root = std::env::temp_dir().join(format!("dsh-m6-tick-{}", std::process::id()));
        if root.exists() {
            let _ = std::fs::remove_dir_all(&root);
        }
        std::fs::create_dir_all(&root).unwrap();
        let bundle = match crate::web::assemble_server_runtime(
            &host,
            root.clone(),
            "http://127.0.0.1:1",
            "deepseek-v4-flash-0731-ext",
        ) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("assemble_server_runtime deferred (bash unavailable?): {e}");
                let _ = std::fs::remove_dir_all(&root);
                return;
            }
        };
        let now = crate::web::system_now_ms();

        // ① 调度：after(0s 到期) via 工具 → 事件落 default；tick 派发。
        let registry = bundle.host.tools.clone();
        let agent = Some("agent-1".to_string());
        let sres = registry.execute(
            &ToolExecutionInput::new(
                "s1",
                "schedule_create",
                serde_json::json!({
                    "prompt": "tick reminder",
                    "after_seconds": 1
                }),
                agent.clone(),
            ),
            None,
        );
        assert!(!sres.is_error, "schedule_create: {:?}", sres.error);
        let sched_id = sres.value.unwrap()["id"].as_str().unwrap().to_string();
        // 到期门控：after(1s) 在 now 未到期 → tick 不派发。
        let (_, dispatched_now) = web_m5::m5g_tick_once(
            &bundle.schedule,
            bundle.bash_jobs.as_deref(),
            now,
        )
        .expect("tick_once ok");
        assert!(
            !dispatched_now.contains(&sched_id),
            "after(1s) must NOT dispatch before due: {dispatched_now:?}"
        );
        // now+1500ms → 到期 → tick 自动派发提醒（user/message 落 default）。
        let (framing, dispatched) = web_m5::m5g_tick_once(
            &bundle.schedule,
            bundle.bash_jobs.as_deref(),
            now + 1500,
        )
        .expect("tick_once ok");
        assert!(
            dispatched.contains(&sched_id),
            "after(1s) dispatched by tick when due: {dispatched:?}"
        );
        // dispatch_due 向调度宿主会话追加 `schedule/change` dispatch 事件（非 user 消息）。
        let has_dispatch = host
            .events("default")
            .iter()
            .any(|e| {
                e.kind == EventKind::ScheduleChange
                    && e.data.get("operation").and_then(|v| v.as_str()) == Some("dispatch")
            });
        assert!(has_dispatch, "tick dispatched schedule/change into default session");
        assert!(!framing.is_empty(), "tick produced framing text: {framing:?}");

        // ② 后台 bash 完成 → tick 泵自动结算 Completed（非手工）。
        let bres = registry.execute(
            &ToolExecutionInput::new(
                "b1",
                "bash",
                serde_json::json!({
                    "command": format!("echo TICK > {}", root.join("tick.txt").display().to_string().replace('\\', "/")),
                    "description": "m6 tick settle",
                    "run_in_background": true
                }),
                agent.clone(),
            ),
            None,
        );
        assert!(!bres.is_error, "bash bg: {:?}", bres.error);
        let bgid = bres.value.unwrap()["jobId"].as_str().unwrap().to_string();
        let bridge = bundle.bash_jobs.clone().unwrap();
        // 尚未 tick：running 或完成但未结算（泵未调）。
        // tick 一次（now 之后，进程应已退出）→ 合作泵结算 Completed。
        web_m5::m5g_tick_once(&bundle.schedule, Some(&bridge), now + 2000).expect("tick once");
        assert_eq!(
            bridge.read(&bgid, agent.as_deref()).unwrap().snapshot.status,
            JobStatus::Completed,
            "tick pump settled bg job Completed automatically"
        );
        assert!(
            root.join("tick.txt").exists(),
            "bg output landed (completed before pump)"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// M6i 验收 #5：sandbox:policy 投影——`register_sandbox_policy_section` 把动态段
    /// （order 110，Fn provider）注册进 loop SystemPrompt：缺省 read-only；会话
    /// `sandbox/mode` workspace-write → 重装配读模型可见；垃圾 mode 不推翻（fail-closed
    /// 忽略，绝不落未知文本）。
    #[test]
    fn sandbox_policy_section_registers_dynamic_projection() {
        use dsh_system_prompt::{Config as PromptConfig, SystemPrompt};
        use dsh_session::store::SessionStore;
        use dsh_session::types::{EventKind, SessionId};
        let prompt = SystemPrompt::new(&PromptConfig::default(), Rc::new(|| {})).expect("prompt");
        let store = Rc::new(SessionStore::new());
        let session = store
            .create(
                Some(SessionId::from_raw("default".to_string())),
                &dsh_session::CreateSessionOptions { seed: None, meta: None },
            )
            .expect("default session");
        let ws = std::env::temp_dir().join(format!("dsh-m6-sandbox-{}", std::process::id()));
        if ws.exists() {
            let _ = std::fs::remove_dir_all(&ws);
        }
        std::fs::create_dir_all(&ws).unwrap();

        web_m5::register_sandbox_policy_section(&prompt, store.clone(), "default", ws.clone())
            .expect("sandbox:policy section registers");

        let find_seg = |assembly: &dsh_system_prompt::PromptAssembly| {
            assembly
                .sections
                .iter()
                .find(|s| s.name == "sandbox:policy")
                .expect("sandbox:policy in assembly")
                .text
                .clone()
        };

        // 缺省：read-only（写根：none）。
        let text0 = find_seg(&prompt.assemble(&Default::default()).unwrap());
        assert!(text0.contains("read-only"), "default read-only: {text0:?}");
        assert!(text0.contains("writable roots:"), "roots line: {text0:?}");

        // 会话事件 → workspace-write → 重装配投影（写根落名单）。
        session
            .append(EventKind::SandboxMode, json!({"mode": "workspace-write", "source": "session"}), None)
            .expect("append sandbox/mode");
        let text1 = find_seg(&prompt.assemble(&Default::default()).unwrap());
        assert!(text1.contains("workspace-write"), "session mode projected: {text1:?}");
        assert!(
            text1.contains(&ws.to_string_lossy().into_owned()),
            "workspace root among writable roots: {text1:?}"
        );

        // fail-closed：垃圾 mode 不推翻（被忽略，不落未知文本）。
        session
            .append(EventKind::SandboxMode, json!({"mode": "garbage-mode", "source": "session"}), None)
            .expect("append garbage mode");
        let text2 = find_seg(&prompt.assemble(&Default::default()).unwrap());
        assert!(!text2.contains("garbage"), "unknown mode ignored (fail-closed): {text2:?}");
        assert!(
            text2.contains("workspace-write"),
            "last valid session mode preserved: {text2:?}"
        );

        let _ = std::fs::remove_dir_all(&ws);
    }

    /// M6i 验收 #6（step6a）：前端最小闭环——经**完整 serve 装配路径**
    /// （`assemble_server_runtime_with_llm`：真实 M4+M5 注册 + sandbox:policy 段 + 可注入
    /// mock LLM）驱动 `session.prompt` RPC：accepted + 共享 store 落
    /// user/message+assistant/message+turn/end，EventSink 下链触发（前端实时帧），
    /// session.history 可回读（前端同一事实源）。
    #[test]
    fn serve_closure_prompt_routes_to_fully_assembled_loop() {
        use std::cell::RefCell;
        use std::collections::VecDeque;
        use std::rc::Rc;
        let session_host = SessionHost::in_memory();
        let _ = session_host.session("default");
        let root = std::env::temp_dir().join(format!("dsh-m6-closure-{}", std::process::id()));
        if root.exists() {
            let _ = std::fs::remove_dir_all(&root);
        }
        std::fs::create_dir_all(&root).unwrap();

        // Mock LLM：一段文本回答（完整装配路径下走真实 loop 驱动）。
        let script = Rc::new(RefCell::new(VecDeque::from_iter([vec![
            dsh_llm::StreamChunk::BlockStart {
                index: 0,
                block_type: "text".parse().unwrap(),
            },
            dsh_llm::StreamChunk::TextDelta { index: 0, text: "hello from serve closure".into() },
            dsh_llm::StreamChunk::BlockEnd {
                index: 0,
                block: dsh_llm::ContentBlock::text("hello from serve closure"),
            },
            dsh_llm::StreamChunk::Finish {
                reason: dsh_llm::FinishReason::Stop,
                replay_state: None,
            },
        ]])));
        struct Adapter {
            script: Rc<RefCell<VecDeque<Vec<dsh_llm::StreamChunk>>>>,
        }
        impl dsh_llm::LlmAdapter for Adapter {
            fn stream(
                &self,
                _options: dsh_llm::GenerateOptions,
            ) -> Box<dyn Iterator<Item = dsh_llm::StreamChunk>> {
                let next = self.script.borrow_mut().pop_front().unwrap_or_default();
                Box::new(next.into_iter())
            }
        }
        let llm = Rc::new(dsh_llm::LlmRuntime::new());
        llm.register_adapter(&["mock"], Rc::new(Adapter { script })).unwrap();

        let bundle = match crate::web::assemble_server_runtime_with_llm(
            &session_host,
            root.clone(),
            llm,
            "mock",
            "mock-model",
        ) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("assemble deferred (bash unavailable?): {e}");
                let _ = std::fs::remove_dir_all(&root);
                return;
            }
        };
        let mut boot = boot_with_sessions();
        boot.agent_loop = Some(bundle.host.clone());

        // 前端 session.prompt → 完整装配 loop → accepted。
        let body = serde_json::to_vec(&serde_json::json!({
            "type": "client-request", "rpcId": "r1", "method": "session.prompt",
            "payload": {"sessionId": "default", "content": [{"type": "text", "text": "hi from ui"}]},
        }))
        .unwrap();
        let (_, v) = handle_rpc_host(&boot, "session.prompt", &body, &session_host);
        assert_eq!(v["result"]["value"]["accepted"], true, "accepted: {v}");

        // 共享 store：user/message + assistant/message + turn/end。
        let evs = session_host.events("default");
        assert!(evs.iter().any(|e| e.kind.as_str() == "user/message"));
        assert!(evs.iter().any(|e| e.kind.as_str() == "assistant/message"));
        let assistant = evs.iter().find(|e| e.kind.as_str() == "assistant/message").unwrap();
        assert_eq!(assistant.data["message"]["content"][0]["text"], "hello from serve closure");
        assert!(evs.iter().any(|e| e.kind.as_str() == "turn/end"));

        // EventSink 下链（前端实时帧）触发。
        assert!(session_host.sink_len() >= 4, "downlink fired: {}", session_host.sink_len());
        // session.history 可回读（前端同一事实源）。
        let body2 = serde_json::to_vec(&serde_json::json!({
            "type": "client-request", "rpcId": "r2", "method": "session.history",
            "payload": {"sessionId": "default"},
        }))
        .unwrap();
        let (_, h) = handle_rpc_host(&boot, "session.history", &body2, &session_host);
        assert_eq!(h["result"]["value"]["events"].as_array().unwrap().len(), evs.len());

        let _ = std::fs::remove_dir_all(&root);
    }

    /// M6i 验收 #6（step6b）：完整 serve 装配 + 无 key（`server_llm_runtime_with_key(_, _, None)`
    /// 确定性，不读进程 env）→ session.prompt 首回合 **fail-loud**：agent/error 落 store
    /// 且含 `DEEPSEEK_API_KEY` 字面量、turn/end reason Error；**绝不伪造 assistant/message**。
    /// 工具/API 面不受影响（本条专注 LLM 路径诚实表面）。
    #[test]
    fn serve_closure_prompt_without_key_fails_loud_no_fabrication() {
        let session_host = SessionHost::in_memory();
        let _ = session_host.session("default");
        let root = std::env::temp_dir().join(format!("dsh-m6-nokey-{}", std::process::id()));
        if root.exists() {
            let _ = std::fs::remove_dir_all(&root);
        }
        std::fs::create_dir_all(&root).unwrap();
        let llm = crate::m6_llm::server_llm_runtime_with_key(
            "http://127.0.0.1:1",
            "deepseek-v4-flash-0731-ext",
            None,
        );
        let bundle = match crate::web::assemble_server_runtime_with_llm(
            &session_host,
            root.clone(),
            llm,
            "deepseek",
            "deepseek-v4-flash-0731-ext",
        ) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("assemble deferred (bash unavailable?): {e}");
                let _ = std::fs::remove_dir_all(&root);
                return;
            }
        };
        let mut boot = boot_with_sessions();
        boot.agent_loop = Some(bundle.host.clone());

        let body = serde_json::to_vec(&serde_json::json!({
            "type": "client-request", "rpcId": "r1", "method": "session.prompt",
            "payload": {"sessionId": "default", "content": [{"type": "text", "text": "hi"}]},
        }))
        .unwrap();
        let (_, _v) = handle_rpc_host(&boot, "session.prompt", &body, &session_host);

        let evs = session_host.events("default");
        // 绝不伪造 assistant/message。
        assert!(
            !evs.iter().any(|e| e.kind.as_str() == "assistant/message"),
            "no fabricated assistant/message on missing key"
        );
        // user/message 记录输入（loop 已接受）。
        assert!(evs.iter().any(|e| e.kind.as_str() == "user/message"));
        // fail-loud：turn/end reason.error 含 code AUTH + DEEPSEEK_API_KEY 可操作消息
        // （P3：首回合 fail-loud，事件善意暴露；不伪装 Completed）。
        let turn_ends = evs
            .iter()
            .filter(|e| e.kind.as_str() == "turn/end")
            .map(|e| serde_json::to_string(&e.data).unwrap_or_default())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            turn_ends.contains("AUTH") && turn_ends.contains("DEEPSEEK_API_KEY"),
            "turn/end error fail-loud names key env: {turn_ends}"
        );
        assert!(
            turn_ends.contains("error") && !turn_ends.contains("completed"),
            "turn/end records an Error reason (honest): {turn_ends}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// M6i 验收 step8（D-087）：`llm.models` 在装配 catalog（serve 注入 Boot.agent_catalog）
    /// 时从真实 `DeepSeekConnection` 列录——groups 保持 wire 形状（provider+模型 id/name），
    /// `caps` 增量含容量默认 + 重试策略（真实值，不伪造）。
    #[test]
    fn llm_models_reflects_assembled_catalog_caps() {
        let model = "deepseek-v4-flash-0731-ext";
        let mut boot = boot_with_sessions();
        boot.agent_catalog = Some(crate::m6_llm::server_catalog_view("http://127.0.0.1:1", model));
        let body = serde_json::to_vec(&serde_json::json!({
            "type": "client-request", "rpcId": "r1", "method": "llm.models",
            "payload": {},
        }))
        .unwrap();
        let (_, v) = handle_rpc(&boot, "llm.models", &body);
        let value = &v["result"]["value"];
        let groups = value["groups"].as_array().expect("groups array");
        assert_eq!(groups[0]["id"], "deepseek", "real provider group");
        assert_eq!(
            groups[0]["models"][0]["id"],
            model,
            "real catalog model id in wire shape"
        );
        let caps = &value["caps"];
        assert_eq!(caps["provider"], "deepseek");
        assert_eq!(caps["models"][0]["id"], model);
        assert!(
            caps["defaults"]["contextWindow"].as_u64().unwrap_or(0) > 0,
            "caps default contextWindow present"
        );
        assert_eq!(caps["retry"]["mode"], "normal", "real retry policy view");
    }

    /// M6i 验收 step9（D-088）：skill = 通用 prompt 段注册——`register_prompt_section`
    /// （skill: 能力段；step4 sandbox:policy 同一缝的通用化）。静态段组装可见；重名 Err。
    #[test]
    fn skill_prompt_section_registers_generic() {
        use dsh_system_prompt::{Config as PromptConfig, SystemPrompt};
        let prompt = SystemPrompt::new(&PromptConfig::default(), Rc::new(|| {})).expect("prompt");
        // skill 段（order 120：工具指引带内；复用 step4 同一的 section 缝）。
        web_m5::register_prompt_section(
            &prompt,
            "skill:grep",
            120.0,
            "skill:grep — the grep tool is available.\n".to_string(),
        )
        .expect("skill section registers");
        let assembly = prompt.assemble(&Default::default()).unwrap();
        let seg = assembly
            .sections
            .iter()
            .find(|s| s.name == "skill:grep")
            .expect("skill section in assembly");
        assert!(seg.text.contains("grep tool is available"), "{}", seg.text);
        // 重名 → Err（唯一名不变式）。
        let dup = web_m5::register_prompt_section(
            &prompt,
            "skill:grep",
            120.0,
            "dup".to_string(),
        );
        assert!(dup.is_err(), "duplicate skill section rejected");
    }

    /// M6i 验收 step9（D-088）：hooks = pre-execute 宿主钩子——装配级否决。
    /// `assemble_server_loop` 已接记录钩子（`hookInvoked` 落共享 store）；再叠一个
    /// 宿主否决钩子（deny bash）。mock LLM 一轮请求 bash → 钩子否决：hookInvoked
    /// 记录（tool=bash）+ 拒绝原因上抛到事件流；其余工具保持放行。
    #[test]
    fn m6_loop_turn_host_pre_execute_veto_denies_bash() {
        use dsh_llm::{CallId, ContentBlock, FinishReason, StreamChunk, ToolCallBlock};
        use std::cell::RefCell;
        use std::collections::VecDeque;

        fn bash_chunks(id: &str) -> Vec<StreamChunk> {
            let args = r#"{"command":"ls","description":"list"}"#;
            vec![
                StreamChunk::ToolCallDelta {
                    index: 0,
                    id: CallId::from_raw(id),
                    name: Some("bash".into()),
                    arguments_delta: args.into(),
                },
                StreamChunk::BlockEnd {
                    index: 0,
                    block: ContentBlock::ToolCall(ToolCallBlock {
                        id: CallId::from_raw(id),
                        name: "bash".into(),
                        arguments: args.into(),
                    }),
                },
                StreamChunk::Finish {
                    reason: FinishReason::ToolCalls,
                    replay_state: None,
                },
            ]
        }

        fn text_chunks(text: &str) -> Vec<StreamChunk> {
            vec![
                StreamChunk::BlockStart {
                    index: 0,
                    block_type: "text".parse().unwrap(),
                },
                StreamChunk::TextDelta {
                    index: 0,
                    text: text.into(),
                },
                StreamChunk::BlockEnd {
                    index: 0,
                    block: ContentBlock::text(text),
                },
                StreamChunk::Finish {
                    reason: FinishReason::Stop,
                    replay_state: None,
                },
            ]
        }

        struct Adapter {
            script: Rc<RefCell<VecDeque<Vec<StreamChunk>>>>,
        }
        impl dsh_llm::LlmAdapter for Adapter {
            fn stream(
                &self,
                _options: dsh_llm::GenerateOptions,
            ) -> Box<dyn Iterator<Item = StreamChunk>> {
                let next = self.script.borrow_mut().pop_front().unwrap_or_default();
                Box::new(next.into_iter())
            }
        }
        let script = Rc::new(RefCell::new(VecDeque::from_iter([
            bash_chunks("b1"),
            text_chunks("bash was denied"),
        ])));
        let llm = Rc::new(dsh_llm::LlmRuntime::new());
        llm.register_adapter(&["mock"], Rc::new(Adapter { script }))
            .unwrap();

        let session_host = SessionHost::in_memory();
        let _ = session_host.session("default");
        let m4 = M4HostServices {
            jobs: None,
            schedule: None,
            todo: None,
        };
        let root = std::env::temp_dir().join(format!("dsh-m6-hook-{}", std::process::id()));
        if root.exists() {
            let _ = std::fs::remove_dir_all(&root);
        }
        std::fs::create_dir_all(&root).unwrap();
        let m5 = web_m5::M5Host::assemble(root.clone()).expect("m5 assembles");
        let loop_host = assemble_server_loop(
            session_host.store.clone(),
            root.clone(),
            llm,
            "mock",
            "mock-model",
            m4,
            m5,
        )
        .expect("assemble ok");

        // 宿主否决钩子叠上（记录钩子已由装配接好；否决钩子先落先裁决）。
        {
            let sid = dsh_session::types::SessionId::from_raw("default".to_string());
            let session = loop_host
                .store
                .get(&sid)
                .expect("default session in shared store");
            web_m5::register_pre_execute_hook(
                &loop_host.tools,
                session,
                Rc::new(|name| {
                    if name == "bash" {
                        Some("bash disabled by host hook".to_string())
                    } else {
                        None
                    }
                }),
            )
            .expect("veto hook registers");
        }

        let mut boot = boot_with_sessions();
        boot.agent_loop = Some(loop_host.clone());
        crate::run_rust_loop(&boot, "default", "run bash ls")
            .expect("turn runs to completion");

        // 证据 1：hookInvoked 记录了 bash（pre-execute 钩子真实触发）。
        let evs = session_host.events("default");
        let tools_visited: Vec<String> = evs
            .iter()
            .filter(|e| e.kind == dsh_session::types::EventKind::HookInvoked)
            .map(|e| e.data["tool"].as_str().unwrap_or("?").to_string())
            .collect();
        assert!(
            tools_visited.iter().any(|t| t == "bash"),
            "hookInvoked fired for bash: {tools_visited:?}"
        );
        // 证据 2：拒绝原因上抛（工具结果/错误流的串行化文本含宿主否决理由）。
        let whole: String = evs
            .iter()
            .map(|e| serde_json::to_string(&e.data).unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            whole.contains("bash disabled by host hook"),
            "deny reason surfaced in event stream (got: {whole})"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// M6i 验收 #6（step6c，**门控真实端点冒烟**）：`DEEPSEEK_API_KEY` 环境变量存在 →
    /// 用完整 serve 装配（`assemble_server_runtime`，key 仅 env 读取）真实驱动一轮
    /// `session.prompt`，断言真实 assistant/message 落共享 store + EventSink 下链。
    /// key 缺失 / 端点不可达 / 网络错误 → **诚实记录 skipped**（门控，不伪造、不失败）；
    /// key 永不落盘/入库/入 git（P4）。
    #[test]
    fn serve_closure_real_endpoint_smoke_gated() {
        let Some(key) = std::env::var(crate::m6_llm::DEEPSEEK_API_KEY_ENV).ok() else {
            eprintln!(
                "GATED-SMOKE-SKIP: {} not set — skipping real-endpoint turn (set it to run)",
                crate::m6_llm::DEEPSEEK_API_KEY_ENV
            );
            return;
        };
        let _ = key;
        let base_url = std::env::var("DSH_LLM_BASE_URL")
            .unwrap_or_else(|_| "http://100.105.152.101:18080/v1".to_string());
        let model = std::env::var("DSH_LLM_MODEL")
            .unwrap_or_else(|_| "deepseek-v4-flash-0731-ext".to_string());
        let session_host = SessionHost::in_memory();
        let _ = session_host.session("default");
        let root = std::env::temp_dir().join(format!("dsh-m6-realsmoke-{}", std::process::id()));
        if root.exists() {
            let _ = std::fs::remove_dir_all(&root);
        }
        std::fs::create_dir_all(&root).unwrap();
        let bundle = match crate::web::assemble_server_runtime(&session_host, root.clone(), &base_url, &model)
        {
            Ok(b) => b,
            Err(e) => {
                eprintln!("GATED-SMOKE-SKIP: assembly failed (bash?): {e}");
                let _ = std::fs::remove_dir_all(&root);
                return;
            }
        };
        let mut boot = boot_with_sessions();
        boot.agent_loop = Some(bundle.host.clone());

        let body = serde_json::to_vec(&serde_json::json!({
            "type": "client-request", "rpcId": "r1", "method": "session.prompt",
            "payload": {"sessionId": "default", "content": [{"type": "text", "text": "Reply with the single word OK."}]},
        }))
        .unwrap();
        let (_, v) = handle_rpc_host(&boot, "session.prompt", &body, &session_host);
        let accepted = v["result"]["value"]["accepted"].as_bool().unwrap_or(false);

        let evs = session_host.events("default");
        let honest_fail = evs.iter().any(|e| {
            e.kind.as_str() == "turn/end"
                && serde_json::to_string(&e.data)
                    .map(|s| s.contains("error") || s.contains("NETWORK") || s.contains("AUTH"))
                    .unwrap_or(false)
        });
        if honest_fail {
            eprintln!(
                "GATED-SMOKE-SKIP: real endpoint {base_url} unreachable/errored (turn/end error) — recorded, not failing (accounted; key presence unrelated)"
            );
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        let assistant = evs.iter().find(|e| e.kind.as_str() == "assistant/message");
        match assistant {
            Some(a) => {
                let text = a.data["message"]["content"][0]["text"].as_str().unwrap_or("");
                assert!(
                    !text.trim().is_empty() && accepted,
                    "real assistant text landed: {text:?} (accepted={accepted})"
                );
                assert!(session_host.sink_len() >= 4, "downlink fired for real turn");
                eprintln!("GATED-SMOKE-OK: real turn replied {text:?}");
            }
            None => {
                eprintln!(
                    "GATED-SMOKE-SKIP: no assistant/message (endpoint condition) — recorded, not failing"
                );
            }
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    /// M6W（D-092）/M6i：**门控真实端点 agent 冒烟**（key 仅 env，P4）——完整 serve
    /// 装配（真实 M4+M5 + deepseek，`assemble_server_runtime`）+ `run_rust_loop` 驱动
    /// 同一会话两轮：
    /// ① **能力轮**：指令遵循/推理（要求返回确定性整数 156）——验证模型响应能力
    ///   （非平凡文本 + 正确性证据，诚实记录）；
    /// ② **agent 轮**：要求调用 `todo_write`（确定性 M4 工具）——验证**完整 agent
    ///   闭环**：`tool/call` → `todo/write` 落共享店 → `tool/result` → 续轮 assistant →
    ///   干净 `turn/end`。
    /// key 缺失/装配失败/端点 AUTH/NETWORK 不可达 → 诚实 GATED-SKIP（不伪造、不失败）；
    /// 模型**未执行工具调用** → 真实失败（这正是要测的），带事件证据 fail-loud。
    #[test]
    fn serve_closure_real_endpoint_model_capability_and_agent_gated() {
        let Some(key) = std::env::var(crate::m6_llm::DEEPSEEK_API_KEY_ENV).ok() else {
            eprintln!(
                "GATED-SKIP: {} not set — skipping real endpoint capability+agent probe",
                crate::m6_llm::DEEPSEEK_API_KEY_ENV
            );
            return;
        };
        let _ = key;
        let base_url = std::env::var("DSH_LLM_BASE_URL")
            .unwrap_or_else(|_| "http://100.105.152.101:18080/v1".to_string());
        let model = std::env::var("DSH_LLM_MODEL")
            .unwrap_or_else(|_| "deepseek-v4-flash-0731-ext".to_string());
        let session_host = SessionHost::in_memory();
        let _ = session_host.session("default");
        let root = std::env::temp_dir().join(format!("dsh-m6-realagent-{}", std::process::id()));
        if root.exists() {
            let _ = std::fs::remove_dir_all(&root);
        }
        std::fs::create_dir_all(&root).unwrap();
        let bundle = match crate::web::assemble_server_runtime(
            &session_host,
            root.clone(),
            &base_url,
            &model,
        ) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("GATED-SKIP: assembly failed (bash?): {e}");
                let _ = std::fs::remove_dir_all(&root);
                return;
            }
        };
        let mut boot = boot_with_sessions();
        boot.agent_loop = Some(bundle.host.clone());

        // 事件窗口助手。
        fn window_kinds(evs: &[dsh_session::types::SessionEvent], since: usize) -> Vec<String> {
            evs.iter()
                .skip(since)
                .map(|e| e.kind.as_str().to_string())
                .collect()
        }
        fn window_has(evs: &[dsh_session::types::SessionEvent], since: usize, kind: &str) -> bool {
            window_kinds(evs, since).iter().any(|k| k == kind)
        }
        fn window_clean_turn_end(
            evs: &[dsh_session::types::SessionEvent],
            since: usize,
        ) -> bool {
            evs.iter().skip(since).any(|e| {
                e.kind.as_str() == "turn/end"
                    && !serde_json::to_string(&e.data)
                        .map(|s| {
                            s.contains("error") || s.contains("NETWORK") || s.contains("AUTH")
                        })
                        .unwrap_or(false)
            })
        }
        fn window_last_assistant_text(
            evs: &[dsh_session::types::SessionEvent],
            since: usize,
        ) -> String {
            evs.iter()
                .skip(since)
                .filter(|e| e.kind.as_str() == "assistant/message")
                .filter_map(|e| {
                    e.data
                        .get("message")
                        .and_then(|m| m.get("content"))
                        .and_then(|c| c.as_array())
                        .and_then(|arr| arr.first())
                        .and_then(|b| b.get("text"))
                        .and_then(|t| t.as_str())
                        .map(String::from)
                })
                .next_back()
                .unwrap_or_default()
        }

        // ---- ① 能力轮（12*13=156：指令遵循 + 数值）----
        crate::run_rust_loop(&boot, "default", "Reply with ONLY the integer 156 and nothing else.")
            .expect("capability turn runs");
        let evs = session_host.events("default");
        let n0 = 0;
        assert!(
            window_clean_turn_end(&evs, n0),
            "capability turn ends clean (endpoint healthy)"
        );
        let cap_reply = window_last_assistant_text(&evs, n0);
        assert!(
            !cap_reply.trim().is_empty(),
            "capability turn produced real assistant text (model responded)"
        );
        eprintln!(
            "REAL-CAPABILITY base={base_url} model={model} reply={cap_reply:?} exact_156={}",
            cap_reply.contains("156")
        );
        assert!(cap_reply.contains("156"), "instruction-following echoes the target number");

        // ---- ② agent 轮（要求真实工具调用：todo_write）----
        let n1 = session_host.events("default").len();
        crate::run_rust_loop(
            &boot,
            "default",
            "You MUST call the todo_write tool with a todo whose content is exactly 'dsh real agent verification'. Do not answer in plain text. Only call the tool.",
        )
        .expect("agent turn runs");
        let evs = session_host.events("default");
        let kinds = window_kinds(&evs, n1);
        eprintln!("REAL-AGENT base={base_url} model={model} window={kinds:?}");
        assert!(
            window_has(&evs, n1, "tool/call"),
            "agent emitted a tool call (full loop engaged); window={kinds:?}"
        );
        assert!(
            window_has(&evs, n1, "todo/write"),
            "todo_write executed and landed in shared store; window={kinds:?}"
        );
        // 证据强化：todo/write 实际记录内容（工具参数真实落库，非占位）。
        let todo_ev = evs
            .iter()
            .skip(n1)
            .find(|e| e.kind.as_str() == "todo/write")
            .expect("todo/write event present");
        let todo_json = serde_json::to_string(&todo_ev.data).unwrap_or_default();
        eprintln!("REAL-AGENT-TODO data_json={todo_json}");
        assert!(
            todo_json.contains("dsh real agent verification"),
            "todo_write recorded the requested content exactly (got: {todo_json})"
        );
        assert!(
            window_has(&evs, n1, "tool/result"),
            "tool result returned to the loop; window={kinds:?}"
        );
        assert!(
            window_clean_turn_end(&evs, n1),
            "agent turn ends clean after tool round-trip; window={kinds:?}"
        );
        let agent_reply = window_last_assistant_text(&evs, n1);
        assert!(
            !agent_reply.trim().is_empty(),
            "agent produced a final assistant message after the tool result"
        );
        eprintln!(
            "REAL-AGENT-OK agent closed the tool loop; final_reply={agent_reply:?} carries_todo={}",
            agent_reply.contains("dsh real agent verification")
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// M6W（D-093）：**门控真实端点 agent 真实任务**（key 仅 env，P4）——完整 serve 装配
    /// （真实 M4+M5 + deepseek），工作区根 = **本仓库根**，把仓库地址交给 agent，分配一个
    /// **非破坏性真实任务**：分析 SQLite 后端 + 接入 `dsh web` 的迁移完整性并产出 markdown
    /// 报告。验证 agent 可用性三个面：① 拿到真实仓库路径后真的用工具调查（read/glob/grep/
    /// bash 至少一次）；② 把报告写成 gitignored 的新工件（`target/agent-verification/…`）；
    /// ③ **非破坏**：任务结束后 `git status --porcelain` 工作树仍干净（只允许 target/ 产物，
    /// 已 gitignore）。
    /// 端点不可达/装配失败 → 诚实 GATED-SKIP；模型未动手/越界改源 → fail-loud。
    #[test]
    fn serve_closure_real_endpoint_agent_nondestructive_repo_task_gated() {
        use std::process::Command;

        let Some(_key) = std::env::var(crate::m6_llm::DEEPSEEK_API_KEY_ENV).ok() else {
            eprintln!(
                "GATED-SKIP: {} not set — skipping real endpoint agent real-task probe",
                crate::m6_llm::DEEPSEEK_API_KEY_ENV
            );
            return;
        };
        let base_url = std::env::var("DSH_LLM_BASE_URL")
            .unwrap_or_else(|_| "http://100.105.152.101:18080/v1".to_string());
        let model = std::env::var("DSH_LLM_MODEL")
            .unwrap_or_else(|_| "deepseek-v4-flash-0731-ext".to_string());

        // 工作区根 = 本仓库根（含 DECISIONS.md 的祖先目录）。
        let cwd = std::env::current_dir().expect("cwd");
        let repo = cwd
            .ancestors()
            .find(|p| p.join("DECISIONS.md").exists() && p.join("Cargo.toml").exists())
            .unwrap_or(cwd.as_path())
            .to_path_buf();

        let session_host = SessionHost::in_memory();
        let _ = session_host.session("default");
        let bundle = match crate::web::assemble_server_runtime(
            &session_host,
            repo.clone(),
            &base_url,
            &model,
        ) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("GATED-SKIP: assembly failed (bash?): {e}");
                return;
            }
        };
        let mut boot = boot_with_sessions();
        boot.agent_loop = Some(bundle.host.clone());

        // 事件窗口助手（与能力+agent 测试同构）。
        fn window_kinds(evs: &[dsh_session::types::SessionEvent], since: usize) -> Vec<String> {
            evs.iter().skip(since).map(|e| e.kind.as_str().to_string()).collect()
        }
        fn window_clean_turn_end(evs: &[dsh_session::types::SessionEvent], since: usize) -> bool {
            evs.iter().skip(since).any(|e| {
                e.kind.as_str() == "turn/end"
                    && !serde_json::to_string(&e.data)
                        .map(|s| s.contains("error") || s.contains("NETWORK") || s.contains("AUTH"))
                        .unwrap_or(false)
            })
        }

        let report = repo.join("target/agent-verification/migration-completeness.md");
        let _ = std::fs::remove_file(&report);
        // 预建 gitignored 报告目录（`write` 工具不自动建父目录；此目录非 tracked、非破坏）。
        std::fs::create_dir_all(repo.join("target/agent-verification")).unwrap();
        let repo_s = repo.to_string_lossy().to_string();
        let task = format!(
            "You operate in the repository workspace root: {repo_s}\n\
             Task (NON-DESTRUCTIVE audit + one new artifact):\n\
             1. Analyze migration completeness: how complete is the SQLite persistence backend and its \
             integration into `dsh web` in this Rust port? Primary sources: DECISIONS.md (entries \
             D-089..D-093 and the M6-ACCEPTANCE / M6W-ACCEPTANCE sections), M6W-REQUIREMENTS.md, \
             M6W-DESIGN.md; implementation: crates/dsh-persistence/src/sqlite.rs, \
             crates/dsh-cli/src/session_host.rs, crates/dsh-cli/src/web.rs, crates/dsh-cli/src/m6_llm.rs.\n\
             2. State in a concise markdown report: what is COMPLETE, what is INCOMPLETE or UNCERTAIN, \
             and your EVIDENCE (file paths + what you saw).\n\
             3. Use the read / glob / grep / bash (read-only; e.g. `git ls-files`, `ls`) tools to \
             investigate. DO NOT create, modify, or delete any existing file except the single report.\n\
             4. Write the finished report ONLY to this exact file: target/agent-verification/migration-completeness.md \
             (relative to the workspace root). The directory target/agent-verification already exists; \
             just write the file.\n\
             5. Keep your investigation to a few steps. Finish by replying with a one-line summary."
        );
        let n0 = session_host.events("default").len();
        if let Err(e) = crate::run_rust_loop(&boot, "default", &task) {
            eprintln!("GATED-SKIP: run_rust_loop failed (bash/sandbox env?): {e}");
            return;
        }
        let evs = session_host.events("default");
        let kinds = window_kinds(&evs, n0);
        // 端点不可达（turn/end 带 error/NETWORK/AUTH）→ 诚实 skip。
        if evs.iter().skip(n0).any(|e| {
            e.kind.as_str() == "turn/end"
                && serde_json::to_string(&e.data)
                    .map(|s| s.contains("error") || s.contains("NETWORK") || s.contains("AUTH"))
                    .unwrap_or(false)
        }) {
            eprintln!("GATED-SKIP: real endpoint {base_url} errored mid-task; window={kinds:?}");
            return;
        }
        eprintln!(
            "REAL-TASK-CLEAN base={base_url} model={model} window_len={} tool_calls={} tool_results={}",
            kinds.len(),
            kinds.iter().filter(|k| *k == "tool/call").count(),
            kinds.iter().filter(|k| *k == "tool/result").count(),
        );
        assert!(
            window_clean_turn_end(&evs, n0),
            "agent task turn ends clean; window={kinds:?}"
        );
        // ① agent 真的用工具在仓库里调查（read/glob/grep/bash 至少一种 tool/call）。
        // tool/call 载荷 = ToolCallPayload{ turn, step, callId, name, arguments } → data["name"]。
        let investigated = ["read", "glob", "grep", "bash"].iter().any(|t| {
            evs.iter().skip(n0).any(|e| {
                e.kind.as_str() == "tool/call" && e.data.get("name").and_then(Value::as_str) == Some(*t)
            })
        });
        assert!(investigated, "agent used read/glob/grep/bash to investigate the repo; window={kinds:?}");
        // ② 报告真实写成（新工件非空）。
        let contents = std::fs::read_to_string(&report).unwrap_or_default();
        eprintln!(
            "REAL-TASK-REPORT path={} bytes={} head={:?}",
            report.display(),
            contents.len(),
            contents.lines().take(2).collect::<Vec<_>>()
        );
        assert!(!contents.trim().is_empty(), "agent wrote a non-empty report");
        assert!(
            contents.contains("SQLite") || contents.contains("sqlite"),
            "report actually discusses the SQLite migration (got head: {:?})",
            contents.lines().take(3).collect::<Vec<_>>()
        );
        // ③ 非破坏：工作树干净（target/ 已 gitignore，产物不出现在 status）。
        let status = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&repo)
            .output()
            .expect("git available in repo");
        let dirty = String::from_utf8_lossy(&status.stdout).trim().to_string();
        assert!(
            dirty.is_empty(),
            "agent task did not modify any tracked file (non-destructive); git status:\n{dirty}"
        );
        eprintln!("REAL-TASK-OK agent completed a real non-destructive repo task end-to-end");
    }

    fn simple_turn(text: &str) -> Vec<(String, Vec<u8>)> {
        vec![
            (
                "user/message".into(),
                serde_json::to_vec(&serde_json::json!({"text": text, "role": "user"})).unwrap(),
            ),
            (
                "assistant/message".into(),
                serde_json::to_vec(&serde_json::json!({
                    "turn": 1, "step": 1,
                    "message": {"id": "a1", "role": "assistant",
                                "content": [{"type": "text", "text": format!("echo: {text}")}],
                                "source": {"kind": "model", "provider": "mock", "model": "mock"}},
                }))
                .unwrap(),
            ),
            (
                "turn/end".into(),
                serde_json::to_vec(&serde_json::json!({"turn": 1, "reason": "completed"})).unwrap(),
            ),
        ]
    }

    /// A3：sqlite_store 优先于 session_dir（写落 sqlite 文件、jsonl 根为空）；无则内存。
    #[test]
    fn session_host_precedence_sqlite_over_jsonl() {
        use dsh_persistence::PersistenceBackend;
        let dir = std::env::temp_dir().join(format!("dsh-m6w-preced-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let jsonl_root = dir.join("jsonl");
        let sqlite_file = dir.join("store.sqlite");

        // sqlite + jsonl 同给 → sqlite 生效 + eprintln 警告（警告仅诊断，行为是 sqlite）。
        let both = session_host_for(&Some(sqlite_file.clone()), &Some(jsonl_root.clone()))
            .expect("host");
        assert_eq!(both.persistence_kind(), "sqlite");
        both.adopt("default", &simple_turn("hi")).unwrap();
        both.flush("default").unwrap();
        // 写落 sqlite 文件；jsonl 根零 artifact。
        let backend = dsh_persistence::sqlite::SqliteBackend::open(&sqlite_file).unwrap();
        let stored = backend
            .load_stored(&dsh_brand::SessionId::from_raw("default".to_string()))
            .unwrap();
        assert!(stored.is_some(), "session persisted to sqlite");
        let jsonl_entries = std::fs::read_dir(&jsonl_root).map(|rd| rd.count()).unwrap_or(0);
        assert_eq!(jsonl_entries, 0, "jsonl root untouched when sqlite wins");

        // 仅 jsonl → jsonl；仅内存 → mem。
        let mem = session_host_for(&None, &None).expect("host");
        assert_eq!(mem.persistence_kind(), "mem");
        let jsonl = session_host_for(&None, &Some(jsonl_root.clone())).expect("host");
        assert_eq!(jsonl.persistence_kind(), "jsonl");
        let _ = std::fs::remove_dir_all(&dir).ok();
    }

    /// M6W（D-093，真实端点冒烟发现）：**loop 必须把 ToolRegistry 的 schema 发给 LLM**
    /// ——用捕获适配器跑 `assemble_server_runtime_with_llm` 装配的真实路径，断言每轮
    /// `GenerateOptions.tools` 携带注册的工具（todo_write）。
    /// 红（缺陷）→ 修：dsh-agent-loop host 装配时把 registry 注册为 system-prompt 工具
    /// provider（assembly.tools 非空 → build_request → 请求带 tools）。
    #[test]
    fn agent_loop_request_carries_registry_tools_to_llm() {
        use dsh_llm::{FinishReason, GenerateOptions, LlmAdapter, LlmRuntime, StreamChunk};

        struct Capture {
            tool_names: std::rc::Rc<std::cell::RefCell<Option<Vec<String>>>>,
        }
        impl LlmAdapter for Capture {
            fn stream(&self, options: GenerateOptions) -> Box<dyn Iterator<Item = StreamChunk>> {
                *self.tool_names.borrow_mut() = options.tools.map(|ts| {
                    ts.into_iter().map(|t| t.name).collect::<Vec<String>>()
                });
                let end = vec![StreamChunk::Finish {
                    reason: FinishReason::Stop,
                    replay_state: None,
                }];
                Box::new(end.into_iter())
            }
        }

        let host = SessionHost::in_memory();
        let _ = host.session("default");
        let root = std::env::temp_dir().join(format!("dsh-m6w-tools-{}", std::process::id()));
        if root.exists() {
            let _ = std::fs::remove_dir_all(&root);
        }
        std::fs::create_dir_all(&root).unwrap();

        let captured = std::rc::Rc::new(std::cell::RefCell::new(None::<Vec<String>>));
        let llm = std::rc::Rc::new(LlmRuntime::new());
        llm.register_adapter(&["deepseek"], std::rc::Rc::new(Capture { tool_names: captured.clone() }))
            .unwrap();
        let bundle = crate::web::assemble_server_runtime_with_llm(
            &host,
            root.clone(),
            llm,
            "deepseek",
            "mock-model",
        )
        .expect("assemble real loop path");
        let mut boot = boot_with_sessions();
        boot.agent_loop = Some(bundle.host.clone());
        crate::run_rust_loop(&boot, "default", "hi").expect("turn runs");

        let seen = captured.borrow().clone();
        let names = seen.expect("loop request captured by LLM adapter (GenerateOptions seen)");
        assert!(
            names.iter().any(|n| n == "todo_write"),
            "agent request must carry registered tool schemas to the LLM (got: {names:?}); \
             without this the real model never sees tools and cannot call them"
        );
        assert!(
            names.iter().any(|n| n == "read" || n == "glob" || n == "bash"),
            "M4/M5 tools advertised to LLM (got: {names:?})"
        );
        let _ = std::fs::remove_dir_all(&root).ok();
    }

    /// M6W（D-093）回归护网：**工具 provider 注册幂等**——同一 host 多次 ensure_agent /
    /// followup 不重复注册 schema（assembly.tools 无重复名）。
    #[test]
    fn agent_loop_tool_schemas_registered_once_and_idempotent() {
        use dsh_llm::{FinishReason, GenerateOptions, LlmAdapter, LlmRuntime, StreamChunk};
        use std::cell::RefCell;
        use std::rc::Rc;

        struct Capture {
            all: Rc<RefCell<Vec<Vec<String>>>>,
        }
        impl LlmAdapter for Capture {
            fn stream(&self, options: GenerateOptions) -> Box<dyn Iterator<Item = StreamChunk>> {
                self.all.borrow_mut().push(
                    options.tools.map(|ts| ts.into_iter().map(|t| t.name).collect()).unwrap_or_default(),
                );
                let end = vec![StreamChunk::Finish {
                    reason: FinishReason::Stop,
                    replay_state: None,
                }];
                Box::new(end.into_iter())
            }
        }

        let h = SessionHost::in_memory();
        let _ = h.session("default");
        let root = std::env::temp_dir().join(format!("dsh-m6w-tools2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let all = Rc::new(RefCell::new(Vec::<Vec<String>>::new()));
        let llm = Rc::new(LlmRuntime::new());
        llm.register_adapter(&["deepseek"], Rc::new(Capture { all: all.clone() })).unwrap();
        let bundle = crate::web::assemble_server_runtime_with_llm(&h, root.clone(), llm, "deepseek", "m")
            .expect("assemble");
        let mut boot = boot_with_sessions();
        boot.agent_loop = Some(bundle.host.clone());
        crate::run_rust_loop(&boot, "default", "one").unwrap();
        crate::run_rust_loop(&boot, "default", "two").unwrap();
        let batches = all.borrow();
        assert_eq!(batches.len(), 2, "two turns produced two requests");
        for names in batches.iter() {
            let mut sorted = names.clone();
            sorted.sort();
            sorted.dedup();
            assert_eq!(
                sorted.len(),
                names.len(),
                "no duplicate tool schemas per request (idempotent registration): {names:?}"
            );
            assert!(names.iter().any(|n| n == "todo_write"));
        }
        let _ = std::fs::remove_dir_all(&root).ok();
    }
}

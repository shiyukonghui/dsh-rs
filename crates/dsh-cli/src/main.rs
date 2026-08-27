//! DSH CLI：从 cordis.yml 启动并交互式驱动 WASM loop。
//!
//! 用法：`dsh <cordis.yml> [--overlay <file> | --patch <file>]... [--wasm-base <dir>] [--watch] [--once <task>] [--session-in <file>] [--session-out <file>] [--dump-config]`
//! - `<cordis.yml>`：主配置（services + 插件包 entries；`name` = 插件包文件夹名，
//!   内含 wasm 组件 + 前端组件；`plugin.json` 清单或构建目录约定定位 wasm/前端）。
//! - `--overlay <file>` / `--patch <file>`（M52 别名，对齐生产 `dsh --patch`）：
//!   profile 叠加层（可多次；argv 顺序——同 id entry 的完整 config 替换，
//!   新 id 追加插入；对应生产 patch overlay 语义）。
//! - `--wasm-base <dir>`：WASM 组件解析基址（默认 `wasm-plugins`）。
//! - `--watch`：HMR 热重载——监视主配置（及 overlay），变化后重新挂载。
//! - `--once <task>`：**headless 单发**（M45，对齐 DSH `dsh --profile headless
//!   "job"`）——提交一个任务，打印最终答案并退出（completed → exit 0）。
//! - `--session-in <file>`：恢复会话（M48，对齐 DSH resume）——从 JSONL 加载
//!   历史事件，后续 turn 的 llm 输入含前轮上下文。
//! - `--session-out <file>`：headless 后把会话保存为 JSONL（M47，对齐 DSH
//!   `session-persistence-jsonl`；`--once` 时生效）。
//! - `--dump-config`：转储生效配置（M56，对齐生产 `dsh --dump-config`——
//!   合并 overlays 后的 entries YAML；不 boot loop）。
//!
//! 从 stdin 逐行读 JSON 作为用户输入（每行一个 turn），经 WASM loop 驱动后
//! 打印响应 JSON；EOF 退出。`--watch` 时 stdin 在后台线程读取，主循环同时
//! 消费 HMR 事件（OS 文件系统通知；事件驱动，见 `dsh_loader::Hmr::watch`）。

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use dsh_loader::Hmr;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: dsh <cordis.yml> [--overlay <file> | --patch <file>]... [--wasm-base <dir>] [--watch] [--once <task>] [--session-in <file>] [--session-out <file>] [--dump-config]");
        eprintln!("       dsh web <cordis.yml> [--web-root <dir>] [--host <h>] [--port <p>] [--overlay <file>]... [--wasm-base <dir>]");
        std::process::exit(2);
    }
    // M70：`dsh web <cordis.yml> ...` 子命令——服务前端 + /api RPC。
    if args[1] == "web" {
        web_main(&args[2..]);
        return;
    }
    let config_path = PathBuf::from(&args[1]);
    let mut overlays: Vec<PathBuf> = Vec::new();
    let mut wasm_base = PathBuf::from("wasm-plugins");
    let mut watch = false;
    let mut once: Option<String> = None;
    let mut session_out: Option<PathBuf> = None;
    let mut session_in: Option<PathBuf> = None;
    let mut dump_config = false;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--overlay" => {
                i += 1;
                overlays.push(PathBuf::from(&args[i]));
            }
            "--patch" => {
                // M52：`--patch` 为 `--overlay` 的别名（对齐生产 CLI
                // `dsh --patch <path>`——argv 顺序 patch overlay：行级 config
                // 替换 + insert 新行）。
                i += 1;
                overlays.push(PathBuf::from(&args[i]));
            }
            "--wasm-base" => {
                i += 1;
                wasm_base = PathBuf::from(&args[i]);
            }
            "--watch" => {
                watch = true;
            }
            "--once" => {
                i += 1;
                once = Some(args[i].clone());
            }
            "--session-out" => {
                i += 1;
                session_out = Some(PathBuf::from(&args[i]));
            }
            "--session-in" => {
                i += 1;
                session_in = Some(PathBuf::from(&args[i]));
            }
            "--dump-config" => {
                // M56：转储生效配置（合并 overlays）后退出，不 boot loop
                dump_config = true;
            }
            other => {
                eprintln!("unknown arg {other}");
                std::process::exit(2);
            }
        }
        i += 1;
    }

    // M56：--dump-config——转储生效配置（合并 overlays）后退出（不 boot loop）
    if dump_config {
        match dsh_cli::dump_config(&config_path, &overlays) {
            Ok(yaml) => {
                println!("{yaml}");
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("dump-config failed: {e}");
                std::process::exit(1);
            }
        }
    }

    let boot = match dsh_cli::boot(&config_path, &overlays, &wasm_base) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("boot failed: {e}");
            std::process::exit(1);
        }
    };

    // M40：注入宿主时钟（毫秒）——timer 服务（ctx.timeout/interval/debounce/
    // throttle）在主循环经 drive_timers 驱动；对齐 Cordis 事件循环驱动 timer。
    boot.ctx.set_timer_clock(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    });

    // M48：恢复会话（`--session-in <file>`）——从 JSONL 加载历史事件，
    // 后续 turn 的 llm 输入含前轮上下文（对齐 DSH resume）。
    if let Some(path) = &session_in {
        if let Err(e) = dsh_cli::restore_session(&boot, path) {
            eprintln!("session restore failed: {e}");
            std::process::exit(1);
        }
    }

    // M45：headless 单发——提交任务 → 打印最终答案 → 按 reason 退出
    if let Some(task) = once {
        match dsh_cli::run_headless(&boot, &task) {
            Ok(result) => {
                // M47：`--session-out` 保存会话（JSONL）供后续恢复/审计
                if let Some(path) = &session_out {
                    if let Err(e) = boot.sessions.lock().unwrap().save_to(path) {
                        eprintln!("session save failed: {e}");
                        std::process::exit(1);
                    }
                }
                println!("{}", result.answer);
                std::process::exit(if result.reason == "completed" { 0 } else { 1 });
            }
            Err(e) => {
                eprintln!("headless failed: {e}");
                std::process::exit(1);
            }
        }
    }

    // HMR：监视主配置 + overlays；变化 → boot.refresh() 重新挂载。
    // M35：事件驱动（notify watcher 后台线程 + mpsc 桥接）；poll() 消费事件。
    // M38：refresh 失败经 error sink 通知（对齐 Cordis `hmr/config-update-failed`
    // 的 parallel 事件——经 ctx.parallel emit，监听者可响应；同时 eprintln 诊断）。
    let mut hmr: Option<Hmr> = None;
    if watch {
        let h = Hmr::new();
        let mut watch_paths = vec![config_path.clone()];
        watch_paths.extend(overlays.iter().cloned());
        for p in &watch_paths {
            let refresh = boot.refresh.clone();
            h.register_config(p, Rc::new(move || refresh()));
        }
        // M38：失败 → `hmr/config-update-failed` parallel 事件（filename, error
        // JSON）+ 控制台诊断。监听者经 `ctx.on("hmr/config-update-failed", …)`
        // 注册；无监听者时 parallel 无副作用（与 Cordis 一致）。
        let ctx = boot.ctx.clone();
        h.set_error_sink(Rc::new(move |filename, error| {
            let args = vec![
                serde_json::Value::String(filename.to_string()),
                serde_json::json!({"message": error.to_string()}),
            ];
            ctx.parallel("hmr/config-update-failed", args);
            eprintln!("hmr/config-update-failed ({filename}): {error}");
        }));
        // 启动 OS 文件系统 watcher（失败则退化为轮询，不阻断启动）
        if let Err(e) = h.watch(&watch_paths) {
            eprintln!("hmr watcher unavailable, falling back to polling: {e}");
        }
        eprintln!("watching {} (HMR, event-driven)", watch_paths.len());
        hmr = Some(h);
    }

    // stdin 后台线程：逐行发主循环（`--watch` 时主循环不能被 stdin 阻塞）
    let (tx, rx) = mpsc::channel::<Option<String>>();
    thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => break,
            };
            if tx.send(Some(line)).is_err() {
                break;
            }
        }
        let _ = tx.send(None);
    });

    // 主循环：先处理 stdin 消息，再轮询 HMR（非阻塞）
    let mut eof = false;
    while !eof {
        // stdin 消息（非阻塞批量取）
        loop {
            match rx.try_recv() {
                Ok(Some(line)) => handle_line(&boot, &line),
                Ok(None) => {
                    eof = true;
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    eof = true;
                    break;
                }
            }
        }
        // HMR 轮询（M38：失败经 error sink 通知——`hmr/config-update-failed`
        // 事件 + eprintln；此处清空 take_errors 防累积，不再重复打印）
        if let Some(h) = &hmr {
            let _ = h.take_errors();
            h.poll();
        }
        // M40：驱动 timer 服务（ctx.timeout/interval/debounce/throttle）
        boot.ctx.drive_timers();
        if !eof {
            thread::sleep(Duration::from_millis(50));
        }
    }
}

/// M70：`dsh web` 子命令——服务 DeepSeek Harness 前端 + `/api` RPC，桥接运行时。
///
/// `dsh web <cordis.yml> [--web-root <dir>] [--host <h>] [--port <p>]
/// [--overlay <f>]... [--wasm-base <dir>] [--session-dir <dir>]
/// [--sqlite-store <file>] [--agent-loop] [--workspace-root <dir>]
/// [--llm-base-url <url>] [--llm-model <model>] [--env-file <f>]
/// [--dynamic-plugins-dir <dir>]`
///
/// `--web-root`：前端 dist 根目录（含 index.html）。默认依次尝试环境变量
/// `DSH_WEB_ROOT`、`D:\Program Files\DeepSeek Harness\resources\host\node_modules\@deepseek-ai\dsh-web-frontend\dist`、
/// `./web-dist`。`--port 0` = 系统分配（打印实际地址）。
fn web_main(args: &[String]) {
    let mut config_path: Option<PathBuf> = None;
    let mut web_root: Option<PathBuf> = None;
    let mut host = "127.0.0.1".to_string();
    let mut port: u16 = 0;
    let mut overlays: Vec<PathBuf> = Vec::new();
    let mut wasm_base = PathBuf::from("wasm-plugins");
    let mut session_dir: Option<PathBuf> = None;
    // M6W（D-092）：SQLite 会话存储文件（优先级高于 --session-dir）。
    let mut sqlite_store: Option<PathBuf> = None;
    // M6（step1b）：服务器执行闭环装配参数。
    let mut enable_agent_loop = false;
    let mut workspace_root: Option<PathBuf> = None;
    let mut llm_base_url: Option<String> = None;
    let mut llm_model: Option<String> = None;
    // M6（step7）：`.env` 文件（进程环境上游；键只在进程环境生效，不落 settings/git）。
    let mut env_file: Option<PathBuf> = None;
    // D-115-Web（阶段 C）：动态插件目录（`<dir>/<pluginId>/package.json + plugin.wasm`）。
    let mut dynamic_plugins_dir: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--web-root" => {
                i += 1;
                web_root = Some(PathBuf::from(&args[i]));
            }
            "--host" => {
                i += 1;
                host = args[i].clone();
            }
            "--port" => {
                i += 1;
                port = args[i].parse().unwrap_or(0);
            }
            "--overlay" | "--patch" => {
                i += 1;
                overlays.push(PathBuf::from(&args[i]));
            }
            "--wasm-base" => {
                i += 1;
                wasm_base = PathBuf::from(&args[i]);
            }
            "--session-dir" => {
                i += 1;
                session_dir = Some(PathBuf::from(&args[i]));
            }
            "--sqlite-store" => {
                i += 1;
                sqlite_store = Some(PathBuf::from(&args[i]));
            }
            "--agent-loop" => {
                enable_agent_loop = true;
            }
            "--workspace-root" => {
                i += 1;
                workspace_root = Some(PathBuf::from(&args[i]));
            }
            "--llm-base-url" => {
                i += 1;
                llm_base_url = Some(args[i].clone());
            }
            "--llm-model" => {
                i += 1;
                llm_model = Some(args[i].clone());
            }
            "--env-file" => {
                i += 1;
                env_file = Some(PathBuf::from(&args[i]));
            }
            "--dynamic-plugins-dir" => {
                i += 1;
                dynamic_plugins_dir = Some(PathBuf::from(&args[i]));
            }
            other if other.starts_with("--") => {
                eprintln!("dsh web: unknown arg {other}");
                std::process::exit(2);
            }
            other => {
                if config_path.is_none() {
                    config_path = Some(PathBuf::from(other));
                } else {
                    eprintln!("dsh web: unexpected positional {other}");
                    std::process::exit(2);
                }
            }
        }
        i += 1;
    }
    let config_path = config_path.unwrap_or_else(|| {
        eprintln!("dsh web: missing <cordis.yml>");
        std::process::exit(2);
    });

    // 前端 dist 根（默认搜索）
    let web_root = web_root.unwrap_or_else(default_web_root);

    let mut boot = match dsh_cli::boot(&config_path, &overlays, &wasm_base) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("boot failed: {e}");
            std::process::exit(1);
        }
    };
    // timer 时钟（web 循环也驱动 timer 服务）
    boot.ctx.set_timer_clock(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    });

    let plugin_roots = default_plugin_roots(&web_root);
    let cfg = dsh_cli::web::WebConfig {
        web_root,
        plugin_roots,
        host: host.clone(),
        port,
        session_dir,
        sqlite_store,
        workspace_root,
        enable_agent_loop,
        llm_base_url,
        llm_model,
        env_file,
        dynamic_plugins_dir,
        config_path,
    };
    match dsh_cli::web::serve(&mut boot, cfg) {
        Ok(server) => {
            println!("dsh web serving at {}", server.addr);
        }
        Err(e) => {
            eprintln!("web serve failed: {e}");
            std::process::exit(1);
        }
    }
}

/// 默认前端 dist 根（按优先级：env → 已安装 DeepSeek Harness → ./web-dist）。
fn default_web_root() -> PathBuf {
    if let Ok(p) = std::env::var("DSH_WEB_ROOT") {
        return PathBuf::from(p);
    }
    let installed = PathBuf::from(
        r"D:\Program Files\DeepSeek Harness\resources\host\node_modules\@deepseek-ai\dsh-web-frontend\dist",
    );
    if installed.join("index.html").exists() {
        return installed;
    }
    PathBuf::from("web-dist")
}

/// 默认 web 插件 bundle 根集（D-115-Web D1：多 root——base 层 + web-app 层）。
///
/// 优先环境变量 `DSH_PLUGIN_ROOT`（单 root，向前兼容）；否则从 web_root 向上找
/// `@deepseek-ai` 目录（web-app 层），并把它的兄弟 bundle（`…/base/node_modules/`
/// 下的 `@deepseek-ai`）作为 base 层一并加入（缺一 → 浏览器 37 pending，根因）。
/// 顺序：base 在前、web-app 在后（同名 id 后层覆盖，对齐 cordis patch）。
fn default_plugin_roots(web_root: &Path) -> Vec<PathBuf> {
    // env 覆盖（单 root 或冒号分隔多 root，兼容既有 DSH_PLUGIN_ROOT 用法）。
    if let Ok(p) = std::env::var("DSH_PLUGIN_ROOT") {
        return p
            .split(';')
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect::<Vec<_>>();
    }
    // web_root = <node_modules>/@deepseek-ai/dsh-web-frontend/dist → 找 @deepseek-ai
    let mut dir: PathBuf = web_root.to_path_buf();
    let mut web_scope: Option<PathBuf> = None;
    for _ in 0..3 {
        let parent = dir.parent();
        if let Some(p) = parent {
            let name: Option<&str> = p.file_name().and_then(|s| s.to_str());
            if name == Some("@deepseek-ai") {
                web_scope = Some(p.to_path_buf());
                break;
            }
            dir = p.to_path_buf();
        }
    }
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(web_scope) = web_scope {
        // 兄弟 base 层：base bundle 与 web-app bundle 是 `packages/bundle/` 下的并列目录
        // （web_scope 一路向上到 `…/packages/bundle`，其下 `base/node_modules/@deepseek-ai`）。
        // 从 web_scope 逐级向上找「含 base/node_modules/@deepseek-ai 的祖辈」。
        let mut probe = web_scope.parent();
        while let Some(p) = probe {
            let base_candidate = p.join("base/node_modules/@deepseek-ai");
            if base_candidate.is_dir() {
                roots.push(base_candidate);
                break;
            }
            probe = p.parent();
        }
        roots.push(web_scope);
    }
    if roots.is_empty() {
        roots.push(dir); // 兜底：找不到时沿用旧行为（web_root 本身）
    }
    roots
}

/// 处理一行 stdin（JSON turn）。
fn handle_line(boot: &dsh_cli::Boot, line: &str) {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return;
    }
    let input: serde_json::Value = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("input must be JSON: {e}");
            return;
        }
    };
    match dsh_cli::run_turn(boot, &input) {
        Ok(result) => {
            println!("{}", serde_json::to_string(&result).unwrap_or_default());
        }
        Err(e) => {
            eprintln!("turn failed: {e}");
        }
    }
}

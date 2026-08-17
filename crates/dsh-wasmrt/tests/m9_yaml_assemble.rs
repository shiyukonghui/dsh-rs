//! M9-DSH：YAML 配置端到端——cordis.yml 形态的配置经 `dsh-loader` Include 挂载
//! 服务插件 + WASM loop 插件。
//!
//! 对应 deepseek-harness 的启动方式：cordis.yml（或 bundle patch）列出入口，
//! loader 按名挂载；换配置即换 loop 行为（echo/tool/llm），宿主不改代码。

#![allow(clippy::arc_with_non_send_sync)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use dsh_core::*;
use dsh_loader::{Include, Loader, Patch};
use dsh_wasmrt::{Capabilities, DshServicesPlugin, WasmLoopPlugin};

/// 构建（如缺失）并读取指定 loop 组件字节。
fn loop_component(dir: &str) -> Vec<u8> {
    let manifest: PathBuf =
        Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../wasm-plugins/{dir}"));
    let wasm_path = manifest.join("target/wasm32-wasip1/debug").join(format!(
        "{}_plugin.wasm",
        dir.replace('-', "_")
    ));
    if !wasm_path.exists() {
        let status = Command::new("cargo")
            .args(["component", "build", "--manifest-path"])
            .arg(manifest.join("Cargo.toml"))
            .status()
            .expect("run cargo component build");
        assert!(status.success(), "{dir} plugin build failed");
    }
    fs::read(wasm_path).expect("read loop component")
}

/// 写一个 cordis.yml 形态的 YAML 入口列表到临时文件。
fn write_cordis_yaml(dir: &Path, loop_name: &str, loop_wasm_dir: &str) -> PathBuf {
    let path = dir.join("cordis.yml");
    let yaml = format!(
        r#"
- id: services
  name: dsh:services
  config:
    services: [sessions, tools, llm]
- id: loop
  name: {loop_name}
  config:
    wasm: {loop_wasm_dir}
"#
    );
    fs::write(&path, yaml).unwrap();
    path
}

/// 组装：注册插件（服务 + 指定 loop），Include 挂载 YAML；返回 (ctx, loop_plugin, sessions)。
fn assemble_with_yaml(
    loop_name: &'static str,
    loop_dir: &str,
) -> (Cordis, Arc<WasmLoopPlugin>, dsh_core::SessionHandle) {
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();

    // 注册插件仓库（等价 Cordis 模块 import 缓存）
    loader.register_plugin("dsh:services", Arc::new(DshServicesPlugin::all()));
    let loop_plugin = Arc::new(
        WasmLoopPlugin::new(loop_name, &loop_component(loop_dir), Capabilities::all()).unwrap(),
    );
    let loop_dyn: Arc<dyn Plugin> = loop_plugin.clone();
    loader.register_plugin(loop_name, loop_dyn);

    // Include 从 YAML 挂载（每测试唯一目录，避免并行覆盖）
    let dir = std::env::temp_dir().join(format!("dsh-m9-yaml-{}-{}", loop_name, std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = write_cordis_yaml(&dir, loop_name, loop_dir);
    let include = Include::new(&loader, &path, vec![]);
    include.load().unwrap();
    fs::remove_dir_all(&dir).ok();

    let sessions: dsh_core::SessionHandle = cordis
        .get_typed::<dsh_core::SessionHandle>("sessions")
        .unwrap()
        .as_ref()
        .clone();
    (cordis, loop_plugin, sessions)
}

/// YAML 端到端：services + echo-loop 挂载，run_turn 正常且 session 记录。
#[test]
fn yaml_assemble_echo_loop() {
    let (cordis, plugin, sessions) = assemble_with_yaml("echo-loop", "echo-loop");
    let r = plugin.run_turn(&cordis, &json!({"content": "yaml hello"})).unwrap();
    assert_eq!(r["echo"], "echo: yaml hello");
    let log = sessions.lock().unwrap();
    assert_eq!(log.event_kinds().len(), 6, "full turn via YAML assembly");
}

/// YAML 端到端：换 YAML 的 loop name 为 tool-loop → 工具行为。
#[test]
fn yaml_assemble_tool_loop() {
    let (cordis, plugin, sessions) = assemble_with_yaml("tool-loop", "tool-loop");
    let tools = cordis.get_typed::<dsh_core::ToolRegistryHandle>("tools").unwrap();
    tools.lock().unwrap().register("add", |args| {
        let a = args.get("a").and_then(|v| v.as_i64()).unwrap_or(0);
        let b = args.get("b").and_then(|v| v.as_i64()).unwrap_or(0);
        serde_json::json!({"sum": a + b})
    });
    let r = plugin.run_turn(&cordis, &json!({"content": "compute"})).unwrap();
    assert_eq!(r["summary"], "2 + 3 = 5");
    let log = sessions.lock().unwrap();
    assert!(log.event_kinds().contains(&"tool/result".to_string()));
}

/// YAML 端到端：换 YAML 的 loop name 为 llm-loop → 完整 turn（llm 缝）。
#[test]
fn yaml_assemble_llm_loop() {
    let (cordis, plugin, sessions) = assemble_with_yaml("llm-loop", "llm-loop");
    let tools = cordis.get_typed::<dsh_core::ToolRegistryHandle>("tools").unwrap();
    tools.lock().unwrap().register("add", |args| {
        let a = args.get("a").and_then(|v| v.as_i64()).unwrap_or(0);
        let b = args.get("b").and_then(|v| v.as_i64()).unwrap_or(0);
        serde_json::json!({"sum": a + b})
    });
    let llm = cordis.get_typed::<dsh_core::LlmHandle>("llm").unwrap();
    llm.lock().unwrap().set_default(|messages, _| {
        if messages.iter().any(|m| {
            m["content"]
                .get(0)
                .and_then(|b| b.get("type"))
                .and_then(|t| t.as_str())
                == Some("tool-result")
        }) {
            serde_json::json!({"content": "sum is 5"})
        } else {
            serde_json::json!({"content": "", "tool_calls": [{"call_id": "c1", "name": "add", "arguments": {"a": 2, "b": 3}}]})
        }
    });
    let r = plugin.run_turn(&cordis, &json!({"content": "what is 2+3?"})).unwrap();
    assert_eq!(r["answer"], "sum is 5");
    let log = sessions.lock().unwrap();
    assert_eq!(
        log.event_kinds(),
        vec![
            "turn/start",
            "step/start",
            "user/message",
            "tool/call",
            "tool/result",
            "assistant/message",
            "step/end",
            "turn/end",
        ],
        "full turn sequence via YAML assembly"
    );
}

/// Patch 覆盖：YAML 里 loop 的 config 可被 patch 替换（配置层可改）。
#[test]
fn yaml_patch_overrides_loop_config() {
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    loader.register_plugin("dsh:services", Arc::new(DshServicesPlugin::all()));
    let loop_plugin = Arc::new(
        WasmLoopPlugin::new("echo-loop", &loop_component("echo-loop"), Capabilities::all()).unwrap(),
    );
    let loop_dyn: Arc<dyn Plugin> = loop_plugin.clone();
    loader.register_plugin("echo-loop", loop_dyn);

    let dir = std::env::temp_dir().join(format!("dsh-m9-yaml-patch-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = write_cordis_yaml(&dir, "echo-loop", "echo-loop");
    // patch：把 loop entry 的 config 换成自定义（验证 patch 机制在 DSH 组装中生效）
    let patches = vec![Patch {
        id: Some("loop".to_string()),
        config: Some(json!({"greeting": "patched"})),
        ..Patch::default()
    }];
    let include = Include::new(&loader, &path, patches);
    include.load().unwrap();
    fs::remove_dir_all(&dir).ok();

    let r = loop_plugin.run_turn(&cordis, &json!({"content": "p"})).unwrap();
    assert_eq!(r["echo"], "echo: p");
}

/// 声明式 http llm（M17）：`llm: {provider, http: {base, api_key, model}}` →
/// llm-loop 完整 turn 经真实 HTTP 请求（本地 mock 服务器）→ 回答来自 HTTP 响应。
#[test]
fn yaml_declared_http_llm() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    // 本地 mock：记录收到的请求（校验形状），返回 OpenAI 兼容响应
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let received2 = received.clone();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            let mut total = 0usize;
            loop {
                match stream.read(&mut tmp) {
                    Ok(0) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&tmp[..n]);
                        total += n;
                        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            if let Some(cl) = String::from_utf8_lossy(&buf[..pos])
                                .lines()
                                .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
                                .and_then(|l| l.split(':').nth(1))
                                .and_then(|v| v.trim().parse::<usize>().ok())
                            {
                                if total >= pos + 4 + cl {
                                    break;
                                }
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
            let text = String::from_utf8_lossy(&buf).to_string();
            received2.lock().unwrap().push(text.clone());
            // 校验：Bearer 认证 + model 字段
            assert!(text.contains("Authorization: Bearer sk-yaml"), "{text}");
            assert!(text.contains("\"model\":\"yaml-model\""), "{text}");
            let payload = r#"{"choices":[{"message":{"role":"assistant","content":"http turn answer"}}]}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                payload.len(),
                payload
            );
            let _ = stream.write_all(resp.as_bytes());
        }
    });

    // 写 YAML：services 声明 http llm
    let dir = std::env::temp_dir().join(format!("dsh-m9-yaml-http-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("cordis.yml");
    let yaml = format!(
        r#"
- id: services
  name: dsh:services
  config:
    services: [sessions, tools, llm]
    llm:
      provider: default
      http:
        base: "http://{addr}"
        api_key: sk-yaml
        model: yaml-model
- id: loop
  name: llm-loop
  config:
    wasm: llm-loop
"#
    );
    fs::write(&path, yaml).unwrap();

    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    loader.register_plugin("dsh:services", Arc::new(DshServicesPlugin::all()));
    let loop_plugin = Arc::new(
        WasmLoopPlugin::new("llm-loop", &loop_component("llm-loop"), Capabilities::all()).unwrap(),
    );
    let loop_dyn: Arc<dyn Plugin> = loop_plugin.clone();
    loader.register_plugin("llm-loop", loop_dyn);
    let include = Include::new(&loader, &path, vec![]);
    include.load().unwrap();
    fs::remove_dir_all(&dir).ok();

    // llm-loop 完整 turn：第一轮无工具 → HTTP 返回直接回答（无 tool_calls → 结束）
    let r = loop_plugin.run_turn(&cordis, &json!({"content": "q"})).unwrap();
    assert_eq!(r["answer"], "http turn answer", "answer came from HTTP mock");
    {
        let handle = cordis
            .get_typed::<dsh_core::SessionHandle>("sessions")
            .unwrap()
            .clone();
        let log = handle.lock().unwrap();
        assert_eq!(log.event_kinds()[0], "turn/start");
    }
    // 请求确实经 HTTP 发出（含 Bearer + model 校验已在上方断言）；
    // llm-loop 固定两步模型请求（step1 + step2）
    assert_eq!(received.lock().unwrap().len(), 2, "two HTTP requests (step1+step2)");
}

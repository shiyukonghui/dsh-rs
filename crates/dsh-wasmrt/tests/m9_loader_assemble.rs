//! M9：DSH 层**配置化组装**——经 `dsh-loader` 从配置挂载服务插件 + WASM loop 插件。
//!
//! 对应 deepseek-harness 的 cordis.yml 组装（agent-loop 行 + services 行）：
//! - `dsh:services` 插件提供 session/tools/llm 服务（缝的承载）；
//! - WASM loop 插件（echo-loop/tool-loop/llm-loop）驱动 turn（缝的消费）。
//!
//! 验证「loop 可替换」的**配置级形态**：换 entry 的 `name`（指向不同 loop 插件）
//! 即换 loop 行为——宿主只按配置组装，不改代码。

#![allow(clippy::arc_with_non_send_sync)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use dsh_core::*;
use dsh_loader::{EntryOptions, Loader};
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

/// 组装：loader 挂载服务插件 + 指定 loop 插件；返回 (ctx, loop_plugin, sessions)。
fn assemble(
    loop_name: &'static str,
    loop_dir: &str,
    services_config: Value,
) -> (Cordis, Arc<WasmLoopPlugin>, dsh_core::SessionHandle) {
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();

    // 服务插件：提供 sessions/tools/llm（config 决定注册哪些）
    let services = Arc::new(DshServicesPlugin::all());
    loader.register_plugin("dsh:services", services);
    loader
        .create(EntryOptions {
            id: "services".to_string(),
            name: "dsh:services".to_string(),
            config: services_config,
            ..EntryOptions::new("services", "dsh:services")
        })
        .unwrap();

    // WASM loop 插件：驱动 turn
    let loop_plugin = Arc::new(
        WasmLoopPlugin::new(loop_name, &loop_component(loop_dir), Capabilities::all()).unwrap(),
    );
    let loop_dyn: Arc<dyn Plugin> = loop_plugin.clone();
    loader.register_plugin(loop_name, loop_dyn);
    loader
        .create(EntryOptions {
            id: "loop".to_string(),
            name: loop_name.to_string(),
            config: json!({}),
            ..EntryOptions::new("loop", loop_name)
        })
        .unwrap();

    let sessions: dsh_core::SessionHandle = cordis
        .get_typed::<dsh_core::SessionHandle>("sessions")
        .unwrap()
        .as_ref()
        .clone();
    (cordis, loop_plugin, sessions)
}

/// 配置级组装：services entry + echo-loop entry，run_turn 正常且 session 记录。
#[test]
fn loader_assemble_echo_loop() {
    let (cordis, plugin, sessions) = assemble("echo-loop", "echo-loop", json!({}));
    let r = plugin.run_turn(&cordis, &json!({"content": "config hello"})).unwrap();
    assert_eq!(r["echo"], "echo: config hello");
    let log = sessions.lock().unwrap();
    assert_eq!(log.event_kinds().len(), 6, "full turn events in sessions service");
}

/// 换 loop 配置即换行为：tool-loop 经 tools 缝调宿主 add 工具。
#[test]
fn loader_assemble_tool_loop() {
    let (cordis, plugin, sessions) = assemble("tool-loop", "tool-loop", json!({}));
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

/// 换 loop 配置即换行为：llm-loop 经 llm 缝驱动完整 turn。
#[test]
fn loader_assemble_llm_loop() {
    let (cordis, plugin, sessions) = assemble("llm-loop", "llm-loop", json!({}));
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
        "full turn sequence via config-driven assembly"
    );
}

/// 服务子集配置：只注册 sessions（无 tools/llm）→ loop 仍可跑（缝回退）。
#[test]
fn loader_assemble_services_subset() {
    let (cordis, plugin, sessions) = assemble(
        "echo-loop",
        "echo-loop",
        json!({"services": ["sessions"]}),
    );
    let r = plugin.run_turn(&cordis, &json!({"content": "subset"})).unwrap();
    assert_eq!(r["echo"], "echo: subset");
    let log = sessions.lock().unwrap();
    assert_eq!(log.event_kinds().len(), 6);
}

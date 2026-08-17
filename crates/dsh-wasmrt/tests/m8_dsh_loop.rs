//! M8-DSH：WASM 组件作为 agent-loop **插件**挂进 Cordis——「loop 本身可替换」闭环。
//!
//! 架构（第一性原理）：
//! - **缝** = WIT（`dsh-loop.wit`）：session/tools/llm/agent-loop；
//! - **loop** = WASM 插件（echo-loop 组件），实现 `agent-loop` 缝；
//! - **缝的承载** = 宿主 Host 实现（`WasmLoopPlugin` 内），不是 native 参考插件。
//!
//! 本测试验证完整闭环：`WasmLoopPlugin` 经 `plugin_arc` 挂进 Cordis（享受 fiber
//! 生命周期与卸载回滚），宿主调用 `run_turn` 驱动 WASM 内的 loop；session 事件
//! 由宿主缝记录并断言。

#![allow(clippy::arc_with_non_send_sync)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use dsh_core::*;
use dsh_wasmrt::{Capabilities, WasmLoopPlugin};

/// 闭包插件（宿主侧服务注册用）。
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
        FnPlugin {
            name,
            body: Box::new(body),
        }
    }
}

impl Plugin for FnPlugin {
    fn name(&self) -> &'static str {
        self.name
    }
    fn apply(&self, ctx: &Cordis, config: Value) -> Result<EffectOutcome, CordisError> {
        (self.body)(ctx, config)
    }
}

/// 构建（如缺失）并读取 echo-loop 组件字节。
fn echo_loop_component() -> Vec<u8> {
    let manifest: PathBuf =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../wasm-plugins/echo-loop");
    let wasm_path = manifest.join("target/wasm32-wasip1/debug/echo_loop_plugin.wasm");
    if !wasm_path.exists() {
        let status = Command::new("cargo")
            .args(["component", "build", "--manifest-path"])
            .arg(manifest.join("Cargo.toml"))
            .status()
            .expect("run cargo component build for echo-loop plugin");
        assert!(status.success(), "echo-loop plugin build failed");
    }
    fs::read(wasm_path).expect("read echo-loop component")
}

/// 构建（如缺失）并读取 tool-loop 组件字节。
fn tool_loop_component() -> Vec<u8> {
    let manifest: PathBuf =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../wasm-plugins/tool-loop");
    let wasm_path = manifest.join("target/wasm32-wasip1/debug/tool_loop_plugin.wasm");
    if !wasm_path.exists() {
        let status = Command::new("cargo")
            .args(["component", "build", "--manifest-path"])
            .arg(manifest.join("Cargo.toml"))
            .status()
            .expect("run cargo component build for tool-loop plugin");
        assert!(status.success(), "tool-loop plugin build failed");
    }
    fs::read(wasm_path).expect("read tool-loop component")
}

/// 构建（如缺失）并读取 llm-loop 组件字节。
fn llm_loop_component() -> Vec<u8> {
    let manifest: PathBuf =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../wasm-plugins/llm-loop");
    let wasm_path = manifest.join("target/wasm32-wasip1/debug/llm_loop_plugin.wasm");
    if !wasm_path.exists() {
        let status = Command::new("cargo")
            .args(["component", "build", "--manifest-path"])
            .arg(manifest.join("Cargo.toml"))
            .status()
            .expect("run cargo component build for llm-loop plugin");
        assert!(status.success(), "llm-loop plugin build failed");
    }
    fs::read(wasm_path).expect("read llm-loop component")
}

/// WASM echo-loop 经 Plugin trait 挂进 Cordis：run_turn 由 WASM 驱动，
/// session 事件由宿主缝记录。
#[test]
fn wasm_loop_mounts_as_plugin_and_runs_turn() {
    let cordis = Cordis::new();
    let plugin = Arc::new(
        WasmLoopPlugin::new("echo-loop", &echo_loop_component(), Capabilities::all()).unwrap(),
    );

    // 经 plugin_arc 挂载（fiber 生命周期；apply 内懒实例化组件）
    let fid = cordis.plugin_arc(plugin.clone(), json!({})).unwrap();
    assert_eq!(cordis.fiber_state(fid), Some(FiberState::Active));

    // 宿主驱动一个 turn（loop 逻辑在 WASM 插件内）
    let result = plugin
        .run_turn(&cordis, &json!({"content": "hello from host"}))
        .unwrap();
    assert_eq!(result["reason"], "completed");
    assert_eq!(result["echo"], "echo: hello from host");

    // session 缝被 WASM loop 写入完整 turn/step 事件序列
    let kinds = plugin.event_kinds();
    assert_eq!(
        kinds,
        vec![
            "turn/start",
            "step/start",
            "user/message",
            "assistant/message",
            "step/end",
            "turn/end",
        ],
        "session events written by wasm loop"
    );

    // 宿主缝投影模型历史（WASM loop 的输出可被宿主消费；M34 生产 Message 形状）
    let messages = plugin.derive_messages();
    assert_eq!(
        messages,
        vec![
            serde_json::json!({
                "id": "u1", "role": "user",
                "content": [{"type": "text", "text": "hello from host"}],
                "source": {"kind": "user"},
            }),
            serde_json::json!({
                "id": "a1", "role": "assistant",
                "content": [{"type": "text", "text": "echo: hello from host"}],
                "source": {"kind": "model", "provider": "mock", "model": "mock"},
            }),
        ],
        "model history projected from wasm loop session (production Message shape)"
    );

    // 卸载：fiber 生命周期 + 实例清理
    cordis.unload(fid).unwrap();
    assert_eq!(cordis.fiber_state(fid), Some(FiberState::Disposed));
}

/// 两次 run_turn：每次驱动独立完成 turn（session 事件追加）。
#[test]
fn wasm_loop_runs_multiple_turns() {
    let cordis = Cordis::new();
    let plugin = Arc::new(
        WasmLoopPlugin::new("echo-loop", &echo_loop_component(), Capabilities::all()).unwrap(),
    );
    let _fid = cordis.plugin_arc(plugin.clone(), json!({})).unwrap();

    let r1 = plugin.run_turn(&cordis, &json!({"content": "first"})).unwrap();
    assert_eq!(r1["echo"], "echo: first");
    let r2 = plugin.run_turn(&cordis, &json!({"content": "second"})).unwrap();
    assert_eq!(r2["echo"], "echo: second");

    // 两轮事件累计（各 6 条）
    let kinds = plugin.event_kinds();
    assert_eq!(kinds.len(), 12);
    assert_eq!(kinds[0], "turn/start");
    assert_eq!(kinds[6], "turn/start");
}

/// tool-loop：WASM loop 调用 tools 缝（宿主 add 工具）→ 结果回 session →
/// 模型历史含 tool 消息——tools 缝双向桥接验证。
#[test]
fn wasm_loop_calls_host_tool() {
    let cordis = Cordis::new();
    let plugin = Arc::new(
        WasmLoopPlugin::new("tool-loop", &tool_loop_component(), Capabilities::all()).unwrap(),
    );
    let _fid = cordis.plugin_arc(plugin.clone(), json!({})).unwrap();

    let result = plugin.run_turn(&cordis, &json!({"content": "compute 2+3"})).unwrap();
    assert_eq!(result["reason"], "completed");
    assert_eq!(result["summary"], "2 + 3 = 5");

    // session 事件含 tool/call + tool/result（WASM loop 经 tools 缝编排）
    let kinds = plugin.event_kinds();
    assert!(kinds.contains(&"tool/call".to_string()), "kinds={kinds:?}");
    assert!(kinds.contains(&"tool/result".to_string()), "kinds={kinds:?}");

    // 模型历史投影含 tool 消息（工具结果可被宿主消费；M34 ToolResultMessage
    // 形状：role=user + content[0].type == "tool-result"）
    let messages = plugin.derive_messages();
    assert!(
        messages.iter().any(|m| {
            m["content"]
                .get(0)
                .and_then(|b| b.get("type"))
                .and_then(|t| t.as_str())
                == Some("tool-result")
        }),
        "tool result in model history: {messages:?}"
    );
}

/// 缝的承载实质化：宿主 provide `sessions`/`tools` 服务（含自定义工具），
/// WASM loop 经缝的写入/调用**落入 Cordis 服务**——宿主经 `ctx` 读取。
#[test]
fn wasm_loop_seam_bridges_cordis_services() {
    let cordis = Cordis::new();
    let sessions = dsh_core::new_session();
    let tools = dsh_core::new_tool_registry();

    // 宿主注册 sessions/tools 服务 + 自定义工具 multiply（非 add）
    {
        let session_handle = sessions.clone();
        let host = FnPlugin::new("host-services", move |ctx, _cfg| {
            ctx.provide("sessions", std::sync::Arc::new(session_handle.clone()))?;
            Ok(EffectOutcome::None)
        });
        cordis.plugin(host, json!({})).unwrap();
    }
    {
        let tools_handle = tools.clone();
        let host = FnPlugin::new("host-tools", move |ctx, _cfg| {
            ctx.provide("tools", std::sync::Arc::new(tools_handle.clone()))?;
            Ok(EffectOutcome::None)
        });
        cordis.plugin(host, json!({})).unwrap();
    }
    tools
        .lock()
        .unwrap()
        .register("multiply", |args| {
            let a = args.get("a").and_then(|v| v.as_i64()).unwrap_or(0);
            let b = args.get("b").and_then(|v| v.as_i64()).unwrap_or(0);
            serde_json::json!({"product": a * b})
        });
    // 宿主覆盖 add：桥接后 WASM loop 调 add 应得宿主实现（而非内存回退）
    tools.lock().unwrap().register("add", |args| {
        let a = args.get("a").and_then(|v| v.as_i64()).unwrap_or(0);
        let b = args.get("b").and_then(|v| v.as_i64()).unwrap_or(0);
        serde_json::json!({"sum": a + b + 100})
    });

    // WASM loop 挂进 Cordis（tool-loop 调 tools 缝的 "add"——服务里未注册，
    // 但桥接优先服务，add 未注册 → 错误；改调 multiply 才能走服务）。
    // 为验证「WASM loop 经桥接调用宿主注册的工具」，用 echo-loop 的 run_turn
    // 不会调工具；这里直接断言桥接方向：向 sessions 服务写入可经 ctx 读取。
    let plugin = Arc::new(
        WasmLoopPlugin::new("echo-loop", &echo_loop_component(), Capabilities::all()).unwrap(),
    );
    let _fid = cordis.plugin_arc(plugin.clone(), json!({})).unwrap();

    let result = plugin.run_turn(&cordis, &json!({"content": "bridge test"})).unwrap();
    assert_eq!(result["echo"], "echo: bridge test");

    // WASM loop 的 session 输出经桥接落入 Cordis `sessions` 服务
    let log = sessions.lock().unwrap();
    assert_eq!(
        log.event_kinds(),
        vec![
            "turn/start",
            "step/start",
            "user/message",
            "assistant/message",
            "step/end",
            "turn/end",
        ],
        "wasm loop session events landed in Cordis sessions service"
    );
    assert_eq!(
        log.derive_messages(),
        vec![
            serde_json::json!({
                "id": "u1", "role": "user",
                "content": [{"type": "text", "text": "bridge test"}],
                "source": {"kind": "user"},
            }),
            serde_json::json!({
                "id": "a1", "role": "assistant",
                "content": [{"type": "text", "text": "echo: bridge test"}],
                "source": {"kind": "model", "provider": "mock", "model": "mock"},
            }),
        ],
        "model history readable via Cordis sessions service (production Message shape)"
    );
    drop(log);

    // tools 缝桥接：WASM tool-loop 调 add → 宿主注册的实现（sum = a+b+100）
    let plugin2 = Arc::new(
        WasmLoopPlugin::new("tool-loop", &tool_loop_component(), Capabilities::all()).unwrap(),
    );
    let _fid2 = cordis.plugin_arc(plugin2.clone(), json!({})).unwrap();
    let r2 = plugin2.run_turn(&cordis, &json!({"content": "compute"})).unwrap();
    // 宿主 add 实现返回 2+3+100=105 → WASM loop 的 summary 引用它
    assert_eq!(r2["summary"], "2 + 3 = 105", "tool result from host-registered add");
    // tool/result 也落入 Cordis sessions 服务
    let log2 = sessions.lock().unwrap();
    assert!(
        log2.event_kinds().contains(&"tool/result".to_string()),
        "tool result landed in Cordis sessions service"
    );
    assert!(
        log2
            .derive_messages()
            .iter()
            .any(|m| m["content"]
                .get(0)
                .and_then(|b| b.get("type"))
                .and_then(|t| t.as_str())
                == Some("tool-result")),
        "tool result (105) in model history via Cordis service"
    );
}

/// 完整 turn 流：WASM llm-loop 经 llm 缝驱动「模型→工具→模型→收尾」，
/// 三个缝（session/tools/llm）都桥接 Cordis 服务——全链路可配置替换。
#[test]
fn wasm_loop_full_turn_with_llm() {
    let cordis = Cordis::new();
    let sessions = dsh_core::new_session();
    let tools = dsh_core::new_tool_registry();
    let llm = dsh_core::new_llm();

    // 宿主注册三个服务
    {
        let h = sessions.clone();
        cordis
            .plugin(FnPlugin::new("svc-sessions", move |ctx, _| {
                ctx.provide("sessions", std::sync::Arc::new(h.clone()))?;
                Ok(EffectOutcome::None)
            }), json!({}))
            .unwrap();
    }
    {
        let h = tools.clone();
        cordis
            .plugin(FnPlugin::new("svc-tools", move |ctx, _| {
                ctx.provide("tools", std::sync::Arc::new(h.clone()))?;
                Ok(EffectOutcome::None)
            }), json!({}))
            .unwrap();
    }
    {
        let h = llm.clone();
        cordis
            .plugin(FnPlugin::new("svc-llm", move |ctx, _| {
                ctx.provide("llm", std::sync::Arc::new(h.clone()))?;
                Ok(EffectOutcome::None)
            }), json!({}))
            .unwrap();
    }

    // 宿主工具：add
    tools.lock().unwrap().register("add", |args| {
        let a = args.get("a").and_then(|v| v.as_i64()).unwrap_or(0);
        let b = args.get("b").and_then(|v| v.as_i64()).unwrap_or(0);
        serde_json::json!({"sum": a + b})
    });

    // 宿主 LLM 适配器：按消息数决定——首轮返回工具调用 add，末轮返回最终回答
    // （M34：消息为生产 Message[] 形状——tool 结果判别 content[0].type）
    {
        let mut svc = llm.lock().unwrap();
        svc.set_default(|messages, _tools| {
            let has_tool_result = messages.iter().any(|m| {
                m["content"]
                    .get(0)
                    .and_then(|b| b.get("type"))
                    .and_then(|t| t.as_str())
                    == Some("tool-result")
            });
            if has_tool_result {
                serde_json::json!({"content": "sum is 5"})
            } else {
                serde_json::json!({
                    "content": "",
                    "tool_calls": [{
                        "call_id": "c1",
                        "name": "add",
                        "arguments": {"a": 2, "b": 3},
                    }],
                })
            }
        });
    }

    // WASM llm-loop 挂进 Cordis
    let plugin = Arc::new(
        WasmLoopPlugin::new("llm-loop", &llm_loop_component(), Capabilities::all()).unwrap(),
    );
    let _fid = cordis.plugin_arc(plugin.clone(), json!({})).unwrap();

    let result = plugin
        .run_turn(&cordis, &json!({"content": "what is 2+3?"}))
        .unwrap();
    assert_eq!(result["reason"], "completed");
    assert_eq!(result["answer"], "sum is 5");

    // session 服务含完整 turn 事件序列（user → tool/call → tool/result → assistant）
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
        "full turn event sequence in Cordis sessions"
    );
    // 模型历史完整：user → tool-result → assistant（工具结果参与模型上下文；
    // M34 生产 Message 形状）
    let messages = log.derive_messages();
    assert_eq!(
        messages,
        vec![
            serde_json::json!({
                "id": "u1", "role": "user",
                "content": [{"type": "text", "text": "what is 2+3?"}],
                "source": {"kind": "user"},
            }),
            serde_json::json!({
                "id": "t1", "role": "user",
                "content": [{
                    "type": "tool-result",
                    "toolCallId": "c1",
                    "content": [{"type": "text", "text": "{\"sum\":5}"}],
                    "isError": false,
                }],
                "source": {"kind": "tool", "callId": "c1"},
            }),
            serde_json::json!({
                "id": "a1", "role": "assistant",
                "content": [{"type": "text", "text": "sum is 5"}],
                "source": {"kind": "model", "provider": "mock", "model": "mock"},
            }),
        ],
        "model history: user + tool result + final answer (production Message shape)"
    );
}

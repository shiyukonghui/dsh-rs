//! M8：WASM 组件插件（组件模型路径）——注册服务 / 事件双向 / 卸载回滚 / 能力授予。
//! 与 M6 C ABI 路径验收等价，但经 `wasmtime::component` 加载。

#![allow(clippy::arc_with_non_send_sync)]

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::Arc;

use dsh_core::*;
use dsh_wasmrt::{load_wasm_component_plugin, Capabilities};

/// 闭包插件（宿主侧监听注册用）。
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

/// 构建（如缺失）并读取 hello 组件插件字节（cargo-component → wasip1 component）。
fn hello_component() -> Vec<u8> {
    let manifest: PathBuf =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../wasm-plugins/hello-component");
    let wasm_path = manifest.join("target/wasm32-wasip1/debug/hello_component_plugin.wasm");
    if !wasm_path.exists() {
        let status = Command::new("cargo")
            .args(["component", "build", "--manifest-path"])
            .arg(manifest.join("Cargo.toml"))
            .status()
            .expect("run cargo component build for hello component plugin");
        assert!(status.success(), "hello component plugin build failed");
    }
    fs::read(wasm_path).expect("read hello component")
}

fn mount_hello(
    cordis: &Cordis,
    caps: Capabilities,
    config: Value,
) -> Result<FiberId, CordisError> {
    let plugin = load_wasm_component_plugin("hello-component", &hello_component(), caps)?;
    cordis.plugin_arc(plugin, config)
}

/// 组件插件注册服务 → `get_value` 可读。
#[test]
fn component_provides_service() {
    let cordis = Cordis::new();
    let plugin = Arc::new(
        dsh_wasmrt::WasmComponentPlugin::new("hello-component", &hello_component(), Capabilities::all())
            .unwrap(),
    );
    // 走 plugin_arc 全流程（apply_body 会 push current fiber）
    let fid = cordis.plugin_arc(plugin.clone(), json!({"greeting": "hi from test"})).unwrap();
    let greeting = cordis.get_value("greeting");
    eprintln!("wasm logs: {:?}", plugin.logs());
    assert_eq!(
        greeting,
        Some(json!({"text": "hi from test"})),
        "service value from wasm component (logs: {:?})",
        plugin.logs()
    );
    let _ = fid;
}

/// 事件双向：`emit("ping")` → wasm `handle_event` → `host_emit(pong)` → 宿主监听收到。
#[test]
fn component_event_roundtrip() {
    let log = Rc::new(RefCell::new(Vec::<String>::new()));
    let log2 = log.clone();
    let cordis = Cordis::new();
    let _fid = mount_hello(&cordis, Capabilities::all(), json!({})).unwrap();

    // 宿主 fiber 内注册 "wasm" 监听（接收 wasm host_emit 的 pong）
    let host = FnPlugin::new("host", move |ctx, _cfg| {
        let l = log2.clone();
        ctx.on(
            "wasm",
            Arc::new(move |_ctx, args, _next| {
                l.borrow_mut()
                    .push(format!("pong:{}", args.first().unwrap_or(&Value::Null)));
                HookResult::Continue
            }),
        )
        .unwrap();
        Ok(EffectOutcome::None)
    });
    cordis.plugin(host, json!({})).unwrap();

    cordis.emit("ping", vec![json!({"n": 1})]);

    let captured = log.borrow().clone();
    assert!(
        captured.iter().any(|l| l.contains("pong") && l.contains("wasm-component")),
        "host received wasm emit, got: {captured:?}"
    );
}

/// 卸载回滚：服务消失，插件 `dispose` 被调用（host_log 可证）。
#[test]
fn component_unload_rolls_back() {
    let cordis = Cordis::new();
    let fid = mount_hello(&cordis, Capabilities::all(), json!({})).unwrap();
    assert!(cordis.get_value("greeting").is_some());

    cordis.unload(fid).unwrap();
    assert!(
        cordis.get_value("greeting").is_none(),
        "service removed after unload"
    );
}

/// 能力拒绝：禁用 provide → apply 失败 → fiber FAILED。
#[test]
fn component_capability_denied() {
    let cordis = Cordis::new();
    // 只给 EMIT|GET，不给 PROVIDE
    let caps = Capabilities::new(dsh_wasmrt::CAPS_EMIT | dsh_wasmrt::CAPS_GET);
    let result = mount_hello(&cordis, caps, json!({}));
    if let Ok(fid) = result {
        // 插件 apply 返回 -1 → fiber FAILED
        assert_eq!(cordis.fiber_state(fid), Some(FiberState::Failed));
    }
}

//! M6：WASM 插件 —— 注册服务 / 事件双向 / 卸载回滚 / 能力授予。
#![allow(clippy::arc_with_non_send_sync)]

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::Arc;

use dsh_core::*;
use dsh_wasmrt::{Capabilities, PluginHost, WasmPlugin};

/// 构建（如缺失）并读取 hello wasm 插件字节（wasm32-unknown-unknown，纯 env ABI）。
fn hello_wasm() -> Vec<u8> {
    let manifest: PathBuf =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../wasm-plugins/hello");
    let wasm_path = manifest.join("target/wasm32-unknown-unknown/release/hello_wasm_plugin.wasm");
    if !wasm_path.exists() {
        let status = Command::new("cargo")
            .args(["build", "--target", "wasm32-unknown-unknown", "--release"])
            .current_dir(&manifest)
            .status()
            .expect("run cargo build for hello wasm plugin");
        assert!(status.success(), "hello wasm plugin build failed");
    }
    fs::read(wasm_path).expect("read hello wasm")
}

fn mount_hello(
    cordis: &Cordis,
    caps: Capabilities,
    config: Value,
) -> (FiberId, Arc<WasmPlugin>) {
    let plugin = Arc::new(WasmPlugin::new("hello", &hello_wasm(), caps).unwrap());
    let fid = cordis.plugin_arc(plugin.clone(), config).unwrap();
    (fid, plugin)
}

struct HostPlugin {
    log: Rc<RefCell<Vec<String>>>,
}

impl Plugin for HostPlugin {
    fn name(&self) -> &'static str {
        "host"
    }

    fn apply(&self, ctx: &Cordis, _config: Value) -> Result<EffectOutcome, CordisError> {
        let log = self.log.clone();
        ctx.on(
            "wasm",
            Arc::new(move |_ctx, args, _next| {
                let v = args.first().cloned().unwrap_or(Value::Null);
                log.borrow_mut().push(v.to_string());
                HookResult::Continue
            }),
        )?;
        Ok(EffectOutcome::None)
    }
}

/// M2 验收 1：wasm 插件注册服务 → 宿主可读。
#[test]
fn wasm_plugin_provides_service() {
    let cordis = Cordis::new();
    let (fid, _) = mount_hello(&cordis, Capabilities::all(), json!({"greeting": "hi wasm"}));
    assert_eq!(cordis.fiber_state(fid), Some(FiberState::Active));

    let v = cordis.get_value("greeting").expect("greeting present");
    assert_eq!(v, json!({"text": "hi wasm"}));
}

/// M2 验收 2：事件双向 —— emit ping → wasm handle_event → host_emit pong → 宿主监听。
#[test]
fn wasm_event_roundtrip() {
    let log = Rc::new(RefCell::new(Vec::new()));
    let cordis = Cordis::new();
    let (fid, wasm) = mount_hello(&cordis, Capabilities::all(), json!({"greeting": "hi"}));
    let _ = cordis.plugin(HostPlugin { log: log.clone() }, json!({}));
    assert_eq!(cordis.fiber_state(fid), Some(FiberState::Active));

    cordis.emit("ping", vec![json!({"n": 1})]);

    // 宿主收到 pong（wasm handle_event 中 host_emit 发出）
    assert_eq!(log.borrow().len(), 1, "host listener should receive pong");
    let received = log.borrow()[0].clone();
    assert!(received.contains("wasm"), "{received}");
    assert!(received.contains("echo"), "{received}");

    // wasm 侧日志：收到 ping + 回读 greeting 成功
    let logs = wasm.logs();
    assert!(logs.iter().any(|l| l.contains("handle_event name=ping")), "{logs:?}");
    assert!(logs.iter().any(|l| l.contains("read greeting=")), "{logs:?}");
}

/// M2 验收 3：卸载 → 服务与监听回滚 + wasm dispose 被调用。
#[test]
fn wasm_unload_rolls_back() {
    let cordis = Cordis::new();
    let (fid, wasm) = mount_hello(&cordis, Capabilities::all(), json!({}));
    assert!(cordis.get_value("greeting").is_some());

    cordis.unload(fid).unwrap();

    // 服务随 fiber 卸载回滚
    assert!(cordis.get_value("greeting").is_none());
    // 监听随 fiber 卸载回滚：再 emit ping 不再触发 wasm（无 pong）
    let log = Rc::new(RefCell::new(Vec::new()));
    let _ = cordis.plugin(HostPlugin { log: log.clone() }, json!({}));
    cordis.emit("ping", vec![]);
    assert!(log.borrow().is_empty());

    // wasm dispose 被调用（host_log 记录）
    let logs = wasm.logs();
    assert!(logs.iter().any(|l| l.contains("wasm dispose")), "{logs:?}");
}

/// M3 验收：能力授予 —— 禁用 provide 后插件调用被拒，apply 失败。
#[test]
fn wasm_capability_denies_provide() {
    let cordis = Cordis::new();
    let plugin = Arc::new(WasmPlugin::new("hello", &hello_wasm(), Capabilities::new(0)).unwrap());
    let fid = cordis.plugin_arc(plugin.clone(), json!({})).unwrap();

    // plugin_apply 返回 -1 → fiber FAILED
    assert_eq!(cordis.fiber_state(fid), Some(FiberState::Failed));
    let err = cordis.fiber_error(fid).expect("error set");
    assert!(err.to_string().contains("apply failed"), "{err}");

    // host 侧记录能力拒绝
    let logs = plugin.logs();
    assert!(logs.iter().any(|l| l.contains("host_provide denied")), "{logs:?}");
}

/// PluginHost 统一加载：native 与 wasm 同一入口。
#[test]
fn plugin_host_loads_both_kinds() {
    let host = dsh_wasmrt::NativeHost;
    let cordis = Cordis::new();

    let native: Arc<dyn Plugin> = Arc::new(HostPlugin {
        log: Rc::new(RefCell::new(Vec::new())),
    });
    let manifest = dsh_wasmrt::PluginManifest {
        name: "host",
        kind: dsh_wasmrt::PluginKind::Native(native.clone()),
        caps: Capabilities::all(),
    };
    let loaded = host.load(&manifest).unwrap();
    let fid = cordis.plugin_arc(loaded, json!({})).unwrap();
    assert_eq!(cordis.fiber_state(fid), Some(FiberState::Active));

    let manifest2 = dsh_wasmrt::PluginManifest {
        name: "hello",
        kind: dsh_wasmrt::PluginKind::WasmBytes(hello_wasm()),
        caps: Capabilities::all(),
    };
    let loaded2 = host.load(&manifest2).unwrap();
    let fid2 = cordis.plugin_arc(loaded2, json!({})).unwrap();
    assert_eq!(cordis.fiber_state(fid2), Some(FiberState::Active));
    assert!(cordis.get_value("greeting").is_some());
}

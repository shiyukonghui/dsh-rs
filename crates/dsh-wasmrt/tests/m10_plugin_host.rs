//! M10：组件模型完善——host `get` bytes 版（组件插件回读服务）+ PluginHost
//! 统一加载组件插件（ComponentBytes manifest）。

#![allow(clippy::arc_with_non_send_sync)]

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::Arc;

use dsh_core::*;
use dsh_wasmrt::{Capabilities, NativeHost, PluginHost, PluginKind, PluginManifest, WasmComponentPlugin};

/// 构建（如缺失）并读取 hello-component 组件字节（dsh-plugin world）。
fn hello_component() -> Vec<u8> {
    let manifest: PathBuf =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../wasm-plugins/hello-component");
    let wasm_path = manifest.join("target/wasm32-wasip1/debug/hello_component_plugin.wasm");
    if !wasm_path.exists() {
        let status = Command::new("cargo")
            .args(["component", "build", "--manifest-path"])
            .arg(manifest.join("Cargo.toml"))
            .status()
            .expect("run cargo component build for hello-component");
        assert!(status.success(), "hello-component build failed");
    }
    fs::read(wasm_path).expect("read hello-component")
}

/// Capabilities::from_json：caps 数组解析（位名映射 + 缺省 abi_only + all）。
#[test]
fn capabilities_from_json() {
    use dsh_wasmrt::{CAPS_EMIT, CAPS_GET, CAPS_PROVIDE, CAPS_WASI_ENV, CAPS_WASI_FS, CAPS_WASI_NET};
    // 缺省 → abi_only
    let c = Capabilities::from_json(None);
    assert!(c.allows(CAPS_PROVIDE) && c.allows(CAPS_EMIT) && c.allows(CAPS_GET));
    assert!(!c.allows(CAPS_WASI_ENV) && !c.allows(CAPS_WASI_FS) && !c.allows(CAPS_WASI_NET));
    // 指定位
    let c = Capabilities::from_json(Some(&json!(["wasi-env", "wasi-net"])));
    assert!(c.allows(CAPS_WASI_ENV) && c.allows(CAPS_WASI_NET));
    assert!(!c.allows(CAPS_PROVIDE) && !c.allows(CAPS_WASI_FS));
    // all
    let c = Capabilities::from_json(Some(&json!(["all"])));
    assert!(c.allows(CAPS_WASI_FS));
}

/// PluginHost：ComponentBytes manifest 统一加载组件插件 → Plugin trait 可用。
#[test]
fn plugin_host_loads_component() {
    let host = NativeHost;
    let manifest = PluginManifest {
        name: "hello-component",
        kind: PluginKind::ComponentBytes(hello_component()),
        caps: Capabilities::all(),
    };
    let plugin = host.load(&manifest).expect("load component via PluginHost");
    assert_eq!(plugin.name(), "hello-component");

    // 经 Cordis 挂载（dsh-plugin world 组件：提供服务）
    let cordis = Cordis::new();
    let fid = cordis.plugin_arc(plugin, json!({"greeting": "via host"})).unwrap();
    assert_eq!(
        cordis.get_value("greeting"),
        Some(json!({"text": "via host"})),
        "component plugin provides service via PluginHost"
    );
    let _ = fid;
}

/// hello-component 直接构造（WasmComponentPlugin）：服务提供 + 卸载回滚。
#[test]
fn component_provides_and_rolls_back() {
    let cordis = Cordis::new();
    let plugin = Arc::new(
        WasmComponentPlugin::new("hello-component", &hello_component(), Capabilities::all()).unwrap(),
    );
    let fid = cordis.plugin_arc(plugin.clone(), json!({"greeting": "hi"})).unwrap();
    assert_eq!(cordis.get_value("greeting"), Some(json!({"text": "hi"})));

    cordis.unload(fid).unwrap();
    assert!(cordis.get_value("greeting").is_none(), "service removed on unload");
}

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

/// host `get` bytes 版：组件内 host.get 回读服务（handle_event → emit 载荷含回读值）。
#[test]
fn component_host_get_bytes_roundtrip() {
    let log = Rc::new(RefCell::new(Vec::<String>::new()));
    let log2 = log.clone();
    let cordis = Cordis::new();
    let plugin = Arc::new(
        WasmComponentPlugin::new("hello-component", &hello_component(), Capabilities::all()).unwrap(),
    );
    let _fid = cordis.plugin_arc(plugin.clone(), json!({"greeting": "roundtrip"})).unwrap();

    // 宿主 fiber 内注册 "wasm" 监听（接收组件 emit 的 pong）
    let log_listener = log2.clone();
    let listener_plugin = FnPlugin::new("host-listener", move |ctx, _cfg| {
        let l = log_listener.clone();
        ctx.on(
            "wasm",
            Arc::new(move |_ctx, args, _next| {
                l.borrow_mut()
                    .push(args.first().cloned().unwrap_or(Value::Null).to_string());
                HookResult::Continue
            }),
        )
        .unwrap();
        Ok(EffectOutcome::None)
    });
    cordis.plugin(listener_plugin, json!({})).unwrap();

    cordis.emit("ping", vec![json!({"n": 1})]);

    let captured = log.borrow().clone();
    assert!(
        captured.iter().any(|l| l.contains("roundtrip")),
        "host get returned service value to wasm, got: {captured:?}"
    );
}

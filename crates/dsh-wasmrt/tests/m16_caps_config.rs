//! M16：能力按 entry 配置统一入口——`PluginManifest::from_config` 从 entry 的
//! `caps` 数组解析能力（复用 `Capabilities::from_json`），C ABI 与组件两路径
//! 均支持（此前仅组件路径经 boot 接入，C ABI 调用点硬编码 caps）。
#![allow(clippy::arc_with_non_send_sync)]

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::Arc;

use dsh_core::*;
use dsh_wasmrt::{Capabilities, NativeHost, PluginHost, PluginKind, PluginManifest};

/// 构建（如缺失）并读取 hello wasm 插件字节（C ABI，wasm32-unknown-unknown）。
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

/// PluginManifest::from_config：缺省（无 caps）→ abi_only（ABI 能力，无 WASI）。
#[test]
fn manifest_from_config_defaults_abi_only() {
    let m = PluginManifest::from_config("hello", PluginKind::WasmBytes(hello_wasm()), &json!({}));
    assert!(m.caps.allows(dsh_wasmrt::CAPS_PROVIDE));
    assert!(m.caps.allows(dsh_wasmrt::CAPS_EMIT));
    assert!(m.caps.allows(dsh_wasmrt::CAPS_GET));
    assert!(!m.caps.allows(dsh_wasmrt::CAPS_WASI_ENV));
}

/// PluginManifest::from_config：`caps` 数组按位映射（含 WASI 位）。
#[test]
fn manifest_from_config_parses_caps_array() {
    let m = PluginManifest::from_config(
        "hello",
        PluginKind::WasmBytes(hello_wasm()),
        &json!({"caps": ["provide", "wasi-env", "wasi-net"]}),
    );
    assert!(m.caps.allows(dsh_wasmrt::CAPS_PROVIDE));
    assert!(m.caps.allows(dsh_wasmrt::CAPS_WASI_ENV));
    assert!(m.caps.allows(dsh_wasmrt::CAPS_WASI_NET));
    assert!(!m.caps.allows(dsh_wasmrt::CAPS_EMIT));
    assert!(!m.caps.allows(dsh_wasmrt::CAPS_GET));
    assert!(!m.caps.allows(dsh_wasmrt::CAPS_WASI_FS));
}

/// C ABI 路径能力按 entry 配置：`caps: [provide]`（无 get）→ 挂载成功（provide
/// 允许）但事件处理中 host_get 被拒（host import 侧检查生效）。
/// 与 `PluginManifest::from_config` 相同的解析路径（`Capabilities::from_json`）。
#[test]
fn c_abi_caps_from_entry_config_enforced() {
    // entry 配置只授 provide（hello 的 apply 提供 greeting 服务；handle_event 需 get）
    let caps = Capabilities::from_json(Some(&json!(["provide"])));
    let plugin = Arc::new(dsh_wasmrt::WasmPlugin::new("hello", &hello_wasm(), caps).unwrap());
    let cordis = Cordis::new();
    let fid = cordis.plugin_arc(plugin.clone(), json!({"greeting": "cfg"})).unwrap();
    assert_eq!(
        cordis.fiber_state(fid),
        Some(FiberState::Active),
        "apply needs provide only -> active"
    );
    assert_eq!(cordis.get_value("greeting"), Some(json!({"text": "cfg"})));

    // 宿主监听 wasm 事件 → emit ping → wasm handle_event 中 host_get 被拒
    let log = Rc::new(RefCell::new(Vec::<String>::new()));
    let log2 = log.clone();
    let host_plugin = FnPlugin::new("host", move |ctx, _cfg| {
        let l = log2.clone();
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
    cordis.plugin(host_plugin, json!({})).unwrap();
    cordis.emit("ping", vec![]);

    // 拒绝记录在插件日志（WasmPlugin::logs）
    let logs = plugin.logs();
    assert!(
        logs.iter().any(|l| l.contains("host_get denied")),
        "host_get denied by caps, got: {logs:?}"
    );
}

/// 组件路径能力按 entry 配置：`caps: [emit, get]`（无 provide）→ apply 中
/// host provide 被拒 → fiber Failed（组件 apply 提供服务依赖 provide 能力）。
#[test]
fn component_caps_from_entry_config_enforced() {
    let host = NativeHost;
    let manifest = PluginManifest::from_config(
        "hello-component",
        PluginKind::ComponentBytes(hello_component()),
        &json!({"caps": ["emit", "get"]}),
    );
    let plugin = host.load(&manifest).expect("load component via from_config");
    let cordis = Cordis::new();
    let fid = cordis.plugin_arc(plugin, json!({"greeting": "cfg"})).unwrap();
    // hello-component 的 apply 经 host provide 注册 greeting 服务 → 被拒 → apply 失败
    assert_eq!(cordis.fiber_state(fid), Some(FiberState::Failed), "provide denied");
    let err = cordis.fiber_error(fid).expect("error recorded");
    assert!(!err.to_string().is_empty());
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

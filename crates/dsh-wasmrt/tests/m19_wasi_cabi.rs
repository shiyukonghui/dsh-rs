//! M19：C ABI 路径 WASI 能力授予——`WasmPlugin` 按 caps 注册 WASI preview1，
//! wasip1 构建的插件可读环境变量（`wasi-env` 位）；无 WASI 位时 import 解析失败。
#![allow(clippy::arc_with_non_send_sync)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use dsh_core::*;
use dsh_wasmrt::{Capabilities, WasmPlugin};

/// 构建（如缺失）并读取 hello-wasi 插件字节（wasm32-wasip1：import WASI preview1）。
fn hello_wasi() -> Vec<u8> {
    let manifest: PathBuf =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../wasm-plugins/hello-wasi");
    let wasm_path = manifest.join("target/wasm32-wasip1/release/hello_wasi_plugin.wasm");
    if !wasm_path.exists() {
        let status = Command::new("cargo")
            .args(["build", "--target", "wasm32-wasip1", "--release"])
            .current_dir(&manifest)
            .status()
            .expect("run cargo build for hello-wasi plugin");
        assert!(status.success(), "hello-wasi plugin build failed");
    }
    fs::read(wasm_path).expect("read hello-wasi wasm")
}

/// caps 含 wasi-env：插件可读环境变量，host_log 记录。
#[test]
fn wasi_env_cap_allows_env_read() {
    // 唯一变量名（并行测试间互不覆盖）
    std::env::set_var("M19_ENV_A", "m19-value");
    let caps = Capabilities::new(dsh_wasmrt::CAPS_PROVIDE | dsh_wasmrt::CAPS_WASI_ENV);
    let plugin = Arc::new(WasmPlugin::new("hello-wasi", &hello_wasi(), caps).unwrap());
    let cordis = Cordis::new();
    let fid = cordis.plugin_arc(plugin.clone(), json!({})).unwrap();
    assert_eq!(cordis.fiber_state(fid), Some(FiberState::Active));

    let logs = plugin.logs();
    assert!(
        logs.iter().any(|l| l.contains("ENV_A=m19-value")),
        "wasi-env granted, got: {logs:?}"
    );
}

/// caps 无 WASI 位：wasip1 插件 import wasi_snapshot_preview1 无法解析 →
/// apply（懒实例化）时失败（fiber Failed）。
#[test]
fn no_wasi_cap_fails_instantiation() {
    let caps = Capabilities::abi_only(); // 仅 ABI 位，无 WASI
    // new 只编译模块；实例化是懒加载（apply 时）
    let plugin = Arc::new(WasmPlugin::new("hello-wasi", &hello_wasi(), caps).unwrap());
    let cordis = Cordis::new();
    let fid = cordis.plugin_arc(plugin.clone(), json!({})).unwrap();
    assert_eq!(
        cordis.fiber_state(fid),
        Some(FiberState::Failed),
        "apply instantiates -> wasi imports unresolved"
    );
    let err = cordis.fiber_error(fid).expect("error recorded");
    assert!(
        err.to_string().contains("instantiate") || err.to_string().contains("wasi"),
        "instantiation failed on missing wasi imports, got: {err}"
    );
}

/// caps 含 wasi-fs：构建 WASI 上下文成功（注册 preview1 不报错），
/// 纯 env ABI 插件（hello）不受影响。
#[test]
fn wasi_fs_cap_builds_ctx_for_env_plugin() {
    let caps = Capabilities::new(
        dsh_wasmrt::CAPS_PROVIDE | dsh_wasmrt::CAPS_WASI_ENV | dsh_wasmrt::CAPS_WASI_FS,
    );
    // 纯 env 插件（hello，无 WASI import）：注册 WASI 无碍
    let manifest: PathBuf =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../wasm-plugins/hello");
    let wasm_path = manifest.join("target/wasm32-unknown-unknown/release/hello_wasm_plugin.wasm");
    let bytes = fs::read(wasm_path).expect("read hello wasm");
    let plugin = Arc::new(WasmPlugin::new("hello", &bytes, caps).unwrap());
    let cordis = Cordis::new();
    let fid = cordis.plugin_arc(plugin.clone(), json!({"greeting": "wasi-ok"})).unwrap();
    assert_eq!(cordis.fiber_state(fid), Some(FiberState::Active));
    assert_eq!(cordis.get_value("greeting"), Some(json!({"text": "wasi-ok"})));
}

/// caps 含 wasi-fs 端到端：插件读取预打开根目录下的文件（M21）。
#[test]
fn wasi_fs_cap_allows_file_read() {
    // 在当前目录写测试文件（build_wasi_p1_ctx 预打开 cwd 为 /）
    let test_file = std::env::current_dir().unwrap().join("dsh_fs_test.txt");
    std::fs::write(&test_file, "fs-cap-ok").unwrap();

    let caps = Capabilities::new(dsh_wasmrt::CAPS_PROVIDE | dsh_wasmrt::CAPS_WASI_FS);
    let plugin = Arc::new(WasmPlugin::new("hello-wasi", &hello_wasi(), caps).unwrap());
    let cordis = Cordis::new();
    let fid = cordis.plugin_arc(plugin.clone(), json!({})).unwrap();
    assert_eq!(cordis.fiber_state(fid), Some(FiberState::Active));

    let logs = plugin.logs();
    assert!(
        logs.iter().any(|l| l.contains("FS_READ=fs-cap-ok")),
        "wasi-fs granted, got: {logs:?}"
    );
    let _ = std::fs::remove_file(&test_file);
}

/// caps 无 wasi-fs：插件 fs 读取失败（未预打开根目录）——apply 仍成功（env
/// 读取可用），但 FS_READ 记录错误。
#[test]
fn no_wasi_fs_cap_denies_file_read() {
    std::env::set_var("M19_ENV_B", "env-ok");
    let caps = Capabilities::new(dsh_wasmrt::CAPS_PROVIDE | dsh_wasmrt::CAPS_WASI_ENV);
    let plugin = Arc::new(WasmPlugin::new("hello-wasi", &hello_wasi(), caps).unwrap());
    let cordis = Cordis::new();
    let fid = cordis.plugin_arc(plugin.clone(), json!({})).unwrap();
    assert_eq!(cordis.fiber_state(fid), Some(FiberState::Active));

    let logs = plugin.logs();
    assert!(
        logs.iter().any(|l| l.contains("FS_READ=<fs-error")),
        "wasi-fs denied, got: {logs:?}"
    );
}

//! 插件包装配（D4）world 判别锁点：`detect_component_kind` 按**导出接口**预检
//! 组件 world——dsh-plugin（`plugin-api`）→ Plugin、dsh-loop（`agent-loop`）→ Loop、
//! 非法字节/非 dsh world（组件编译失败）→ Unknown（装配层据此 fail-loud）。
//! 复用已构建组件（缺失才 cargo component build），零慢速测试。

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use dsh_wasmrt::{ComponentKind, detect_component_kind};

/// 读取指定插件包（`wasm-plugins/<dir>`）的组件字节；缺失则构建。
fn component_bytes(dir: &str) -> Vec<u8> {
    let manifest: PathBuf =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../../wasm-plugins/{dir}"));
    let wasm = manifest
        .join("target/wasm32-wasip1/debug")
        .join(format!("{}_plugin.wasm", dir.replace('-', "_")));
    if !wasm.exists() {
        let status = Command::new("cargo")
            .args(["component", "build", "--manifest-path"])
            .arg(manifest.join("Cargo.toml"))
            .status()
            .expect("run cargo component build");
        assert!(status.success(), "{dir} build failed");
    }
    fs::read(&wasm).expect("read component")
}

/// dsh-loop world（echo-loop 导出 agent-loop）→ Loop。
#[test]
fn detect_dsh_loop_world() {
    assert_eq!(
        detect_component_kind(&component_bytes("echo-loop")),
        ComponentKind::Loop
    );
}

/// dsh-plugin world（hello-component 导出 plugin-api）→ Plugin。
#[test]
fn detect_dsh_plugin_world() {
    assert_eq!(
        detect_component_kind(&component_bytes("hello-component")),
        ComponentKind::Plugin
    );
}

/// 非法字节 / 空 → Unknown（装配层据此 fail-loud：非 dsh world）。
#[test]
fn detect_non_component_bytes() {
    assert_eq!(detect_component_kind(b"not a wasm component at all"), ComponentKind::Unknown);
    assert_eq!(detect_component_kind(b""), ComponentKind::Unknown);
}

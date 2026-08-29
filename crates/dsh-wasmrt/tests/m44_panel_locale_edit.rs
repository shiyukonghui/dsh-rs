// 面板改写 #12 / D-200：panel-locale-edit 服务装配单元——locale 设置编辑声明单元（第十三卡）。
// panel-settings-edit 定型机械复制（E2E §2「其余 ns = 复制声明单元，机械工作」首个兑现）：
// fieldsFrom 指宿主 settings/describe（pick=locale），保存指 settings/update；零自有端点。
use std::path::{Path, PathBuf};
use std::process::Command;

use dsh_core::Value;
use dsh_wasmrt::WasmRemoteEndpointPlugin;
use serde_json::json;

fn component() -> Vec<u8> {
    let manifest: PathBuf =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../wasm-plugins/panel-locale-edit");
    let wasm_path = manifest.join("target/wasm32-wasip1/debug/panel_locale_edit_plugin.wasm");
    if !wasm_path.exists() {
        let status = Command::new("cargo")
            .env("CARGO_NET_OFFLINE", "true")
            .args(["component", "build", "--manifest-path"])
            .arg(manifest.join("Cargo.toml"))
            .status()
            .expect("run cargo component build for panel-locale-edit");
        assert!(status.success(), "panel-locale-edit build failed");
    }
    std::fs::read(wasm_path).expect("read panel-locale-edit component")
}

fn plugin() -> WasmRemoteEndpointPlugin {
    WasmRemoteEndpointPlugin::new("panel-locale-edit", &component(), Default::default(), None)
        .expect("panel-locale-edit plugin constructs")
}

#[test]
fn describe_ui_returns_valid_dynamic_form_declaration() {
    let r = plugin()
        .handle("panel-locale-edit", "describeUI", br#"{}"#, None)
        .unwrap();
    assert_eq!(r["ok"], true, "{r}");
    let decl = &r["value"];
    assert_eq!(decl["cardId"], "panel-locale-edit.edit");
    assert_eq!(decl["type"], "config");
    let view = &decl["view"];
    assert_eq!(view["kind"], "form");
    assert_eq!(
        view["fieldsFrom"],
        json!({ "rpc": ["settings", "describe"], "pick": "locale" }),
        "D-194 契约，pick 换 ns 即新卡（机械复制的本质）"
    );
    assert!(view.get("fields").is_none(), "fieldsFrom 与 fields 二选一");
    assert_eq!(view["actions"][0]["rpc"], json!(["settings", "update"]));
}

#[test]
fn static_ui_json_matches_describe_ui() {
    let r = plugin()
        .handle("panel-locale-edit", "describeUI", br#"{}"#, None)
        .unwrap();
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../wasm-plugins/panel-locale-edit/web/ui.json");
    let static_decl: Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("read web/ui.json"))
            .expect("ui.json is JSON");
    assert_eq!(static_decl, r["value"], "一份契约");
}

#[test]
fn no_proprietary_data_endpoints_fail_loud() {
    let p = plugin();
    for m in ["describe", "update", "list"] {
        let r = p.handle("panel-locale-edit", m, br#"{}"#, None).unwrap();
        assert_eq!(r["ok"], false, "声明单元不得有数据端点: {m} -> {r}");
    }
}

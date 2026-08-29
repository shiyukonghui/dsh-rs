// 面板改写 #8 / S4（D-194）：panel-settings-edit 服务装配单元——设置编辑**声明单元**。
// 照 panel-chat 定型（D-193-B）：设置域是宿主域，单元只拥有 v2 声明（form + fieldsFrom）；
// **零自有数据端点**（describe/update 走宿主既表面经 canonical 别名）。
use std::path::{Path, PathBuf};
use std::process::Command;

use dsh_core::Value;
use dsh_wasmrt::WasmRemoteEndpointPlugin;
use serde_json::json;

fn component() -> Vec<u8> {
    let manifest: PathBuf =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../wasm-plugins/panel-settings-edit");
    let wasm_path =
        manifest.join("target/wasm32-wasip1/debug/panel_settings_edit_plugin.wasm");
    if !wasm_path.exists() {
        let status = Command::new("cargo")
            .env("CARGO_NET_OFFLINE", "true")
            .args(["component", "build", "--manifest-path"])
            .arg(manifest.join("Cargo.toml"))
            .status()
            .expect("run cargo component build for panel-settings-edit");
        assert!(status.success(), "panel-settings-edit build failed");
    }
    std::fs::read(wasm_path).expect("read panel-settings-edit component")
}

fn plugin() -> WasmRemoteEndpointPlugin {
    WasmRemoteEndpointPlugin::new("panel-settings-edit", &component(), Default::default(), None)
        .expect("panel-settings-edit plugin constructs")
}

#[test]
fn describe_ui_returns_valid_dynamic_form_declaration() {
    let r = plugin()
        .handle("panel-settings-edit", "describeUI", br#"{}"#, None)
        .unwrap();
    assert_eq!(r["ok"], true, "{r}");
    let decl = &r["value"];
    assert_eq!(decl["$schema"], "dsh/plugin-ui/v2");
    assert_eq!(decl["kind"], "card");
    assert_eq!(decl["cardId"], "panel-settings-edit.edit");
    assert_eq!(decl["type"], "config");
    let view = &decl["view"];
    assert_eq!(view["kind"], "form");
    assert_eq!(
        view["fieldsFrom"],
        json!({ "rpc": ["settings", "describe"], "pick": "ui-theme", "nsSelect": true }),
        "动态 fields 投影面 + D-201 nsSelect（一卡通用编辑全部 ns）"
    );
    assert!(
        view.get("fields").is_none(),
        "fieldsFrom 与静态 fields 二选一（S1 校验同规）"
    );
    assert_eq!(view["actions"][0]["rpc"], json!(["settings", "update"]), "乐观锁保存面");
}

#[test]
fn static_ui_json_matches_describe_ui() {
    let r = plugin()
        .handle("panel-settings-edit", "describeUI", br#"{}"#, None)
        .unwrap();
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../wasm-plugins/panel-settings-edit/web/ui.json");
    let static_decl: Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("read web/ui.json"))
            .expect("ui.json is JSON");
    assert_eq!(static_decl, r["value"], "一份契约");
}

/// 声明单元不伪装能力：设置协议在宿主既表面，本单元无自有数据端点。
#[test]
fn no_proprietary_data_endpoints_fail_loud() {
    let p = plugin();
    for m in ["describe", "update", "list"] {
        let r = p.handle("panel-settings-edit", m, br#"{}"#, None).unwrap();
        assert_eq!(r["ok"], false, "声明单元不得有数据端点: {m} -> {r}");
    }
}

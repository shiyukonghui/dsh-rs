// 面板改写 #11 / D-199：panel-approval 服务装配单元——待审批清单声明单元（第十二卡）。
// 声明单元定型（panel-chat 系）：数据面 approval/pending + 决定走 session.approval.decide
// （均为宿主原生臂，D-198）；rowActions 带 args.decision（C6 扩展）；零自有数据端点。
use std::path::{Path, PathBuf};
use std::process::Command;

use dsh_core::Value;
use dsh_wasmrt::WasmRemoteEndpointPlugin;
use serde_json::json;

fn component() -> Vec<u8> {
    let manifest: PathBuf =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../wasm-plugins/panel-approval");
    let wasm_path = manifest.join("target/wasm32-wasip1/debug/panel_approval_plugin.wasm");
    if !wasm_path.exists() {
        let status = Command::new("cargo")
            .env("CARGO_NET_OFFLINE", "true")
            .args(["component", "build", "--manifest-path"])
            .arg(manifest.join("Cargo.toml"))
            .status()
            .expect("run cargo component build for panel-approval");
        assert!(status.success(), "panel-approval build failed");
    }
    std::fs::read(wasm_path).expect("read panel-approval component")
}

fn plugin() -> WasmRemoteEndpointPlugin {
    WasmRemoteEndpointPlugin::new("panel-approval", &component(), Default::default(), None)
        .expect("panel-approval plugin constructs")
}

#[test]
fn describe_ui_returns_valid_list_declaration() {
    let r = plugin()
        .handle("panel-approval", "describeUI", br#"{}"#, None)
        .unwrap();
    assert_eq!(r["ok"], true, "{r}");
    let decl = &r["value"];
    assert_eq!(decl["cardId"], "panel-approval.pending");
    assert_eq!(decl["type"], "session");
    let view = &decl["view"];
    assert_eq!(view["kind"], "list");
    assert_eq!(view["dataRpc"], json!(["approval", "pending"]), "D-198 薄臂");
    assert_eq!(view["rowsPath"], "items");
    // 同 rpc 双动作：decision 由声明字面量 args 区分（C6/D-198 契约扩展）。
    let ra = view["rowActions"].as_array().expect("rowActions");
    assert_eq!(ra.len(), 2, "{ra:?}");
    assert_eq!(ra[0]["rpc"], json!(["session.approval", "decide"]));
    assert_eq!(ra[0]["args"]["decision"], "allowedOnce");
    assert_eq!(ra[1]["args"]["decision"], "rejected");
    assert_eq!(ra[1]["confirm"], true, "拒绝是破坏性决定，必须确认");
}

#[test]
fn static_ui_json_matches_describe_ui() {
    let r = plugin()
        .handle("panel-approval", "describeUI", br#"{}"#, None)
        .unwrap();
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../wasm-plugins/panel-approval/web/ui.json");
    let static_decl: Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("read web/ui.json"))
            .expect("ui.json is JSON");
    assert_eq!(static_decl, r["value"], "一份契约");
}

#[test]
fn no_proprietary_data_endpoints_fail_loud() {
    let p = plugin();
    for m in ["pending", "decide", "list"] {
        let r = p.handle("panel-approval", m, br#"{}"#, None).unwrap();
        assert_eq!(r["ok"], false, "声明单元不得有数据端点: {m} -> {r}");
    }
}

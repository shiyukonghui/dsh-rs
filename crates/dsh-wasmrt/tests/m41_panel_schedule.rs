// 面板改写 #9 / D-195：panel-schedule 服务装配单元——调度清单**声明单元**（读端）。
// 照 panel-chat/panel-settings-edit 定型：调度协议在宿主（`schedule/list` 薄臂 fold
// 事件日志权威），单元只拥有 v2 list 声明；零自有数据端点。
use std::path::{Path, PathBuf};
use std::process::Command;

use dsh_core::Value;
use dsh_wasmrt::WasmRemoteEndpointPlugin;
use serde_json::json;

fn component() -> Vec<u8> {
    let manifest: PathBuf =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../wasm-plugins/panel-schedule");
    let wasm_path = manifest.join("target/wasm32-wasip1/debug/panel_schedule_plugin.wasm");
    if !wasm_path.exists() {
        let status = Command::new("cargo")
            .env("CARGO_NET_OFFLINE", "true")
            .args(["component", "build", "--manifest-path"])
            .arg(manifest.join("Cargo.toml"))
            .status()
            .expect("run cargo component build for panel-schedule");
        assert!(status.success(), "panel-schedule build failed");
    }
    std::fs::read(wasm_path).expect("read panel-schedule component")
}

fn plugin() -> WasmRemoteEndpointPlugin {
    WasmRemoteEndpointPlugin::new("panel-schedule", &component(), Default::default(), None)
        .expect("panel-schedule plugin constructs")
}

#[test]
fn describe_ui_returns_valid_list_declaration() {
    let r = plugin()
        .handle("panel-schedule", "describeUI", br#"{}"#, None)
        .unwrap();
    assert_eq!(r["ok"], true, "{r}");
    let decl = &r["value"];
    assert_eq!(decl["$schema"], "dsh.panel-ui/v2");
    assert_eq!(decl["kind"], "card");
    assert_eq!(decl["cardId"], "panel-schedule.list");
    assert_eq!(decl["type"], "runtime", "调度归 runtime 分类");
    let view = &decl["view"];
    assert_eq!(view["kind"], "list");
    assert_eq!(view["dataRpc"], json!(["schedule", "list"]), "宿主薄臂");
    assert_eq!(view["rowsPath"], "items");
    let cols = view["columns"].as_array().expect("columns");
    assert_eq!(cols.len(), 4, "{cols:?}");
    // D-195 写切片 A：删除行动作（C6 confirm 形——破坏性动作必须声明确认）。
    let ra = view["rowActions"].as_array().expect("rowActions");
    assert_eq!(ra.len(), 1);
    assert_eq!(ra[0]["rpc"], json!(["schedule", "delete"]));
    assert_eq!(ra[0]["scope"], "row");
    assert_eq!(ra[0]["confirm"], true);
}

#[test]
fn static_ui_json_matches_describe_ui() {
    let r = plugin()
        .handle("panel-schedule", "describeUI", br#"{}"#, None)
        .unwrap();
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../wasm-plugins/panel-schedule/web/ui.json");
    let static_decl: Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("read web/ui.json"))
            .expect("ui.json is JSON");
    assert_eq!(static_decl, r["value"], "一份契约");
}

#[test]
fn no_proprietary_data_endpoints_fail_loud() {
    let p = plugin();
    for m in ["list", "create", "delete"] {
        let r = p.handle("panel-schedule", m, br#"{}"#, None).unwrap();
        assert_eq!(r["ok"], false, "声明单元不得有数据端点: {m} -> {r}");
    }
}

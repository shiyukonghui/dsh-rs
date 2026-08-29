// 面板改写 #10 / D-197：panel-schedule-create 服务装配单元——调度创建表单声明单元。
// 静态 form 卡（照 llm-deepseek 表单契约）+ 声明单元纪律（照 panel-chat）：保存动作
// 指宿主 `schedule/create` 薄臂（D-196 画布形已由 roundtrip 测钉死）；零自有数据端点。
use std::path::{Path, PathBuf};
use std::process::Command;

use dsh_core::Value;
use dsh_wasmrt::WasmRemoteEndpointPlugin;
use serde_json::json;

fn component() -> Vec<u8> {
    let manifest: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../wasm-plugins/panel-schedule-create");
    let wasm_path =
        manifest.join("target/wasm32-wasip1/debug/panel_schedule_create_plugin.wasm");
    if !wasm_path.exists() {
        let status = Command::new("cargo")
            .env("CARGO_NET_OFFLINE", "true")
            .args(["component", "build", "--manifest-path"])
            .arg(manifest.join("Cargo.toml"))
            .status()
            .expect("run cargo component build for panel-schedule-create");
        assert!(status.success(), "panel-schedule-create build failed");
    }
    std::fs::read(wasm_path).expect("read panel-schedule-create component")
}

fn plugin() -> WasmRemoteEndpointPlugin {
    WasmRemoteEndpointPlugin::new(
        "panel-schedule-create",
        &component(),
        Default::default(),
        None,
    )
    .expect("panel-schedule-create plugin constructs")
}

#[test]
fn describe_ui_returns_valid_form_declaration() {
    let r = plugin()
        .handle("panel-schedule-create", "describeUI", br#"{}"#, None)
        .unwrap();
    assert_eq!(r["ok"], true, "{r}");
    let decl = &r["value"];
    assert_eq!(decl["cardId"], "panel-schedule-create.form");
    assert_eq!(decl["type"], "runtime");
    let view = &decl["view"];
    assert_eq!(view["kind"], "form");
    let fields = view["fields"].as_array().expect("fields");
    assert_eq!(fields.len(), 3, "{fields:?}");
    assert_eq!(view["actions"][0]["rpc"], json!(["schedule", "create"]));
    // 静态 fields（无 fieldsFrom）——二选一校验的另一侧。
    assert!(view.get("fieldsFrom").is_none());
}

#[test]
fn static_ui_json_matches_describe_ui() {
    let r = plugin()
        .handle("panel-schedule-create", "describeUI", br#"{}"#, None)
        .unwrap();
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../wasm-plugins/panel-schedule-create/web/ui.json");
    let static_decl: Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("read web/ui.json"))
            .expect("ui.json is JSON");
    assert_eq!(static_decl, r["value"], "一份契约");
}

#[test]
fn no_proprietary_data_endpoints_fail_loud() {
    let p = plugin();
    for m in ["create", "list", "delete"] {
        let r = p.handle("panel-schedule-create", m, br#"{}"#, None).unwrap();
        assert_eq!(r["ok"], false, "声明单元不得有数据端点: {m} -> {r}");
    }
}

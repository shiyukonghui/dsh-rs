// 面板改写 #6（D-192）：panel-settings 服务装配单元——设置概览卡（config 分类首卡）。
//
// 宿主侧 `settingsDescribe` 投影（与原生 settings.describe 同形状，redact 在源头）由
// 单元拍平成 {ns, field, value} 概览行；value 非对象 → 单行 field="—"。
// 服务失败透传，不伪造空表。只读卡（写端 = 动态 fields 契约演进，另立决策）。
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use dsh_core::Value;
use dsh_wasmrt::{RemoteServiceProjector, WasmRemoteEndpointPlugin};
use serde_json::json;

fn component() -> Vec<u8> {
    let manifest: PathBuf =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../wasm-plugins/panel-settings");
    let wasm_path = manifest.join("target/wasm32-wasip1/debug/panel_settings_plugin.wasm");
    if !wasm_path.exists() {
        let status = Command::new("cargo")
            .env("CARGO_NET_OFFLINE", "true")
            .args(["component", "build", "--manifest-path"])
            .arg(manifest.join("Cargo.toml"))
            .status()
            .expect("run cargo component build for panel-settings");
        assert!(status.success(), "panel-settings build failed");
    }
    std::fs::read(wasm_path).expect("read panel-settings component")
}

struct SettingsProjector {
    response: Value,
    calls: RefCell<usize>,
}

impl RemoteServiceProjector for SettingsProjector {
    fn get(&self, service: &str, _payload: &[u8]) -> Vec<u8> {
        *self.calls.borrow_mut() += 1;
        match service {
            "settingsDescribe" => serde_json::to_vec(&self.response).unwrap_or_default(),
            _ => serde_json::to_vec(&json!({
                "ok": false,
                "error": { "code": "unknown-service", "message": service },
            }))
            .unwrap_or_default(),
        }
    }
    fn set(&self, _s: &str, _p: &[u8]) -> Vec<u8> {
        Vec::new()
    }
}

fn plugin(response: Value) -> (WasmRemoteEndpointPlugin, Rc<SettingsProjector>) {
    let projector = Rc::new(SettingsProjector {
        response,
        calls: RefCell::new(0),
    });
    let p = WasmRemoteEndpointPlugin::new(
        "panel-settings",
        &component(),
        Default::default(),
        Some(projector.clone()),
    )
    .expect("panel-settings plugin constructs");
    (p, projector)
}

#[test]
fn describe_ui_returns_valid_list_declaration() {
    let (p, _c) = plugin(json!({"ok": true, "value": {"namespaces": []}}));
    let r = p
        .handle("panel-settings", "describeUI", br#"{}"#, None)
        .unwrap();
    assert_eq!(r["ok"], true, "{r}");
    let decl = &r["value"];
    assert_eq!(decl["$schema"], "dsh.panel-ui/v2");
    assert_eq!(decl["cardId"], "panel-settings.list");
    assert_eq!(decl["type"], "config", "设置归 config 分类（D-181 语义表）");
    let size = decl["size"].as_object().expect("size 对象");
    assert!(size["w"].as_u64().unwrap() <= 4 && size["h"].as_u64().unwrap() <= 8);
    assert!(size.get("x").is_none() && size.get("y").is_none());
    let view = &decl["view"];
    assert_eq!(view["kind"], "list");
    assert_eq!(view["rowsPath"], "items");
    assert_eq!(view["dataRpc"], json!(["panel-settings", "list"]));
    assert!(!view["columns"].as_array().unwrap().is_empty());
}

#[test]
fn static_ui_json_matches_describe_ui() {
    let (p, _c) = plugin(json!({"ok": true, "value": {"namespaces": []}}));
    let r = p
        .handle("panel-settings", "describeUI", br#"{}"#, None)
        .unwrap();
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../wasm-plugins/panel-settings/web/ui.json");
    let static_decl: Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("read web/ui.json"))
            .expect("ui.json is JSON");
    assert_eq!(static_decl, r["value"], "一份契约");
}

/// 行拍平：对象 value 每顶层键一行；非对象 value 单行 field="—"。
#[test]
fn list_flattens_namespaces_to_rows() {
    let (p, calls) = plugin(json!({"ok": true, "value": {"namespaces": [
        {"ns": "ui-theme", "applies": "live", "revision": 3,
         "value": {"mode": "dark", "fontSize": 14}},
        {"ns": "odd", "applies": "restart", "revision": 0, "value": "scalar"}
    ]}}));
    let r = p
        .handle("panel-settings", "list", br#"{}"#, None)
        .unwrap();
    assert_eq!(r["ok"], true, "{r}");
    let items = r["value"]["items"].as_array().expect("items");
    assert_eq!(items.len(), 3, "两 ns：对象 2 行 + 标量 1 行，得 {items:?}");
    // 字段序不作断言（value 对象键序非契约）——按 (ns,field) 查找。
    let row = |ns: &str, field: &str| -> &Value {
        items
            .iter()
            .find(|i| i["ns"] == ns && i["field"] == field)
            .unwrap_or_else(|| panic!("缺行 ({ns},{field}): {items:?}"))
    };
    assert_eq!(row("ui-theme", "mode")["value"], "dark");
    assert_eq!(row("ui-theme", "fontSize")["value"], 14);
    assert_eq!(row("odd", "—")["value"], "scalar", "非对象 value 单行占位");
    assert_eq!(*calls.calls.borrow(), 1);
}

#[test]
fn list_service_failure_is_fail_loud() {
    let (p, _calls) = plugin(json!({
        "ok": false,
        "error": { "code": "no-settings", "message": "no settings provider assembled" }
    }));
    let r = p
        .handle("panel-settings", "list", br#"{}"#, None)
        .unwrap();
    assert_eq!(r["ok"], false, "服务失败不得报成功：{r}");
    assert_eq!(r["error"]["code"], "no-settings");
    assert!(r["value"]["items"].is_null(), "错误响应不得夹带伪造空表：{r}");
}

#[test]
fn unknown_endpoint_fail_loud() {
    let (p, _c) = plugin(json!({"ok": true, "value": {"namespaces": []}}));
    let r = p
        .handle("panel-settings", "nope", br#"{}"#, None)
        .unwrap();
    assert_eq!(r["ok"], false);
    assert!(r.to_string().contains("nope"));
}

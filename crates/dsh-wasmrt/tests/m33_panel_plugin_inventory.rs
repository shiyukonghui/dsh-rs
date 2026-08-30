// C4（D-185）：panel-plugin-inventory 服务装配单元——首个 harness 面板改写（插件清单卡）。
//
// 验证「面板 → 服务装配单元」改写型（m32 同族）：
// 1. describeUI：v2 list 卡声明，且与静态 web/ui.json **逐字段一致**（一份契约）；
// 2. list：经 host-services "loader" 投影出真实行（group 过滤、disabled/fiber 状态映射），
//    **服务失败透传错误——绝不伪造空表**；
// 3. 未知端点 fail-loud。
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use dsh_core::Value;
use dsh_wasmrt::{RemoteServiceProjector, WasmRemoteEndpointPlugin};
use serde_json::json;

fn component() -> Vec<u8> {
    let manifest: PathBuf =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../wasm-plugins/panel-plugin-inventory");
    let wasm_path =
        manifest.join("target/wasm32-wasip1/debug/panel_plugin_inventory_plugin.wasm");
    if !wasm_path.exists() {
        let status = Command::new("cargo")
            .env("CARGO_NET_OFFLINE", "true")
            .args(["component", "build", "--manifest-path"])
            .arg(manifest.join("Cargo.toml"))
            .status()
            .expect("run cargo component build for panel-plugin-inventory");
        assert!(status.success(), "panel-plugin-inventory build failed");
    }
    std::fs::read(wasm_path).expect("read panel-plugin-inventory component")
}

/// 测试投影器：真实形态的 "loader" 服务（RemoteHost 投影同构）。
struct LoaderProjector {
    entries: RefCell<Vec<Value>>,
}

impl RemoteServiceProjector for LoaderProjector {
    fn get(&self, service: &str, _payload: &[u8]) -> Vec<u8> {
        match service {
            "loader" => serde_json::to_vec(&json!({
                "ok": true,
                "entries": self.entries.borrow().clone(),
            }))
            .unwrap_or_default(),
            _ => serde_json::to_vec(&json!({
                "ok": false,
                "error": { "code": "unknown-service", "message": service },
            }))
            .unwrap_or_default(),
        }
    }
    fn set(&self, service: &str, _payload: &[u8]) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "ok": false,
            "error": { "code": "read-only", "message": service },
        }))
        .unwrap_or_default()
    }
}

fn plugin_with(entries: Vec<Value>) -> WasmRemoteEndpointPlugin {
    let bytes = component();
    let projector = Rc::new(LoaderProjector { entries: RefCell::new(entries) });
    WasmRemoteEndpointPlugin::new(
        "panel-plugin-inventory",
        &bytes,
        Default::default(),
        Some(projector),
    )
    .expect("panel-plugin-inventory plugin constructs")
}

/// describeUI：v2 list 卡（§4.1 契约字段齐）。
#[test]
fn describe_ui_returns_valid_list_declaration() {
    let plugin = plugin_with(vec![]);
    let result = plugin
        .handle("panel-plugin-inventory", "describeUI", br#"{}"#, None)
        .unwrap();
    assert_eq!(result["ok"], true, "envelope ok: {result}");
    let decl = &result["value"];
    assert_eq!(decl["$schema"], "dsh.panel-ui/v2");
    assert_eq!(decl["kind"], "card");
    assert_eq!(decl["cardId"], "panel-plugin-inventory.list");
    let type_enum = [
        "model", "config", "capability", "runtime", "resource", "session", "misc",
    ];
    let ty = decl["type"].as_str().unwrap_or("");
    assert!(type_enum.contains(&ty), "type 必须落闭集，收到 {ty}");
    // size 封顶且无坐标（同 m32 纪律）
    let size = decl["size"].as_object().expect("size 是对象");
    let w = size["w"].as_u64().expect("w 数字");
    let h = size["h"].as_u64().expect("h 数字");
    assert!(w <= 4 && h <= 8, "封顶 w≤4/h≤8，收到 {w}x{h}");
    assert!(size.get("x").is_none() && size.get("y").is_none(), "声明里无坐标");
    // list 视图契约
    let view = &decl["view"];
    assert_eq!(view["kind"], "list");
    assert_eq!(view["rowsPath"], "items", "数据面位置必须显式");
    assert_eq!(
        view["dataRpc"],
        json!(["panel-plugin-inventory", "list"]),
        "dataRpc 显式"
    );
    let cols = view["columns"].as_array().expect("columns 数组");
    assert!(!cols.is_empty(), "列定义非空");
    for c in cols {
        // D-225：label 位=LocalizedText 契约（字符串 | 非空字符串值对象）。
        let txt = |v: &Value| v.is_string() || v.as_object().is_some_and(|m| !m.is_empty() && m.values().all(Value::is_string));
        assert!(c["key"].as_str().is_some() && txt(&c["label"]), "{c:?}");
    }
}

/// 一份契约：静态 web/ui.json 与 describeUI 逐字段一致（同 m32 断言）。
#[test]
fn static_ui_json_matches_describe_ui() {
    let plugin = plugin_with(vec![]);
    let result = plugin
        .handle("panel-plugin-inventory", "describeUI", br#"{}"#, None)
        .unwrap();
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../wasm-plugins/panel-plugin-inventory/web/ui.json");
    let static_decl: Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("read web/ui.json"))
            .expect("web/ui.json is JSON");
    assert_eq!(
        static_decl,
        result["value"],
        "静态与 describeUI 必须逐字段一致（一份契约）"
    );
}

/// list：group 过滤 + disabled/fiber 状态映射（行投影只在这定义——双权威禁令）。
#[test]
fn list_projects_loader_entries() {
    let plugin = plugin_with(vec![
        json!({"id": "core", "name": "svc-core", "disabled": false, "group": false, "fiber": 7}),
        json!({"id": "off", "name": "svc-off", "disabled": true, "group": false, "fiber": null}),
        json!({"id": "idle", "name": "svc-idle", "disabled": false, "group": false, "fiber": null}),
        json!({"id": "grp", "name": "group-a", "disabled": false, "group": true, "fiber": null}),
    ]);
    let result = plugin
        .handle("panel-plugin-inventory", "list", br#"{}"#, None)
        .unwrap();
    assert_eq!(result["ok"], true, "{result}");
    let items = result["value"]["items"].as_array().expect("items 数组");
    assert_eq!(items.len(), 3, "group 条目必须过滤：{items:?}");
    assert_eq!(items[0]["name"], "svc-core");
    assert_eq!(items[0]["state"], "active", "有 fiber → active");
    assert_eq!(items[1]["state"], "disabled", "disabled → disabled（优先）");
    assert_eq!(items[2]["state"], "ready", "无 fiber 未禁用 → ready");
    assert_eq!(items[0]["id"], "core");
}

/// 诚实纪律：loader 服务失败 → ok:false 透传，**绝不伪造空表**。
#[test]
fn list_service_failure_is_fail_loud() {
    let bytes = component();
    struct BrokenProjector;
    impl RemoteServiceProjector for BrokenProjector {
        fn get(&self, service: &str, _p: &[u8]) -> Vec<u8> {
            serde_json::to_vec(&json!({
                "ok": false,
                "error": { "code": "service-down", "message": format!("{service} down") },
            }))
            .unwrap()
        }
        fn set(&self, _s: &str, _p: &[u8]) -> Vec<u8> {
            Vec::new()
        }
    }
    let plugin = WasmRemoteEndpointPlugin::new(
        "panel-plugin-inventory",
        &bytes,
        Default::default(),
        Some(Rc::new(BrokenProjector)),
    )
    .expect("plugin");
    let result = plugin
        .handle("panel-plugin-inventory", "list", br#"{}"#, None)
        .unwrap();
    assert_eq!(result["ok"], false, "服务失败不得报成功：{result}");
    assert_eq!(result["error"]["code"], "service-down");
    assert!(
        result["value"]["items"].is_null(),
        "错误响应里不得夹带伪造空表安抚：{result}"
    );
}

#[test]
fn unknown_endpoint_fail_loud() {
    let plugin = plugin_with(vec![]);
    let result = plugin
        .handle("panel-plugin-inventory", "nope", br#"{}"#, None)
        .unwrap();
    assert_eq!(result["ok"], false);
    assert!(result.to_string().contains("nope"));
}

// 面板改写 #2（D-187）：panel-runtime-status 服务装配单元——运行时状态卡。
//
// C4 改写型的第二次复制：describeUI 与静态 ui.json 一份契约；status 端点跨服务聚合
// （loader + dynamicPlugins 投影），**任一服务失败整体 fail-loud，不部分伪造**。
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use dsh_core::Value;
use dsh_wasmrt::{RemoteServiceProjector, WasmRemoteEndpointPlugin};
use serde_json::json;

fn component() -> Vec<u8> {
    let manifest: PathBuf =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../wasm-plugins/panel-runtime-status");
    let wasm_path =
        manifest.join("target/wasm32-wasip1/debug/panel_runtime_status_plugin.wasm");
    if !wasm_path.exists() {
        let status = Command::new("cargo")
            .env("CARGO_NET_OFFLINE", "true")
            .args(["component", "build", "--manifest-path"])
            .arg(manifest.join("Cargo.toml"))
            .status()
            .expect("run cargo component build for panel-runtime-status");
        assert!(status.success(), "panel-runtime-status build failed");
    }
    std::fs::read(wasm_path).expect("read panel-runtime-status component")
}

/// 双服务桩：loader entries + dynamicPlugins plugins（形状与 RemoteHost 投影同构）。
struct AggProjector {
    loader: Value,
    dynamic: Value,
}

impl RemoteServiceProjector for AggProjector {
    fn get(&self, service: &str, _payload: &[u8]) -> Vec<u8> {
        let v = match service {
            "loader" => &self.loader,
            "dynamicPlugins" => &self.dynamic,
            _ => &json!({"ok": false, "error": {"code": "unknown-service", "message": service}}),
        };
        serde_json::to_vec(v).unwrap_or_default()
    }
    fn set(&self, _s: &str, _p: &[u8]) -> Vec<u8> {
        Vec::new()
    }
}

fn plugin(loader: Value, dynamic: Value) -> WasmRemoteEndpointPlugin {
    let projector = Rc::new(AggProjector { loader, dynamic });
    WasmRemoteEndpointPlugin::new(
        "panel-runtime-status",
        &component(),
        Default::default(),
        Some(projector),
    )
    .expect("panel-runtime-status plugin constructs")
}

/// describeUI：v2 status 卡（items 全由数据面驱动，无静态硬编码）。
#[test]
fn describe_ui_returns_valid_status_declaration() {
    let p = plugin(json!({"ok": true, "entries": []}), json!({"ok": true, "plugins": []}));
    let r = p
        .handle("panel-runtime-status", "describeUI", br#"{}"#, None)
        .unwrap();
    assert_eq!(r["ok"], true, "{r}");
    let decl = &r["value"];
    assert_eq!(decl["$schema"], "dsh/plugin-ui/v2");
    assert_eq!(decl["kind"], "card");
    assert_eq!(decl["cardId"], "panel-runtime-status.status");
    let type_enum = [
        "model", "config", "capability", "runtime", "resource", "session", "misc",
    ];
    let ty = decl["type"].as_str().unwrap_or("");
    assert!(type_enum.contains(&ty), "type 闭集，收到 {ty}");
    let size = decl["size"].as_object().expect("size 对象");
    let w = size["w"].as_u64().expect("w");
    let h = size["h"].as_u64().expect("h");
    assert!(w <= 4 && h <= 8 && size.get("x").is_none() && size.get("y").is_none());
    let view = &decl["view"];
    assert_eq!(view["kind"], "status");
    assert_eq!(
        view["dataRpc"],
        json!(["panel-runtime-status", "status"]),
        "dataRpc 显式"
    );
    assert!(
        view.get("items").is_none(),
        "status 数据面驱动：声明不硬编码 items（静态兜底留给声明方自愿）"
    );
}

/// 一份契约：静态 ui.json == describeUI。
#[test]
fn static_ui_json_matches_describe_ui() {
    let p = plugin(json!({"ok": true, "entries": []}), json!({"ok": true, "plugins": []}));
    let r = p
        .handle("panel-runtime-status", "describeUI", br#"{}"#, None)
        .unwrap();
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../wasm-plugins/panel-runtime-status/web/ui.json");
    let static_decl: Value = serde_json::from_str(
        &std::fs::read_to_string(path).expect("read web/ui.json"),
    )
    .expect("ui.json is JSON");
    assert_eq!(static_decl, r["value"], "一份契约");
}

/// 跨服务聚合：条目/active/disabled/动态包计数 + tone 规则（disabled>0 → warn）。
#[test]
fn status_aggregates_loader_and_dynamic_plugins() {
    let p = plugin(
        json!({"ok": true, "entries": [
            {"id":"a","name":"svc-a","disabled":false,"group":false,"fiber":1},
            {"id":"b","name":"svc-b","disabled":true,"group":false,"fiber":null},
            {"id":"g","name":"grp","disabled":false,"group":true,"fiber":null}
        ]}),
        json!({"ok": true, "plugins": [{"pluginId":"p1"},{"pluginId":"p2"},{"pluginId":"p3"}]}),
    );
    let r = p
        .handle("panel-runtime-status", "status", br#"{}"#, None)
        .unwrap();
    assert_eq!(r["ok"], true, "{r}");
    let items = r["value"]["items"].as_array().expect("items 数组");
    let item = |label: &str| -> &Value {
        items
            .iter()
            .find(|i| i["label"] == label)
            .unwrap_or_else(|| panic!("缺条目 {label}: {items:?}"))
    };
    assert_eq!(item("loader 条目")["value"], 2, "group 不计入条目数");
    assert_eq!(item("fiber 活跃")["value"], 1);
    assert_eq!(item("fiber 活跃")["tone"], "ok");
    assert_eq!(item("禁用")["value"], 1);
    assert_eq!(item("禁用")["tone"], "warn", "disabled>0 必须 warn");
    assert_eq!(item("动态包")["value"], 3);
}

/// 诚实纪律：任一服务失败 → 整体 fail-loud，不部分伪造。
#[test]
fn status_fail_loud_when_any_service_down() {
    let p = plugin(
        json!({"ok": true, "entries": []}),
        json!({"ok": false, "error": {"code": "service-down", "message": "dynamic down"}}),
    );
    let r = p
        .handle("panel-runtime-status", "status", br#"{}"#, None)
        .unwrap();
    assert_eq!(r["ok"], false, "部分失败不得报成功：{r}");
    assert_eq!(r["error"]["code"], "service-down");
    assert!(
        r["value"]["items"].is_null(),
        "错误响应不得夹带半真半假的 items：{r}"
    );
}

#[test]
fn unknown_endpoint_fail_loud() {
    let p = plugin(json!({"ok": true, "entries": []}), json!({"ok": true, "plugins": []}));
    let r = p
        .handle("panel-runtime-status", "nope", br#"{}"#, None)
        .unwrap();
    assert_eq!(r["ok"], false);
    assert!(r.to_string().contains("nope"));
}

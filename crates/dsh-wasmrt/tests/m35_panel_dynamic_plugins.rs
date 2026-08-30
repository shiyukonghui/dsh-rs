// 面板改写 #3（D-188）：panel-dynamic-plugins 服务装配单元——动态插件清单卡。
//
// 改写型第三次复制：describeUI 与静态 ui.json 一份契约；list 端点经 host-services
// "dynamicPlugins" 投影出行（state: activeRun→running/否则 defined）；服务失败透传。
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use dsh_core::Value;
use dsh_wasmrt::{RemoteServiceProjector, WasmRemoteEndpointPlugin};
use serde_json::json;

fn component() -> Vec<u8> {
    let manifest: PathBuf =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../wasm-plugins/panel-dynamic-plugins");
    let wasm_path =
        manifest.join("target/wasm32-wasip1/debug/panel_dynamic_plugins_plugin.wasm");
    if !wasm_path.exists() {
        let status = Command::new("cargo")
            .env("CARGO_NET_OFFLINE", "true")
            .args(["component", "build", "--manifest-path"])
            .arg(manifest.join("Cargo.toml"))
            .status()
            .expect("run cargo component build for panel-dynamic-plugins");
        assert!(status.success(), "panel-dynamic-plugins build failed");
    }
    std::fs::read(wasm_path).expect("read panel-dynamic-plugins component")
}

/// "dynamicPlugins" 服务桩（形状与 RemoteHost 投影同构）。
struct DynProjector {
    response: Value,
}

impl RemoteServiceProjector for DynProjector {
    fn get(&self, service: &str, _payload: &[u8]) -> Vec<u8> {
        match service {
            "dynamicPlugins" => serde_json::to_vec(&self.response).unwrap_or_default(),
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

fn plugin(response: Value) -> WasmRemoteEndpointPlugin {
    let projector = Rc::new(DynProjector { response });
    WasmRemoteEndpointPlugin::new(
        "panel-dynamic-plugins",
        &component(),
        Default::default(),
        Some(projector),
    )
    .expect("panel-dynamic-plugins plugin constructs")
}

#[test]
fn describe_ui_returns_valid_list_declaration() {
    let p = plugin(json!({"ok": true, "plugins": []}));
    let r = p
        .handle("panel-dynamic-plugins", "describeUI", br#"{}"#, None)
        .unwrap();
    assert_eq!(r["ok"], true, "{r}");
    let decl = &r["value"];
    assert_eq!(decl["$schema"], "dsh.panel-ui/v2");
    assert_eq!(decl["kind"], "card");
    assert_eq!(decl["cardId"], "panel-dynamic-plugins.list");
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
    assert_eq!(view["kind"], "list");
    assert_eq!(view["rowsPath"], "items", "数据面位置必须显式");
    assert_eq!(
        view["dataRpc"],
        json!(["panel-dynamic-plugins", "list"]),
        "dataRpc 显式"
    );
    let cols = view["columns"].as_array().expect("columns 数组");
    assert!(!cols.is_empty());
    for c in cols {
        // D-225：label 位=LocalizedText 契约（字符串 | 非空字符串值对象）。
        let txt = |v: &Value| v.is_string() || v.as_object().is_some_and(|m| !m.is_empty() && m.values().all(Value::is_string));
        assert!(c["key"].as_str().is_some() && txt(&c["label"]), "{c:?}");
    }
}

#[test]
fn static_ui_json_matches_describe_ui() {
    let p = plugin(json!({"ok": true, "plugins": []}));
    let r = p
        .handle("panel-dynamic-plugins", "describeUI", br#"{}"#, None)
        .unwrap();
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../wasm-plugins/panel-dynamic-plugins/web/ui.json");
    let static_decl: Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("read web/ui.json"))
            .expect("ui.json is JSON");
    assert_eq!(static_decl, r["value"], "一份契约");
}

/// 行投影：state = activeRun 有无（running/defined）；name 取当前包名。
#[test]
fn list_projects_dynamic_plugins() {
    let p = plugin(json!({"ok": true, "plugins": [
        {
            "pluginId": "hello", "agentId": "default",
            "packages": [
                {"packageId": "p1", "name": "Hello v1", "purpose": "demo", "hasHostHalf": true, "hasClientHalf": false},
                {"packageId": "p2", "name": "Hello v2", "purpose": "demo", "hasHostHalf": true, "hasClientHalf": false}
            ],
            "currentPackageId": "p2",
            "activeRun": {"pluginRunId": "dyn:hello", "packageId": "p2"}
        },
        {
            "pluginId": "idle-one", "agentId": "default",
            "packages": [{"packageId": "q1", "name": "Idle", "purpose": "idle demo", "hasHostHalf": true, "hasClientHalf": false}],
            "currentPackageId": "q1"
        }
    ]}));
    let r = p
        .handle("panel-dynamic-plugins", "list", br#"{}"#, None)
        .unwrap();
    assert_eq!(r["ok"], true, "{r}");
    let items = r["value"]["items"].as_array().expect("items 数组");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["pluginId"], "hello");
    assert_eq!(items[0]["name"], "Hello v2", "name 取 currentPackageId 对应包");
    assert_eq!(items[0]["state"], "running", "activeRun → running");
    assert_eq!(items[1]["name"], "Idle");
    assert_eq!(items[1]["state"], "defined", "无 activeRun → defined");
}

/// 诚实纪律：服务失败 → ok:false 透传，不伪造空表。
#[test]
fn list_service_failure_is_fail_loud() {
    let p = plugin(json!({"ok": false, "error": {"code": "service-down", "message": "down"}}));
    let r = p
        .handle("panel-dynamic-plugins", "list", br#"{}"#, None)
        .unwrap();
    assert_eq!(r["ok"], false, "服务失败不得报成功：{r}");
    assert_eq!(r["error"]["code"], "service-down");
    assert!(r["value"]["items"].is_null(), "错误响应不得夹带伪造空表：{r}");
}

#[test]
fn unknown_endpoint_fail_loud() {
    let p = plugin(json!({"ok": true, "plugins": []}));
    let r = p
        .handle("panel-dynamic-plugins", "nope", br#"{}"#, None)
        .unwrap();
    assert_eq!(r["ok"], false);
    assert!(r.to_string().contains("nope"));
}

// ---- C6（D-189）：写端点 stop/undefine（row.pluginId 校验 + 宿主 set 透传） ----

/// set 记录桩：记录 (service, payload) 调用，返回固定响应。
struct SetDynProjector {
    set_calls: RefCell<Vec<(String, Value)>>,
    set_response: Value,
}

impl RemoteServiceProjector for SetDynProjector {
    fn get(&self, service: &str, _payload: &[u8]) -> Vec<u8> {
        match service {
            "dynamicPlugins" => serde_json::to_vec(&json!({"ok": true, "plugins": []})).unwrap(),
            _ => serde_json::to_vec(&json!({"ok": false, "error": {"code": "unknown-service"}})).unwrap(),
        }
    }
    fn set(&self, service: &str, payload: &[u8]) -> Vec<u8> {
        let parsed: Value = serde_json::from_slice(payload).unwrap_or(Value::Null);
        self.set_calls
            .borrow_mut()
            .push((service.to_string(), parsed));
        serde_json::to_vec(&self.set_response).unwrap_or_default()
    }
}

fn plugin_set(set_response: Value) -> (WasmRemoteEndpointPlugin, Rc<SetDynProjector>) {
    let projector = Rc::new(SetDynProjector {
        set_calls: RefCell::new(vec![]),
        set_response,
    });
    let p = WasmRemoteEndpointPlugin::new(
        "panel-dynamic-plugins",
        &component(),
        Default::default(),
        Some(projector.clone()),
    )
    .expect("plugin constructs");
    (p, projector)
}

/// 身份校验（渲染器不是安全边界）：坏 body 一律 fail-loud 且**不触达宿主服务**。
#[test]
fn stop_requires_row_plugin_id_fail_loud() {
    let (p, calls) = plugin_set(json!({"ok": true}));
    for body in [
        "{}",
        r#"{"row":{}}"#,
        r#"{"row":{"pluginId":""}}"#,
        r#"{"row":{"pluginId":7}}"#,
        r#"{"pluginId":"sneaky"}"#,
    ] {
        let r = p
            .handle("panel-dynamic-plugins", "stop", body.as_bytes(), None)
            .unwrap();
        assert_eq!(r["ok"], false, "坏 body 必须 fail-loud: {body} -> {r}");
    }
    assert!(
        calls.set_calls.borrow().is_empty(),
        "校验失败不得触达宿主 set 服务（不猜测身份）"
    );
}

#[test]
fn stop_passthrough_success() {
    let (p, calls) = plugin_set(json!({"ok": true}));
    let r = p
        .handle(
            "panel-dynamic-plugins",
            "stop",
            br#"{"row":{"pluginId":"hello","state":"running"}}"#,
            None,
        )
        .unwrap();
    assert_eq!(r["ok"], true, "{r}");
    let recorded = calls.set_calls.borrow();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].0, "dynamicStop");
    assert_eq!(recorded[0].1, json!({"pluginId": "hello"}), "payload 只带身份字段");
}

#[test]
fn stop_service_failure_passthrough() {
    let (p, _calls) = plugin_set(json!({
        "ok": false,
        "error": { "code": "internal", "message": "dynamic plugin ghost is not running" }
    }));
    let r = p
        .handle(
            "panel-dynamic-plugins",
            "stop",
            br#"{"row":{"pluginId":"ghost"}}"#,
            None,
        )
        .unwrap();
    assert_eq!(r["ok"], false, "宿主失败必须透传：{r}");
    assert_eq!(r["error"]["code"], "internal");
    assert!(r["error"]["message"].as_str().unwrap().contains("not running"));
}

#[test]
fn undefine_passthrough_and_validation() {
    let (p, calls) = plugin_set(json!({"ok": true}));
    let r = p
        .handle(
            "panel-dynamic-plugins",
            "undefine",
            br#"{"row":{"pluginId":"hello"}}"#,
            None,
        )
        .unwrap();
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(calls.set_calls.borrow()[0].0, "dynamicUndefine");
    let r = p
        .handle("panel-dynamic-plugins", "undefine", br#"{"row":{}}"#, None)
        .unwrap();
    assert_eq!(r["ok"], false, "缺身份 fail-loud：{r}");
    assert_eq!(calls.set_calls.borrow().len(), 1, "坏 body 不再触达服务");
}

/// 声明层：rowActions 齐（启用无确认=非破坏；停止/卸载 confirm:true=破坏必确认）。
#[test]
fn declaration_carries_confirm_row_actions() {
    let p = plugin(json!({"ok": true, "plugins": []}));
    let r = p
        .handle("panel-dynamic-plugins", "describeUI", br#"{}"#, None)
        .unwrap();
    let actions = r["value"]["view"]["rowActions"].as_array().expect("rowActions");
    assert_eq!(actions.len(), 3, "{actions:?}");
    for a in actions {
        assert_eq!(a["scope"], "row");
        let rpc = a["rpc"].as_array().unwrap();
        assert_eq!(rpc[0], "panel-dynamic-plugins");
        if a["name"] == "activate" {
            assert!(a.get("confirm").is_none(), "启用非破坏，无需确认");
        } else {
            assert_eq!(a["confirm"], true, "破坏性动作必须声明确认");
        }
    }
}

/// D-202：启用端点自校验——缺 packageId 必须 fail-loud 且绝不触达宿主服务。
#[test]
fn activate_requires_package_id_fail_loud() {
    let p = plugin(json!({"ok": true, "plugins": []}));
    let r = p
        .handle("panel-dynamic-plugins", "activate", br#"{"row":{"pluginId":"x"}}"#, None)
        .unwrap();
    assert_eq!(r["ok"], false, "缺 packageId 必须红: {r}");
    assert!(r["error"]["message"]
        .as_str()
        .unwrap()
        .contains("packageId"));
}

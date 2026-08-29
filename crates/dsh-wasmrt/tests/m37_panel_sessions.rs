// 面板改写 #5（D-191）：panel-sessions 服务装配单元——会话清单卡（session 分类首卡）。
//
// 改写型第五次复制：describeUI 与静态 ui.json 一份契约；list 端点经 host-services
// "sessionCandidates" 投影出行（零加工直传 {sessionId,label,createdAt}——时间格式化属
// 渲染器演进，双权威禁令）；服务失败透传，不伪造空表。
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use dsh_core::Value;
use dsh_wasmrt::{RemoteServiceProjector, WasmRemoteEndpointPlugin};
use serde_json::json;

fn component() -> Vec<u8> {
    let manifest: PathBuf =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../wasm-plugins/panel-sessions");
    let wasm_path = manifest.join("target/wasm32-wasip1/debug/panel_sessions_plugin.wasm");
    if !wasm_path.exists() {
        let status = Command::new("cargo")
            .env("CARGO_NET_OFFLINE", "true")
            .args(["component", "build", "--manifest-path"])
            .arg(manifest.join("Cargo.toml"))
            .status()
            .expect("run cargo component build for panel-sessions");
        assert!(status.success(), "panel-sessions build failed");
    }
    std::fs::read(wasm_path).expect("read panel-sessions component")
}

struct CandProjector {
    response: Value,
    calls: RefCell<usize>,
}

impl RemoteServiceProjector for CandProjector {
    fn get(&self, service: &str, _payload: &[u8]) -> Vec<u8> {
        *self.calls.borrow_mut() += 1;
        match service {
            "sessionCandidates" => serde_json::to_vec(&self.response).unwrap_or_default(),
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

fn plugin(response: Value) -> (WasmRemoteEndpointPlugin, Rc<CandProjector>) {
    let projector = Rc::new(CandProjector {
        response,
        calls: RefCell::new(0),
    });
    let p = WasmRemoteEndpointPlugin::new(
        "panel-sessions",
        &component(),
        Default::default(),
        Some(projector.clone()),
    )
    .expect("panel-sessions plugin constructs");
    (p, projector)
}

#[test]
fn describe_ui_returns_valid_list_declaration() {
    let (p, _c) = plugin(json!({"ok": true, "candidates": []}));
    let r = p
        .handle("panel-sessions", "describeUI", br#"{}"#, None)
        .unwrap();
    assert_eq!(r["ok"], true, "{r}");
    let decl = &r["value"];
    assert_eq!(decl["$schema"], "dsh/plugin-ui/v2");
    assert_eq!(decl["kind"], "card");
    assert_eq!(decl["cardId"], "panel-sessions.list");
    assert_eq!(decl["type"], "session", "会话相关归 session 分类（D-181 语义表）");
    let size = decl["size"].as_object().expect("size 对象");
    assert!(size["w"].as_u64().unwrap() <= 4 && size["h"].as_u64().unwrap() <= 8);
    assert!(size.get("x").is_none() && size.get("y").is_none());
    let view = &decl["view"];
    assert_eq!(view["kind"], "list");
    assert_eq!(view["rowsPath"], "items");
    assert_eq!(view["dataRpc"], json!(["panel-sessions", "list"]));
    assert!(!view["columns"].as_array().unwrap().is_empty());
}

#[test]
fn static_ui_json_matches_describe_ui() {
    let (p, _c) = plugin(json!({"ok": true, "candidates": []}));
    let r = p
        .handle("panel-sessions", "describeUI", br#"{}"#, None)
        .unwrap();
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../wasm-plugins/panel-sessions/web/ui.json");
    let static_decl: Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("read web/ui.json"))
            .expect("ui.json is JSON");
    assert_eq!(static_decl, r["value"], "一份契约");
}

/// 行投影零加工：候选字段原样入行（时间格式化属渲染器演进）。
#[test]
fn list_projects_session_candidates_verbatim() {
    let (p, calls) = plugin(json!({"ok": true, "candidates": [
        {"sessionId": "s-1", "label": "s-1", "createdAt": 1760000000000u64},
        {"sessionId": "s-2", "label": "s-2", "createdAt": 1760000009000u64}
    ]}));
    let r = p
        .handle("panel-sessions", "list", br#"{}"#, None)
        .unwrap();
    assert_eq!(r["ok"], true, "{r}");
    let items = r["value"]["items"].as_array().expect("items");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["sessionId"], "s-1");
    assert_eq!(items[0]["createdAt"], 1760000000000u64, "epoch 原样，不格式化");
    assert_eq!(items[1]["sessionId"], "s-2");
    assert_eq!(*calls.calls.borrow(), 1, "单服务探测");
}

#[test]
fn list_service_failure_is_fail_loud() {
    let (p, _calls) = plugin(json!({"ok": false, "error": {"code": "sink-down", "message": "down"}}));
    let r = p
        .handle("panel-sessions", "list", br#"{}"#, None)
        .unwrap();
    assert_eq!(r["ok"], false, "服务失败不得报成功：{r}");
    assert_eq!(r["error"]["code"], "sink-down");
    assert!(r["value"]["items"].is_null(), "错误响应不得夹带伪造空表：{r}");
}

#[test]
fn unknown_endpoint_fail_loud() {
    let (p, _c) = plugin(json!({"ok": true, "candidates": []}));
    let r = p
        .handle("panel-sessions", "nope", br#"{}"#, None)
        .unwrap();
    assert_eq!(r["ok"], false);
    assert!(r.to_string().contains("nope"));
}

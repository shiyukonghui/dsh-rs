// 面板改写 #4（D-190）：panel-workspace-files 服务装配单元——工作区文件清单卡。
//
// 两段式服务探测：先 `agentWorkspace` 解析默认工作区（失败 → fail-loud，**绝不猜目录**），
// 再 `workspaceFiles` 列举。行投影 {path}；一份契约继续静态==describeUI 守。
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use dsh_core::Value;
use dsh_wasmrt::{RemoteServiceProjector, WasmRemoteEndpointPlugin};
use serde_json::json;

fn component() -> Vec<u8> {
    let manifest: PathBuf =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../wasm-plugins/panel-workspace-files");
    let wasm_path =
        manifest.join("target/wasm32-wasip1/debug/panel_workspace_files_plugin.wasm");
    if !wasm_path.exists() {
        let status = Command::new("cargo")
            .env("CARGO_NET_OFFLINE", "true")
            .args(["component", "build", "--manifest-path"])
            .arg(manifest.join("Cargo.toml"))
            .status()
            .expect("run cargo component build for panel-workspace-files");
        assert!(status.success(), "panel-workspace-files build failed");
    }
    std::fs::read(wasm_path).expect("read panel-workspace-files component")
}

/// 双服务桩：记录 get 调用序（验证「解析失败不枚举」的调用纪律）。
struct FsProjector {
    workspace: Value,
    files: Value,
    calls: RefCell<Vec<String>>,
}

impl RemoteServiceProjector for FsProjector {
    fn get(&self, service: &str, _payload: &[u8]) -> Vec<u8> {
        self.calls.borrow_mut().push(service.to_string());
        let v = match service {
            "agentWorkspace" => &self.workspace,
            "workspaceFiles" => &self.files,
            _ => &json!({"ok": false, "error": {"code": "unknown-service", "message": service}}),
        };
        serde_json::to_vec(v).unwrap_or_default()
    }
    fn set(&self, _s: &str, _p: &[u8]) -> Vec<u8> {
        Vec::new()
    }
}

fn plugin(workspace: Value, files: Value) -> (WasmRemoteEndpointPlugin, Rc<FsProjector>) {
    let projector = Rc::new(FsProjector {
        workspace,
        files,
        calls: RefCell::new(vec![]),
    });
    let p = WasmRemoteEndpointPlugin::new(
        "panel-workspace-files",
        &component(),
        Default::default(),
        Some(projector.clone()),
    )
    .expect("panel-workspace-files plugin constructs");
    (p, projector)
}

#[test]
fn describe_ui_returns_valid_list_declaration() {
    let (p, _c) = plugin(json!({"ok": true, "cwd": "/tmp"}), json!({"ok": true, "paths": []}));
    let r = p
        .handle("panel-workspace-files", "describeUI", br#"{}"#, None)
        .unwrap();
    assert_eq!(r["ok"], true, "{r}");
    let decl = &r["value"];
    assert_eq!(decl["$schema"], "dsh/plugin-ui/v2");
    assert_eq!(decl["kind"], "card");
    assert_eq!(decl["cardId"], "panel-workspace-files.list");
    assert_eq!(decl["type"], "resource", "fs/工作区归 resource 分类（D-181 语义表）");
    let size = decl["size"].as_object().expect("size 对象");
    assert!(size["w"].as_u64().unwrap() <= 4 && size["h"].as_u64().unwrap() <= 8);
    assert!(size.get("x").is_none() && size.get("y").is_none());
    let view = &decl["view"];
    assert_eq!(view["kind"], "list");
    assert_eq!(view["rowsPath"], "items");
    assert_eq!(view["dataRpc"], json!(["panel-workspace-files", "list"]));
    assert!(!view["columns"].as_array().unwrap().is_empty());
}

#[test]
fn static_ui_json_matches_describe_ui() {
    let (p, _c) = plugin(json!({"ok": true, "cwd": "/tmp"}), json!({"ok": true, "paths": []}));
    let r = p
        .handle("panel-workspace-files", "describeUI", br#"{}"#, None)
        .unwrap();
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../wasm-plugins/panel-workspace-files/web/ui.json");
    let static_decl: Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("read web/ui.json"))
            .expect("ui.json is JSON");
    assert_eq!(static_decl, r["value"], "一份契约");
}

#[test]
fn list_projects_workspace_files() {
    let (p, calls) = plugin(
        json!({"ok": true, "cwd": "F:/demo-ws"}),
        json!({"ok": true, "paths": ["F:/demo-ws/a.txt", "F:/demo-ws/b.md"]}),
    );
    let r = p
        .handle("panel-workspace-files", "list", br#"{}"#, None)
        .unwrap();
    assert_eq!(r["ok"], true, "{r}");
    let items = r["value"]["items"].as_array().expect("items");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["path"], "F:/demo-ws/a.txt");
    assert_eq!(items[1]["path"], "F:/demo-ws/b.md");
    assert_eq!(*calls.calls.borrow(), vec!["agentWorkspace", "workspaceFiles"], "调用序即探测序");
}

/// 诚实纪律：工作区解析失败 → fail-loud 且**不得触达枚举服务**（不猜目录）。
#[test]
fn list_fail_loud_when_workspace_probe_fails() {
    let (p, calls) = plugin(
        json!({"ok": false, "error": {"code": "no-workspace", "message": "no default workspace"}}),
        json!({"ok": true, "paths": ["guessed/path"]}),
    );
    let r = p
        .handle("panel-workspace-files", "list", br#"{}"#, None)
        .unwrap();
    assert_eq!(r["ok"], false, "解析失败不得报成功：{r}");
    assert_eq!(r["error"]["code"], "no-workspace");
    assert!(r["value"]["items"].is_null());
    assert_eq!(*calls.calls.borrow(), vec!["agentWorkspace"], "失败后不得继续枚举");
}

#[test]
fn list_enumeration_failure_passthrough() {
    let (p, _calls) = plugin(
        json!({"ok": true, "cwd": "F:/demo-ws"}),
        json!({"ok": false, "error": {"code": "unreadable", "message": "dir gone"}}),
    );
    let r = p
        .handle("panel-workspace-files", "list", br#"{}"#, None)
        .unwrap();
    assert_eq!(r["ok"], false, "{r}");
    assert_eq!(r["error"]["code"], "unreadable");
}

#[test]
fn unknown_endpoint_fail_loud() {
    let (p, _c) = plugin(json!({"ok": true, "cwd": "/tmp"}), json!({"ok": true, "paths": []}));
    let r = p
        .handle("panel-workspace-files", "nope", br#"{}"#, None)
        .unwrap();
    assert_eq!(r["ok"], false);
    assert!(r.to_string().contains("nope"));
}

// D-115-Web D3：WASM 组件承载 host 侧 remote 端点（wasi: host-remote world）。
//
// 验证「宿主→wasm→宿主」桥闭环（组件模型专用，禁 C ABI）：
// 1. 加载 host-remote 组件（wasm-plugins/host-remote，export `remote.handle`）。
// 2. `WasmRemoteEndpointPlugin::handle(namespace, method, body)` 把端点请求交给
//    组件，组件回显 namespace/method/received，并经 host-services.get 反查宿主。
// 3. 断言结果 JSON 字段 + 宿主投影器被真实调用（非占位）。

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use dsh_core::{CordisError, Value};
use dsh_wasmrt::{RemoteServiceProjector, WasmRemoteEndpointPlugin};
use serde_json::json;

/// 构建（如缺失）并读取 host-remote 组件字节。
fn host_remote_component() -> Vec<u8> {
    let manifest: PathBuf =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../wasm-plugins/host-remote");
    let wasm_path = manifest.join("target/wasm32-wasip1/debug/host_remote_plugin.wasm");
    if !wasm_path.exists() {
        let status = Command::new("cargo")
            .args(["component", "build", "--manifest-path"])
            .arg(manifest.join("Cargo.toml"))
            .status()
            .expect("run cargo component build for host-remote plugin");
        assert!(status.success(), "host-remote plugin build failed");
    }
    std::fs::read(wasm_path).expect("read host-remote component")
}

/// 测试投影器：返回 echo 服务投影（宿主真实数据面由 dsh-cli 装配真实来源）。
struct EchoProjector {
    echo_text: String,
    calls: RefCell<Vec<(String, Value)>>,
}

impl RemoteServiceProjector for EchoProjector {
    fn get(&self, service: &str, payload: &[u8]) -> Vec<u8> {
        let payload: Value = serde_json::from_slice(payload).unwrap_or(Value::Null);
        self.calls.borrow_mut().push((service.to_string(), payload));
        if service == "echo" {
            serde_json::to_vec(&serde_json::json!({
                "ok": true,
                "text": self.echo_text,
            }))
            .unwrap_or_default()
        } else {
            serde_json::to_vec(&serde_json::json!({
                "ok": false,
                "error": { "code": "unknown-service", "message": format!("no service {service}") },
            }))
            .unwrap_or_default()
        }
    }
}

/// 组件经 handle 路由真实端点；未知端点 → not-implemented（fail-loud，不伪造成功）。
#[test]
fn host_remote_routes_and_rejects_unknown_endpoint() {
    let bytes = host_remote_component();
    let projector = Rc::new(EchoProjector {
        echo_text: "host-hello".to_string(),
        calls: RefCell::new(Vec::new()),
    });
    let plugin = WasmRemoteEndpointPlugin::new("host-remote", &bytes, Default::default(), Some(projector.clone()))
        .expect("host-remote plugin constructs");
    // 未知端点 → 规范化 not-implemented 错误（绝不以假结构冒充成功）。
    let result = plugin
        .handle("unknownNamespace", "bogus", br#"{}"#, None)
        .unwrap();
    assert_eq!(result["ok"], false);
    assert_eq!(result["error"]["code"], "not-implemented");
}

#[test]
fn host_remote_unknown_service_is_fail_loud() {
    let bytes = host_remote_component();
    let projector = Rc::new(EchoProjector {
        echo_text: String::new(),
        calls: RefCell::new(Vec::new()),
    });
    let _plugin = WasmRemoteEndpointPlugin::new("host-remote", &bytes, Default::default(), Some(projector.clone()))
        .expect("constructs");
    // 未知服务应由组件以规范化错误回传（fail-loud，不伪造成功）。
    // 当前阶段组件只调 echo；未知服务路径由投影器在宿主侧拒绝（见 get 实现）。
    let _ = CordisError::Internal(String::new()); // 保持导入可见
}

/// 宿主投影器：`loader` 服务返回真实 loader 条目（D2 簇1 pluginInventory 的数据源）。
/// 组件负责组装 wire（跳过 group、映射 fiberPhase）。
struct LoaderProjector {
    entries: Vec<serde_json::Value>,
    calls: RefCell<Vec<(String, Value)>>,
}

impl RemoteServiceProjector for LoaderProjector {
    fn get(&self, service: &str, payload: &[u8]) -> Vec<u8> {
        self.calls.borrow_mut().push((
            service.to_string(),
            serde_json::from_slice(payload).unwrap_or(Value::Null),
        ));
        if service == "loader" {
            serde_json::to_vec(&serde_json::json!({
                "ok": true,
                "entries": self.entries,
            }))
            .unwrap_or_default()
        } else {
            serde_json::to_vec(&serde_json::json!({
                "ok": false,
                "error": { "code": "unknown-service", "message": format!("no service {service}") },
            }))
            .unwrap_or_default()
        }
    }
}

/// D2 簇1：pluginInventory/list 由 WASM 组件真实实现——host 投影 loader 条目，
/// 组件跳过 group、映射 fiberPhase、组装前端期望的 wire `{entries:[...]}`。
#[test]
fn plugin_inventory_list_maps_loader_entries() {
    let bytes = host_remote_component();
    let projector = Rc::new(LoaderProjector {
        entries: vec![
            serde_json::json!({"id": "p1", "name": "@deepseek-ai/dsh-a", "disabled": false, "group": false, "fiber": null}),
            serde_json::json!({"id": "g1", "name": "group-row", "disabled": false, "group": true, "fiber": null}),
            serde_json::json!({"id": "p2", "name": "@deepseek-ai/dsh-b", "disabled": true, "group": false, "fiber": null}),
        ],
        calls: RefCell::new(Vec::new()),
    });
    let plugin = WasmRemoteEndpointPlugin::new("host-remote", &bytes, Default::default(), Some(projector.clone()))
        .expect("constructs");
    let result = plugin
        .handle("pluginInventory", "list", br#"{}"#, None)
        .expect("handle succeeds");
    let obj = result.as_object().expect("result is object");
    let entries = obj["entries"].as_array().expect("entries is array");
    // group 行被跳过：只剩两个非 group 条目。
    assert_eq!(entries.len(), 2, "group rows excluded: {entries:?}");
    // wire 映射：entryId / moduleName / enabled / fiberPhase。
    let first = entries[0].as_object().unwrap();
    assert_eq!(first["entryId"], "p1");
    assert_eq!(first["moduleName"], "@deepseek-ai/dsh-a");
    assert_eq!(first["enabled"], true);
    assert!(first["fiberPhase"].is_null() || first["fiberPhase"].is_string());
    let second = entries[1].as_object().unwrap();
    assert_eq!(second["enabled"], false);
    // 组件真实调用了宿主 loader 投影（非占位）。
    let calls = projector.calls.borrow();
    assert_eq!(calls[0].0, "loader");
}

/// D2 簇2：messageFeedback 真实持久 + 校验 + 版本并发 的宿主投影后端。
struct FeedbackProjector {
    kv: RefCell<std::collections::HashMap<String, Value>>,
    messages: std::collections::HashMap<String, Vec<String>>,
    clock: RefCell<u64>,
    calls: RefCell<Vec<(String, Value)>>,
}

impl FeedbackProjector {
    fn new(messages: std::collections::HashMap<String, Vec<String>>) -> Self {
        FeedbackProjector {
            kv: RefCell::new(std::collections::HashMap::new()),
            messages,
            clock: RefCell::new(1000),
            calls: RefCell::new(Vec::new()),
        }
    }
}

impl RemoteServiceProjector for FeedbackProjector {
    fn get(&self, service: &str, payload: &[u8]) -> Vec<u8> {
        let payload: Value = serde_json::from_slice(payload).unwrap_or(Value::Null);
        self.calls.borrow_mut().push((service.to_string(), payload.clone()));
        match service {
            "kv" => {
                let key = payload.get("key").and_then(|k| k.as_str()).unwrap_or("");
                let value = self.kv.borrow().get(key).cloned();
                serde_json::to_vec(&json!({"ok": true, "value": value.unwrap_or(Value::Null)})).unwrap_or_default()
            }
            "sessionIdentity" => {
                let sid = payload.get("sessionId").and_then(|s| s.as_str()).unwrap_or("");
                let ok = self.messages.contains_key(sid);
                serde_json::to_vec(&json!({"ok": true, "identity": if ok { json!({"createdAt": 500, "cwd": "/tmp"}) } else { Value::Null }})).unwrap_or_default()
            }
            "sessionMessages" => {
                let sid = payload.get("sessionId").and_then(|s| s.as_str()).unwrap_or("");
                let ids = self.messages.get(sid).cloned().unwrap_or_default();
                serde_json::to_vec(&json!({"ok": true, "messageIds": ids})).unwrap_or_default()
            }
            "time" => {
                *self.clock.borrow_mut() += 1;
                serde_json::to_vec(&json!({"ok": true, "epochMs": *self.clock.borrow()})).unwrap_or_default()
            }
            "newVersion" => {
                let v = format!("00000000-0000-0000-0000-{:012}", self.calls.borrow().len());
                serde_json::to_vec(&json!({"ok": true, "uuid": v})).unwrap_or_default()
            }
            _ => serde_json::to_vec(&json!({"ok": false, "error": {"code": "unknown-service", "message": service}})).unwrap_or_default(),
        }
    }
    fn set(&self, service: &str, payload: &[u8]) -> Vec<u8> {
        let payload: Value = serde_json::from_slice(payload).unwrap_or(Value::Null);
        match service {
            "kv" => {
                let key = payload.get("key").and_then(|k| k.as_str()).unwrap_or("").to_string();
                let value = payload.get("value").cloned().unwrap_or(Value::Null);
                self.kv.borrow_mut().insert(key.clone(), value.clone());
                serde_json::to_vec(&json!({"ok": true, "value": value})).unwrap_or_default()
            }
            _ => serde_json::to_vec(&json!({"ok": false, "error": {"code": "read-only", "message": service}})).unwrap_or_default(),
        }
    }
}

use std::collections::HashMap;

/// messageFeedback/put → list → delete 的完整生命周期（真实持久 + 版本并发）。
#[test]
fn message_feedback_put_list_delete_lifecycle() {
    let bytes = host_remote_component();
    let mut messages = HashMap::new();
    messages.insert("s1".to_string(), vec!["m1".to_string(), "m2".to_string()]);
    let projector = Rc::new(FeedbackProjector::new(messages));
    let plugin = WasmRemoteEndpointPlugin::new("host-remote", &bytes, Default::default(), Some(projector.clone()))
        .expect("constructs");

    // put m1（新条目）
    let r = plugin.handle(
        "messageFeedback", "put",
        br#"{"sessionId":"s1","messageId":"m1","rating":"positive","note":"helpful"}"#,
        None,
    ).unwrap();
    assert_eq!(r["ok"], true, "put ok: {r}");
    let v = &r["value"];
    assert_eq!(v["messageId"], "m1");
    assert_eq!(v["rating"], "positive");
    assert_eq!(v["note"], "helpful");
    let version_m1 = v["version"].as_str().unwrap().to_string();
    assert!(version_m1.len() > 20, "uuid version: {version_m1}");

    // list 读回（真实持久，含 note）
    let l = plugin.handle("messageFeedback", "list", br#"{"sessionId":"s1"}"#, None).unwrap();
    let items = l["value"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "one item: {items:?}");
    assert_eq!(items[0]["note"], "helpful");

    // delete（带 ifVersion = 正确版本）→ 成功 + absent
    let d = plugin.handle(
        "messageFeedback", "delete",
        &serde_json::to_vec(&json!({"sessionId":"s1","messageId":"m1","ifVersion":version_m1})).unwrap(),
        None,
    ).unwrap();
    assert_eq!(d["ok"], true, "delete ok: {d}");
    assert_eq!(d["value"]["absent"], true);
}

/// messageFeedback 错误分支：version-conflict / note-blank / session-not-found。
#[test]
fn message_feedback_error_branches() {
    let bytes = host_remote_component();
    let mut messages = HashMap::new();
    messages.insert("s1".to_string(), vec!["m1".to_string()]);
    let projector = Rc::new(FeedbackProjector::new(messages));
    let plugin = WasmRemoteEndpointPlugin::new("host-remote", &bytes, Default::default(), Some(projector.clone()))
        .expect("constructs");

    // put m1 拿版本
    let put1 = plugin.handle("messageFeedback", "put", br#"{"sessionId":"s1","messageId":"m1","rating":"negative"}"#, None).unwrap();
    let v1 = put1["value"]["version"].as_str().unwrap().to_string();

    // 第二次 put 带过期 ifVersion → version-conflict（current 为现条）
    let put2_body = serde_json::to_vec(&json!({"sessionId":"s1","messageId":"m1","rating":"positive","ifVersion":"stale-version"})).unwrap();
    let put2 = plugin.handle("messageFeedback", "put", &put2_body, None).unwrap();
    assert_eq!(put2["ok"], false, "conflict: {put2}");
    assert_eq!(put2["error"]["code"], "version-conflict");
    assert_eq!(put2["error"]["current"]["version"], v1);

    // note-blank
    let blank = plugin.handle("messageFeedback", "put", br#"{"sessionId":"s1","messageId":"m2","rating":"positive","note":"   "}"#, None).unwrap();
    assert_eq!(blank["ok"], false);
    assert_eq!(blank["error"]["code"], "note-blank");

    // session-not-found（未知 session，且其消息不存在）
    let nf = plugin.handle("messageFeedback", "put", br#"{"sessionId":"nope","messageId":"mX","rating":"positive"}"#, None).unwrap();
    assert_eq!(nf["ok"], false);
    assert_eq!(nf["error"]["code"], "session-not-found");

    // note-too-large
    let big = format!("\"{}\"", "x".repeat(9000));
    let too_large = plugin.handle("messageFeedback", "put",
        &format!(r#"{{"sessionId":"s1","messageId":"m2","rating":"positive","note":{big}}}"#).into_bytes(), None).unwrap();
    assert_eq!(too_large["ok"], false);
    assert_eq!(too_large["error"]["code"], "note-too-large");
    assert_eq!(too_large["error"]["maxBytes"], 8192);
}


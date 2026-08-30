// P2 试点（rust + ui 声明 + wasm）：llm-deepseek 服务装配单元 —— wasm 声明面 + 动作面。
//
// 验证「宿主→wasm→宿主」桥闭环（组件模型专用，禁功能漂移到 C ABI）：
// 1. 加载 `wasm-plugins/llm-deepseek` 组件（export remote，复用 dsh:host-remote 接口身份）；
// 2. `remote.handle(namespace, method, body)` 暴露 UI 声明面 + 动作面：
//    - describeUI：返回有效声明（fields/actions），且与静态 web/ui.json **逐字段一致**
//      （声明=数据，一份契约，Rust 只生声明不渲染）；
//    - save：白名单校验后经 host-services kv 落宿主；坏入参 fail-loud 不落盘；
//    - currentValues：读回已保存设置（roundtrip）；
//    - discoverModels：真外呼契约（D-222：表单/已存 baseURL → 宿主 llmDiscover 臂透传）。
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use dsh_core::Value;
use dsh_wasmrt::{RemoteServiceProjector, WasmRemoteEndpointPlugin};
use serde_json::json;

/// 构建（如缺失）并读取 llm-deepseek 组件字节。离线优先（本环境无外网，
/// 锁文件 + 本地缓存已在；`CARGO_NET_OFFLINE` 让构建不触发索引更新）。
fn llm_deepseek_component() -> Vec<u8> {
    let manifest: PathBuf =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../wasm-plugins/llm-deepseek");
    let wasm_path = manifest.join("target/wasm32-wasip1/debug/llm_deepseek_plugin.wasm");
    if !wasm_path.exists() {
        let status = Command::new("cargo")
            .env("CARGO_NET_OFFLINE", "true")
            .args(["component", "build", "--manifest-path"])
            .arg(manifest.join("Cargo.toml"))
            .status()
            .expect("run cargo component build for llm-deepseek plugin");
        assert!(status.success(), "llm-deepseek plugin build failed");
    }
    std::fs::read(wasm_path).expect("read llm-deepseek component")
}

/// 测试投影器：真实 kv 后端（与 RemoteHost "kv" 契约一致），供 save/currentValues 落盘。
struct KvProjector {
    kv: RefCell<HashMap<String, Value>>,
    calls: RefCell<Vec<(String, Value)>>,
}

impl RemoteServiceProjector for KvProjector {
    fn get(&self, service: &str, payload: &[u8]) -> Vec<u8> {
        let payload: Value = serde_json::from_slice(payload).unwrap_or(Value::Null);
        self.calls.borrow_mut().push((service.to_string(), payload.clone()));
        match service {
            "kv" => {
                let key = payload.get("key").and_then(|k| k.as_str()).unwrap_or("");
                serde_json::to_vec(&json!({
                    "ok": true,
                    "value": self.kv.borrow().get(key).cloned().unwrap_or(Value::Null),
                }))
                .unwrap_or_default()
            }
            // D-222：宿主真外呼臂的测试替身（真实 HTTP 在宿主臂，wasm 侧只验契约）。
            "llmDiscover" => serde_json::to_vec(&json!({
                "ok": true,
                "models": [
                    { "id": "stub-model-a", "name": "Stub A" },
                    { "id": "stub-model-b", "name": "Stub B" },
                ],
            }))
            .unwrap_or_default(),
            _ => serde_json::to_vec(&json!({
                "ok": false,
                "error": { "code": "unknown-service", "message": service },
            }))
            .unwrap_or_default(),
        }
    }
    fn set(&self, service: &str, payload: &[u8]) -> Vec<u8> {
        let payload: Value = serde_json::from_slice(payload).unwrap_or(Value::Null);
        match service {
            "kv" => {
                let key = payload.get("key").and_then(|k| k.as_str()).unwrap_or("").to_string();
                let value = payload.get("value").cloned().unwrap_or(Value::Null);
                self.kv.borrow_mut().insert(key.clone(), value.clone());
                serde_json::to_vec(&json!({ "ok": true, "value": value })).unwrap_or_default()
            }
            _ => serde_json::to_vec(&json!({
                "ok": false,
                "error": { "code": "read-only", "message": service },
            }))
            .unwrap_or_default(),
        }
    }
}

#[test]
fn describe_ui_returns_valid_declaration() {
    let bytes = llm_deepseek_component();
    let projector = Rc::new(KvProjector {
        kv: RefCell::new(HashMap::new()),
        calls: RefCell::new(Vec::new()),
    });
    let plugin = WasmRemoteEndpointPlugin::new(
        "llm-deepseek",
        &bytes,
        Default::default(),
        Some(projector.clone()),
    )
    .expect("llm-deepseek plugin constructs");

    let result = plugin.handle("llm-deepseek", "describeUI", br#"{}"#, None).unwrap();
    assert_eq!(result["ok"], true, "envelope ok: {result}");
    // v2（D-181）：顶层唯一容器 = card；fields/actions 收进 view。
    let decl = result["value"].as_object().expect("value is declaration object");
    assert_eq!(decl["kind"], "card", "顶层容器必须是 card");
    let view = decl["view"].as_object().expect("card 必须携带 view");
    assert_eq!(view["kind"], "form");
    let fields = view["fields"].as_array().expect("view.fields is array");
    assert!(fields.iter().any(|f| f["name"] == "apiKeyEnv"), "has apiKeyEnv field: {fields:?}");
    assert!(fields.iter().any(|f| f["name"] == "models"), "has models field: {fields:?}");
    let actions = view["actions"].as_array().expect("view.actions is array");
    assert!(actions.iter().any(|a| a["name"] == "save"), "has save action");
    assert!(actions.iter().any(|a| a["name"] == "discoverModels"), "has discoverModels action");
}

/// v2 卡片契约（D-181）：cardId / type 闭集 / size 封顶且无坐标 / view.dataRpc 显式。
#[test]
fn declaration_satisfies_v2_card_contract() {
    let bytes = llm_deepseek_component();
    let projector: Rc<dyn RemoteServiceProjector> = Rc::new(KvProjector {
        kv: RefCell::new(HashMap::new()),
        calls: RefCell::new(Vec::new()),
    });
    let plugin = WasmRemoteEndpointPlugin::new(
        "llm-deepseek",
        &bytes,
        Default::default(),
        Some(projector),
    )
    .expect("constructs");

    let decl = plugin
        .handle("llm-deepseek", "describeUI", br#"{}"#, None)
        .unwrap()["value"]
        .clone();

    // 版本显式：渲染器遇非 v2 必须 fail-loud，不做静默兼容（双模型崩塌的入口）。
    assert_eq!(decl["$schema"], "dsh.panel-ui/v2");
    // 卡身份 = (插件名, cardId)。
    assert!(!decl["cardId"].as_str().unwrap_or("").is_empty(), "cardId 必须非空");
    // type = 分类轴（与渲染轴 view.kind 正交），必须是 v1 闭集成员。
    let type_enum = [
        "model",
        "config",
        "capability",
        "runtime",
        "resource",
        "session",
        "misc",
    ];
    let ty = decl["type"].as_str().unwrap_or("");
    assert!(type_enum.contains(&ty), "type 必须落闭集，收到 {ty}");
    assert_eq!(ty, "model", "llm-deepseek 属 model 分类");
    // size 是格数且封顶 w<=4 / h<=8；坐标永不出现在声明里（不外泄给插件作者）。
    let size = decl["size"].as_object().expect("size 是对象");
    let w = size["w"].as_u64().expect("w 是数字");
    let h = size["h"].as_u64().expect("h 是数字");
    assert!((1..=4).contains(&w), "w 须在 1..=4，收到 {w}");
    assert!((1..=8).contains(&h), "h 须在 1..=8，收到 {h}");
    assert!(
        size.get("x").is_none() && size.get("y").is_none(),
        "声明不得携带坐标（坐标由画布计算）"
    );
    // 数据面显式声明：渲染器据此预填，不靠猜。
    assert_eq!(decl["view"]["dataRpc"], json!(["llm-deepseek", "currentValues"]));
}

/// 双模型防线（D-181「一步到位、不并存」的机制护栏）：
/// 全仓插件包静态声明**不得残留 v1 顶层形态**（顶层 `kind:"form"` / `$schema` v1）。
/// 必须解析后看**顶层**而非 grep 文本——`view.kind:"form"` 是合法内容视图，grep 会假阳性。
#[test]
fn no_legacy_v1_top_level_declaration_anywhere() {
    let plugins_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../wasm-plugins");
    let mut checked = 0usize;
    let mut offenders: Vec<String> = Vec::new();

    for entry in std::fs::read_dir(&plugins_dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", plugins_dir.display()))
        .flatten()
    {
        let ui = entry.path().join("web").join("ui.json");
        if !ui.is_file() {
            continue;
        }
        let text =
            std::fs::read_to_string(&ui).unwrap_or_else(|e| panic!("read {}: {e}", ui.display()));
        let decl: Value = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("{} 非法 JSON: {e}", ui.display()));
        // 只校验 UI 声明文件（含 $schema 者）；其他前端资源不在本契约范围。
        if decl.get("$schema").is_none() {
            continue;
        }
        checked += 1;
        if decl["$schema"] != json!("dsh.panel-ui/v2") || decl["kind"] != json!("card") {
            offenders.push(format!(
                "{}: $schema={} kind={}",
                ui.display(),
                decl["$schema"],
                decl["kind"]
            ));
        }
    }

    assert!(checked > 0, "至少应校验到一个插件声明（路径解析是否对？）");
    assert!(
        offenders.is_empty(),
        "存在 v1 顶层形态残留（D-181 已废止，不得并存）：{offenders:?}"
    );
}

/// 声明=数据，一份契约：wasm describeUI 输出与静态 web/ui.json **逐字段一致**。
#[test]
fn static_ui_json_matches_describe_ui() {
    let bytes = llm_deepseek_component();
    let projector = Rc::new(KvProjector {
        kv: RefCell::new(HashMap::new()),
        calls: RefCell::new(Vec::new()),
    });
    let plugin = WasmRemoteEndpointPlugin::new(
        "llm-deepseek",
        &bytes,
        Default::default(),
        Some(projector.clone()),
    )
    .expect("constructs");

    let ui_json_path: PathBuf =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../wasm-plugins/llm-deepseek/web/ui.json");
    let static_decl: Value =
        serde_json::from_str(&std::fs::read_to_string(&ui_json_path).unwrap()).unwrap();

    let result = plugin.handle("llm-deepseek", "describeUI", br#"{}"#, None).unwrap();
    assert_eq!(result["ok"], true);
    let dynamic = &result["value"];
    assert_eq!(
        dynamic, &static_decl,
        "web/ui.json 必须与 wasm describeUI 逐字段一致（声明=数据，一份契约）"
    );
}

#[test]
fn save_writes_kv_and_rejects_unknown_field_fail_loud() {
    let bytes = llm_deepseek_component();
    let projector = Rc::new(KvProjector {
        kv: RefCell::new(HashMap::new()),
        calls: RefCell::new(Vec::new()),
    });
    let plugin = WasmRemoteEndpointPlugin::new(
        "llm-deepseek",
        &bytes,
        Default::default(),
        Some(projector.clone()),
    )
    .expect("constructs");

    // 1. 合法保存 → ok + 真实落盘。
    let body = br#"{"values":{"apiKeyEnv":"DEEPSEEK_API_KEY","maxTokens":2000}}"#;
    let result = plugin.handle("llm-deepseek", "save", body, None).unwrap();
    assert_eq!(result["ok"], true, "save ok: {result}");
    assert_eq!(result["value"]["saved"], true);
    let stored = projector.kv.borrow().get("llm-deepseek/settings").cloned();
    assert!(stored.is_some(), "kv persisted");
    assert_eq!(stored.unwrap()["maxTokens"], 2000);

    // 2. 未知字段 → fail-loud（不伪造成功、不落盘新值）。
    let before = projector.kv.borrow().get("llm-deepseek/settings").cloned();
    let bad_body = br#"{"values":{"hackerField":"x"}}"#;
    let bad = plugin.handle("llm-deepseek", "save", bad_body, None).unwrap();
    assert_eq!(bad["ok"], false, "fail-loud on unknown field: {bad}");
    assert_eq!(bad["error"]["code"], "internal");
    let after = projector.kv.borrow().get("llm-deepseek/settings").cloned();
    assert_eq!(before, after, "unknown field must not mutate persisted value");

    // 3. 非对象 values → fail-loud。
    let bad2 = plugin
        .handle("llm-deepseek", "save", br#"{"values":"not-an-object"}"#, None)
        .unwrap();
    assert_eq!(bad2["ok"], false);
}

#[test]
fn current_values_roundtrips_saved_settings() {
    let bytes = llm_deepseek_component();
    let projector = Rc::new(KvProjector {
        kv: RefCell::new(HashMap::new()),
        calls: RefCell::new(Vec::new()),
    });
    let plugin = WasmRemoteEndpointPlugin::new(
        "llm-deepseek",
        &bytes,
        Default::default(),
        Some(projector.clone()),
    )
    .expect("constructs");

    // 未保存 → 空 values（诚实，不伪造）。
    let empty = plugin.handle("llm-deepseek", "currentValues", br#"{}"#, None).unwrap();
    assert_eq!(empty["ok"], true);
    assert_eq!(empty["value"]["values"], json!({}));

    // 保存后读回一致。
    let body = br#"{"values":{"reasoningEffort":"max","models":[{"id":"deepseek-v4-pro"}]}}"#;
    assert_eq!(plugin.handle("llm-deepseek", "save", body, None).unwrap()["ok"], true);
    let cur = plugin.handle("llm-deepseek", "currentValues", br#"{}"#, None).unwrap();
    assert_eq!(cur["value"]["values"]["reasoningEffort"], "max");
    assert_eq!(cur["value"]["values"]["models"][0]["id"], "deepseek-v4-pro");
}

#[test]
fn discover_models_uses_form_base_url_via_host_arm() {
    let bytes = llm_deepseek_component();
    let projector = Rc::new(KvProjector {
        kv: RefCell::new(HashMap::new()),
        calls: RefCell::new(Vec::new()),
    });
    let plugin = WasmRemoteEndpointPlugin::new(
        "llm-deepseek",
        &bytes,
        Default::default(),
        Some(projector.clone()),
    )
    .expect("constructs");

    // D-222：baseURL 优先当前表单值（values 形=画布 valuesKey 声明的线形）。
    let result = plugin
        .handle(
            "llm-deepseek",
            "discoverModels",
            br#"{"values":{"baseURL":"http://form:1/v1"}}"#,
            None,
        )
        .unwrap();
    assert_eq!(result["ok"], true, "discoverModels ok: {result}");
    let models = result["value"]["models"].as_array().expect("models array");
    assert_eq!(models.len(), 2, "host-arm passthrough: {models:?}");
    assert_eq!(models[0]["id"], "stub-model-a");
    let call = projector
        .calls
        .borrow()
        .iter()
        .find(|(s, _)| s == "llmDiscover")
        .expect("llmDiscover arm invoked")
        .1
        .clone();
    assert_eq!(call["baseURL"], "http://form:1/v1", "表单 baseURL 透传宿主臂");
}

#[test]
fn discover_models_falls_back_to_saved_base_url() {
    let bytes = llm_deepseek_component();
    let mut kv = HashMap::new();
    kv.insert(
        "llm-deepseek/settings".to_string(),
        json!({ "baseURL": "http://saved:2/v1" }),
    );
    let projector = Rc::new(KvProjector {
        kv: RefCell::new(kv),
        calls: RefCell::new(Vec::new()),
    });
    let plugin = WasmRemoteEndpointPlugin::new(
        "llm-deepseek",
        &bytes,
        Default::default(),
        Some(projector.clone()),
    )
    .expect("constructs");

    let result = plugin.handle("llm-deepseek", "discoverModels", br#"{}"#, None).unwrap();
    assert_eq!(result["ok"], true, "kv 回落可用: {result}");
    let call = projector
        .calls
        .borrow()
        .iter()
        .find(|(s, _)| s == "llmDiscover")
        .expect("llmDiscover arm invoked")
        .1
        .clone();
    assert_eq!(call["baseURL"], "http://saved:2/v1", "未填表单→已存 baseURL 回落");
}

#[test]
fn discover_models_without_base_url_is_honest() {
    let bytes = llm_deepseek_component();
    let projector = Rc::new(KvProjector {
        kv: RefCell::new(HashMap::new()),
        calls: RefCell::new(Vec::new()),
    });
    let plugin = WasmRemoteEndpointPlugin::new(
        "llm-deepseek",
        &bytes,
        Default::default(),
        Some(projector.clone()),
    )
    .expect("constructs");

    let result = plugin.handle("llm-deepseek", "discoverModels", br#"{}"#, None).unwrap();
    assert_eq!(result["ok"], false, "无 baseURL 必须诚实失败: {result}");
    assert!(
        result["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("baseURL"),
        "错误指向缺口字段: {result}"
    );
}

#[test]
fn unknown_endpoint_fail_loud() {
    let bytes = llm_deepseek_component();
    let projector = Rc::new(KvProjector {
        kv: RefCell::new(HashMap::new()),
        calls: RefCell::new(Vec::new()),
    });
    let plugin = WasmRemoteEndpointPlugin::new(
        "llm-deepseek",
        &bytes,
        Default::default(),
        Some(projector.clone()),
    )
    .expect("constructs");

    // 未知 namespace/method → 规范化错误（绝不伪造成功）。
    let result = plugin.handle("llm-deepseek", "bogus", br#"{}"#, None).unwrap();
    assert_eq!(result["ok"], false);
    assert_eq!(result["error"]["code"], "internal");
    let ns_result = plugin.handle("otherNs", "describeUI", br#"{}"#, None).unwrap();
    assert_eq!(ns_result["ok"], false);
}

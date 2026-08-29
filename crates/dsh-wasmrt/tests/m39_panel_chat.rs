// 面板改写 #7 / C8-4（D-193 收口片）：panel-chat 服务装配单元——聊天卡的**声明单元**。
//
// 架构裁决（D-193-B）：会话协议归宿主原生臂（session·list/history/prompt），单元只
// 拥有 v2 chat 声明（describeUI）。**无自有数据端点**——任何数据面调用 fail-loud
// （「单元不伪装能力」的显式断言）。ui.json 与 describeUI 一份契约。
use std::path::{Path, PathBuf};
use std::process::Command;

use dsh_core::Value;
use dsh_wasmrt::WasmRemoteEndpointPlugin;
use serde_json::json;

fn component() -> Vec<u8> {
    let manifest: PathBuf =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../wasm-plugins/panel-chat");
    let wasm_path = manifest.join("target/wasm32-wasip1/debug/panel_chat_plugin.wasm");
    if !wasm_path.exists() {
        let status = Command::new("cargo")
            .env("CARGO_NET_OFFLINE", "true")
            .args(["component", "build", "--manifest-path"])
            .arg(manifest.join("Cargo.toml"))
            .status()
            .expect("run cargo component build for panel-chat");
        assert!(status.success(), "panel-chat build failed");
    }
    std::fs::read(wasm_path).expect("read panel-chat component")
}

fn plugin() -> WasmRemoteEndpointPlugin {
    WasmRemoteEndpointPlugin::new("panel-chat", &component(), Default::default(), None)
        .expect("panel-chat plugin constructs")
}

#[test]
fn describe_ui_returns_valid_chat_declaration() {
    let r = plugin()
        .handle("panel-chat", "describeUI", br#"{}"#, None)
        .unwrap();
    assert_eq!(r["ok"], true, "{r}");
    let decl = &r["value"];
    assert_eq!(decl["$schema"], "dsh/plugin-ui/v2");
    assert_eq!(decl["kind"], "card");
    assert_eq!(decl["cardId"], "panel-chat.chat");
    assert_eq!(decl["type"], "session", "聊天卡归 session 分类");
    let size = decl["size"].as_object().expect("size 对象");
    assert!(size["w"].as_u64().unwrap() <= 4 && size["h"].as_u64().unwrap() <= 8);
    assert!(size.get("x").is_none() && size.get("y").is_none());
    let view = &decl["view"];
    assert_eq!(view["kind"], "chat");
    // 三数据面全部指宿主原生臂（D-193-B：会话协议归宿主，单元不代理）。
    assert_eq!(view["sessionSource"], json!(["session", "list"]));
    assert_eq!(view["historyRpc"], json!(["session", "history"]));
    assert_eq!(view["sendRpc"], json!(["session", "prompt"]));
    assert_eq!(view["stream"], "session-events", "闭集单值");
}

#[test]
fn static_ui_json_matches_describe_ui() {
    let r = plugin()
        .handle("panel-chat", "describeUI", br#"{}"#, None)
        .unwrap();
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../wasm-plugins/panel-chat/web/ui.json");
    let static_decl: Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("read web/ui.json"))
            .expect("ui.json is JSON");
    assert_eq!(static_decl, r["value"], "一份契约");
}

/// 单元不伪装能力：chat 声明的数据面在宿主——本单元**没有任何**自有数据端点。
#[test]
fn no_proprietary_data_endpoints_fail_loud() {
    let p = plugin();
    for m in ["list", "send", "history", "status"] {
        let r = p
            .handle("panel-chat", m, br#"{}"#, None)
            .unwrap();
        assert_eq!(r["ok"], false, "声明单元不得有数据端点: {m} -> {r}");
        assert!(r.to_string().contains(m), "错误点名缺失端点: {r}");
    }
}

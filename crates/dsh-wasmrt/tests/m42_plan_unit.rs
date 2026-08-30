// D-217（P3 薄服务族首刀）：plan 单元——计划模式**读+判定面**经 wasm 接缝承载。
//
// 核心验证：
// 1. `plan/projection`：host service "planEvents" 事件 → 单元内 **真·dsh-plan crate**
//    折叠 → {active,pending}，与 native 同事件序列结果**逐字节一致**（对拍=核心验收）；
// 2. `plan/exitCheck`：三判定分支（not-in-plan-mode / needs-heading / no-review-channel）
//    + 放行路径，判定结果=数据（ok:true），非传输错误；
// 3. 服务失败透传（不伪造空投影）；未知端点 fail-loud。
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use dsh_session::types::SessionEvent;
use dsh_wasmrt::{RemoteServiceProjector, WasmRemoteEndpointPlugin};
use serde_json::{json, Value};

fn component() -> Vec<u8> {
    let manifest: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../wasm-plugins/plan");
    let wasm_path = manifest.join("target/wasm32-wasip1/debug/plan_plugin.wasm");
    if !wasm_path.exists() {
        let status = Command::new("cargo")
            .env("CARGO_NET_OFFLINE", "true")
            .args(["component", "build", "--manifest-path"])
            .arg(manifest.join("Cargo.toml"))
            .status()
            .expect("run cargo component build for plan");
        assert!(status.success(), "plan unit build failed");
    }
    std::fs::read(wasm_path).expect("read plan component")
}

/// 测试投影器：真实形态的 "planEvents"（RemoteHost 投影同构：三相关 kind 透传）。
struct PlanEventsProjector {
    events: Vec<Value>,
    fail: bool,
}

impl RemoteServiceProjector for PlanEventsProjector {
    fn get(&self, service: &str, _payload: &[u8]) -> Vec<u8> {
        match service {
            "planEvents" if !self.fail => serde_json::to_vec(&json!({"ok": true, "events": self.events}))
                .unwrap_or_default(),
            "planEvents" => serde_json::to_vec(&json!({
                "ok": false,
                "error": { "code": "service", "message": "session store down" },
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
        serde_json::to_vec(&json!({"ok": false, "error": { "code": "read-only", "message": service }}))
            .unwrap_or_default()
    }
}

fn plugin(events: Vec<Value>, fail: bool) -> WasmRemoteEndpointPlugin {
    let bytes = component();
    let projector = Rc::new(PlanEventsProjector { events, fail });
    WasmRemoteEndpointPlugin::new("plan", &bytes, Default::default(), Some(projector))
        .expect("plan plugin constructs")
}

// ---- 事件 fixture（SessionEvent wire 形态：type/seq/time/data） ----

fn plan_mode(seq: u64, active: bool) -> Value {
    json!({"type": "plan/mode", "seq": seq, "time": 1000 + seq as i64, "data": {"active": active}})
}
fn cmd_run(seq: u64, id: &str, args: &str) -> Value {
    json!({"type": "command/run", "seq": seq, "time": 1000 + seq as i64,
           "data": {"commandId": id, "name": "plan", "args": args, "source": {"kind": "user"}}})
}
fn cmd_done(seq: u64, id: &str) -> Value {
    json!({"type": "command/done", "seq": seq, "time": 1000 + seq as i64,
           "data": {"commandId": id, "kind": "success"}})
}

fn parse_native(events: &[Value]) -> Vec<SessionEvent> {
    events
        .iter()
        .map(|v| serde_json::from_value::<SessionEvent>(v.clone()).expect("fixture parses as SessionEvent"))
        .collect()
}

/// 投影对拍（核心验收）：单元 wasm 结果 == native dsh-plan 同序列结果，逐用例一致。
fn assert_projection_equiv(events: Vec<Value>) {
    let plugin = plugin(events.clone(), false);
    let got = plugin
        .handle("plan", "projection", br#"{"sessionId":"s1"}"#, None)
        .expect("projection call");
    assert_eq!(got["ok"], json!(true), "projection ok: {got}");
    let native = dsh_plan::plan_projection_from_events(&parse_native(&events));
    assert_eq!(got["value"], native, "wasm/native 折叠对拍（事件数 {}）", events.len());
}

#[test]
fn projection_matches_native_on_fixture_sequences() {
    assert_projection_equiv(vec![]); // 空日志 = {active:false,pending:false}
    assert_projection_equiv(vec![plan_mode(1, true)]); // active 落定
    assert_projection_equiv(vec![plan_mode(1, true), plan_mode(2, false)]); // last-wins
    // 命令在跑：running.wanted=true ≠ active=false → pending。
    assert_projection_equiv(vec![cmd_run(1, "c1", "")]);
    // 命令落定：success 且 wanted≠active → wanted 悬置 pending。
    assert_projection_equiv(vec![cmd_run(1, "c1", ""), cmd_done(2, "c1")]);
    // 落定后 plan/mode 清 wanted：回到无 pending。
    assert_projection_equiv(vec![cmd_run(1, "c1", ""), cmd_done(2, "c1"), plan_mode(3, true)]);
    // args=off 的 wanted=false 且 active=false → wanted 相等不落 → 无 pending。
    assert_projection_equiv(vec![cmd_run(1, "c2", "off"), cmd_done(2, "c2")]);
}

#[test]
fn exit_check_branches_match_native() {
    // 不在 plan mode（空日志）。
    let p = plugin(vec![], false);
    let v = p
        .handle("plan", "exitCheck", r##"{"sessionId":"s1","plan":"# 标题","reviewChannel":true}"##.as_bytes(), None)
        .expect("exitCheck call");
    assert_eq!(v["ok"], json!(true));
    assert_eq!(v["value"]["allow"], json!(false));
    assert_eq!(v["value"]["reason"], json!("not-in-plan-mode"));

    // 在 plan mode + 有标题 + 评审通道 → 放行。
    let p = plugin(vec![plan_mode(1, true)], false);
    let v = p
        .handle("plan", "exitCheck", r##"{"sessionId":"s1","plan":"# 计划标题","reviewChannel":true}"##.as_bytes(), None)
        .expect("exitCheck call");
    assert_eq!(v["value"]["allow"], json!(true), "放行: {v}");

    // 无标题 → needs-heading。
    let v = p
        .handle("plan", "exitCheck", r#"{"sessionId":"s1","plan":"正文无标题","reviewChannel":true}"#.as_bytes(), None)
        .expect("exitCheck call");
    assert_eq!(v["value"]["reason"], json!("needs-heading"));

    // 无评审通道 → no-review-channel。
    let v = p
        .handle("plan", "exitCheck", r##"{"sessionId":"s1","plan":"# 计划标题","reviewChannel":false}"##.as_bytes(), None)
        .expect("exitCheck call");
    assert_eq!(v["value"]["reason"], json!("no-review-channel"));

    // 对拍：native exit_plan_mode_check 同输入同结论（放行/拒绝布尔一致）。
    let events = parse_native(&[plan_mode(1, true)]);
    assert!(dsh_plan::exit_plan_mode_check(&events, "# 计划标题", true).is_ok());
    assert!(dsh_plan::exit_plan_mode_check(&events, "正文无标题", true).is_err());
    assert!(dsh_plan::exit_plan_mode_check(&events, "# 计划标题", false).is_err());
}

#[test]
fn service_failure_passthrough_and_unknown_endpoint() {
    // 服务失败 → 透传错误（不伪造 {active:false}）。
    let p = plugin(vec![], true);
    let v = p
        .handle("plan", "projection", br#"{"sessionId":"s1"}"#, None)
        .expect("projection call");
    assert_eq!(v["ok"], json!(false), "服务失败透传: {v}");
    assert_eq!(v["error"]["code"], json!("service"));
    // 未知端点 fail-loud。
    let p = plugin(vec![], false);
    let v = p.handle("plan", "bogus", br#"{}"#, None).expect("unknown call");
    assert_eq!(v["ok"], json!(false));
    assert!(v["error"]["message"].as_str().unwrap().contains("not provided"));
}

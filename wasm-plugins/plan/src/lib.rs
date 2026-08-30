// plan 薄服务单元（D-217 / P3 薄服务族首刀）：计划模式**读+判定面**经 wasm 承载。
//
// - `plan/projection` {sessionId} → host service "planEvents"（plan/mode +
//   command/run[name=plan] + command/done 三类事件透传）→ **真·dsh-plan crate**
//   在 wasm 内折叠 → {active, pending}（与宿主原生路径同一语义 crate=对拍由构造成立，
//   本单元证明的是「执行可迁移到接缝之内」）。
// - `plan/exitCheck` {sessionId, plan, reviewChannel} → dsh_plan::exit_plan_mode_check
//   → {allow, reason}（判定结果=数据 ok:true；校验不通过不是传输错误）。
// - 写面（enter/exit 落事件）留宿主 v2（需事件追加接缝，另立决策——面板改写
//   「只读卡先行」同款纪律）。
// - 服务失败透传，不伪造空投影（诚实纪律）。

#[allow(warnings)]
mod bindings;

use bindings::dsh::host_remote::host_services;
use bindings::exports::dsh::host_remote::remote::Guest;
use dsh_session::types::SessionEvent;
use serde_json::{json, Value};

fn error(code: &str, message: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({ "ok": false, "error": { "code": code, "message": message } }))
        .unwrap_or_default()
}

/// 经 host service 拉折叠相关事件并反序列化（失败 → Err 携带原因）。
fn plan_events(payload: &Value) -> Result<Vec<SessionEvent>, (String, String)> {
    let bytes = host_services::get("planEvents", &payload.to_string().into_bytes());
    let proj: Value = serde_json::from_slice(&bytes)
        .map_err(|e| ("decode".into(), format!("planEvents unparseable: {e}")))?;
    if proj.get("ok").and_then(Value::as_bool) != Some(true) {
        let err = proj.get("error").cloned().unwrap_or_else(|| json!({"code":"service","message":"planEvents failure"}));
        return Err((
            err.get("code").and_then(Value::as_str).unwrap_or("service").to_string(),
            err.get("message").and_then(Value::as_str).unwrap_or("planEvents failure").to_string(),
        ));
    }
    let events: Vec<SessionEvent> = serde_json::from_value(
        proj.get("events").cloned().unwrap_or_else(|| json!([])),
    )
    .map_err(|e| ("decode".into(), format!("planEvents events invalid: {e}")))?;
    Ok(events)
}

fn projection(body: &Value) -> Vec<u8> {
    match plan_events(body) {
        Err((code, msg)) => error(&code, &msg),
        Ok(events) => {
            let view = dsh_plan::plan_projection_from_events(&events);
            serde_json::to_vec(&json!({ "ok": true, "value": view })).unwrap_or_default()
        }
    }
}

fn exit_check(body: &Value) -> Vec<u8> {
    let plan = body.get("plan").and_then(Value::as_str).unwrap_or("");
    let review_channel = body.get("reviewChannel").and_then(Value::as_bool).unwrap_or(false);
    match plan_events(body) {
        Err((code, msg)) => error(&code, &msg),
        Ok(events) => match dsh_plan::exit_plan_mode_check(&events, plan, review_channel) {
            Ok(()) => serde_json::to_vec(&json!({ "ok": true, "value": { "allow": true } })).unwrap_or_default(),
            Err(check) => {
                let reason = match check {
                    dsh_plan::ExitCheck::NotInPlanMode => "not-in-plan-mode",
                    dsh_plan::ExitCheck::NeedsHeading => "needs-heading",
                    dsh_plan::ExitCheck::NoReviewChannel => "no-review-channel",
                    // 枚举含 Ok 变体（判定语义位）：Err(Ok) 非预期组合，按放行处理并如实标注。
                    dsh_plan::ExitCheck::Ok => "ok",
                };
                let allow = reason == "ok";
                serde_json::to_vec(&json!({ "ok": true, "value": { "allow": allow, "reason": reason } }))
                    .unwrap_or_default()
            }
        },
    }
}

struct PlanUnit;

impl Guest for PlanUnit {
    fn handle(namespace: String, method: String, body: Vec<u8>) -> Vec<u8> {
        let body_value: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        match (namespace.as_str(), method.as_str()) {
            ("plan", "projection") => projection(&body_value),
            ("plan", "exitCheck") => exit_check(&body_value),
            _ => error(
                "internal",
                &format!("plan: endpoint {namespace}/{method} not provided by this unit"),
            ),
        }
    }
}

bindings::export!(PlanUnit with_types_in bindings);

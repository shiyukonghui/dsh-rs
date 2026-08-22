//! M2f：dsh-tools pre-phase 审批（approval）接线测试——pre-execute 决策（allow/deny/ask）、
//! ask 经 ApprovalProvider 解析的四种逐字结果、无通道/无 agent 退化、pre-decisions
//! waterfall 到 allow、guards 在 allow 后仍单调拦截。

#![allow(clippy::type_complexity)] // Rc<dyn Fn> hook 与 seam 自洽
#![allow(clippy::result_large_err)]

use std::cell::RefCell;
use std::rc::Rc;

use dsh_llm::ContentBlock;
use dsh_tools::{
    define_tool, ApprovalOutcome, DefineToolOptions, PreToolDecision, ToolExecution,
    ToolExecutionInput, ToolExecutionResult, ToolRegistry, ToolExecutionMode,
};
use serde_json::json;

const AGENT: &str = "agent-1";

fn registry() -> ToolRegistry {
    let r = ToolRegistry::new(ToolExecutionMode::Native);
    r.register_global(Rc::new(
        define_tool(DefineToolOptions {
            name: "echo".to_string(),
            description: "echo".into(),
            parameters: json!({
                "text": { "type": "string", "required": true },
            }),
            output_schema: json!({ "type": "json" }),
            render: Rc::new(|_, v| vec![ContentBlock::text(v.to_string())]),
            execute: Rc::new(|_, _| Ok(json!("ran"))),
            is_concurrency_safe: Some(Rc::new(|_| true)),
            ..Default::default()
        })
        .unwrap(),
    ))
    .unwrap();
    r
}

fn input(agent: Option<&str>) -> ToolExecutionInput {
    ToolExecutionInput::new("c1", "echo", json!({ "text": "hi" }), agent.map(String::from))
}

fn run(r: &ToolRegistry, inp: &ToolExecutionInput) -> ToolExecutionResult {
    r.execute(inp, None)
}

// ---------------------------------------------------------------------------
// pre-decision：deny / allow / ask
// ---------------------------------------------------------------------------

#[test]
fn pre_decision_deny_materializes_error_result() {
    let r = registry();
    let _d = r
        .add_pre_decision(
            Rc::new(|_e: &ToolExecution| Some(PreToolDecision::Deny {
                reason: "policy says no".to_string(),
            })),
            None,
        )
        .unwrap();
    let out = run(&r, &input(Some(AGENT)));
    assert!(out.is_error);
    assert_eq!(
        out.error.as_ref().map(|e| e.message.as_str()),
        Some("policy says no")
    );
    assert_eq!(
        out.content[0].as_text().map(|t| t.text()).unwrap(),
        "Error: policy says no"
    );
}

#[test]
fn pre_decisions_waterfall_to_allow_and_first_non_allow_wins() {
    let r = registry();
    // 第一个 pass-through（None），第二个 deny → deny 生效
    let _a = r.add_pre_decision(Rc::new(|_e: &ToolExecution| None), None).unwrap();
    let _b = r
        .add_pre_decision(
            Rc::new(|_e: &ToolExecution| Some(PreToolDecision::Deny {
                reason: "second wins".to_string(),
            })),
            None,
        )
        .unwrap();
    let out = run(&r, &input(Some(AGENT)));
    assert!(out.is_error);
    assert_eq!(
        out.content[0].as_text().map(|t| t.text()).unwrap(),
        "Error: second wins"
    );
}

#[test]
fn guards_still_enforce_after_allow() {
    let r = registry();
    let _pre = r
        .add_pre_decision(
            Rc::new(|_e: &ToolExecution| Some(PreToolDecision::Allow)),
            None,
        )
        .unwrap();
    let _guard = r
        .add_guard(
            Rc::new(|name, _| (name == "echo").then(|| "hard-guarded".to_string())),
            None,
        )
        .unwrap();
    let out = run(&r, &input(Some(AGENT)));
    assert!(out.is_error);
    assert_eq!(
        out.content[0].as_text().map(|t| t.text()).unwrap(),
        "Error: hard-guarded"
    );
}

// ---------------------------------------------------------------------------
// ask 解析（逐字拒绝原因对齐 serviceAsk；sync 差值见 D-034）
// ---------------------------------------------------------------------------

#[test]
fn ask_without_provider_denies_not_yet_supported() {
    let r = registry();
    let _pre = r
        .add_pre_decision(
            Rc::new(|_e: &ToolExecution| Some(PreToolDecision::Ask { reason: None })),
            None,
        )
        .unwrap();
    let out = run(&r, &input(Some(AGENT)));
    assert!(out.is_error);
    assert_eq!(
        out.content[0].as_text().map(|t| t.text()).unwrap(),
        "Error: tool \"echo\" requires approval (not yet supported)"
    );
}

#[test]
fn ask_without_provider_uses_custom_reason() {
    let r = registry();
    let _pre = r
        .add_pre_decision(
            Rc::new(|_e: &ToolExecution| Some(PreToolDecision::Ask {
                reason: Some("needs a human".to_string()),
            })),
            None,
        )
        .unwrap();
    let out = run(&r, &input(Some(AGENT)));
    assert!(out.is_error);
    assert_eq!(
        out.content[0].as_text().map(|t| t.text()).unwrap(),
        "Error: needs a human"
    );
}

#[test]
fn ask_allowed_once_runs_body() {
    let r = registry();
    r.set_approval_provider(Some(Rc::new(|_e: &ToolExecution, _r: Option<&str>| {
        ApprovalOutcome::AllowedOnce
    })));
    let _pre = r
        .add_pre_decision(
            Rc::new(|_e: &ToolExecution| Some(PreToolDecision::Ask { reason: None })),
            None,
        )
        .unwrap();
    let out = run(&r, &input(Some(AGENT)));
    assert!(!out.is_error);
    assert_eq!(out.value.as_ref().unwrap(), &json!("ran"));
}

#[test]
fn ask_rejected_denies_with_verbatim_reason() {
    let r = registry();
    let saw = Rc::new(RefCell::new(None::<(String, Option<String>)>));
    let saw2 = saw.clone();
    r.set_approval_provider(Some(Rc::new(move |e: &ToolExecution, reason: Option<&str>| {
        saw2.replace(Some((e.call.name.clone(), reason.map(String::from))));
        ApprovalOutcome::Rejected
    })));
    let _pre = r
        .add_pre_decision(
            Rc::new(|_e: &ToolExecution| Some(PreToolDecision::Ask {
                reason: Some("confirm interaction".to_string()),
            })),
            None,
        )
        .unwrap();
    let out = run(&r, &input(Some(AGENT)));
    assert!(out.is_error);
    assert_eq!(
        out.content[0].as_text().map(|t| t.text()).unwrap(),
        "Error: the user rejected tool \"echo\""
    );
    // 决策者收到 tool 名与 ask reason
    assert_eq!(
        saw.borrow().clone(),
        Some(("echo".to_string(), Some("confirm interaction".to_string())))
    );
}

#[test]
fn ask_cancelled_denies_with_verbatim_reason() {
    let r = registry();
    r.set_approval_provider(Some(Rc::new(|_e: &ToolExecution, _r: Option<&str>| {
        ApprovalOutcome::Cancelled
    })));
    let _pre = r
        .add_pre_decision(
            Rc::new(|_e: &ToolExecution| Some(PreToolDecision::Ask { reason: None })),
            None,
        )
        .unwrap();
    let out = run(&r, &input(Some(AGENT)));
    assert!(out.is_error);
    assert_eq!(
        out.content[0].as_text().map(|t| t.text()).unwrap(),
        "Error: approval for tool \"echo\" was cancelled"
    );
}

#[test]
fn ask_unavailable_denies_with_verbatim_reason() {
    let r = registry();
    r.set_approval_provider(Some(Rc::new(|_e: &ToolExecution, _r: Option<&str>| {
        ApprovalOutcome::Unavailable
    })));
    let _pre = r
        .add_pre_decision(
            Rc::new(|_e: &ToolExecution| Some(PreToolDecision::Ask { reason: None })),
            None,
        )
        .unwrap();
    let out = run(&r, &input(Some(AGENT)));
    assert!(out.is_error);
    assert_eq!(
        out.content[0].as_text().map(|t| t.text()).unwrap(),
        "Error: tool \"echo\" requires approval, but no approval channel is available"
    );
}

#[test]
fn ask_with_no_agent_denies_routing() {
    let r = registry();
    r.set_approval_provider(Some(Rc::new(|_e: &ToolExecution, _r: Option<&str>| {
        ApprovalOutcome::AllowedOnce
    })));
    let _pre = r
        .add_pre_decision(
            Rc::new(|_e: &ToolExecution| Some(PreToolDecision::Ask { reason: None })),
            None,
        )
        .unwrap();
    // agent 缺失 → 即使通道存在也拒绝（没有 agent 可路由到 UI/审计）
    let out = run(&r, &input(None));
    assert!(out.is_error);
    assert_eq!(
        out.content[0].as_text().map(|t| t.text()).unwrap(),
        "Error: tool \"echo\" requires approval, but the call has no agent to route it through"
    );
}

// ---------------------------------------------------------------------------
// 审批通道生命周期（set → 前值 → clear）
// ---------------------------------------------------------------------------

#[test]
fn set_approval_provider_returns_previous_and_can_clear() {
    let r = registry();
    assert!(r.approval_provider().is_none());
    let p = Rc::new(|_e: &ToolExecution, _r: Option<&str>| ApprovalOutcome::Rejected);
    let previous = r.set_approval_provider(Some(p.clone()));
    assert!(previous.is_none());
    assert!(r.approval_provider().is_some());
    let previous = r.set_approval_provider(None);
    assert!(previous.is_some());
    assert!(r.approval_provider().is_none());
}

// ---------------------------------------------------------------------------
// 经 agent-loop 集成（service 接线）：真实闭环里硬守卫的 deny 落在 tool/result 上
// ---------------------------------------------------------------------------

#[test]
fn ask_without_provider_in_loop_produces_tool_error_result() {
    // 该场景在 dsh-agent-loop tests/m2f_interaction.rs 闭环验证；
    // 此处仅确认 execute 结果携带 error.info（TS 侧 deny 时 error.info 为空）。
    let r = registry();
    let _pre = r
        .add_pre_decision(
            Rc::new(|_e: &ToolExecution| Some(PreToolDecision::Ask { reason: None })),
            None,
        )
        .unwrap();
    let out = run(&r, &input(Some(AGENT)));
    assert!(out.is_error);
    assert!(!out.content.is_empty());
    assert_eq!(out.error.as_ref().map(|e| e.message.as_str()).unwrap(), "tool \"echo\" requires approval (not yet supported)");
}

//! M3e: guard 切片测试——timeout-policy（TOOL_TIMEOUT 结构化替换 + 同步最小 executor
//! 路径）与 repeat-tool-reminder（阈值检测 [3,5,8] 纯逻辑 + gentle/detailed 逐字）。
//!
//! 对齐 deepseek-harness packages/guard/{timeout-policy,repeat-tool-reminder}。完整
//! agent-loop 接线（依赖 fs/shell M5 通道）不在 M3：此处交付 seam + 最小 executor
//! 路径（同步 wall-clock 判定），消息全部逐字。

use dsh_llm::ContentBlock;
use dsh_tools::{
    define_tool, guard, DefineToolOptions, ToolExecution, ToolExecutionInput, ToolExecutionMode,
    ToolRegistry, ToolRunContext,
};
use serde_json::{json, Value};
use std::rc::Rc;

fn echo_def(name: &str) -> dsh_tools::ToolDefinition {
    define_tool(DefineToolOptions {
        name: name.to_string(),
        description: format!("{name} a value"),
        parameters: json!({ "text": { "type": "string", "required": true } }),
        output_schema: json!({ "type": "json" }),
        render: Rc::new(|_, value| vec![ContentBlock::text(serde_json::to_string(value).unwrap())]),
        execute: Rc::new(|args, _| Ok(args["text"].clone())),
        ..Default::default()
    })
    .unwrap()
}

/// 声明超时预算 + body sleep 的工具（同步执行下 wall-clock 判定）。
fn slow_def(name: &str, timeout_ms: f64, sleep_ms: u64) -> dsh_tools::ToolDefinition {
    define_tool(DefineToolOptions {
        name: name.to_string(),
        description: format!("{name} sleeps"),
        parameters: json!({}),
        output_schema: json!({ "type": "json" }),
        timeout_ms: Some(timeout_ms),
        render: Rc::new(|_, value| vec![ContentBlock::text(serde_json::to_string(value).unwrap())]),
        execute: Rc::new(move |_, _| {
            std::thread::sleep(std::time::Duration::from_millis(sleep_ms));
            Ok(json!({ "done": true }))
        }),
        ..Default::default()
    })
    .unwrap()
}

fn input(name: &str, args: Value) -> ToolExecutionInput {
    ToolExecutionInput::new("call-1", name, args, Some("agent-1".to_string()))
}

fn registry() -> ToolRegistry {
    ToolRegistry::new(ToolExecutionMode::Native)
}

// ---------------------------------------------------------------------------
// timeout-policy：消息逐字 + 结构化替换结果 + 同步最小 executor 路径
// ---------------------------------------------------------------------------

#[test]
fn tool_timeout_message_verbatim() {
    assert_eq!(guard::tool_timeout_message(100), "tool call timed out after 100ms");
}

#[test]
fn tool_timeout_result_structure() {
    let exec = ToolExecution {
        call: ToolRunContext::new("c1", "c1", "slow", Some("agent-1".to_string())),
        args: json!({}),
    };
    let r = guard::tool_timeout_result(&exec, 100);
    assert!(r.is_error);
    assert_eq!(
        r.content[0].as_text().map(|t| t.text()),
        Some("Error: tool call timed out after 100ms")
    );
    let err = r.error.as_ref().unwrap();
    assert_eq!(err.message, "tool call timed out after 100ms");
    let info = err.info.as_ref().expect("timeout carries failure info");
    assert_eq!(info.code, "TOOL_TIMEOUT");
    assert_eq!(info.name, "ToolTimeoutError");
    assert_eq!(guard::TOOL_TIMEOUT, "TOOL_TIMEOUT");
}

#[test]
fn timeout_exceeded_pure_decision() {
    assert!(guard::timeout_exceeded(Some(50.0), 200));
    assert!(!guard::timeout_exceeded(Some(50.0), 10));
    assert!(guard::timeout_exceeded(Some(50.0), 50)); // 边界：>= 即超
    assert!(!guard::timeout_exceeded(None, 999));       // 无预算永不超时
    assert!(!guard::timeout_exceeded(Some(f64::NAN), 999)); // 非有限值视为无预算
    assert!(!guard::timeout_exceeded(Some(0.0), 999));
}

/// 同步最小 executor 路径：声明 budget 且 body 超过 → 替换 TOOL_TIMEOUT（逐字）。
#[test]
fn executor_substitutes_on_timeout() {
    let r = registry();
    r.register_global(Rc::new(slow_def("slow", 20.0, 200))).unwrap();
    let out = r.execute(&input("slow", json!({})), None);
    assert!(out.is_error);
    assert_eq!(
        out.content[0].as_text().map(|t| t.text()),
        Some("Error: tool call timed out after 20ms")
    );
    let err = out.error.as_ref().unwrap();
    assert_eq!(err.info.as_ref().unwrap().code, "TOOL_TIMEOUT");
}

/// 声明 budget 但 body 快 → 保留工具自身结果（不误报）。
#[test]
fn executor_keeps_fast_budgeted_result() {
    let r = registry();
    r.register_global(Rc::new(slow_def("fast", 10_000.0, 0))).unwrap();
    let out = r.execute(&input("fast", json!({})), None);
    assert!(!out.is_error);
    assert_eq!(
        out.content[0].as_text().map(|t| t.text()),
        Some("{\"done\":true}")
    );
}

/// 无 budget 的工具 → 完全委托（不触碰结果）。
#[test]
fn executor_delegates_unbudgeted() {
    let r = registry();
    r.register_global(Rc::new(echo_def("echo"))).unwrap();
    let out = r.execute(&input("echo", json!({ "text": "x" })), None);
    assert!(!out.is_error);
    assert_eq!(out.content[0].as_text().map(|t| t.text()), Some("\"x\""));
}

// ---------------------------------------------------------------------------
// repeat-tool-reminder：canonicalize / wildcard / thresholds / 消息逐字
// ---------------------------------------------------------------------------

#[test]
fn canonicalize_deep_key_sorts() {
    let a = json!({ "b": 2, "a": 1, "nested": { "y": [1, 2], "x": null } });
    let b = json!({ "nested": { "x": null, "y": [1, 2] }, "a": 1, "b": 2 });
    let ca = guard::canonicalize(&a);
    let cb = guard::canonicalize(&b);
    assert_eq!(ca, cb);
    assert_eq!(ca, r#"{"a":1,"b":2,"nested":{"x":null,"y":[1,2]}}"#);
}

#[test]
fn canonicalize_array_order_preserved() {
    let a = json!({ "arr": [3, 1, 2] });
    let b = json!({ "arr": [1, 3, 2] });
    assert_ne!(guard::canonicalize(&a), guard::canonicalize(&b));
}

#[test]
fn wildcard_literal_and_star() {
    // `*` 通配任意；其余（含 `.`）逐字（对齐 wildcardToRegExp 转义）。
    assert!(guard::wildcard_matches("pro*", "probe"));
    assert!(guard::wildcard_matches("*probe", "probe"));
    assert!(guard::wildcard_matches("probe", "probe"));
    assert!(!guard::wildcard_matches("probe", "probeX"));
    // 点号是字面量，不命中 `probe` 里的任意字符。
    assert!(!guard::wildcard_matches("pr.be", "probe"));
    assert!(guard::wildcard_matches("pr.be", "pr.be"));
}

#[test]
fn thresholds_validation_fails_loud() {
    assert_eq!(
        guard::validate_thresholds(&[]),
        Err("repeat-tool-reminder: `thresholds` must not be empty".to_string())
    );
    assert!(guard::validate_thresholds(&[1, 3])
        .err()
        .unwrap()
        .contains("every threshold must be an integer >= 2"));
    assert!(guard::validate_thresholds(&[3, 3])
        .err()
        .unwrap()
        .contains("must not contain duplicates"));
    // 归一化升序。
    assert_eq!(guard::validate_thresholds(&[5, 2]).unwrap(), vec![2, 5]);
}

#[test]
fn preview_arguments_caps_with_ellipsis() {
    let canonical = r#"{"body":"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"}"#;
    let p = guard::preview_arguments(canonical, 24);
    assert!(p.starts_with(r#"{"body":"xxxxxxxxxxxxxxx"#)); // 24-char head (9 + 15)
    assert!(p.ends_with("… (+27 more chars)")); // 51 - 24 = 27
    // 未超限原样返回。
    assert_eq!(guard::preview_arguments("abc", 500), "abc");
}

#[test]
fn gentle_and_detailed_messages_verbatim() {
    // gentle 逐字（对齐 TS GENTLE_REMINDER）。
    let expected_gentle = "You are repeating the exact same tool call with identical arguments. \
        Carefully analyze the previous result before calling again: if the task is not \
        complete, try a different approach or different arguments instead of repeating the call.";
    assert_eq!(guard::GENTLE_REMINDER, expected_gentle);

    let d = guard::detailed_reminder("probe", 5, r#"{"q":"same"}"#);
    assert_eq!(
        d,
        "Repeated tool call detected:\n\
         - tool: probe\n\
         - consecutive_calls: 5\n\
         - arguments: {\"q\":\"same\"}\n\
         The repeated calls are not making progress. Do not call this tool with these \
         exact arguments again. Inspect the latest result and choose a different action, \
         different arguments, or finish the task if enough evidence has been gathered."
    );
}

// ---------------------------------------------------------------------------
// repeat-tool-reminder：追踪器状态机（链 / 阈值 / include-exclude / 重置）
// ---------------------------------------------------------------------------

fn tracker() -> guard::RepeatTracker {
    guard::RepeatTracker::new(&[3, 5, 8], &[], &[], 500).expect("default tracker ok")
}

#[test]
fn tracker_default_threshold_escalation() {
    let mut t = tracker();
    assert!(t.observe(Some("a1"), "probe", &json!({ "q": "same" })).is_none());
    assert!(t.observe(Some("a1"), "probe", &json!({ "q": "same" })).is_none());
    let g = t.observe(Some("a1"), "probe", &json!({ "q": "same" })).expect("gentle at 3");
    assert_eq!(g.text, guard::GENTLE_REMINDER);
    assert_eq!(g.count, 3);
    // count 4 不在 [3,5,8] → 无提醒。
    assert!(t.observe(Some("a1"), "probe", &json!({ "q": "same" })).is_none());
    let d5 = t.observe(Some("a1"), "probe", &json!({ "q": "same" })).expect("detailed at 5");
    assert!(d5.text.contains("consecutive_calls: 5"));
    assert!(d5.text.contains("- tool: probe"));
    assert!(d5.text.contains(r#"{"q":"same"}"#));
}

#[test]
fn tracker_keys_gentle_to_thresholds_zero() {
    let mut t = guard::RepeatTracker::new(&[4, 2], &[], &[], 500).unwrap(); // 升序归一化
    assert!(t.observe(Some("a1"), "probe", &json!({})).is_none());
    let g = t.observe(Some("a1"), "probe", &json!({})).expect("gentle at 2");
    assert_eq!(g.text, guard::GENTLE_REMINDER);
    assert_eq!(g.count, 2);
    // count 3 不在 [2,4] → 无提醒。
    assert!(t.observe(Some("a1"), "probe", &json!({})).is_none());
    let d4 = t.observe(Some("a1"), "probe", &json!({})).expect("detailed at 4");
    assert!(d4.text.contains("consecutive_calls: 4"));
}

#[test]
fn tracker_different_call_resets() {
    let mut t = tracker();
    assert!(t.observe(Some("a1"), "probe", &json!({ "q": 1 })).is_none());
    assert!(t.observe(Some("a1"), "probe", &json!({ "q": 1 })).is_none());
    assert!(t.observe(Some("a1"), "other", &json!({})).is_none()); // 不同 → 重置
    assert!(t.observe(Some("a1"), "probe", &json!({ "q": 1 })).is_none());
    assert!(t.observe(Some("a1"), "probe", &json!({ "q": 1 })).is_none());
    // 重置后第 3 次连续 probe 才触发。
    let g = t.observe(Some("a1"), "probe", &json!({ "q": 1 })).expect("3rd after reset");
    assert_eq!(g.count, 3);
}

#[test]
fn tracker_excluded_transparent() {
    let mut t = guard::RepeatTracker::new(&[3, 5, 8], &[], &["other"], 500).unwrap();
    assert!(t.observe(Some("a1"), "other", &json!({})).is_none());
    assert!(t.observe(Some("a1"), "other", &json!({})).is_none());
    assert!(t.observe(Some("a1"), "probe", &json!({ "q": 1 })).is_none());
    assert!(t.observe(Some("a1"), "other", &json!({})).is_none());
    assert!(t.observe(Some("a1"), "probe", &json!({ "q": 1 })).is_none());
    let g = t.observe(Some("a1"), "probe", &json!({ "q": 1 })).expect("3rd probe");
    assert_eq!(g.count, 3);
}

#[test]
fn tracker_include_patterns() {
    let mut t = guard::RepeatTracker::new(&[3, 5, 8], &["pro*"], &[], 500).unwrap();
    assert!(t.observe(Some("a1"), "other", &json!({})).is_none());
    assert!(t.observe(Some("a1"), "other", &json!({})).is_none());
    assert!(t.observe(Some("a1"), "probe", &json!({})).is_none());
    assert!(t.observe(Some("a1"), "probe", &json!({})).is_none());
    let g = t.observe(Some("a1"), "probe", &json!({})).expect("3rd probe tracked");
    assert_eq!(g.count, 3);
}

#[test]
fn tracker_per_agent_isolated() {
    let mut t = tracker();
    assert!(t.observe(Some("a"), "probe", &json!({ "q": 1 })).is_none());
    assert!(t.observe(Some("a"), "probe", &json!({ "q": 1 })).is_none());
    assert!(t.observe(Some("b"), "probe", &json!({ "q": 1 })).is_none());
    assert!(t.observe(Some("b"), "probe", &json!({ "q": 1 })).is_none());
    assert!(t.observe(Some("b"), "probe", &json!({ "q": 1 })).is_some()); // b 第 3 次触发
    // a 的链不受 b 影响：a 自己的第 3 次也触发（count 3，非被 b 计数污染）。
    let a3 = t.observe(Some("a"), "probe", &json!({ "q": 1 })).expect("a 第 3 次触发");
    assert_eq!(a3.count, 3);
}

#[test]
fn tracker_ignores_no_agent_and_resets_on_user_prompt() {
    let mut t = tracker();
    // 无 agent 的直接 execute：不参与链（不计数）。
    assert!(t.observe(None, "probe", &json!({ "q": 1 })).is_none());
    assert!(t.observe(None, "probe", &json!({ "q": 1 })).is_none());
    assert!(t.observe(Some("a1"), "probe", &json!({ "q": 1 })).is_none());
    // 用户插话 → 重置链。
    t.reset("a1");
    assert!(t.observe(Some("a1"), "probe", &json!({ "q": 1 })).is_none());
    assert!(t.observe(Some("a1"), "probe", &json!({ "q": 1 })).is_none());
    t.observe(Some("a1"), "probe", &json!({ "q": 1 })).expect("3rd after reset");
}

#[test]
fn tracker_preview_cap_in_detailed_only() {
    let mut t = guard::RepeatTracker::new(&[2, 3], &[], &[], 24).unwrap();
    let big = "x".repeat(400);
    assert!(t.observe(Some("a1"), "probe", &json!({ "body": big.clone() })).is_none());
    assert!(t.observe(Some("a1"), "probe", &json!({ "body": big.clone() })).expect("gentle at 2")
        .text.contains("repeating the exact same tool call"));
    let d = t.observe(Some("a1"), "probe", &json!({ "body": big.clone() })).expect("detailed at 3");
    assert!(d.text.contains("- arguments: {\"body\":\"xxxxxxxxxxxxxxx")); // 24-char head
    assert!(d.text.contains("… (+387 more chars)")); // 411 - 24 = 387
}

#[test]
fn tracker_validation_fails_loud_on_constructor() {
    assert!(guard::RepeatTracker::new(&[], &[], &[], 500).is_err());
    assert!(guard::RepeatTracker::new(&[1, 3], &[], &[], 500).is_err());
    assert!(guard::RepeatTracker::new(&[3, 3], &[], &[], 500).is_err());
    assert!(guard::RepeatTracker::new(&[3], &[], &[], 0).is_err()); // preview < 1
}

#[test]
fn drop_agent_clears_its_chain() {
    let mut t = tracker();
    assert!(t.observe(Some("a1"), "probe", &json!({ "q": 1 })).is_none());
    assert!(t.observe(Some("a1"), "probe", &json!({ "q": 1 })).is_none());
    t.drop_agent("a1");
    assert!(t.observe(Some("a1"), "probe", &json!({ "q": 1 })).is_none());
    assert!(t.observe(Some("a1"), "probe", &json!({ "q": 1 })).is_none());
    t.observe(Some("a1"), "probe", &json!({ "q": 1 })).expect("fresh after drop");
}

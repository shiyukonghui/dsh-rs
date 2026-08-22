//! M4d dsh-subagent 纯语义测试（TDD 红-绿）。
//!
//! 对齐 `packages/subagent/subagent/src/{descriptor,depth}.ts` + provider 边界。
//! 覆盖：descriptor snapshot/fold 逐字、深度预算、provider 能力登记、list entry 派生、
//! control 模式判别。

use dsh_subagent::catalog::category_child;
use dsh_subagent::depth::{resolve_child_depth, resolve_child_depth_bounded, validate_max_depth, DepthError};
use dsh_subagent::descriptor::{snapshot_descriptor, fold_descriptor_from_events, DescriptorInput, Descriptor};
use dsh_subagent::provider::ProviderCapabilities;
use serde_json::json;

// ---- descriptor ----

/// one-shot 快照：version=2、mode、provider、可选 label。
#[test]
fn snapshot_one_shot() {
    let input = DescriptorInput::OneShot {
        mode: "one-shot".to_string(),
        provider: "spawn".to_string(),
        label: Some("audit".to_string()),
    };
    let d = snapshot_descriptor(&input).expect("snapshot");
    match d {
        Descriptor::OneShot { version, provider, label, .. } => {
            assert_eq!(version, 2);
            assert_eq!(provider, "spawn");
            assert_eq!(label.as_deref(), Some("audit"));
        }
        _ => panic!("应是 one-shot"),
    }
}

/// continuable 快照：label 必填 + 可选 agentProvider/agentModel。
#[test]
fn snapshot_continuable() {
    let input = DescriptorInput::Continuable {
        mode: "continuable".to_string(),
        provider: "fork".to_string(),
        label: "worker".to_string(),
        agent_provider: Some("deepseek".to_string()),
        agent_model: None,
        persona: None,
        tool_filter: None,
    };
    let d = snapshot_descriptor(&input).expect("snapshot");
    match d {
        Descriptor::Continuable { provider, label, agent_provider, .. } => {
            assert_eq!(provider, "fork");
            assert_eq!(label, "worker");
            assert_eq!(agent_provider.as_deref(), Some("deepseek"));
        }
        _ => panic!("应是 continuable"),
    }
}

/// fold：首条 descriptor 权威（后续同类型不重写）。
#[test]
fn fold_descriptor_first_wins() {
    let events = vec![
        json!({ "type": "subagent/descriptor", "data": {
            "version": 2, "mode": "one-shot", "provider": "spawn", "label": "a" } }),
        json!({ "type": "subagent/descriptor", "data": {
            "version": 2, "mode": "continuable", "provider": "fork", "label": "b" } }),
    ];
    let d = fold_descriptor_from_events(&events).expect("fold").expect("has descriptor");
    match d {
        Descriptor::OneShot { label, .. } => assert_eq!(label.as_deref(), Some("a")),
        _ => panic!("首条 descriptor 权威"),
    }
}

/// fold：无 descriptor 事件 → None。
#[test]
fn fold_descriptor_none() {
    let events = vec![json!({ "type": "turn/start", "data": {} })];
    let d = fold_descriptor_from_events(&events).expect("fold ok");
    assert!(d.is_none());
}

/// fold：版本不符 → None（本 runtime 不识别）。
#[test]
fn fold_descriptor_unsupported_version() {
    let events = vec![json!({ "type": "subagent/descriptor", "data": {
        "version": 1, "mode": "one-shot", "provider": "spawn" } })];
    let d = fold_descriptor_from_events(&events).expect("fold ok");
    assert!(d.is_none());
}

/// fold：当前版本但未知字段 → fail loud（Err）。
#[test]
fn fold_descriptor_unknown_field_fails() {
    let events = vec![json!({ "type": "subagent/descriptor", "data": {
        "version": 2, "mode": "one-shot", "provider": "spawn", "surprise": 1 } })];
    assert!(fold_descriptor_from_events(&events).is_err());
}

// ---- depth ----

/// 根 agent 深度 0；childDepth = max(header, runtime)+1。
#[test]
fn resolve_depth_increments() {
    // header=0, runtime 无 → child 1
    assert_eq!(resolve_child_depth(Some(0), None).expect("d"), 1);
    // header=2, runtime=3 → max+1=4
    assert_eq!(resolve_child_depth(Some(2), Some(3)).expect("d"), 4);
    // 无 header 无 runtime → 1
    assert_eq!(resolve_child_depth(None, None).expect("d"), 1);
}

/// 越界 depth → DepthError{attempted, max}。
#[test]
fn resolve_depth_overflow() {
    let err = resolve_child_depth_bounded(Some(10), None, 5).expect_err("越界");
    assert_eq!(err, DepthError::Overflow { attempted: 11, max: 5 });
}

/// maxDepth 校验：非负整数合法；负数/非整数 fail。
#[test]
fn validate_max_depth_checks() {
    assert!(validate_max_depth(None).is_ok());
    assert!(validate_max_depth(Some(3)).is_ok());
    assert!(validate_max_depth(Some(-1)).is_err());
    assert!(validate_max_depth(Some(0)).is_ok(), "0 视为顶层预算");
}

// ---- provider ----

/// in-process（spawn/fork）capabilities 全 true（outputSchema/depthLimit/toolFilter/persona）。
#[test]
fn inproc_provider_all_capabilities() {
    for name in ["spawn", "fork"] {
        let caps = ProviderCapabilities::for_provider(name);
        assert!(caps.output_schema, "{name}");
        assert!(caps.depth_limit, "{name}");
        assert!(caps.tool_filter, "{name}");
        assert!(caps.persona, "{name}");
    }
}

/// out-of-process providers 登记为 NO_START_CAPABILITIES（全 false）。
#[test]
fn outproc_provider_no_capabilities() {
    for name in ["acp", "claude-code", "codex", "dsh-sdk"] {
        let caps = ProviderCapabilities::for_provider(name);
        assert!(!caps.output_schema, "{name}");
        assert!(!caps.persona, "{name}");
    }
}

// ---- catalog ----

/// child 分类：one-shot 有 label、continuable 必有 label。
#[test]
fn category_classification() {
    let c1 = category_child("sess-1", "one-shot", "running", true, Some("audit".to_string()));
    assert_eq!(c1.mode, "one-shot");
    assert_eq!(c1.activity, "running");
    assert!(c1.has_children);
    let c2 = category_child("sess-2", "continuable", "inactive", false, Some("worker".to_string()));
    assert_eq!(c2.mode, "continuable");
    assert_eq!(c2.activity, "inactive");
}

/// diagnostic：corrupt/unsupported/unavailable。
#[test]
fn category_diagnostic() {
    let d = dsh_subagent::catalog::diagnostic_row("sess-9", "corrupt");
    assert_eq!(d.kind, "diagnostic");
    assert_eq!(d.reason.as_deref(), Some("corrupt"));
}

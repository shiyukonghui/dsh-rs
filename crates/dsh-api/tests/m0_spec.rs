//! dsh-api spec 契约测试。
//!
//! 权威参考：`packages/host/apiproxy/src/api/{rpc-map,rpc}.ts`。断言：① 方法目录 52 项与
//! `RpcMethodMap` 完全一致（逐 wire 断言）；② 错误码目录 39 项与 `RpcErrorDetailsMap` 一致；
//! ③ 四象限消息模型；④ session 域 request/value 模式可在仓库内解析（M3 dispatch 的锚点）。

use dsh_api::spec::*;

#[test]
fn method_catalog_matches_rpc_method_map() {
    // 逐项对照 rpc-map.ts（型号：wire = "{namespace}.{method}"）
    let expected: &[&str] = &[
        "session.list", "session.search", "session.create", "session.history", "session.models",
        "session.selectModel", "session.rename", "session.fork", "session.prompt",
        "session.attachment", "session.updateQueue", "session.cancel",
        "subagent.list", "subagent.history", "subagent.prompt", "subagent.interrupt",
        "host.describe", "host.pickDirectory", "host.listDirectory", "host.createDirectory",
        "host.openPath",
        "workspace.list", "workspace.create", "workspace.rename", "workspace.delete",
        "workspace.insertBefore", "workspace.insertSessionBefore", "workspace.archiveSession",
        "skill.list",
        "agentPreset.list", "agentPreset.select", "agentPreset.read", "agentPreset.copy",
        "agentPreset.openDocument", "agentPreset.remove",
        "goal.create", "goal.edit", "goal.pause", "goal.resume", "goal.complete", "goal.clear",
        "settings.describe", "settings.openDocument", "settings.update", "settings.replace",
        "settings.mutate",
        "credentials.describe", "credentials.set", "credentials.unset",
        "llm.providers", "llm.models", "llm.discoverModels",
    ];
    assert_eq!(expected.len(), 52, "RpcMethodMap has 52 client-request methods");
    let catalog: Vec<&str> = methods().iter().map(|m| m.wire.as_str()).collect();
    assert_eq!(catalog, expected, "method catalog must match RpcMethodMap exactly");
}

#[test]
fn method_entries_split_into_namespace_and_method() {
    for m in methods() {
        assert_eq!(m.wire, format!("{}.{}", m.namespace, m.method));
        assert!(!m.request_schema.is_empty());
        assert!(!m.value_schema.is_empty());
    }
}

#[test]
fn method_lookup_and_validation() {
    assert!(has_method("session.list"));
    assert!(has_method("llm.discoverModels"));
    assert!(!has_method("session.nope"));
    assert!(!has_method(""));
    let find = find_method("session.create").expect("find");
    assert_eq!(find.namespace, "session");
    assert_eq!(find.method, "create");
    assert!(find_method("not.a.method").is_none());
}

#[test]
fn namespaces_are_complete_and_unique() {
    let ns = namespaces();
    assert_eq!(
        ns,
        vec!["session", "subagent", "host", "workspace", "skill", "agentPreset", "goal", "settings", "credentials", "llm"]
    );
}

#[test]
fn error_codes_match_rpc_error_details_map() {
    let expected: &[&str] = &[
        "bad-request", "cancelled", "session-not-found", "model-unavailable", "session-conflict",
        "invalid-time-zone", "workspace-attach-failed", "workspace-not-found",
        "workspace-invalid-path", "workspace-name-conflict", "workspace-move-invalid",
        "directory-unreadable", "directory-exists", "directory-create-failed",
        "directory-picker-unavailable", "agent-preset-read-only", "agent-preset-locked",
        "agent-preset-conflict", "agent-preset-not-found", "agent-preset-invalid", "agent-busy",
        "attachment-error", "queue-item-not-found", "steer-unavailable", "command-error",
        "unknown-command", "settings-rejected", "settings-conflict", "credential-rejected",
        "model-discovery-failed", "title-invalid", "fork-unavailable",
        "subagent-parent-unavailable", "subagent-not-found", "subagent-catalog-diagnostic",
        "subagent-not-resumable", "subagent-unauthorized", "subagent-delivery-unavailable",
        "internal",
    ];
    assert_eq!(expected.len(), 39, "RpcErrorDetailsMap has 39 error codes");
    let codes: Vec<&str> = error_codes().iter().map(|e| e.code.as_str()).collect();
    assert_eq!(codes, expected);
}

#[test]
fn error_code_lookup_reports_details_marks() {
    let e = find_error_code("session-conflict").expect("find");
    assert_eq!(e.detail_mark("sessionId"), "required");
    assert_eq!(e.detail_mark("existingCwd"), "optional");
    assert_eq!(e.detail_mark("bogus"), "");
    assert!(has_error_code("internal"));
    assert!(!has_error_code("not-a-code"));
}

#[test]
fn message_model_has_the_four_quadrant_forms() {
    let types = message_types();
    // 目录集合无契约性顺序（serde_json 规范序解析，D-014）——断言集合而非顺序
    assert_eq!(types.len(), 4);
    for t in ["client-request", "server-response", "server-request", "client-response"] {
        assert!(types.iter().any(|k| k == t), "missing message type {t}");
    }
    for t in types.iter() {
        let shape = message_shape(t).expect("shape");
        assert_eq!(shape.discriminant, "type");
    }
    // RpcResult：ok 判别；RpcReceipt：carrier 收据
    assert!(has_rpc_result());
    assert!(has_rpc_receipt());
}

#[test]
fn session_domain_schemas_resolve_inside_the_repo() {
    // M3 dispatch 的锚点：每个 session.* 方法都能在 schemas/session.json 解析出 request/value 模式
    for m in methods() {
        if m.namespace != "session" {
            continue;
        }
        let req = session_request_schema(&m.wire).unwrap_or_else(|| panic!("{} request schema", m.wire));
        let val = session_value_schema(&m.wire).unwrap_or_else(|| panic!("{} value schema", m.wire));
        assert!(req.is_object() && val.is_object(), "{} schemas must be objects", m.wire);
    }
    // 未知方法解析失败（fail loud，不静默）
    assert!(session_request_schema("session.nope").is_none());
    assert!(session_value_schema("host.describe").is_none(), "host domain not in session schema yet");
}

#[test]
fn session_value_schema_anchors_frontend_validation() {
    let v = session_value_schema("session.list").expect("schema");
    assert_eq!(v["required"][0], serde_json::json!("items"));
    assert_eq!(v["properties"]["items"]["type"], serde_json::json!("array"));
}

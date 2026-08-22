//! M4g dsh-workflow 桩测试（TDD 红-绿）。
//!
//! 对齐 `packages/workflow/{workflow,workflow-worker-thread}/src/{types,meta}.ts`：
//! - `validate_meta`：meta 必须 object；name/description 非空 string；whenToUse 可选 string；
//!   phases 数组每项只认 title/detail/provider/model，title 非空；全部 violation 一次列出，
//!   code=META_INVALID。
//! - WorkflowErrorCode 全码（SCRIPT_PARSE…CANCELLED）可路由。
//! - 事件载荷构造：WorkflowRunInfo/WorkflowAgentInfo/EndInfo（seq 1-based）。
//! - 诚实桩：未实现 JS 引擎 → 结构化 isError（UNSUPPORTED_OPTION code），不伪装成功。

use dsh_workflow::error::{WorkflowError, WorkflowErrorCode};
use dsh_workflow::meta::validate_meta;
use dsh_workflow::stub::{run_stub, StubRequest};
use serde_json::json;

/// 合法 meta 规范化：name/description 必留，可选字段有则留。
#[test]
fn meta_valid_normalizes() {
    let input = json!({
        "name": "audit",
        "description": "fan out audit",
        "whenToUse": "when auditing files",
        "phases": [
            { "title": "collect", "detail": "gather files" },
            { "title": "review", "provider": "deepseek", "model": "x" },
        ],
    });
    let meta = validate_meta(&input).unwrap();
    assert_eq!(meta["name"], "audit");
    assert_eq!(meta["description"], "fan out audit");
    assert_eq!(meta["whenToUse"], "when auditing files");
    assert_eq!(meta["phases"][0]["title"], "collect");
    assert_eq!(meta["phases"][1]["model"], "x");
}

/// meta 未知字段 + 缺 name/description → META_INVALID 且列出全部 violation。
#[test]
fn meta_invalid_lists_violations() {
    let bad = json!({
        "surprise": 1,
        "name": "",
        "phases": [ { "hack": true, "title": "" } ],
    });
    let err = validate_meta(&bad).unwrap_err();
    assert!(matches!(err.code, WorkflowErrorCode::MetaInvalid));
    let texts: Vec<String> = err.violations.iter().map(|v| v.message.clone()).collect();
    let join = texts.join("|");
    assert!(join.contains("meta.surprise is not a recognized field"), "{join}");
    assert!(join.contains("meta.name must be a non-empty string"), "{join}");
    assert!(join.contains("meta.description must be a non-empty string"), "{join}");
    assert!(join.contains("meta.phases[0].hack is not a recognized field"), "{join}");
    assert!(join.contains("meta.phases[0].title must be a non-empty string"), "{join}");
}

/// name 非 string / phases 非数组 → violation。
#[test]
fn meta_shape_violations() {
    let err = validate_meta(&json!({ "name": 5, "description": "d", "phases": "nope" })).unwrap_err();
    let join = err.violations.iter().map(|v| v.message.as_str()).collect::<Vec<_>>().join("|");
    assert!(join.contains("meta.name must be a non-empty string"), "{join}");
    assert!(join.contains("meta.phases must be an array"), "{join}");
}

/// 错误码全码可路由（fatality=true 默认）。
#[test]
fn error_codes_routable() {
    let e = WorkflowError::new("parse failed", WorkflowErrorCode::ScriptParse);
    assert!(matches!(e.code, WorkflowErrorCode::ScriptParse));
    assert!(e.fatal);
}

/// 事件载荷：WorkflowRunInfo / WorkflowAgentInfo（seq 1-based）/ EndInfo（outcome）。
#[test]
fn event_payload_shapes() {
    let run = dsh_workflow::event::run_info("run-1", "audit", "desc");
    assert_eq!(run["id"], "run-1");
    let agent = dsh_workflow::event::agent_start_info(1, "collector", Some("collect"), "child-9");
    assert_eq!(agent["seq"], 1);
    assert_eq!(agent["label"], "collector");
    assert_eq!(agent["phase"], "collect");
    assert_eq!(agent["childId"], "child-9");
    let end = dsh_workflow::event::agent_end_info(agent.clone(), "completed");
    assert_eq!(end["outcome"], "completed");
}

/// 诚实桩：未实现引擎 → 结构化 isError（UNSUPPORTED_OPTION）。
#[test]
fn stub_is_error_not_fake_success() {
    let req = StubRequest {
        script: "return 1".to_string(),
        meta_name: "audit".to_string(),
    };
    let result = run_stub(req);
    let err = result.expect_err("桩必须报错");
    assert!(matches!(err.code, WorkflowErrorCode::UnsupportedOption));
    assert!(err.message.contains("workflow execution is not implemented"), "{}", err.message);
}

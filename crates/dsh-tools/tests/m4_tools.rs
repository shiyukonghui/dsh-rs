//! M4h: dsh-tools M4 管理工具「定义构造器」测试。
//!
//! 覆盖每个工具：定义可注册到 ToolRegistry；参数 schema 拒坏参/收好参；
//! 未注入宿主句柄 → 结构化 not-bound 错误；注入槽绑定 fake executor 后正路走通；
//! workflow 恒结构化 isError（META_INVALID / UNSUPPORTED_OPTION）。
//! 对齐参考 TS（逐字文案见各断言）。

use dsh_tools::{ToolExecutionInput, ToolExecutionMode, ToolExecute, ToolRegistry};
use serde_json::{json, Value};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn input(name: &str, args: Value) -> ToolExecutionInput {
    ToolExecutionInput::new("call-m4", name, args, Some("agent-1".to_string()))
}

fn registry() -> ToolRegistry {
    ToolRegistry::new(ToolExecutionMode::Native)
}

/// 取已注册工具的模型面 schema（视图可解析 + to_tool_schema 投影）。
fn registered(r: &ToolRegistry, name: &str) -> dsh_llm::ToolSchema {
    r.get(name, None).expect("registered").to_tool_schema()
}

/// 一个返回固定值的最小 fake executor（验证注入正路）。
fn fake_executor(value: Value) -> ToolExecute {
    Arc::new(move |_, _| Ok(value.clone()))
}

// ---------------------------------------------------------------------------
// todo_write
// ---------------------------------------------------------------------------

#[test]
fn todo_write_registers_and_runs_success() {
    let r = registry();
    let def = dsh_tools::m4::todo_write(false).expect("defines");
    r.register_global(def.clone()).unwrap();

    assert!(r.get("todo_write", None).is_some());
    let out = r.execute(
        &input(
            "todo_write",
            json!({
                "todos": [
                    {"content": "  alpha  ", "status": "in_progress"},
                    {"content": "beta", "status": "pending"},
                ],
            }),
        ),
        None,
    );
    assert!(!out.is_error, "unexpected error: {:?}", out.error);
    let value = out.value.expect("canonical value");
    // 对齐参考：{todos, counts:{pending,inProgress,completed}}，content 已 trim。
    assert_eq!(value["todos"][0]["content"], json!("alpha"));
    assert_eq!(value["counts"], json!({"pending": 1, "inProgress": 1, "completed": 0}));
    // render 逐字对齐参考。
    assert_eq!(
        out.content[0].as_text().map(|t| t.text()).unwrap(),
        "Updated todo list: 1 pending, 1 in progress, 0 completed."
    );
}

#[test]
fn todo_write_enforces_single_active_discipline() {
    let r = registry();
    r.register_global(dsh_tools::m4::todo_write(false).unwrap()).unwrap();
    let out = r.execute(
        &input(
            "todo_write",
            json!({
                "todos": [
                    {"content": "a", "status": "in_progress"},
                    {"content": "b", "status": "in_progress"},
                ],
            }),
        ),
        None,
    );
    assert!(out.is_error);
    assert_eq!(out.error.unwrap().info.unwrap().code, "INVALID_ARGS");
}

#[test]
fn todo_write_rejects_bad_arguments_at_schema() {
    let r = registry();
    r.register_global(dsh_tools::m4::todo_write(true).unwrap()).unwrap();
    // todos 不是数组 → schema 拒。
    let out = r.execute(&input("todo_write", json!({ "todos": "nope" })), None);
    assert!(out.is_error);
    assert_eq!(out.error.unwrap().info.unwrap().code, "INVALID_ARGS");
    // 缺 todos → schema 拒。
    let out = r.execute(&input("todo_write", json!({})), None);
    assert!(out.is_error);
}

#[test]
fn todo_write_requires_owning_agent() {
    let r = registry();
    r.register_global(dsh_tools::m4::todo_write(true).unwrap()).unwrap();
    let mut inp = input(
        "todo_write",
        json!({ "todos": [{"content": "x", "status": "pending"}] }),
    );
    inp.agent = None;
    let out = r.execute(&inp, None);
    assert!(out.is_error);
    // 对齐参考：拒绝无 agent 调用者。
    let info = out.error.unwrap().info.unwrap();
    assert_eq!(info.code, "INVALID_ARGS");
    assert!(info.message.contains("requires an owning agent"));
}

#[test]
fn todo_write_flow_and_parallel_description_register() {
    let r = registry();
    r.register_global(dsh_tools::m4::todo_write(true).unwrap()).unwrap();
    // 并行描述含 PARALLEL 子句、不含 SINGLE 子句。
    let to_schema = registered(&r, "todo_write");
    assert!(to_schema.description.contains("several at once when work genuinely runs in parallel"));
    assert!(!to_schema.description.contains("Keep AT MOST ONE todo"));
}

// ---------------------------------------------------------------------------
// job_output / job_list / job_kill
// ---------------------------------------------------------------------------

#[test]
fn job_output_registers_and_unbound_is_structured_error() {
    let r = registry();
    let tool = dsh_tools::m4::job_output().expect("defines");
    r.register_global(tool.definition()).unwrap();
    assert!(r.get("job_output", None).is_some());

    let out = r.execute(
        &input("job_output", json!({ "job_id": "j1", "wait": true, "timeout_ms": 500 })),
        None,
    );
    assert!(out.is_error);
    let info = out.error.unwrap().info.unwrap();
    assert_eq!(info.code, "NOT_BOUND");
    assert!(info.message.contains("JobRegistry"), "message: {}", info.message);
}

#[test]
fn job_output_bad_args_rejected_before_unbound() {
    let r = registry();
    r.register_global(dsh_tools::m4::job_output().unwrap().definition()).unwrap();
    // 缺 job_id → schema 拒（先于 not-bound）。
    let out = r.execute(&input("job_output", json!({})), None);
    assert!(out.is_error);
    assert_eq!(out.error.unwrap().info.unwrap().code, "INVALID_ARGS");
}

#[test]
fn job_output_bound_runs_render() {
    let r = registry();
    let tool = dsh_tools::m4::job_output().unwrap();
    r.register_global(tool.definition()).unwrap();
    tool.bind(fake_executor(json!({
        "text": "(no new output)",
        "job": {
            "id": "j1", "kind": "term", "label": "build", "status": "killed",
            "startedAt": 1, "finishedAt": 2,
        },
    })));
    let out = r.execute(&input("job_output", json!({ "job_id": "j1" })), None);
    assert!(!out.is_error, "error: {:?}", out.error);
    // render：text + statusLine（无 detail → [status: killed]）。
    assert_eq!(
        out.content[0].as_text().map(|t| t.text()).unwrap(),
        "(no new output)\n[status: killed]"
    );
    assert_eq!(out.value.unwrap()["job"]["status"], json!("killed"));
}

#[test]
fn job_list_registers_and_renders_empty() {
    let r = registry();
    let tool = dsh_tools::m4::job_list().unwrap();
    r.register_global(tool.definition()).unwrap();
    tool.bind(fake_executor(json!([])));
    let out = r.execute(&input("job_list", json!({})), None);
    assert!(!out.is_error);
    assert_eq!(out.content[0].as_text().map(|t| t.text()).unwrap(), "(no background jobs)");
}

#[test]
fn job_kill_outcomes_render() {
    let r = registry();
    let tool = dsh_tools::m4::job_kill().unwrap();
    r.register_global(tool.definition()).unwrap();
    for (outcome, label_contains) in [
        ("already-finished", "had already finished"),
        ("cancellation-requested", "requested cancellation of job"),
    ] {
        tool.bind(fake_executor(json!({
            "outcome": outcome,
            "job": {
                "id": "j2", "kind": "sha", "label": "tick", "status": "completed",
                "startedAt": 1, "finishedAt": 2,
            },
        })));
        let out = r.execute(
            &input("job_kill", json!({ "job_id": "j2", "reason": "done" })),
            None,
        );
        assert!(!out.is_error, "error: {:?}", out.error);
        assert!(out.content[0].as_text().unwrap().text().contains(label_contains));
    }
}

// ---------------------------------------------------------------------------
// schedule_create / schedule_list / schedule_delete
// ---------------------------------------------------------------------------

#[test]
fn schedule_create_params_and_unbound_error() {
    let r = registry();
    let tool = dsh_tools::m4::schedule_create().unwrap();
    r.register_global(tool.definition()).unwrap();
    assert!(r.get("schedule_create", None).is_some());

    // 好参（prompt + after_seconds）→ 未注入 → NOT_BOUND。
    let out = r.execute(
        &input("schedule_create", json!({ "prompt": "ping", "after_seconds": 60 })),
        None,
    );
    assert!(out.is_error);
    assert_eq!(out.error.unwrap().info.unwrap().code, "NOT_BOUND");

    // 缺 prompt → schema 拒。
    let out = r.execute(&input("schedule_create", json!({ "after_seconds": 60 })), None);
    assert!(out.is_error);
    assert_eq!(out.error.unwrap().info.unwrap().code, "INVALID_ARGS");
}

#[test]
fn schedule_create_at_oneof_accepts_object_and_rejects_bad() {
    let r = registry();
    let tool = dsh_tools::m4::schedule_create().unwrap();
    r.register_global(tool.definition()).unwrap();
    // oneOf 分支（at 为字符串）接受 → NOT_BOUND（说明参数校验已过）。
    let out = r.execute(
        &input(
            "schedule_create",
            json!({ "prompt": "wake", "at": "2026-01-01T00:00:00Z" }),
        ),
        None,
    );
    assert_eq!(out.error.unwrap().info.unwrap().code, "NOT_BOUND");
    // at 为错误类型（number）→ schema 拒。
    let out = r.execute(
        &input("schedule_create", json!({ "prompt": "wake", "at": 5 })),
        None,
    );
    assert!(out.is_error);
    assert_eq!(out.error.unwrap().info.unwrap().code, "INVALID_ARGS");
}

#[test]
fn schedule_list_and_delete_define_and_unbound() {
    let r = registry();
    let list = dsh_tools::m4::schedule_list().unwrap();
    r.register_global(list.definition()).unwrap();
    let out = r.execute(&input("schedule_list", json!({})), None);
    assert_eq!(out.error.unwrap().info.unwrap().code, "NOT_BOUND");

    let del = dsh_tools::m4::schedule_delete().unwrap();
    r.register_global(del.definition()).unwrap();
    let out = r.execute(&input("schedule_delete", json!({ "id": "s1" })), None);
    assert_eq!(out.error.unwrap().info.unwrap().code, "NOT_BOUND");
    let out = r.execute(&input("schedule_delete", json!({})), None);
    assert_eq!(out.error.unwrap().info.unwrap().code, "INVALID_ARGS");
}

// ---------------------------------------------------------------------------
// exit_plan_mode
// ---------------------------------------------------------------------------

#[test]
fn exit_plan_mode_defines_and_unbound_then_bound() {
    let r = registry();
    let tool = dsh_tools::m4::exit_plan_mode().unwrap();
    r.register_global(tool.definition()).unwrap();
    assert!(r.get("exit_plan_mode", None).is_some());

    // 未注入 → NOT_BOUND（好参草案）。
    let out = r.execute(
        &input("exit_plan_mode", json!({ "plan": "# Ship it\n\n1. test" })),
        None,
    );
    assert!(out.is_error);
    assert_eq!(out.error.unwrap().info.unwrap().code, "NOT_BOUND");
    // 缺 plan → schema 拒。
    let out = r.execute(&input("exit_plan_mode", json!({})), None);
    assert_eq!(out.error.unwrap().info.unwrap().code, "INVALID_ARGS");

    // 注入 fake（宿主负责「离开 plan mode + 写 command 事件」）→ 正路；
    // 绑定到同一个 M4Tool（共享槽即刻生效）。
    tool.bind(fake_executor(json!({ "approved": true })));
    let out = r.execute(
        &input("exit_plan_mode", json!({ "plan": "# Ship it\n\n1. test" })),
        None,
    );
    assert!(!out.is_error, "error: {:?}", out.error);
    assert_eq!(out.value, Some(json!({ "approved": true })));
    assert_eq!(
        out.content[0].as_text().map(|t| t.text()).unwrap(),
        "Plan approved — plan mode exited; carry out the plan starting with your next step."
    );
}

// ---------------------------------------------------------------------------
// workflow（M4 桩：execute 恒结构化 isError）
// ---------------------------------------------------------------------------

#[test]
fn workflow_stub_always_is_error_unsupported_option() {
    let r = registry();
    r.register_global(dsh_tools::m4::workflow().unwrap()).unwrap();
    assert!(r.get("workflow", None).is_some());

    // meta 合法 → UNSUPPORTED_OPTION（M4 桩）。
    let out = r.execute(
        &input(
            "workflow",
            json!({
                "script": "return 42",
                "meta": { "name": "a-b", "description": "one" },
            }),
        ),
        None,
    );
    assert!(out.is_error);
    let info = out.error.unwrap().info.unwrap();
    assert_eq!(info.code, "UNSUPPORTED_OPTION");
    assert_eq!(info.name, "WorkflowError");

    // meta 非法 → META_INVALID（参数级校验走 dsh-workflow validate_meta；
    // 须通过 schema 基础类型约束、再由 validate_meta 判未知字段违规）。
    let out = r.execute(
        &input(
            "workflow",
            json!({
                "script": "return 42",
                "meta": { "name": "a-b", "description": "one", "bogus": 1 },
            }),
        ),
        None,
    );
    assert!(out.is_error);
    let info = out.error.unwrap().info.unwrap();
    assert_eq!(info.code, "META_INVALID");

    // 缺 script → schema 拒。
    let out = r.execute(
        &input("workflow", json!({ "meta": { "name": "a", "description": "b" } })),
        None,
    );
    assert!(out.is_error);
    assert_eq!(out.error.unwrap().info.unwrap().code, "INVALID_ARGS");
}

// ---------------------------------------------------------------------------
// 对齐参考：description 逐字校验
// ---------------------------------------------------------------------------

#[test]
fn descriptions_match_reference() {
    let r = registry();
    for name in [
        "todo_write", "job_output", "job_list", "job_kill", "schedule_create",
        "schedule_list", "schedule_delete", "exit_plan_mode", "workflow",
    ] {
        let def = match name {
            "todo_write" => dsh_tools::m4::todo_write(false).unwrap(),
            "job_output" => dsh_tools::m4::job_output().unwrap().definition(),
            "job_list" => dsh_tools::m4::job_list().unwrap().definition(),
            "job_kill" => dsh_tools::m4::job_kill().unwrap().definition(),
            "schedule_create" => dsh_tools::m4::schedule_create().unwrap().definition(),
            "schedule_list" => dsh_tools::m4::schedule_list().unwrap().definition(),
            "schedule_delete" => dsh_tools::m4::schedule_delete().unwrap().definition(),
            "exit_plan_mode" => dsh_tools::m4::exit_plan_mode().unwrap().definition(),
            "workflow" => dsh_tools::m4::workflow().unwrap(),
            _ => unreachable!(),
        };
        r.register_global(def).unwrap();
        assert!(r.get(name, None).is_some(), "{name} not registered");
    }

    let job_list = registered(&r, "job_list");
    assert_eq!(
        job_list.description,
        "List your background jobs (running and finished) with their ids, kinds, and statuses."
    );

    let exit = registered(&r, "exit_plan_mode");
    assert_eq!(
        exit.description,
        "Use only in plan mode. Present your plan for the user's review and, on approval, leave plan mode. Send the COMPLETE plan as markdown, starting with a # heading that names it. The user may approve (carry out the plan from your next step) or keep planning — their feedback comes back in the tool result; revise and present again."
    );

    let job_kill = registered(&r, "job_kill");
    assert_eq!(
        job_kill.description,
        "Request cancellation of a running background job by job id. Returns immediately; the job settles as killed once its work actually stops."
    );
}

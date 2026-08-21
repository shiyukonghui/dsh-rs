//! M2b-2: dsh-tools ToolRuntime 注册表/视图/限制/执行管线测试。
//! 覆盖：register（重复/保留名/超时/disposer）、view 解析（遮蔽/限制过滤/自有覆盖/
//! run_code 注入）、restrict/presentAs 消息、executionMode fail-closed、execute 管线
//! （成功/犯错/无效输出/取消/guards/finalize/render 异常）。

use dsh_llm::ContentBlock;
use dsh_scope::{bind_scope_parent, ScopeKey};
use dsh_tools::{
    define_tool, DefineToolOptions, ToolDefinition, ToolExecutionClass, ToolExecutionInput,
    ToolExecutionMode, ToolRegistry, ToolRestriction, ToolSignal, ToolFailureData,
};
use serde_json::{json, Value};
use std::cell::Cell;
use std::rc::Rc;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn echo_def(name: &str) -> ToolDefinition {
    define_tool(DefineToolOptions {
        name: name.to_string(),
        description: format!("{name} a value"),
        parameters: json!({
            "text": { "type": "string", "required": true },
        }),
        output_schema: json!({ "type": "json" }),
        render: Rc::new(|_, value| {
            vec![ContentBlock::text(serde_json::to_string(value).unwrap())]
        }),
        execute: Rc::new(|args, _| Ok(args["text"].clone())),
        ..Default::default()
    })
    .unwrap()
}

#[allow(clippy::type_complexity)]
fn json_tool(
    name: &str,
    body: Rc<dyn Fn(&Value) -> Result<Value, ToolFailureData>>,
) -> ToolDefinition {
    define_tool(DefineToolOptions {
        name: name.to_string(),
        output_schema: json!({ "type": "json" }),
        render: Rc::new(|_, v| vec![ContentBlock::text(v.to_string())]),
        execute: Rc::new(move |_, _| body(&json!(null))),
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

fn kid(parent: &ScopeKey) -> ScopeKey {
    let k = ScopeKey::new();
    bind_scope_parent(k.clone(), parent.clone()).unwrap();
    k
}

// ---------------------------------------------------------------------------
// register
// ---------------------------------------------------------------------------

#[test]
fn register_global_then_get_schemas_known_names() {
    let r = registry();
    r.register_global(Rc::new(echo_def("echo"))).unwrap();

    assert!(r.get("echo", None).is_some());
    assert!(r.get("nope", None).is_none());
    assert_eq!(r.known_names(None), vec!["echo"]);
    let schemas = r.schemas(None);
    assert_eq!(schemas.len(), 1);
    assert_eq!(schemas[0].name, "echo");
    assert_eq!(schemas[0].parameters["type"], json!("object"));
}

#[test]
fn register_duplicate_global_message() {
    let r = registry();
    r.register_global(Rc::new(echo_def("echo"))).unwrap();
    let err = r.register_global(Rc::new(echo_def("echo"))).err().unwrap();
    assert_eq!(
        err,
        "tool \"echo\" is already registered (for a per-agent variant, register through that agent's `agent.ctx` instead)"
    );
}

#[test]
fn register_duplicate_scoped_message() {
    let r = registry();
    let agent = ScopeKey::new();
    r.register(Rc::new(echo_def("echo")), Some(&agent)).unwrap();
    let err = r.register(Rc::new(echo_def("echo")), Some(&agent)).err().unwrap();
    assert_eq!(err, "tool \"echo\" is already registered in this scope");
}

#[test]
fn register_reserved_run_code_rejected() {
    let r = registry();
    let mut tool = define_tool(DefineToolOptions {
        name: "rc".to_string(),
        parameters: json!({ "code": { "type": "string", "required": true } }),
        output_schema: json!({ "type": "json" }),
        ..Default::default()
    })
    .unwrap();
    tool.name = "run_code".to_string();
    let err = r.register_global(Rc::new(tool)).err().unwrap();
    assert_eq!(
        err,
        "tool name \"run_code\" is reserved for the Code Mode presentation transport and cannot be registered or shadowed"
    );
}

#[test]
fn register_rejects_non_positive_timeout() {
    let r = registry();
    // define_tool 自己会先拦截非法 timeout；这里用合法 def 再改坏字段，验证 register 层的运行时检查
    let mut tool = echo_def("slow");
    tool.name = "slow".to_string();
    tool.timeout_ms = Some(-5.0);
    let err = r.register_global(Rc::new(tool)).err().unwrap();
    assert_eq!(err, "tool \"slow\" timeoutMs must be a positive finite number");
}

#[test]
fn register_disposer_removes_tool() {
    let r = registry();
    let d = r.register_global(Rc::new(echo_def("echo"))).unwrap();
    assert!(r.get("echo", None).is_some());
    d();
    assert!(r.get("echo", None).is_none());
    assert_eq!(r.known_names(None), Vec::<String>::new());
}

// ---------------------------------------------------------------------------
// 视图/遮蔽
// ---------------------------------------------------------------------------

#[test]
fn scoped_tool_shadows_global_within_scope_only() {
    let r = registry();
    let root = ScopeKey::new();
    let child = kid(&root);
    let sibling = kid(&root);

    r.register_global(Rc::new(echo_def("echo"))).unwrap();
    r.register(
        Rc::new(define_tool(DefineToolOptions {
            name: "echo".to_string(),
            description: "child echo".to_string(),
            parameters: json!({ "text": { "type": "string", "required": true } }),
            output_schema: json!({ "type": "string" }),
            render: Rc::new(|_, v| vec![ContentBlock::text(v.as_str().unwrap().to_string())]),
            execute: Rc::new(|_, _| Ok(json!("child"))),
            ..Default::default()
        })
        .unwrap()),
        Some(&child),
    )
    .unwrap();

    assert_eq!(r.schemas(Some(&child))[0].description, "child echo");
    assert_eq!(r.schemas(None)[0].description, "echo a value");
    assert_eq!(r.schemas(Some(&sibling))[0].description, "echo a value");
    assert_eq!(r.schemas(Some(&root))[0].description, "echo a value");
}

#[test]
fn ancestor_layers_shadow_farthest_first() {
    let r = registry();
    let root = ScopeKey::new();
    let mid = kid(&root);
    let leaf = kid(&mid);

    r.register_global(Rc::new(echo_def("echo"))).unwrap();
    r.register(
        Rc::new(define_tool(DefineToolOptions {
            name: "echo".to_string(),
            description: "mid echo".to_string(),
            parameters: json!({ "text": { "type": "string", "required": true } }),
            output_schema: json!({ "type": "string" }),
            render: Rc::new(|_, v| vec![ContentBlock::text(v.as_str().unwrap().to_string())]),
            execute: Rc::new(|_, _| Ok(json!("mid"))),
            ..Default::default()
        })
        .unwrap()),
        Some(&mid),
    )
    .unwrap();
    r.register(
        Rc::new(define_tool(DefineToolOptions {
            name: "echo".to_string(),
            description: "leaf echo".to_string(),
            parameters: json!({ "text": { "type": "string", "required": true } }),
            output_schema: json!({ "type": "string" }),
            render: Rc::new(|_, v| vec![ContentBlock::text(v.as_str().unwrap().to_string())]),
            execute: Rc::new(|_, _| Ok(json!("leaf"))),
            ..Default::default()
        })
        .unwrap()),
        Some(&leaf),
    )
    .unwrap();

    assert_eq!(r.schemas(Some(&leaf))[0].description, "leaf echo");
    assert_eq!(r.schemas(Some(&mid))[0].description, "mid echo");
    assert_eq!(r.schemas(None)[0].description, "echo a value");
}

// ---------------------------------------------------------------------------
// restrict
// ---------------------------------------------------------------------------

#[test]
fn restrict_empty_is_noop_error() {
    let r = registry();
    let agent = ScopeKey::new();
    let err = r.restrict(ToolRestriction::default(), &agent).err().unwrap();
    assert_eq!(
        err,
        "tools.restrict({}) is a no-op: pass `allow` and/or `deny` (an empty filter is almost always a materialized-empty-config bug)"
    );
}

#[test]
fn restrict_unknown_names_error_lists_known() {
    let r = registry();
    let agent = ScopeKey::new();
    let err = r.restrict(ToolRestriction::allow(&["bogus"]), &agent).err().unwrap();
    assert_eq!(
        err,
        "tools.restrict() names unknown global tool \"bogus\"; known global tools: (none)"
    );

    r.register_global(Rc::new(echo_def("echo"))).unwrap();
    let err = r.restrict(ToolRestriction::deny(&["ghost", "echo"]), &agent).err().unwrap();
    assert_eq!(
        err,
        "tools.restrict() names unknown global tool \"ghost\"; known global tools: echo"
    );

    let err = r
        .restrict(ToolRestriction::deny(&["run_code"]), &agent)
        .err()
        .unwrap();
    assert_eq!(
        err,
        "tools.restrict() cannot name reserved Code Mode presentation transport \"run_code\"; restrict end-capability tools instead"
    );
}

#[test]
fn restrict_deny_hides_from_schemas_keeps_known_names() {
    let r = registry();
    let root = ScopeKey::new();
    let child = kid(&root);
    r.register_global(Rc::new(echo_def("echo"))).unwrap();
    r.register_global(Rc::new(echo_def("list"))).unwrap();

    assert_eq!(r.schemas(Some(&child)).len(), 2);
    let d = r.restrict(ToolRestriction::deny(&["echo"]), &child).unwrap();

    let names: Vec<String> = r.schemas(Some(&child)).iter().map(|s| s.name.clone()).collect();
    assert_eq!(names, vec!["list"]);
    assert_eq!(r.schemas(None).len(), 2);
    assert!(r.known_names(Some(&child)).contains(&"echo".to_string()));

    d();
    assert_eq!(r.schemas(Some(&child)).len(), 2);
}

#[test]
fn restrict_allow_keeps_only_listed() {
    let r = registry();
    let agent = ScopeKey::new();
    r.register_global(Rc::new(echo_def("echo"))).unwrap();
    r.register_global(Rc::new(echo_def("list"))).unwrap();

    let _d = r.restrict(ToolRestriction::allow(&["echo"]), &agent).unwrap();
    let names: Vec<String> = r.schemas(Some(&agent)).iter().map(|s| s.name.clone()).collect();
    assert_eq!(names, vec!["echo"]);
}

// ---------------------------------------------------------------------------
// presentAs / run_code 注入
// ---------------------------------------------------------------------------

#[test]
fn present_as_conflict_message() {
    let r = registry();
    let agent = ScopeKey::new();
    r.present_as(ToolExecutionMode::Code, &agent).unwrap();
    let err = r.present_as(ToolExecutionMode::Both, &agent).err().unwrap();
    assert_eq!(
        err,
        "tools.presentAs(\"both\") conflicts with \"code\" already declared for this scope; one composition selects one presentation"
    );
}

#[test]
fn code_mode_injects_run_code_and_collapses_others() {
    let r = registry();
    let agent = ScopeKey::new();
    r.register_global(Rc::new(echo_def("echo"))).unwrap();
    r.present_as(ToolExecutionMode::Code, &agent).unwrap();

    let names: Vec<String> = r.known_names(Some(&agent)).into_iter().collect();
    assert!(names.contains(&"run_code".to_string()));
    assert!(names.contains(&"echo".to_string()));

    let out = r.execute(&input("echo", json!({ "text": "x" })), Some(&agent));
    assert!(out.is_error);
    let err = out.error.unwrap();
    assert_eq!(err.message, "unknown tool \"echo\": only `run_code` is callable directly — call `echo` from inside a `run_code` program instead");
    assert_eq!(err.info.unwrap().code, "UNKNOWN_TOOL");

    let out = r.execute(&input("run_code", json!({ "code": "1" })), Some(&agent));
    assert!(out.is_error, "run_code is a placeholder until M5 code runtime");
    let err2 = out.error.unwrap();
    assert!(err2.message.contains("code runtime"), "{}", err2.message);
}

// ---------------------------------------------------------------------------
// executionMode
// ---------------------------------------------------------------------------

#[test]
fn execution_mode_fail_closed() {
    let r = registry();
    let agent = ScopeKey::new();

    r.register_global(Rc::new(echo_def("plain"))).unwrap();
    assert_eq!(
        r.execution_mode(&input("plain", json!({ "text": "x" })), Some(&agent)),
        ToolExecutionClass::Exclusive
    );

    let mut safe = echo_def("safe");
    safe.is_concurrency_safe = Some(Rc::new(|_| true));
    r.register_global(Rc::new(safe)).unwrap();
    assert_eq!(
        r.execution_mode(&input("safe", json!({ "text": "x" })), Some(&agent)),
        ToolExecutionClass::Parallel
    );

    let mut unsafe_tool = echo_def("unsafe");
    unsafe_tool.is_concurrency_safe = Some(Rc::new(|_| false));
    r.register_global(Rc::new(unsafe_tool)).unwrap();
    assert_eq!(
        r.execution_mode(&input("unsafe", json!({ "text": "x" })), Some(&agent)),
        ToolExecutionClass::Exclusive
    );

    assert_eq!(
        r.execution_mode(&input("ghost", json!({})), Some(&agent)),
        ToolExecutionClass::Exclusive
    );
}

// ---------------------------------------------------------------------------
// execute 管线
// ---------------------------------------------------------------------------

#[test]
fn execute_success_runs_body_render_and_validates() {
    let r = registry();
    r.register_global(Rc::new(echo_def("echo"))).unwrap();
    let out = r.execute(&input("echo", json!({ "text": "hello" })), None);
    assert!(!out.is_error);
    assert_eq!(out.value, Some(json!("hello")));
    assert_eq!(out.content.len(), 1);
    assert_eq!(out.content[0].as_text().map(|t| t.text()).unwrap(), "\"hello\"");
    assert_eq!(out.execution.call.name, "echo");
}

#[test]
fn execute_unknown_tool_message() {
    let r = registry();
    let out = r.execute(&input("ghost", json!({})), None);
    assert!(out.is_error);
    assert_eq!(out.error.unwrap().message, "unknown tool \"ghost\"");
}

#[test]
fn execute_tool_failure_is_error_result() {
    let r = registry();
    r.register_global(Rc::new(json_tool("boom", Rc::new(|_| {
        Err(ToolFailureData::new("kaboom", "BOOM", "SomeError"))
    }))))
    .unwrap();
    let out = r.execute(&input("boom", json!({})), None);
    assert!(out.is_error);
    assert_eq!(out.content[0].as_text().map(|t| t.text()).unwrap(), "Error: kaboom");
    let info = out.error.unwrap();
    assert_eq!(info.message, "kaboom");
    assert_eq!(info.info.unwrap().code, "BOOM");
}

#[test]
fn execute_invalid_output_is_tool_output_error() {
    let r = registry();
    r.register_global(Rc::new(define_tool(DefineToolOptions {
        name: "bad".to_string(),
        output_schema: json!({ "type": "string" }),
        render: Rc::new(|_, v| vec![ContentBlock::text(v.to_string())]),
        execute: Rc::new(|_, _| Ok(json!({ "not": "a string" }))),
        ..Default::default()
    })
    .unwrap()))
    .unwrap();
    let out = r.execute(&input("bad", json!({})), None);
    assert!(out.is_error);
    let info = out.error.unwrap();
    assert_eq!(info.info.unwrap().code, "INVALID_TOOL_OUTPUT");
    assert_eq!(info.message, "tool \"bad\" returned invalid output: \"value\" must be a string");
}

#[test]
fn execute_render_panic_is_output_error_mentioning_render() {
    let r = registry();
    r.register_global(Rc::new(define_tool(DefineToolOptions {
        name: "renderpanic".to_string(),
        output_schema: json!({ "type": "json" }),
        render: Rc::new(|_, _| panic!("render blew up")),
        execute: Rc::new(|_, _| Ok(json!("ok"))),
        ..Default::default()
    })
    .unwrap()))
    .unwrap();
    let out = r.execute(&input("renderpanic", json!({})), None);
    assert!(out.is_error);
    let info = out.error.unwrap();
    assert_eq!(info.info.unwrap().code, "INVALID_TOOL_OUTPUT");
    assert!(info.message.contains("output.render failed: render blew up"), "{}", info.message);
}

#[test]
fn execute_aborted_before_dispatch() {
    let r = registry();
    r.register_global(Rc::new(echo_def("echo"))).unwrap();
    let inp = input("echo", json!({ "text": "x" }));
    inp.signal.abort("cancelled by user");
    let out = r.execute(&inp, None);
    assert!(out.is_error);
    assert_eq!(out.error.unwrap().info.unwrap().code, "ABORTED_BEFORE_DISPATCH");
}

#[test]
fn execute_aborted_after_body() {
    let r = registry();
    r.register_global(Rc::new(define_tool(DefineToolOptions {
        name: "selfabort".to_string(),
        output_schema: json!({ "type": "json" }),
        render: Rc::new(|_, v| vec![ContentBlock::text(v.to_string())]),
        execute: Rc::new(|_, ctx| {
            ctx.signal.abort("inside");
            Ok(json!("late"))
        }),
        ..Default::default()
    })
    .unwrap()))
    .unwrap();
    let out = r.execute(&input("selfabort", json!({})), None);
    assert!(out.is_error);
    assert_eq!(out.error.unwrap().info.unwrap().code, "ABORTED");
}

#[test]
fn execute_signal_reason_visible() {
    let r = registry();
    r.register_global(Rc::new(echo_def("echo"))).unwrap();
    let inp = input("echo", json!({ "text": "x" }));
    inp.signal.abort("user cancelled");
    let _ = r.execute(&inp, None);
    assert_eq!(inp.signal.reason().as_deref(), Some("user cancelled"));
    assert!(inp.signal.aborted());
}

#[test]
fn execute_guard_blocks_before_body() {
    let r = registry();
    let agent = ScopeKey::new();
    let ran = Rc::new(Cell::new(false));
    let ran2 = ran.clone();
    r.register(
        Rc::new(define_tool(DefineToolOptions {
            name: "secret".to_string(),
            output_schema: json!({ "type": "json" }),
            render: Rc::new(|_, v| vec![ContentBlock::text(v.to_string())]),
            execute: Rc::new(move |_, _| {
                ran2.set(true);
                Ok(json!("ran"))
            }),
            ..Default::default()
        })
        .unwrap()),
        Some(&agent),
    )
    .unwrap();
    let _guard = r
        .add_guard(
            Rc::new(|name, _| {
                if name == "secret" {
                    Some("secret tool requires approval".to_string())
                } else {
                    None
                }
            }),
            Some(&agent),
        )
        .unwrap();

    let out = r.execute(&input("secret", json!({})), Some(&agent));
    assert!(out.is_error);
    assert!(!ran.get(), "body must not run when guard blocks");
    assert_eq!(out.content[0].as_text().map(|t| t.text()).unwrap(), "Error: secret tool requires approval");
}

#[test]
fn execute_finalize_transforms_content() {
    let r = registry();
    r.register_global(Rc::new(define_tool(DefineToolOptions {
        name: "tx".to_string(),
        output_schema: json!({ "type": "json" }),
        render: Rc::new(|_, v| vec![ContentBlock::text(v.to_string())]),
        execute: Rc::new(|_, _| Ok(json!("raw"))),
        finalize_content: Some(Rc::new(|_, snapshot| {
            let mut c = snapshot.content.clone();
            c.push(ContentBlock::text(" (finalized)"));
            Some(c)
        })),
        ..Default::default()
    })
    .unwrap()))
    .unwrap();
    let out = r.execute(&input("tx", json!({})), None);
    assert!(!out.is_error);
    assert_eq!(out.content.len(), 2);
    assert_eq!(out.content[1].as_text().map(|t| t.text()).unwrap(), " (finalized)");
    assert_eq!(out.value, Some(json!("raw")));
}

// 保持 ToolSignal 可见性引用（输入信号来自调用方；此处确认工具收到的信号是同一共享句柄）
#[test]
fn execute_tool_sees_shared_call_signal() {
    let r = registry();
    let captured: Rc<Cell<Option<bool>>> = Rc::new(Cell::new(None));
    let cap2 = captured.clone();
    r.register_global(Rc::new(define_tool(DefineToolOptions {
        name: "probe".to_string(),
        output_schema: json!({ "type": "json" }),
        render: Rc::new(|_, v| vec![ContentBlock::text(v.to_string())]),
        execute: Rc::new(move |_, ctx| {
            cap2.set(Some(ctx.signal.aborted()));
            Ok(json!("probed"))
        }),
        ..Default::default()
    })
    .unwrap()))
    .unwrap();
    let _ = r.execute(&input("probe", json!({})), None);
    assert_eq!(captured.get(), Some(false));
}

// 保留 ToolSignal 类型引用（signal 复合字段在输入中可用）
#[allow(dead_code)]
fn _signal_is_public(s: &ToolSignal) -> &ToolSignal {
    s
}

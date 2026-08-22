//! M2e-3：tool-calls 调度测试——schedule execute_tool_calls（parseArguments、
//! tool/call + tool/result 事件 + sourceEventSeqs、并行/独占屏障、concluded、
//! 未知工具错误结果）。

#![allow(clippy::type_complexity)] // 录制工具闭包（Rc<dyn Fn>）与 define_tool seam 一致
#![allow(clippy::result_large_err)]

use std::cell::RefCell;
use std::rc::Rc;

use dsh_llm::{
    CallId, ContentBlock, Message, MessageSource, Role, ToolCallBlock,
};
use dsh_session::{EventKind, Session, SessionId};
use dsh_tools::{
    define_tool, DefineToolOptions, ToolRegistry, CODE_UNKNOWN_TOOL,
};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn session() -> Rc<Session> {
    Rc::new(Session::create(SessionId::from_raw("s0"), None, None).unwrap())
}

fn echo_def(name: &str) -> dsh_tools::ToolDefinition {
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

/// 并行 echo（is_concurrency_safe → true）。
fn parallel_echo_def(name: &str) -> dsh_tools::ToolDefinition {
    let mut def = echo_def(name);
    def.is_concurrency_safe = Some(Rc::new(|_| true));
    def
}

/// 把 model 产生的 arguments 原样记录下来的工具（用于 parseArguments 断言）。
/// 直接构造 `ToolDefinition`（不经 define_tool 的对象校验包装），参数空 schema 接受任何形态。
fn recorder_def(name: &str, log: Rc<RefCell<Vec<Value>>>, conclude: bool) -> dsh_tools::ToolDefinition {
    dsh_tools::types::ToolDefinition {
        name: name.to_string(),
        description: "record arguments".into(),
        parameters: json!({}),
        output: dsh_tools::types::ToolOutputDefinition {
            schema: dsh_tools::value_schema_spec_to_json_schema(&json!({ "type": "json" })).unwrap(),
            render: Rc::new(|_, v| vec![ContentBlock::text(v.to_string())]),
            presentation_meta: None,
        },
        timeout_ms: None,
        execute: Rc::new(move |args, ctx| {
            log.borrow_mut().push(args.clone());
            if conclude {
                ctx.conclude_turn();
            }
            Ok(json!(args))
        }),
        finalize_content: None,
        is_concurrency_safe: None,
        present_call: None,
        present_result: None,
    }
}

fn call(id: &str, name: &str, arguments: &str) -> ToolCallBlock {
    ToolCallBlock {
        id: CallId::from_raw(id),
        name: name.into(),
        arguments: arguments.into(),
    }
}

fn registry(defs: Vec<Rc<dsh_tools::ToolDefinition>>) -> ToolRegistry {
    let r = ToolRegistry::new(dsh_tools::ToolExecutionMode::Native);
    for d in defs {
        r.register_global(d).unwrap();
    }
    r
}

fn event(s: &Rc<Session>, kind: EventKind) -> Vec<dsh_session::SessionEvent> {
    s.events().into_iter().filter(|e| e.kind == kind).collect()
}

fn run(
    s: &Rc<Session>,
    tools: &ToolRegistry,
    turn: u64,
    step: u64,
    calls: &[ToolCallBlock],
) -> (bool, Vec<Message>) {
    let mut accepted = Vec::new();
    let concluded = dsh_agent_loop::execute_tool_calls(
        s,
        tools,
        None,
        Some("agent-1"),
        8,
        turn,
        step,
        calls,
        &mut |m| accepted.push(m),
    )
    .unwrap();
    (concluded, accepted)
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[test]
fn single_parallel_call_commits_events_with_source_seq() {
    let s = session();
    let tools = registry(vec![Rc::new(parallel_echo_def("echo"))]);
    let (concluded, accepted) = run(&s, &tools, 1, 1, &[call("c1", "echo", r#"{"text":"hi"}"#)]);
    assert!(!concluded);
    assert!(accepted.is_empty());

    let tool_calls = event(&s, EventKind::ToolCall);
    assert_eq!(tool_calls.len(), 1);
    let tc = &tool_calls[0].data;
    assert_eq!(tc["turn"], json!(1));
    assert_eq!(tc["step"], json!(1));
    assert_eq!(tc["callId"], json!("c1"));
    assert_eq!(tc["name"], json!("echo"));
    assert_eq!(tc["arguments"], json!(r#"{"text":"hi"}"#)); // 原始字符串

    let results = event(&s, EventKind::ToolResult);
    assert_eq!(results.len(), 1);
    let tr = &results[0];
    // 链接到自己的 call 事件
    assert_eq!(tr.source_event_seqs(), Some(&vec![tool_calls[0].seq]));
    // 消息：role=user、tool-result block 包裹原始内容、isError=false
    let msg: Message = serde_json::from_value(tr.data["message"].clone()).unwrap();
    assert_eq!(msg.role, Role::User);
    assert!(matches!(&msg.source, MessageSource::Tool(t) if t.call_id == CallId::from_raw("c1")));
    assert_eq!(msg.content.len(), 1);
    let ContentBlock::ToolResult(block) = &msg.content[0] else {
        panic!("expected a tool-result block");
    };
    assert_eq!(block.tool_call_id, CallId::from_raw("c1"));
    assert_eq!(block.is_error, Some(false));
    let ContentBlock::Text(t) = &block.content[0] else {
        panic!("expected text inside the tool-result block");
    };
    assert_eq!(t.text(), "\"hi\"");
    assert!(tr.data.get("error").is_none());
    assert!(tr.data.get("meta").is_none());
}

#[test]
fn exclusive_default_tool_executes_sequentially() {
    // 无 is_concurrency_safe → exclusive；两个调用各自成组，仍按模型顺序提交。
    let s = session();
    let tools = registry(vec![Rc::new(echo_def("echo"))]);
    let (_, _) = run(
        &s,
        &tools,
        1,
        1,
        &[
            call("c1", "echo", r#"{"text":"a"}"#),
            call("c2", "echo", r#"{"text":"b"}"#),
        ],
    );
    let results = event(&s, EventKind::ToolResult);
    assert_eq!(results.len(), 2);
    let order: Vec<String> = results
        .iter()
        .map(|e| {
            let msg: Message = serde_json::from_value(e.data["message"].clone()).unwrap();
            let ContentBlock::ToolResult(b) = &msg.content[0] else {
                panic!("tool-result block expected");
            };
            if let ContentBlock::Text(t) = &b.content[0] {
                t.text().to_string()
            } else {
                String::new()
            }
        })
        .collect();
    assert_eq!(order, vec!["\"a\"", "\"b\""]);
    // 每个 result 链接到自己的 call
    let tool_calls = event(&s, EventKind::ToolCall);
    assert_eq!(results[0].source_event_seqs(), Some(&vec![tool_calls[0].seq]));
    assert_eq!(results[1].source_event_seqs(), Some(&vec![tool_calls[1].seq]));
}

#[test]
fn mixed_parallel_and_exclusive_commit_all_in_model_order() {
    // p 并行、x 独占：p,x 同组时 x 成为新屏障，但仍完整提交；顺序模型序。
    let s = session();
    let tools = registry(vec![
        Rc::new(parallel_echo_def("p")),
        Rc::new(echo_def("x")),
    ]);
    let (_, _) = run(
        &s,
        &tools,
        1,
        1,
        &[
            call("c1", "p", r#"{"text":"p1"}"#),
            call("c2", "x", r#"{"text":"x1"}"#),
            call("c3", "p", r#"{"text":"p2"}"#),
        ],
    );
    let tool_calls = event(&s, EventKind::ToolCall);
    let results = event(&s, EventKind::ToolResult);
    assert_eq!(tool_calls.len(), 3);
    assert_eq!(results.len(), 3);
    let names: Vec<String> = tool_calls
        .iter()
        .map(|e| e.data["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, vec!["p", "x", "p"]);
    for (tc, tr) in tool_calls.iter().zip(results.iter()) {
        assert_eq!(tr.source_event_seqs(), Some(&vec![tc.seq]));
    }
}

#[test]
fn parse_arguments_empty_means_empty_object_invalid_json_is_raw_string() {
    let s = session();
    let log = Rc::new(RefCell::new(Vec::new()));
    let tools = registry(vec![Rc::new(recorder_def("rec", log.clone(), false))]);
    let (_, _) = run(
        &s,
        &tools,
        1,
        1,
        &[
            call("c1", "rec", ""),           // 空串 → {}
            call("c2", "rec", "{bad json"),  // 坏 JSON → 原样字符串
        ],
    );
    assert_eq!(log.borrow().len(), 2);
    assert_eq!(log.borrow()[0], json!({}));
    assert_eq!(log.borrow()[1], json!("{bad json"));
}

#[test]
fn concluded_true_when_any_result_carries_concludes_turn() {
    let s = session();
    let log = Rc::new(RefCell::new(Vec::new()));
    let tools = registry(vec![
        Rc::new(recorder_def("finish", log.clone(), true)),
        Rc::new(recorder_def("plain", log.clone(), false)),
    ]);
    let (concluded, _) = run(
        &s,
        &tools,
        1,
        1,
        &[call("c1", "finish", "{}"), call("c2", "plain", "{}")],
    );
    assert!(concluded);
    // 即使 conclude，已排队的调用仍完整提交
    let results = event(&s, EventKind::ToolResult);
    assert_eq!(results.len(), 2);
}

#[test]
fn unknown_tool_produces_error_result_event() {
    let s = session();
    let tools = registry(vec![Rc::new(echo_def("echo"))]);
    let (_, _) = run(&s, &tools, 1, 1, &[call("c1", "nope", "{}")]);
    let results = event(&s, EventKind::ToolResult);
    assert_eq!(results.len(), 1);
    let tr = &results[0].data;
    let error = tr["error"].clone();
    assert_eq!(error["name"], json!("ToolNotFoundError"));
    assert_eq!(error["code"], json!(CODE_UNKNOWN_TOOL));
    let msg: Message = serde_json::from_value(tr["message"].clone()).unwrap();
    let ContentBlock::ToolResult(block) = &msg.content[0] else {
        panic!("tool-result block expected");
    };
    assert_eq!(block.is_error, Some(true));
    let ContentBlock::Text(t) = &block.content[0] else {
        panic!("text block expected");
    };
    assert!(t.text().starts_with("Error: unknown tool"));
}

#[test]
fn tool_call_payload_ignores_unused_agent_and_max_parallel() {
    // ToolExecutionInput 把 agent 归因传给工具（记入 call 的 agent 字段）。
    let s = session();
    let seen_agent = Rc::new(RefCell::new(None::<String>));
    let tools = registry(vec![Rc::new(define_tool(DefineToolOptions {
        name: "who".into(),
        description: "whoami".into(),
        output_schema: json!({ "type": "json" }),
        render: Rc::new(|_, v| vec![ContentBlock::text(v.to_string())]),
        execute: {
            let seen_agent = seen_agent.clone();
            Rc::new(move |_, ctx| {
                *seen_agent.borrow_mut() = ctx.agent.clone();
                Ok(json!(ctx.agent))
            })
        },
        ..Default::default()
    })
    .unwrap())]);
    let (_, _) = run(&s, &tools, 1, 1, &[call("c1", "who", "{}")]);
    assert_eq!(seen_agent.borrow().as_deref(), Some("agent-1"));
}

//! dsh-terminal：6 工具纯面测试（M5-DESIGN §6.2；逐字工具/tool-terminal）。

use dsh_terminal::{
    parse_terminal_open_args, parse_terminal_send_args, render_terminal_close,
    render_terminal_list, render_terminal_read, render_terminal_send, render_terminal_send_read,
    render_terminal_signal, render_terminal_spawn, terminal_close_schema, terminal_list_schema,
    terminal_open_schema, terminal_read_schema, terminal_send_schema, terminal_signal_schema,
    RenderedTerminalSession, TerminalCloseOutcome, TerminalRenderStatus, TerminalWaitReason,
};
use serde_json::json;

#[test]
fn schemas_are_verbatim() {
    let open_value = terminal_open_schema();
    let open = open_value.as_object().expect("obj");
    assert_eq!(open["type"]["required"], json!(true));
    assert_eq!(open["name"]["type"], json!("string"));
    assert_eq!(
        open["cwd"]["description"],
        "Initial working directory. Defaults to the deployment workspace root."
    );

    let send_on_value = terminal_send_schema(true);
    let send = send_on_value.as_object().expect("obj");
    assert_eq!(send["sessionId"]["required"], json!(true));
    assert!(send.contains_key("submit"));
    assert!(send.contains_key("run_in_background"));

    let send_off_value = terminal_send_schema(false);
    let send_off = send_off_value.as_object().expect("obj");
    assert!(!send_off.contains_key("run_in_background"));

    let sig_value = terminal_signal_schema();
    let sig = sig_value.as_object().expect("obj");
    assert_eq!(
        sig["signal"]["enum"],
        json!(["SIGINT", "SIGTERM", "SIGKILL", "SIGTSTP", "SIGHUP"])
    );

    let read_value = terminal_read_schema();
    let read = read_value.as_object().expect("obj");
    assert_eq!(
        read["count"]["description"],
        "Requested line count (default 500; backend caps apply)."
    );

    let close_value = terminal_close_schema();
    assert!(close_value
        .as_object()
        .expect("obj")
        .contains_key("sessionId"));
    assert!(terminal_list_schema()
        .as_object()
        .expect("obj res")
        .is_empty());
}

#[test]
fn parse_open_args() {
    let (backend, name, cwd) =
        parse_terminal_open_args(&json!({ "type": "shell", "name": "main", "cwd": "/tmp" }))
            .expect("ok");
    assert_eq!(backend, "shell");
    assert_eq!(name.as_deref(), Some("main"));
    assert_eq!(cwd.as_deref(), Some("/tmp"));
    assert!(parse_terminal_open_args(&json!({"name": "x"})).is_err());
}

#[test]
fn parse_send_args_submit_defaults_true() {
    let (id, text, submit, bg) =
        parse_terminal_send_args(&json!({ "sessionId": "pty-1", "text": "ls" })).expect("ok");
    assert_eq!(id, "pty-1");
    assert_eq!(text, "ls");
    assert!(submit, "submit 缺省 true");
    assert_eq!(bg, None);
    let (_, _, submit2, bg2) = parse_terminal_send_args(
        &json!({ "sessionId": "pty-1", "text": "x", "submit": false, "run_in_background": true }),
    )
    .expect("ok");
    assert!(!submit2);
    assert_eq!(bg2, Some(true));
    assert!(
        parse_terminal_send_args(&json!({"text": "x"})).is_err(),
        "缺 sessionId → err"
    );
}

// ---------- 渲染词汇快照 ----------

#[test]
fn render_spawn_with_and_without_name() {
    let with_name = render_terminal_spawn("pty-1", Some("main"), "shell", "", 100_000);
    assert_eq!(
        with_name,
        "started terminal session pty-1 (main) [type: shell]\n(no startup output)"
    );
    let no_name = render_terminal_spawn("pty-1", None, "shell", "Hello\r\n", 100_000);
    assert_eq!(
        no_name,
        "started terminal session pty-1 [type: shell]\nHello\r\n"
    );
}

#[test]
fn render_send_full_markers() {
    let text = render_terminal_send(
        "$ ls\r\nfile.txt\r\n",
        TerminalWaitReason::InferredIdle,
        &TerminalRenderStatus::Running,
        false,
        100_000,
    );
    assert_eq!(
        text,
        "$ ls\r\nfile.txt\r\n\n[wait: inferred_idle]\n[session: running]"
    );
}

#[test]
fn render_send_exited_status_and_no_output() {
    let text = render_terminal_send(
        "",
        TerminalWaitReason::SessionExit,
        &TerminalRenderStatus::Exited {
            exit_code: Some(7),
            signal: None,
        },
        false,
        100_000,
    );
    assert_eq!(
        text,
        "(no new output)\n[wait: session_exit]\n[session: exited code=7 signal=null]"
    );
}

#[test]
fn render_send_respects_byte_cap_with_tail() {
    let long = "x".repeat(500);
    let text = render_terminal_send(
        &long,
        TerminalWaitReason::StdinRead,
        &TerminalRenderStatus::Running,
        false,
        100,
    );
    assert!(
        text.ends_with("[output truncated]"),
        "cap 时补截断标记: {text:?}"
    );
    assert!(text.len() <= 110, "text len {}", text.len());
}

#[test]
fn render_send_read_delta_and_truncated() {
    assert_eq!(render_terminal_send_read("a\nb\n", false), "a\nb\n");
    assert_eq!(
        render_terminal_send_read("a", true),
        "a\n[output truncated]"
    );
    assert_eq!(render_terminal_send_read("", true), "[output truncated]");
}

#[test]
fn render_read_pagination_markers() {
    let text = render_terminal_read("line-a\nline-b\n", 42, 1, 2, false, 100_000);
    assert_eq!(text, "line-a\nline-b\n\n[lines: 1-2 of 42]");
    let empty = render_terminal_read("", 0, 0, 0, false, 100_000);
    assert_eq!(empty, "(no retained output)\n[lines: 0-0 of 0]");
}

#[test]
fn render_signal_and_close() {
    assert_eq!(
        render_terminal_signal("SIGINT", 4242),
        "delivered SIGINT to foreground process group 4242"
    );
    assert_eq!(
        render_terminal_close("pty-1", TerminalCloseOutcome::Closed),
        "closed terminal session pty-1"
    );
    assert_eq!(
        render_terminal_close("pty-1", TerminalCloseOutcome::AlreadyClosing),
        "terminal session pty-1 was already closing"
    );
}

#[test]
fn render_list_sessions_and_empty() {
    assert_eq!(render_terminal_list(&[], 100_000), "(no terminal sessions)");
    let sessions = vec![
        RenderedTerminalSession {
            session_id: "pty-1".into(),
            name: Some("main".into()),
            backend_type: "shell".into(),
            pid: Some(1234),
            status: TerminalRenderStatus::Running,
        },
        RenderedTerminalSession {
            session_id: "pty-2".into(),
            name: None,
            backend_type: "shell".into(),
            pid: None,
            status: TerminalRenderStatus::Exited {
                exit_code: Some(0),
                signal: None,
            },
        },
    ];
    let text = render_terminal_list(&sessions, 100_000);
    assert_eq!(
        text,
        "pty-1 (main) [shell] running pid=1234\npty-2 [shell] exited code=0 signal=null"
    );
}

#[test]
fn status_render_from_session_status() {
    use dsh_terminal::TerminalSessionStatus;
    assert_eq!(
        TerminalRenderStatus::from(TerminalSessionStatus::Running).render(),
        "running"
    );
    assert_eq!(
        TerminalRenderStatus::from(TerminalSessionStatus::Exited).render(),
        "exited code=null signal=null"
    );
    assert_eq!(
        TerminalRenderStatus::from(TerminalSessionStatus::Aborted).render(),
        "exited code=null signal=null"
    );
}

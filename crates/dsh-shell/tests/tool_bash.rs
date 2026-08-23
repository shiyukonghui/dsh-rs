//! dsh-shell：tool-bash 纯面测试（M5-DESIGN §5.3；逐字对齐 `tool-bash/src/`）。
//!
//! 覆盖：camelCase `timeoutMs` schema 逐字、execute 校验文案、模型面标记词汇快照
//! （body / `[stderr]` / `(no output)` / 截断 spill / sandbox 拒绝 + 升级提示 / 超时 /
//! 信号 / 退出码——非零退出是报告不是 isError）。

use dsh_sandbox::SandboxMode;
use dsh_shell::{
    bash_tool_parameters, parse_bash_args, render_bash_process_read, render_bash_result,
    ShellCollectedOutput, ShellProcessRead, ShellRunResult, ShellSandboxInfo,
};
use serde_json::json;
use std::path::PathBuf;

fn out(text: &str) -> ShellCollectedOutput {
    ShellCollectedOutput {
        text: text.to_string(),
        truncated: false,
        spill_path: None,
    }
}
fn out_trunc(text: &str, spill: Option<&str>) -> ShellCollectedOutput {
    ShellCollectedOutput {
        text: text.to_string(),
        truncated: true,
        spill_path: spill.map(PathBuf::from),
    }
}

fn result(
    exit: Option<i32>,
    timed_out: bool,
    signal: Option<&str>,
    stdout: &str,
    stderr: &str,
) -> ShellRunResult {
    ShellRunResult {
        exit_code: exit,
        signal: signal.map(|s| s.to_string()),
        timed_out,
        aborted: false,
        timeout_ms: 300,
        stdout: out(stdout),
        stderr: out(stderr),
        sandbox: None,
    }
}

// ---------- 参数解析（参考 validateBashArgs） ----------

#[test]
fn parse_ok_full_args() {
    let args = parse_bash_args(&json!({
        "command": "git status",
        "description": "Show working tree status",
        "timeoutMs": 5000,
        "workdir": "/tmp",
        "run_in_background": true,
    }))
    .expect("parse ok");
    assert_eq!(args.command, "git status");
    assert_eq!(args.description, "Show working tree status");
    assert_eq!(args.timeout_ms, Some(5000));
    assert_eq!(args.workdir.as_deref(), Some("/tmp"));
    assert_eq!(args.run_in_background, Some(true));
    assert_eq!(args.sandbox_permissions, None);
}

#[test]
fn parse_rejects_blank_command() {
    assert_eq!(
        parse_bash_args(&json!({"command": "   ", "description": "x"})).unwrap_err(),
        "invalid command: expected a non-empty string"
    );
    assert_eq!(
        parse_bash_args(&json!({"description": "x"})).unwrap_err(),
        "invalid command: expected a non-empty string"
    );
}

#[test]
fn parse_rejects_blank_description() {
    assert_eq!(
        parse_bash_args(&json!({"command": "ls", "description": ""})).unwrap_err(),
        "invalid description: expected a non-empty string"
    );
}

#[test]
fn parse_rejects_bad_timeout_with_verbatim_message() {
    for bad in [json!(0), json!(-1), json!("abc")] {
        let err =
            parse_bash_args(&json!({ "command": "ls", "description": "d", "timeoutMs": bad }))
                .unwrap_err();
        assert!(
            err.starts_with("invalid timeoutMs: expected a positive number, got "),
            "err: {err}"
        );
    }
}

#[test]
fn parse_enforces_escalation_pairing() {
    assert!(parse_bash_args(
        &json!({"command":"ls","description":"d","sandbox_permissions":"workspace-write","justification":"needs write"})
    ).is_ok());
    assert!(
        parse_bash_args(
            &json!({"command":"ls","description":"d","sandbox_permissions":"workspace-write"})
        )
        .is_err(),
        "justification 缺 → 失败"
    );
    assert!(
        parse_bash_args(&json!({"command":"ls","description":"d","justification":"reason"}))
            .is_err(),
        "sandbox_permissions 缺 → 失败"
    );
}

// ---------- schema DSL（camelCase timeoutMs，逐字快照关键字段） ----------

#[test]
fn schema_uses_camelcase_timeout_ms_and_required_fields() {
    let schema = bash_tool_parameters(true, &[]);
    let params = schema.as_object().expect("object");
    assert!(
        params.contains_key("timeoutMs"),
        "timeoutMs 用 camelCase（与 m4 snake 分叉，DIV）"
    );
    assert!(
        params.contains_key("run_in_background"),
        "enableRunInBackground 默认开启"
    );
    assert_eq!(params["command"]["required"], json!(true));
    assert_eq!(params["description"]["required"], json!(true));
    assert!(!params.contains_key("sandbox_permissions"));
    assert!(!params.contains_key("justification"));
}

#[test]
fn schema_background_toggle_and_escalation_fields() {
    let off = bash_tool_parameters(false, &[]);
    assert!(!off
        .as_object()
        .expect("obj")
        .contains_key("run_in_background"));

    let modes = [SandboxMode::ReadOnly, SandboxMode::WorkspaceWrite];
    let on_value = bash_tool_parameters(false, &modes);
    let on = on_value.as_object().expect("obj");
    assert!(on.contains_key("sandbox_permissions"));
    assert!(on.contains_key("justification"));
    assert_eq!(
        on["sandbox_permissions"]["enum"],
        json!(["read-only", "workspace-write"])
    );
}

// ---------- renderResult 标记词汇（逐字参考 render.ts） ----------

#[test]
fn render_exit_zero_has_no_markers() {
    let r = result(Some(0), false, None, "ok-line\n", "");
    let text = render_bash_result(&r, &[]);
    assert_eq!(text, "ok-line\n");
}

#[test]
fn render_nonzero_exit_is_last_marker() {
    let r = result(Some(7), false, None, "some", "err-text");
    let text = render_bash_result(&r, &[]);
    assert!(text.ends_with("[exit code: 7]"), "text: {text:?}");
    // stderr 段
    let idx_err = text.find("\n[stderr]\n").expect("stderr section");
    let idx_exit = text.rfind("[exit code: 7]").expect("exit marker");
    assert!(idx_err < idx_exit, "stderr 在退出标记之前");
}

#[test]
fn render_timed_out_marker() {
    let r = result(Some(0), true, None, "", "");
    let text = render_bash_result(&r, &[]);
    assert_eq!(text, "(no output)\n[timed out after 300ms]");
}

#[test]
fn render_signal_prefers_signal_marker_over_exit_code() {
    let r = result(Some(0), false, Some("SIGTERM"), "", "");
    let text = render_bash_result(&r, &[]);
    assert_eq!(text, "(no output)\n[killed by signal: SIGTERM]");
}

#[test]
fn render_stderr_section_single_newline_glue() {
    let r = result(Some(1), false, None, "out-without-newline", "boom");
    let text = render_bash_result(&r, &[]);
    assert_eq!(text, "out-without-newline\n[stderr]\nboom\n[exit code: 1]");
}

#[test]
fn render_empty_output_is_no_output_placeholder() {
    let r = result(Some(0), false, None, "", "");
    assert_eq!(render_bash_result(&r, &[]), "(no output)");
}

#[test]
fn render_truncated_appends_spill_notice() {
    let r = ShellRunResult {
        exit_code: Some(0),
        signal: None,
        timed_out: false,
        aborted: false,
        timeout_ms: 300,
        stdout: out_trunc("head-only", Some("C:\\tmp\\spill.txt")),
        stderr: out(""),
        sandbox: None,
    };
    let text = render_bash_result(&r, &[]);
    assert!(
        text.contains("[output truncated; full output: C:\\tmp\\spill.txt]"),
        "text: {text:?}"
    );
    let no_path = ShellRunResult {
        stdout: out_trunc("head", None),
        ..r
    };
    assert!(render_bash_result(&no_path, &[]).contains("(unavailable)"));
}

#[test]
fn render_sandbox_denial_with_and_without_escalation_hint() {
    let mut r = result(Some(1), false, None, "", "");
    r.sandbox = Some(ShellSandboxInfo {
        mode: SandboxMode::ReadOnly,
        denied: true,
        runner_failed: None,
    });
    let no_hint = render_bash_result(&r, &[]);
    assert!(
        no_hint.contains("[sandbox: file access denied under read-only mode]"),
        "text: {no_hint:?}"
    );
    assert!(
        !no_hint.contains("escalation available"),
        "无 escalate 目标 → 无提示"
    );
    let hinted = render_bash_result(&r, &[SandboxMode::WorkspaceWrite]);
    let denial = hinted.find("file access denied").expect("denial marker");
    let hint = hinted.find("escalation available").expect("hint present");
    let exit = hinted.rfind("[exit code: 1]").expect("exit marker last");
    assert!(denial < hint && hint < exit, "denial → hint → exit 顺序");
}

#[test]
fn render_marker_order_timeout_before_exit() {
    let mut r = result(Some(1), true, None, "x", "");
    let text = render_bash_result(&r, &[]);
    let t = text.find("[timed out after 300ms]").expect("timeout");
    let e = text.rfind("[exit code: 1]").expect("exit last");
    assert!(t < e);
    let _ = &mut r.signal; // 占位保持可读
}

// ---------- renderProcessRead（后台增量） ----------

#[test]
fn render_process_read_lossy_notice_with_paths() {
    let read = ShellProcessRead {
        delta: "partial".to_string(),
        lossy: true,
        stdout_spill_path: Some(PathBuf::from("C:\\tmp\\a.txt")),
        stderr_spill_path: Some(PathBuf::from("C:\\tmp\\b.txt")),
    };
    let text = render_bash_process_read(&read, None, &[]);
    assert!(text.contains(
        "[some output was dropped from memory; full output: C:\\tmp\\a.txt, C:\\tmp\\b.txt]"
    ));
}

#[test]
fn render_process_read_pure_delta_when_no_notices() {
    let read = ShellProcessRead {
        delta: "out\n".to_string(),
        lossy: false,
        stdout_spill_path: None,
        stderr_spill_path: None,
    };
    assert_eq!(render_bash_process_read(&read, None, &[]), "out\n");
}

#[test]
fn render_process_read_notices_only_when_delta_empty() {
    let read = ShellProcessRead {
        delta: String::new(),
        lossy: false,
        stdout_spill_path: None,
        stderr_spill_path: None,
    };
    let sandbox = ShellSandboxInfo {
        mode: SandboxMode::WorkspaceWrite,
        denied: true,
        runner_failed: None,
    };
    let text = render_bash_process_read(&read, Some(&sandbox), &[]);
    assert_eq!(
        text,
        "[sandbox: file access denied under workspace-write mode]"
    );
}

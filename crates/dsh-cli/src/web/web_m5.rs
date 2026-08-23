//! M5h: M5 执行工具的 web 接线（step7；M5-DESIGN §8）。
//!
//! 职责：把 M5 各 crate 的纯面（schema/parse/render）装配成 `dsh-tools` 可注册工具，
//! 并把宿主服务句柄（[`M5HostServices`]）bind 进对应工具的 execute 槽（同 `Rc` 生效）。
//!
//! 诚实接线原则（D-068）：工具一律先注册（定义可见、schema 可校验、模型可见 renderers
//! 单源），再按「宿主服务句柄是否在场」决定 execute 真实委托 vs 结构化 `NOT_BOUND`——
//! 绝不无句柄假装成功（M4 同款承诺，D-052）。本轮真实绑定：terminal 六件套（本箱
//! ConPTY 可实测）；bash/run_code/fs(read/write/edit/read_image)/glob/grep/
//! str_replace_editor 后续轮按服务柄接入（D-068 记录待办）。

use std::cell::RefCell;
use std::rc::Rc;

use dsh_shell::bash_tool_parameters;
use dsh_terminal::{
    parse_terminal_close_args, parse_terminal_open_args, parse_terminal_read_args,
    parse_terminal_send_args, parse_terminal_signal_args, render_terminal_close,
    render_terminal_list, render_terminal_read, render_terminal_send, render_terminal_spawn,
    terminal_close_schema, terminal_list_schema, terminal_open_schema, terminal_read_schema,
    terminal_send_schema, terminal_signal_schema, RenderedTerminalSession, TerminalCloseOutcome,
    TerminalConfig, TerminalError, TerminalRenderStatus, TerminalSendRequest, TerminalSessionId,
    TerminalSessionService, TerminalSignal, TerminalWaitReason,
};
use dsh_tools::types::{ContentBlock, CODE_INVALID_ARGS};
use dsh_tools::{define_m5_tool, M5Tool, ToolExecute, ToolFailureData};
use serde_json::{json, Value};

/// M5 渲染预算（与各渲染纯面 max_bytes 对齐；超载自渲染层截断）。
const M5_RENDER_MAX_BYTES: usize = 256 * 1024;

/// 结构化错误 code：宿主句柄缺失（复用 M4 NOT_BOUND 词表）。
pub use dsh_tools::m4::CODE_NOT_BOUND;

/// M5h 宿主服务句柄集合：terminal 工具组的 bind 目标。
///
/// `register_m5_tools_with_host` 接受可选的 `&M5HostServices`：有句柄 → 对应工具 bind
/// 到真实服务（fail loud 不再 NOT_BOUND）；无句柄 → 注册定义但保持 `NOT_BOUND`。
/// 本轮仅装配 `terminal`；shell/fs/code_runtime 句柄随后续 binder 轮加入（D-068）。
#[derive(Default)]
pub struct M5HostServices {
    /// 终端会话注册表（terminal_open/send/read/signal/close/list 的真实句柄）。
    pub terminal: Option<Rc<RefCell<TerminalSessionService>>>,
}

/// 注册全部 M5 工具（M5-DESIGN §8 工具集）到一个 registry。
///
/// 所有工具注册后可见；execute 由宿主句柄在场与否决定委托或 NOT_BOUND。
pub fn register_m5_tools_with_host(
    registry: &dsh_tools::ToolRegistry,
    host: Option<&M5HostServices>,
) {
    // ---- terminal 六件套（真实绑定；无句柄 → NOT_BOUND，schemas/r/parse 始终在场） ----
    let (open, send, read, signal, close, list) = (
        terminal_open_tool(),
        terminal_send_tool(),
        terminal_read_tool(),
        terminal_signal_tool(),
        terminal_close_tool(),
        terminal_list_tool(),
    );
    if let Some(term) = host.and_then(|h| h.terminal.clone()) {
        open.bind(terminal_open_executor(term.clone()));
        send.bind(terminal_send_executor(term.clone()));
        read.bind(terminal_read_executor(term.clone()));
        signal.bind(terminal_signal_executor(term.clone()));
        close.bind(terminal_close_executor(term.clone()));
        list.bind(terminal_list_executor(term));
    }
    for (name, tool) in [
        ("terminal_open", open),
        ("terminal_send", send),
        ("terminal_read", read),
        ("terminal_signal", signal),
        ("terminal_close", close),
        ("terminal_list", list),
    ] {
        registry
            .register_global(tool.definition())
            .unwrap_or_else(|e| panic!("{name}: {e}"));
    }

    // ---- bash / fs 族 / 搜索 / sr-editor：登记定义（纯面 schema + 校验），
    // execute 待对应宿主句柄接入（本轮 NOT_BOUND，诚实）。
    // 注：`run_code` 不在此登记——注册表保留该名注入 Code Mode 占位传输（诚实
    // "requires a code runtime" 桩）；真实运行面绑定属 registry/run_code binder 步（D-068）。
    let bash = define_m5_tool(
        "bash",
        "Run a shell command in the host workspace, returning its output, exit code, and sandbox status.".into(),
        bash_tool_parameters(true, &[]),
        json!({"type":"object","additionalProperties":true}),
        Rc::new(|_a, v| vec![ContentBlock::text(render_bash_value(v))]),
    )
    .expect("bash defines");
    registry
        .register_global(Rc::clone(&bash.definition()))
        .expect("register bash");

    let fs_read = define_m5_tool(
        "read",
        "Read a file from the workspace with an optional line window (UTF-8).".into(),
        json!({
            "file_path": {"type":"string","required":true},
            "offset": {"type":"integer"},
            "limit": {"type":"integer"},
        }),
        json!({"type":"object","additionalProperties":true}),
        Rc::new(|_a, v| vec![ContentBlock::text(render_passthrough(v))]),
    )
    .expect("read defines");
    registry
        .register_global(fs_read.definition())
        .expect("register read");

    let fs_write = define_m5_tool(
        "write",
        "Write or create a text file atomically in the workspace (UTF-8).".into(),
        json!({
            "file_path": {"type":"string","required":true},
            "content": {"type":"string","required":true},
            "description": {"type":"string"},
        }),
        json!({"type":"object","additionalProperties":true}),
        Rc::new(|_a, v| vec![ContentBlock::text(render_passthrough(v))]),
    )
    .expect("write defines");
    registry
        .register_global(fs_write.definition())
        .expect("register write");

    let fs_edit = define_m5_tool(
        "edit",
        "Replace all exact occurrences of old_string with new_string in a text file (version-guarded by read-before-edit observation).".into(),
        json!({
            "file_path": {"type":"string","required":true},
            "old_string": {"type":"string","required":true},
            "new_string": {"type":"string","required":true},
            "replace_all": {"type":"boolean"},
            "description": {"type":"string"},
        }),
        json!({"type":"object","additionalProperties":true}),
        Rc::new(|_a, v| vec![ContentBlock::text(render_passthrough(v))]),
    )
    .expect("edit defines");
    registry
        .register_global(fs_edit.definition())
        .expect("register edit");

    let fs_read_image = define_m5_tool(
        "read_image",
        "Read an image file and return it as inline media (PNG/JPEG/WebP/GIF).".into(),
        json!({"file_path": {"type":"string","required":true}}),
        json!({"type":"object","additionalProperties":true}),
        Rc::new(|_a, v| vec![ContentBlock::text(render_passthrough(v))]),
    )
    .expect("read_image defines");
    registry
        .register_global(fs_read_image.definition())
        .expect("register read_image");

    let glob = define_m5_tool(
        "glob",
        "List files matching a glob pattern under the workspace, excluding VCS dirs.".into(),
        json!({
            "pattern": {"type":"string","required":true},
            "path": {"type":"string"},
        }),
        json!({"type":"object","additionalProperties":true}),
        Rc::new(|_a, v| vec![ContentBlock::text(render_passthrough(v))]),
    )
    .expect("glob defines");
    registry
        .register_global(glob.definition())
        .expect("register glob");

    let grep = define_m5_tool(
        "grep",
        "Search file contents with an ignore-aware regex under the workspace.".into(),
        json!({
            "pattern": {"type":"string","required":true},
            "path": {"type":"string"},
            "include": {"type":"string"},
        }),
        json!({"type":"object","additionalProperties":true}),
        Rc::new(|_a, v| vec![ContentBlock::text(render_passthrough(v))]),
    )
    .expect("grep defines");
    registry
        .register_global(grep.definition())
        .expect("register grep");

    let sr_editor = define_m5_tool(
        "str_replace_editor",
        "View a file and apply unique string replacement or line insertion (read-before-edit)."
            .into(),
        json!({
            "file_path": {"type":"string","required":true},
            "view": {"type":"boolean"},
            "old_string": {"type":"string"},
            "new_string": {"type":"string"},
            "replace_all": {"type":"boolean"},
            "insert_line": {"type":"integer"},
            "new_str": {"type":"string"},
        }),
        json!({"type":"object","additionalProperties":true}),
        Rc::new(|_a, v| vec![ContentBlock::text(render_passthrough(v))]),
    )
    .expect("str_replace_editor defines");
    registry
        .register_global(sr_editor.definition())
        .expect("register str_replace_editor");

    // 保持方言参数被校验（bash/run_code 次轮 bind 前，execute 已带类型语义校验）。
}

// ---------------------------------------------------------------------------
// terminal 六件套工具构造 + 宿主 executor
// ---------------------------------------------------------------------------

fn terminal_open_tool() -> M5Tool {
    define_m5_tool(
        "terminal_open",
        "Start a terminal session attached to the requested backend (e.g. bash), ready for later send/read.".into(),
        terminal_open_schema(),
        json!({"type":"object","additionalProperties":true}),
        Rc::new(|_a, v| {
            let text = render_terminal_spawn(
                v["sessionId"].as_str().unwrap_or("?"),
                v["name"].as_str(),
                v["type"].as_str().unwrap_or("?"),
                "", // 本轮无 startup output → 渲染层补齐 "(no startup output)"
                M5_RENDER_MAX_BYTES,
            );
            vec![ContentBlock::text(text)]
        }),
    )
    .expect("terminal_open defines")
}

fn terminal_send_tool() -> M5Tool {
    define_m5_tool(
        "terminal_send",
        "Send text to a terminal session and wait for delivery (viewport + wait reason).".into(),
        terminal_send_schema(false),
        json!({"type":"object","additionalProperties":true}),
        Rc::new(|_a, v| {
            let status = render_status_from_value(&v["sessionStatus"]);
            let text = render_terminal_send(
                v["viewport"].as_str().unwrap_or(""),
                wait_reason_from_str(v["waitReason"].as_str().unwrap_or("session_exit")),
                &status,
                v["truncated"].as_bool().unwrap_or(false),
                M5_RENDER_MAX_BYTES,
            );
            vec![ContentBlock::text(text)]
        }),
    )
    .expect("terminal_send defines")
}

fn terminal_read_tool() -> M5Tool {
    define_m5_tool(
        "terminal_read",
        "Read retained output from a terminal session (optionally a line window).".into(),
        terminal_read_schema(),
        json!({"type":"object","additionalProperties":true}),
        Rc::new(|_a, v| {
            let text = render_terminal_read(
                v["text"].as_str().unwrap_or(""),
                v["totalLines"].as_u64().unwrap_or(0) as usize,
                v["lineBegin"].as_u64().unwrap_or(0) as usize,
                v["lineEnd"].as_u64().unwrap_or(0) as usize,
                v["truncated"].as_bool().unwrap_or(false),
                M5_RENDER_MAX_BYTES,
            );
            vec![ContentBlock::text(text)]
        }),
    )
    .expect("terminal_read defines")
}

fn terminal_signal_tool() -> M5Tool {
    define_m5_tool(
        "terminal_signal",
        "Deliver a signal to a terminal session's process (best-effort on this platform).".into(),
        terminal_signal_schema(),
        json!({"type":"object","additionalProperties":true}),
        Rc::new(|_a, v| {
            // ConPTY/Windows 无前台进程组（D-064 DIV）→ 不声称虚构 pgid（参考 render
            // 的 "to foreground process group N" 在此平台为假，改用诚实短句）。
            let sig = v["signal"].as_str().unwrap_or("?");
            vec![ContentBlock::text(format!("delivered {sig}"))]
        }),
    )
    .expect("terminal_signal defines")
}

fn terminal_close_tool() -> M5Tool {
    define_m5_tool(
        "terminal_close",
        "Close a terminal session owned by the caller.".into(),
        terminal_close_schema(),
        json!({"type":"object","additionalProperties":true}),
        Rc::new(|_a, v| {
            let text = render_terminal_close(
                v["sessionId"].as_str().unwrap_or("?"),
                TerminalCloseOutcome::Closed,
            );
            vec![ContentBlock::text(text)]
        }),
    )
    .expect("terminal_close defines")
}

fn terminal_list_tool() -> M5Tool {
    define_m5_tool(
        "terminal_list",
        "List terminal sessions owned by the caller (id, name, backend, status).".into(),
        terminal_list_schema(),
        json!({"type":"object","additionalProperties":true}),
        Rc::new(|_a, v| {
            let sessions: Vec<RenderedTerminalSession> = v["sessions"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .map(|s| RenderedTerminalSession {
                            session_id: s["sessionId"].as_str().unwrap_or("?").to_string(),
                            name: s["name"].as_str().map(str::to_string),
                            backend_type: s["type"].as_str().unwrap_or("?").to_string(),
                            pid: None,
                            status: render_status_from_value(&s["status"]),
                        })
                        .collect()
                })
                .unwrap_or_default();
            let text = render_terminal_list(&sessions, M5_RENDER_MAX_BYTES);
            vec![ContentBlock::text(text)]
        }),
    )
    .expect("terminal_list defines")
}

fn terminal_open_executor(svc: Rc<RefCell<TerminalSessionService>>) -> ToolExecute {
    Rc::new(move |args, ctx| {
        let owner = required_agent(ctx.agent.as_deref(), "terminal_open")?;
        let (backend, name, _cwd) =
            parse_terminal_open_args(args).map_err(|m| invalid_args("terminal_open", m))?;
        let id = svc
            .borrow_mut()
            .open(owner, &backend, name.as_deref(), TerminalConfig::default())
            .map_err(|e| terminal_failure("terminal_open", e))?;
        Ok(json!({
            "sessionId": id.as_str(),
            "name": name,
            "type": backend,
        }))
    })
}

fn terminal_send_executor(svc: Rc<RefCell<TerminalSessionService>>) -> ToolExecute {
    Rc::new(move |args, ctx| {
        let owner = required_agent(ctx.agent.as_deref(), "terminal_send")?;
        let (id, text, submit, background) =
            parse_terminal_send_args(args).map_err(|e| terminal_failure("terminal_send", e))?;
        if background == Some(true) {
            return Err(unsupported(
                "terminal_send/run_in_background requires the jobs producer bridge (not wired yet)",
            ));
        }
        let req = TerminalSendRequest {
            text,
            submit,
            signal: None,
        };
        let res = svc
            .borrow_mut()
            .send(owner, &TerminalSessionId::from_raw(id.clone()), &req)
            .map_err(|e| terminal_failure("terminal_send", e))?;
        Ok(json!({
            "sessionId": id,
            "viewport": res.viewport,
            "waitReason": wait_reason_str(res.wait_reason),
            "sessionStatus": status_json(res.session_status),
            "truncated": res.truncated,
        }))
    })
}

fn terminal_read_executor(svc: Rc<RefCell<TerminalSessionService>>) -> ToolExecute {
    Rc::new(move |args, ctx| {
        let owner = required_agent(ctx.agent.as_deref(), "terminal_read")?;
        let (id, offset, count) =
            parse_terminal_read_args(args).map_err(|e| terminal_failure("terminal_read", e))?;
        let text = svc
            .borrow_mut()
            .read(owner, &TerminalSessionId::from_raw(id.clone()))
            .map_err(|e| terminal_failure("terminal_read", e))?;
        let total = text.matches('\n').count() + usize::from(!text.is_empty());
        let begin = offset.map(|o| o as usize).unwrap_or(0).min(total);
        let end = (begin + count.map(|c| c as usize).unwrap_or(500)).min(total);
        Ok(json!({
            "sessionId": id,
            "text": text,
            "totalLines": total,
            "lineBegin": begin,
            "lineEnd": end,
            "truncated": false,
        }))
    })
}

fn terminal_signal_executor(svc: Rc<RefCell<TerminalSessionService>>) -> ToolExecute {
    Rc::new(move |args, ctx| {
        let owner = required_agent(ctx.agent.as_deref(), "terminal_signal")?;
        let (id, sig) =
            parse_terminal_signal_args(args).map_err(|e| terminal_failure("terminal_signal", e))?;
        let parsed = parse_signal(&sig)
            .ok_or_else(|| invalid_args("terminal_signal", format!("unknown signal: {sig}")))?;
        svc.borrow_mut()
            .signal(owner, &TerminalSessionId::from_raw(id.clone()), parsed)
            .map_err(|e| terminal_failure("terminal_signal", e))?;
        Ok(json!({
            "sessionId": id,
            "signal": parsed.as_str(),
            "delivered": true,
        }))
    })
}

fn terminal_close_executor(svc: Rc<RefCell<TerminalSessionService>>) -> ToolExecute {
    Rc::new(move |args, ctx| {
        let owner = required_agent(ctx.agent.as_deref(), "terminal_close")?;
        let id =
            parse_terminal_close_args(args).map_err(|e| terminal_failure("terminal_close", e))?;
        svc.borrow_mut()
            .close(owner, &TerminalSessionId::from_raw(id.clone()))
            .map_err(|e| terminal_failure("terminal_close", e))?;
        Ok(json!({ "sessionId": id, "outcome": "closed" }))
    })
}

fn terminal_list_executor(svc: Rc<RefCell<TerminalSessionService>>) -> ToolExecute {
    Rc::new(move |_args, ctx| {
        let owner = required_agent(ctx.agent.as_deref(), "terminal_list")?;
        let sessions: Vec<Value> = svc
            .borrow()
            .list()
            .into_iter()
            .filter(|v| v.owner == owner)
            .map(|v| {
                json!({
                    "sessionId": v.id.as_str(),
                    "name": v.name,
                    "type": v.backend,
                    "status": status_json(v.status),
                })
            })
            .collect();
        Ok(json!({ "sessions": sessions }))
    })
}

// ---------------------------------------------------------------------------
// 助手：agent / 错误 / 状态 / 信号映射
// ---------------------------------------------------------------------------

fn required_agent<'a>(agent: Option<&'a str>, tool: &str) -> Result<&'a str, ToolFailureData> {
    match agent {
        Some(a) if !a.trim().is_empty() => Ok(a),
        _ => Err(ToolFailureData::new(
            format!("{tool} requires an owning agent"),
            CODE_INVALID_ARGS,
            "ToolArgsError",
        )),
    }
}

fn invalid_args(tool: &str, message: String) -> ToolFailureData {
    ToolFailureData::new(
        format!("{tool}: {message}"),
        CODE_INVALID_ARGS,
        "ToolArgsError",
    )
}

fn unsupported(message: impl Into<String>) -> ToolFailureData {
    ToolFailureData::new(message, "UNSUPPORTED_OPTION", "ToolUnsupportedError")
}

fn terminal_failure(tool: &str, e: TerminalError) -> ToolFailureData {
    ToolFailureData::new(
        format!("{tool}: {}", e.message),
        format!("{:?}", e.code),
        "TerminalError",
    )
}

fn wait_reason_str(r: TerminalWaitReason) -> &'static str {
    match r {
        TerminalWaitReason::StdinRead => "stdin_read",
        TerminalWaitReason::InferredIdle => "inferred_idle",
        TerminalWaitReason::Timeout => "timeout",
        TerminalWaitReason::SessionExit => "session_exit",
    }
}

fn wait_reason_from_str(s: &str) -> TerminalWaitReason {
    match s {
        "stdin_read" => TerminalWaitReason::StdinRead,
        "inferred_idle" => TerminalWaitReason::InferredIdle,
        "timeout" => TerminalWaitReason::Timeout,
        _ => TerminalWaitReason::SessionExit,
    }
}

fn status_json(s: dsh_terminal::TerminalSessionStatus) -> Value {
    match s {
        dsh_terminal::TerminalSessionStatus::Running => json!({ "kind": "running" }),
        dsh_terminal::TerminalSessionStatus::Exited
        | dsh_terminal::TerminalSessionStatus::Aborted => {
            json!({ "kind": "exited" })
        }
    }
}

fn render_status_from_value(v: &Value) -> TerminalRenderStatus {
    let exit_code = v["exitCode"].as_i64().map(|i| i as i32);
    let signal = v["signal"].as_str().map(str::to_string);
    match v["kind"].as_str() {
        Some("exited") => TerminalRenderStatus::Exited { exit_code, signal },
        _ => TerminalRenderStatus::Running,
    }
}

fn parse_signal(sig: &str) -> Option<TerminalSignal> {
    let upper = sig.trim().to_ascii_uppercase();
    let bare = upper.strip_prefix("SIG").unwrap_or(&upper);
    match bare {
        "INT" => Some(TerminalSignal::Sigint),
        "TERM" => Some(TerminalSignal::Sigterm),
        "KILL" => Some(TerminalSignal::Sigkill),
        "TSTP" => Some(TerminalSignal::Sigstp),
        "HUP" => Some(TerminalSignal::Sighup),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// NOT_BOUND 工具的渲染（本轮不 reachable，保持诚实可空格子）
// ---------------------------------------------------------------------------

fn render_passthrough(v: &Value) -> String {
    if v.is_null() {
        "(no output)".to_string()
    } else {
        serde_json::to_string_pretty(v).unwrap_or_else(|_| "(unrenderable output)".to_string())
    }
}

fn render_bash_value(v: &Value) -> String {
    // 与 tool_bash 纯面渲染保持同词表；未 bind 前不可达。
    render_passthrough(v)
}

/// 允许外部（web.rs 测试 / 未来装配）复用本模块的专用输出 schema（permissive object）。
pub fn permissive_output_schema() -> Value {
    json!({"type":"object","additionalProperties":true})
}

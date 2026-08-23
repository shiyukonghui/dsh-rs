//! dsh-terminal 模型面：6 工具纯面（M5-DESIGN §6.2，逐字 `tool-terminal/src/{index,render}.ts`）。
//!
//! 参数 schema（sessionId required 等）逐字；渲染词汇：spawn 确认 / send `[wait: …]`
//! `[session: …]` / read `[lines: a-b of c]` / signal `delivered …` / close /
//! list 逐会话一行；输出封顶（UTF-8 边界保 head/tail + `\n[output truncated]` 标记）。
//! binding（define_tool / registry 接线）留 step7 web.rs。

use crate::types::{TerminalError, TerminalSessionStatus, TerminalWaitReason};
use serde_json::{json, Value};

/// 缺省 read 行数（参考 terminal_read 默认 500）。
pub const DEFAULT_TERMINAL_READ_LINES: usize = 500;

const TRUNCATED_MARKER: &str = "\n[output truncated]";

fn byte_len(s: &str) -> usize {
    s.len()
}

/// UTF-8 边界保尾。
fn retain_tail(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut start = s.len() - max_bytes;
    while start > 0 && !s.is_char_boundary(start) {
        start -= 1;
    }
    s[start..].to_string()
}

fn retain_head(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// 内容 + 后缀，超限 → 内容保尾截断 + `[output truncated]`。
fn fit_with_suffix(content: &str, suffix: &str, max_bytes: usize) -> String {
    let fixed = byte_len(suffix);
    if fixed >= max_bytes {
        return retain_tail(suffix, max_bytes);
    }
    format!("{}{}", retain_tail(content, max_bytes - fixed), suffix)
}

/// 前缀 + 内容，超限 → 前缀 + 内容保尾 + `[output truncated]`。
fn fit_with_prefix(prefix: &str, content: &str, max_bytes: usize) -> String {
    let fixed = format!("{prefix}{TRUNCATED_MARKER}");
    if byte_len(&fixed) >= max_bytes {
        return retain_head(&fixed, max_bytes);
    }
    format!(
        "{prefix}{}{}",
        retain_tail(content, max_bytes - byte_len(&fixed)),
        TRUNCATED_MARKER
    )
}

/// 元数据后缀 + 上游截断标记；整体超限 → 内容保尾换统一截断标记。
fn bound_body_with_suffix(
    content: &str,
    metadata: &str,
    upstream_truncated: bool,
    max_bytes: usize,
) -> String {
    let suffix = format!(
        "{metadata}{}",
        if upstream_truncated {
            TRUNCATED_MARKER
        } else {
            ""
        }
    );
    let complete = format!("{content}{suffix}");
    if byte_len(&complete) <= max_bytes {
        return complete;
    }
    fit_with_suffix(content, &format!("{metadata}{TRUNCATED_MARKER}"), max_bytes)
}

/// 完整确认文本封顶（保 head + 标记；镜像 boundTerminalText）。
pub fn bound_terminal_text(text: &str, max_bytes: usize) -> String {
    if byte_len(text) <= max_bytes {
        return text.to_string();
    }
    if byte_len(TRUNCATED_MARKER) >= max_bytes {
        return retain_tail(TRUNCATED_MARKER, max_bytes);
    }
    format!(
        "{}{}",
        retain_head(text, max_bytes - byte_len(TRUNCATED_MARKER)),
        TRUNCATED_MARKER
    )
}

/// 会话状态渲染：running | exited code=… signal=…（模型可见）。
#[derive(Debug, Clone, PartialEq)]
pub enum TerminalRenderStatus {
    Running,
    Exited {
        exit_code: Option<i32>,
        signal: Option<String>,
    },
}

impl From<TerminalSessionStatus> for TerminalRenderStatus {
    fn from(s: TerminalSessionStatus) -> Self {
        match s {
            TerminalSessionStatus::Running => TerminalRenderStatus::Running,
            TerminalSessionStatus::Exited | TerminalSessionStatus::Aborted => {
                TerminalRenderStatus::Exited {
                    exit_code: None,
                    signal: None,
                }
            }
        }
    }
}

impl TerminalRenderStatus {
    pub fn render(&self) -> String {
        match self {
            TerminalRenderStatus::Running => "running".to_string(),
            TerminalRenderStatus::Exited { exit_code, signal } => format!(
                "exited code={} signal={}",
                exit_code
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "null".to_string()),
                signal.as_deref().unwrap_or("null")
            ),
        }
    }
}

/// 渲染用的会话快照（list 输出；step7 由 registry view 映射）。
#[derive(Debug, Clone)]
pub struct RenderedTerminalSession {
    pub session_id: String,
    pub name: Option<String>,
    pub backend_type: String,
    pub pid: Option<u32>,
    pub status: TerminalRenderStatus,
}

// ---------- 参数 schema（逐字 tool-terminal index.ts） ----------

pub fn terminal_open_schema() -> Value {
    json!({
        "type": { "type": "string", "required": true, "description": "Registered terminal backend type, usually \"shell\"." },
        "name": { "type": "string", "description": "Optional owner-local display name such as \"main\" or \"gdb\"." },
        "cwd": { "type": "string", "description": "Initial working directory. Defaults to the deployment workspace root." }
    })
}

pub fn terminal_send_schema(background_enabled: bool) -> Value {
    let mut spec = serde_json::Map::new();
    spec.insert("sessionId".into(), json!({ "type": "string", "required": true, "description": "Terminal session id returned by terminal_open or terminal_list." }));
    spec.insert("text".into(), json!({ "type": "string", "required": true, "description": "UTF-8 text to write to the terminal." }));
    spec.insert("submit".into(), json!({ "type": "boolean", "description": "Submit Enter after text (default true). Set false for control characters or incomplete REPL input." }));
    if background_enabled {
        spec.insert("run_in_background".into(), json!({ "type": "boolean", "description": "Return a job id immediately; collect with job_output or stop with job_kill." }));
    }
    Value::Object(spec)
}

pub fn terminal_read_schema() -> Value {
    json!({
        "sessionId": { "type": "string", "required": true, "description": "Terminal session id." },
        "offset": { "type": "number", "description": "Newest-relative line offset (default 0)." },
        "count": { "type": "number", "description": "Requested line count (default 500; backend caps apply)." }
    })
}

pub fn terminal_signal_schema() -> Value {
    json!({
        "sessionId": { "type": "string", "required": true, "description": "Terminal session id." },
        "signal": { "type": "string", "required": true, "enum": ["SIGINT", "SIGTERM", "SIGKILL", "SIGTSTP", "SIGHUP"], "description": "Signal to deliver. Shell-targeted SIGKILL is rejected; use terminal_close." }
    })
}

pub fn terminal_close_schema() -> Value {
    json!({
        "sessionId": { "type": "string", "required": true, "description": "Terminal session id." }
    })
}

pub fn terminal_list_schema() -> Value {
    json!({})
}

fn session_id_of(args: &Value, tool: &str) -> Result<String, TerminalError> {
    match args.get("sessionId").and_then(|v| v.as_str()) {
        Some(id) if !id.trim().is_empty() => Ok(id.to_string()),
        _ => Err(TerminalError::new(
            crate::types::TerminalErrorCode::NoSession,
            format!("{tool}: invalid sessionId: expected a non-empty string"),
        )),
    }
}

pub fn parse_terminal_open_args(
    args: &Value,
) -> Result<(String, Option<String>, Option<String>), String> {
    let backend = match args.get("type").and_then(|v| v.as_str()) {
        Some(t) if !t.trim().is_empty() => t.to_string(),
        _ => return Err("terminal_open: invalid type: expected a non-empty string".into()),
    };
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let cwd = args
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    Ok((backend, name, cwd))
}

pub fn parse_terminal_send_args(
    args: &Value,
) -> Result<(String, String, bool, Option<bool>), TerminalError> {
    let id = session_id_of(args, "terminal_send")?;
    let text = match args.get("text").and_then(|v| v.as_str()) {
        Some(t) => t.to_string(),
        None => {
            return Err(TerminalError::new(
                crate::types::TerminalErrorCode::NoSession,
                "terminal_send: invalid text: expected a string".to_string(),
            ))
        }
    };
    let submit = args.get("submit").and_then(|v| v.as_bool()).unwrap_or(true);
    let run_in_background = args.get("run_in_background").and_then(|v| v.as_bool());
    Ok((id, text, submit, run_in_background))
}

pub fn parse_terminal_read_args(
    args: &Value,
) -> Result<(String, Option<u64>, Option<u64>), TerminalError> {
    let id = session_id_of(args, "terminal_read")?;
    let offset = args.get("offset").and_then(|v| v.as_u64());
    let count = args.get("count").and_then(|v| v.as_u64());
    Ok((id, offset, count))
}

pub fn parse_terminal_signal_args(args: &Value) -> Result<(String, String), TerminalError> {
    let id = session_id_of(args, "terminal_signal")?;
    let sig = match args.get("signal").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => {
            return Err(TerminalError::new(
                crate::types::TerminalErrorCode::NoSession,
                "terminal_signal: invalid signal: expected a string".to_string(),
            ))
        }
    };
    Ok((id, sig))
}

pub fn parse_terminal_close_args(args: &Value) -> Result<String, TerminalError> {
    session_id_of(args, "terminal_close")
}

// ---------- 渲染（逐字 reference render.ts） ----------

/// `started terminal session … [type: …]` + MOTD（空 → `(no startup output)`）。
pub fn render_terminal_spawn(
    session_id: &str,
    name: Option<&str>,
    backend_type: &str,
    motd: &str,
    max_bytes: usize,
) -> String {
    let label = match name {
        Some(n) => format!("{session_id} ({n})"),
        None => session_id.to_string(),
    };
    let prefix = format!("started terminal session {label} [type: {backend_type}]\n");
    let motd = if motd.is_empty() {
        "(no startup output)"
    } else {
        motd
    };
    let complete = format!("{prefix}{motd}");
    if byte_len(&complete) <= max_bytes {
        complete
    } else {
        fit_with_prefix(&prefix, motd, max_bytes)
    }
}

/// `{viewport}\n[wait: …]\n[session: …]`（viewport 空 → `(no new output)`）。
pub fn render_terminal_send(
    viewport: &str,
    wait_reason: TerminalWaitReason,
    status: &TerminalRenderStatus,
    truncated: bool,
    max_bytes: usize,
) -> String {
    let output = if viewport.is_empty() {
        "(no new output)"
    } else {
        viewport
    };
    let role = match wait_reason {
        TerminalWaitReason::StdinRead => "stdin_read",
        TerminalWaitReason::InferredIdle => "inferred_idle",
        TerminalWaitReason::Timeout => "timeout",
        TerminalWaitReason::SessionExit => "session_exit",
    };
    let session = status.render();
    let metadata = format!("\n[wait: {role}]\n[session: {session}]");
    bound_body_with_suffix(output, &metadata, truncated, max_bytes)
}

/// 后台增量读取：delta + 截断标记（无元数据）。
pub fn render_terminal_send_read(delta: &str, truncated: bool) -> String {
    if !truncated {
        return delta.to_string();
    }
    let separator = if delta.is_empty() || delta.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    format!("{delta}{separator}[output truncated]")
}

/// `{text}\n[lines: begin-end of total]`（text 空 → `(no retained output)`）。
pub fn render_terminal_read(
    text: &str,
    total_lines: usize,
    line_begin: usize,
    line_end: usize,
    truncated: bool,
    max_bytes: usize,
) -> String {
    let output = if text.is_empty() {
        "(no retained output)"
    } else {
        text
    };
    let metadata = format!("\n[lines: {line_begin}-{line_end} of {total_lines}]");
    bound_body_with_suffix(output, &metadata, truncated, max_bytes)
}

/// `delivered {signal} to foreground process group {target_pgid}`。
pub fn render_terminal_signal(signal: &str, target_pgid: u32) -> String {
    format!("delivered {signal} to foreground process group {target_pgid}")
}

/// close 结果（outcome: closed | already-closing）。
pub fn render_terminal_close(session_id: &str, outcome: TerminalCloseOutcome) -> String {
    match outcome {
        TerminalCloseOutcome::Closed => format!("closed terminal session {session_id}"),
        TerminalCloseOutcome::AlreadyClosing => {
            format!("terminal session {session_id} was already closing")
        }
    }
}

/// 逐会话一行；空 → `(no terminal sessions)`。
pub fn render_terminal_list(sessions: &[RenderedTerminalSession], max_bytes: usize) -> String {
    if sessions.is_empty() {
        return "(no terminal sessions)".to_string();
    }
    let text = sessions
        .iter()
        .map(|s| {
            let name = s
                .name
                .as_deref()
                .map(|n| format!(" ({n})"))
                .unwrap_or_default();
            let pid = s.pid.map(|p| format!(" pid={p}")).unwrap_or_default();
            format!(
                "{}{} [{}] {}{}",
                s.session_id,
                name,
                s.backend_type,
                s.status.render(),
                pid
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    bound_body_with_suffix(&text, "", false, max_bytes)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalCloseOutcome {
    Closed,
    AlreadyClosing,
}

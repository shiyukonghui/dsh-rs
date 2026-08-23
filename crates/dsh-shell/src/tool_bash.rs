//! dsh-shell 模型面：tool-bash 纯面（M5-DESIGN §5.3）。
//!
//! 逐字参考 `tool-bash/src/{index.ts, render.ts}`：作者面参数 DSL（camelCase
//! `timeoutMs`，与 m4 既有 snake `timeout_ms` 分叉，DIV 记录）、execute 校验
//! （command/description 非空、timeoutMs 正有限、escalation 两字段同现）、
//! 模型面标记词汇（stdout 正文 + `[stderr]` 段 + 空输出 + 截断/spill + sandbox 拒绝 +
//! 超时/信号/退出码；**非零退出是报告不是 isError**）。binding（define_tool /
//! 宿主接线 / 后台 JobRegistry）留 step7 web.rs。

use crate::types::{ShellCollectedOutput, ShellProcessRead, ShellRunResult, ShellSandboxInfo};
use dsh_sandbox::{
    escalation_hint_marker, sandbox_denial_marker, validate_escalation_args, SandboxMode,
};
use serde_json::{json, Value};

/// 解析后的 tool-bash 参数（execute 侧值校验发生在 schema 校验之外）。
#[derive(Debug, Clone, PartialEq)]
pub struct BashToolArgs {
    pub command: String,
    pub description: String,
    pub timeout_ms: Option<u64>,
    pub workdir: Option<String>,
    pub run_in_background: Option<bool>,
    pub sandbox_permissions: Option<String>,
    pub justification: Option<String>,
}

/// 参考 `validateBashArgs`：schema 漏标的语义约束在此硬校验（`describe` 不参与）。
pub fn parse_bash_args(args: &Value) -> Result<BashToolArgs, String> {
    let obj = args
        .as_object()
        .ok_or_else(|| "invalid arguments: expected an object".to_string())?;
    let command = match obj.get("command").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => String::new(),
    };
    if command.trim().is_empty() {
        return Err("invalid command: expected a non-empty string".to_string());
    }
    let description = match obj.get("description").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => String::new(),
    };
    if description.trim().is_empty() {
        return Err("invalid description: expected a non-empty string".to_string());
    }
    let timeout_ms = match obj.get("timeoutMs") {
        None | Some(Value::Null) => None,
        Some(v) => {
            let n = v.as_f64().ok_or_else(|| invalid_timeout(v).to_string())?;
            if !n.is_finite() || n <= 0.0 {
                return Err(invalid_timeout(v).to_string());
            }
            Some(n as u64)
        }
    };
    let workdir = obj
        .get("workdir")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let run_in_background = obj.get("run_in_background").and_then(|v| v.as_bool());
    let sandbox_permissions = obj
        .get("sandbox_permissions")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let justification = obj
        .get("justification")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    validate_escalation_args(sandbox_permissions.as_deref(), justification.as_deref())?;
    Ok(BashToolArgs {
        command,
        description,
        timeout_ms,
        workdir,
        run_in_background,
        sandbox_permissions,
        justification,
    })
}

fn invalid_timeout(v: &Value) -> String {
    format!("invalid timeoutMs: expected a positive number, got {v}")
}

/// 参考 `index.ts` 的参数 DSL（逐字描述文案）；`escalationModes` 非空才公布 escalate
/// 字段，`backgroundEnabled` 控制 `run_in_background`。
pub fn bash_tool_parameters(background_enabled: bool, escalation_modes: &[SandboxMode]) -> Value {
    let mut params = serde_json::Map::new();
    params.insert(
        "command".into(),
        json!({ "type": "string", "required": true, "description": "The bash command to execute." }),
    );
    params.insert(
        "description".into(),
        json!({
            "type": "string",
            "required": true,
            "description": "Clear, concise description of what this command does in active voice, 5-10 words (shown in the UI). Examples: \"ls\" → \"List files in current directory\"; \"git status\" → \"Show working tree status\"; \"npm install\" → \"Install package dependencies\"."
        }),
    );
    params.insert(
        "timeoutMs".into(),
        json!({"type": "number", "description": "Timeout in milliseconds. The executor applies its configured default and cap, and kills the command on expiry."}),
    );
    params.insert(
        "workdir".into(),
        json!({"type": "string", "description": "Working directory for this command. Defaults to the session workspace; a relative path is resolved against it."}),
    );
    if background_enabled {
        params.insert(
            "run_in_background".into(),
            json!({"type": "boolean", "description": "Run in the background and return a job id immediately (collect with job_output, stop with job_kill). No timeout applies."}),
        );
    }
    if !escalation_modes.is_empty() {
        let modes: Vec<&str> = escalation_modes.iter().map(|m| m.as_str()).collect();
        params.insert(
            "sandbox_permissions".into(),
            json!({
                "type": "string",
                "enum": modes,
                "description": "The wider sandbox mode this command needs. Only valid as a one-shot retry of a command the sandbox just denied; requires justification and user approval."
            }),
        );
        params.insert(
            "justification".into(),
            json!({
                "type": "string",
                "description": "Required with sandbox_permissions: one sentence for the user explaining why this exact command needs the wider access."
            }),
        );
    }
    Value::Object(params)
}

/// 逐字参考 `streamText`：截断流追加完整输出路径（可无）。
fn stream_text(output: &ShellCollectedOutput) -> String {
    if !output.truncated {
        return output.text.clone();
    }
    let path = output
        .spill_path
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "(unavailable)".to_string());
    format!("{}\n[output truncated; full output: {path}]", output.text)
}

/// 逐字参考 `renderResult`：一个前台结果 → 模型可见文本。
pub fn render_bash_result(result: &ShellRunResult, escalation_modes: &[SandboxMode]) -> String {
    let out = stream_text(&result.stdout);
    let err = stream_text(&result.stderr);

    let mut body = out;
    if !err.is_empty() {
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str("[stderr]\n");
        body.push_str(&err);
    }
    if body.is_empty() {
        body = "(no output)".to_string();
    }

    let mut markers: Vec<String> = Vec::new();
    if result.sandbox.as_ref().is_some_and(|s| s.denied) {
        let mode = result
            .sandbox
            .as_ref()
            .expect("denied implies sandbox")
            .mode;
        markers.push(sandbox_denial_marker(mode));
        if !escalation_modes.is_empty() {
            markers.push(escalation_hint_marker("command"));
        }
    }
    // 命令可能捕获 SIGTERM 后以 0 退出（超时仍报中断）。
    if result.timed_out {
        markers.push(format!("[timed out after {}ms]", result.timeout_ms));
    }
    if let Some(signal) = &result.signal {
        markers.push(format!("[killed by signal: {signal}]"));
    } else if result.exit_code != Some(0) {
        // None（无码终止）输出 "null"，镜像 TS `exitCode !== 0`。
        let code = result
            .exit_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "null".to_string());
        markers.push(format!("[exit code: {code}]"));
    }
    if markers.is_empty() {
        return body;
    }
    if !body.ends_with('\n') {
        body.push('\n');
    }
    body.push_str(&markers.join("\n"));
    body
}

/// 逐字参考 `renderProcessRead`：后台增量 → 模型可见 delta + 丢失/沙箱提示。
pub fn render_bash_process_read(
    read: &ShellProcessRead,
    sandbox: Option<&ShellSandboxInfo>,
    escalation_modes: &[SandboxMode],
) -> String {
    let mut notices: Vec<String> = Vec::new();
    if read.lossy {
        let paths: Vec<String> = [&read.stdout_spill_path, &read.stderr_spill_path]
            .into_iter()
            .flatten()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        let joined = if paths.is_empty() {
            "(unavailable)".to_string()
        } else {
            paths.join(", ")
        };
        notices.push(format!(
            "[some output was dropped from memory; full output: {joined}]"
        ));
    }
    if let Some(sb) = sandbox {
        if sb.runner_failed == Some(true) {
            notices.push(format!(
                "[sandbox: the sandbox runner itself failed under {} mode — the command did not run; this is a sandbox problem, not a command failure]",
                sb.mode
            ));
        } else if sb.denied {
            notices.push(sandbox_denial_marker(sb.mode));
            if !escalation_modes.is_empty() {
                notices.push(escalation_hint_marker("command"));
            }
        }
    }
    if notices.is_empty() {
        return read.delta.clone();
    }
    let joined = notices.join("\n");
    if read.delta.is_empty() || read.delta.ends_with('\n') {
        format!("{}{joined}", read.delta)
    } else {
        format!("{}\n{joined}", read.delta)
    }
}

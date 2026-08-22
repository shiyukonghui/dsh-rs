//! M4h: M4 管理工具的「定义构造器」（纯定义面）。
//!
//! 对齐参考 TS：
//! - `packages/todo/tool-todo/src/index.ts`（todo_write）
//! - `packages/jobs/tool-jobs/src/index.ts`（job_output/job_list/job_kill）
//! - `packages/schedule/schedule/src/tools.ts`（schedule_create/list/delete）
//! - `packages/plan/plan-mode/src/index.ts`（exit_plan_mode）
//! - `packages/workflow/tool-workflow/src/index.ts`（workflow，M4 桩）
//!
//! 本模块只产定义（name/description/parameters/output schema/render/execute 行为），
//! 真实宿主句柄（JobRegistry / schedule 域 / plan-mode 服务）由 web.rs 经
//! [`M4Tool::bind`] 注入；未注入前 execute 返回结构化 `NOT_BOUND` 错误，绝不伪装
//! 成功。`todo_write` 自包含（to_todo_list 校验 + 规范化输出，无需宿主）；
//! `workflow` M4 为桩（meta 校验后恒 `UNSUPPORTED_OPTION` isError）。

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::{json, Value};

use crate::schema::{define_tool, DefineToolOptions, ToolDefinitionError};
use crate::types::{
    ContentBlock, ToolDefinition, ToolExecute, ToolFailureData, ToolRender, CODE_INVALID_ARGS,
};

/// 宿主句柄未注入的结构化错误 code（本模块定义；注册表按
/// `{message, info:{code, name}}` 物化为 isError）。
pub const CODE_NOT_BOUND: &str = "NOT_BOUND";

/// 一个可绑定宿主执行器的 M4 工具定义。注册的是 [`M4Tool::definition`] 返回的
/// `Rc<ToolDefinition>`；`execute` 从共享槽读入：已绑定 → 委托真实宿主句柄，
/// 未绑定 → 结构化 `NOT_BOUND` 错误。`bind` 在注册后随时可调（同 `Rc` 生效）。
pub struct M4Tool {
    def: Rc<ToolDefinition>,
    slot: Rc<RefCell<Option<ToolExecute>>>,
}

impl M4Tool {
    /// 待注册/已注册的定义（`Rc`，`bind` 后同一定义即刻委托宿主）。
    pub fn definition(&self) -> Rc<ToolDefinition> {
        self.def.clone()
    }

    /// 绑定真实宿主执行器（JobRegistry / schedule 域 / plan-mode 服务句柄）。
    pub fn bind(&self, executor: ToolExecute) {
        *self.slot.borrow_mut() = Some(executor);
    }

    /// 当前是否已绑定宿主执行器。
    pub fn is_bound(&self) -> bool {
        self.slot.borrow().is_some()
    }
}

/// 构建一个 slot 承载的 M4 工具定义（未绑定 → `NOT_BOUND` 结构化错误）。
fn define_bound(
    name: &str,
    description: String,
    parameters: Value,
    output_schema: Value,
    render: ToolRender,
    host_kind: &str,
) -> Result<M4Tool, ToolDefinitionError> {
    let slot: Rc<RefCell<Option<ToolExecute>>> = Rc::new(RefCell::new(None));
    let slot_for_execute = slot.clone();
    let unbound = not_bound_failure(name, host_kind);
    let execute: ToolExecute = Rc::new(move |args, ctx| {
        let executor = slot_for_execute.borrow().clone();
        match executor {
            Some(f) => f(args, ctx),
            None => Err(unbound.clone()),
        }
    });
    let def = define_tool(DefineToolOptions {
        name: name.to_string(),
        description,
        parameters,
        output_schema,
        render,
        execute,
        ..Default::default()
    })?;
    Ok(M4Tool {
        def: Rc::new(def),
        slot,
    })
}

// ---------------------------------------------------------------------------
// 共享 schema 构造
// ---------------------------------------------------------------------------

/// `PublicJobSnapshot` 输出 schema（对齐 tool-jobs PUBLIC_TASK_SCHEMA），
/// 返回 JSON Schema spec 对象（可再作为某属性的 value schema）。
fn public_task_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "id": { "type": "string", "required": true },
            "kind": { "type": "string", "required": true },
            "label": { "type": "string", "required": true },
            "status": {
                "type": "string",
                "required": true,
                "enum": ["running", "stopping", "completed", "killed", "failed"],
            },
            "detail": { "type": "string" },
            "startedAt": { "type": "integer", "required": true },
            "finishedAt": { "type": "integer" },
        }
    })
}

/// 把一个对象 schema 作为某属性的 value schema，并标记该属性必填。
fn required_property(schema: Value) -> Value {
    match schema {
        Value::Object(mut map) => {
            map.insert("required".to_string(), Value::Bool(true));
            Value::Object(map)
        }
        other => other,
    }
}

/// `[status: X]` 或 `[status: X, detail]`（对齐 tool-jobs statusLine）。
fn status_line(job: &Value) -> String {
    let status = job.get("status").and_then(Value::as_str).unwrap_or_default();
    match job.get("detail").and_then(Value::as_str) {
        Some(detail) => format!("[status: {status}, {detail}]"),
        None => format!("[status: {status}]"),
    }
}

/// 未绑定宿主时的结构化错误构造器（供绑定前复用；与 `define_bound` 内部一致）。
pub fn not_bound_failure(tool: &str, host_kind: &str) -> ToolFailureData {
    ToolFailureData::new(
        format!(
            "{tool} cannot execute: no host {host_kind} executor is bound (web.rs must inject one)"
        ),
        CODE_NOT_BOUND,
        "ToolNotBoundError",
    )
}

// ---------------------------------------------------------------------------
// todo_write（自包含：无需宿主）
// ---------------------------------------------------------------------------

const TODO_HEAD: &str = "Record and update a structured task list for the current work. Send the ENTIRE \
    list every call — it REPLACES the previous list (there are no partial updates, \
    no per-item edits). Use it to plan multi-step work and show progress: add one \
    todo per concrete step before you start. ";

const TODO_PARALLEL: &str = "Mark every todo being actively worked \
    on `in_progress` — several at once when work genuinely runs in parallel (e.g. \
    concurrent subagents or background commands), one for sequential work; while \
    work remains, at least one task should be `in_progress`. ";

const TODO_SINGLE: &str = "Keep AT MOST ONE todo `in_progress` at a \
    time; while work remains, exactly one active task should be `in_progress`. ";

const TODO_TAIL: &str = "Mark a todo \
    `completed` the moment it is done (do not batch completions), and allow no \
    `in_progress` item only once all work is complete. Skip the list for trivial \
    single-step tasks. Statuses: `pending` (not started), `in_progress` (being \
    worked on now), `completed` (finished).";

/// `todo_write` 模型面描述（parallel/single 仅活动子句变化——对齐 tool-todo
/// `describe(allowParallel)`）。
fn todo_describe(allow_parallel: bool) -> String {
    if allow_parallel {
        format!("{TODO_HEAD}{TODO_PARALLEL}{TODO_TAIL}")
    } else {
        format!("{TODO_HEAD}{TODO_SINGLE}{TODO_TAIL}")
    }
}

/// 构建 `todo_write` 工具（自包含；`allow_parallel_in_progress` 为部署策略——
/// 对齐参考 Config，而非模型参数）。
pub fn todo_write(
    allow_parallel_in_progress: bool,
) -> Result<Rc<ToolDefinition>, ToolDefinitionError> {
    let allow_parallel = allow_parallel_in_progress;
    let render: ToolRender = Rc::new(|_, value| {
        let counts = &value["counts"];
        let pending = counts["pending"].as_i64().unwrap_or(0);
        let in_progress = counts["inProgress"].as_i64().unwrap_or(0);
        let completed = counts["completed"].as_i64().unwrap_or(0);
        vec![ContentBlock::text(format!(
            "Updated todo list: {pending} pending, {in_progress} in progress, {completed} completed."
        ))]
    });
    let execute: ToolExecute = Rc::new(move |args, ctx| {
        if ctx.agent.is_none() {
            // 对齐参考：拒绝无 agent 调用者（无处写入清单），绝不静默 no-op。
            return Err(ToolFailureData::new(
                "todo_write requires an owning agent session".to_string(),
                CODE_INVALID_ARGS,
                "TodoWriteError",
            ));
        }
        let raw = args
            .get("todos")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        match dsh_session_query::todo::to_todo_list(&raw, allow_parallel) {
            Ok(list) => {
                let todos = serde_json::to_value(&list).unwrap_or_else(|_| json!([]));
                let counts = dsh_session_query::todo::todo_counts(&list);
                Ok(json!({ "todos": todos, "counts": counts }))
            }
            Err(e) => Err(ToolFailureData::new(
                format!("todo list rejected: {e:?}"),
                CODE_INVALID_ARGS,
                "TodoListError",
            )),
        }
    });
    let def = define_tool(DefineToolOptions {
        name: "todo_write".to_string(),
        description: todo_describe(allow_parallel),
        parameters: json!({
            "todos": {
                "type": "array",
                "required": true,
                "description": "The COMPLETE task list, replacing any previous list.",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "content": {
                            "type": "string",
                            "required": true,
                            "description": "What the task is — a short imperative line.",
                        },
                        "status": {
                            "type": "string",
                            "required": true,
                            "enum": ["pending", "in_progress", "completed"],
                            "description": "pending (not started) | in_progress (now) | completed (done).",
                        },
                    },
                },
            },
        }),
        output_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "todos": {
                    "type": "array",
                    "required": true,
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "content": { "type": "string", "required": true },
                            "status": {
                                "type": "string",
                                "required": true,
                                "enum": ["pending", "in_progress", "completed"],
                            },
                        },
                    },
                },
                "counts": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": true,
                    "properties": {
                        "pending": { "type": "integer", "required": true },
                        "inProgress": { "type": "integer", "required": true },
                        "completed": { "type": "integer", "required": true },
                    },
                },
            },
        }),
        render,
        execute,
        ..Default::default()
    })?;
    Ok(Rc::new(def))
}

// ---------------------------------------------------------------------------
// job_output / job_list / job_kill（宿主 JobRegistry 注入点）
// ---------------------------------------------------------------------------

/// `job_output` 工具（宿主 JobRegistry 注入：read/wait；未注入 → `NOT_BOUND`）。
pub fn job_output() -> Result<M4Tool, ToolDefinitionError> {
    let render: ToolRender = Rc::new(|_, value| {
        let text = value["text"].as_str().unwrap_or_default();
        let body = if text.is_empty() {
            "(no new output)".to_string()
        } else {
            text.to_string()
        };
        let separator = if body.ends_with('\n') { "" } else { "\n" };
        let line = format!("{body}{separator}{}", status_line(&value["job"]));
        vec![ContentBlock::text(line)]
    });
    define_bound(
        "job_output",
        "Read a background job. Stream jobs return only output since the previous read; \
         final-output jobs return their result after settlement. Every response ends with \
         `[status: ...]`. Reads are non-blocking unless `wait: true`, which waits up to the \
         configured cap."
            .to_string(),
        json!({
            "job_id": {
                "type": "string",
                "required": true,
                "description": "Job id returned by the tool that started the background work.",
            },
            "wait": {
                "type": "boolean",
                "description": "Block until the job reaches a terminal status or the timeout expires. A timed-out wait returns [status: running] and leaves the job alive.",
            },
            "timeout_ms": {
                "type": "number",
                "description": "Max wait in milliseconds (only meaningful with wait: true). Defaults to the configured wait timeout; capped by the configured maximum.",
            },
        }),
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "text": { "type": "string", "required": true },
                "job": required_property(public_task_schema()),
            },
        }),
        render,
        "JobRegistry",
    )
}

/// `job_list` 工具（宿主 JobRegistry 注入：list；未注入 → `NOT_BOUND`）。
pub fn job_list() -> Result<M4Tool, ToolDefinitionError> {
    let render: ToolRender = Rc::new(|_, value| {
        let jobs = value.as_array().cloned().unwrap_or_default();
        if jobs.is_empty() {
            return vec![ContentBlock::text("(no background jobs)".to_string())];
        }
        let text = jobs
            .iter()
            .map(|t| {
                format!(
                    "{} [{}] {} — {}",
                    t["id"].as_str().unwrap_or_default(),
                    t["kind"].as_str().unwrap_or_default(),
                    t["status"].as_str().unwrap_or_default(),
                    t["label"].as_str().unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        vec![ContentBlock::text(text)]
    });
    define_bound(
        "job_list",
        "List your background jobs (running and finished) with their ids, kinds, and statuses."
            .to_string(),
        json!({}),
        json!({ "type": "array", "items": public_task_schema() }),
        render,
        "JobRegistry",
    )
}

/// `job_kill` 工具（宿主 JobRegistry 注入：kill；未注入 → `NOT_BOUND`）。
pub fn job_kill() -> Result<M4Tool, ToolDefinitionError> {
    let render: ToolRender = Rc::new(|_, value| {
        let outcome = value["outcome"].as_str().unwrap_or_default();
        let id = value["job"]["id"].as_str().unwrap_or_default();
        let line = if outcome == "already-finished" {
            format!("job {id} had already finished {}", status_line(&value["job"]))
        } else {
            format!("requested cancellation of job {id}")
        };
        vec![ContentBlock::text(line)]
    });
    define_bound(
        "job_kill",
        "Request cancellation of a running background job by job id. Returns immediately; the \
         job settles as killed once its work actually stops."
            .to_string(),
        json!({
            "job_id": {
                "type": "string",
                "required": true,
                "description": "Job id returned by the tool that started the background work.",
            },
            "reason": {
                "type": "string",
                "description": "Optional short reason, recorded in the log and forwarded to the job.",
            },
        }),
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "outcome": {
                    "type": "string",
                    "required": true,
                    "enum": ["already-finished", "cancellation-requested"],
                },
                "job": required_property(public_task_schema()),
            },
        }),
        render,
        "JobRegistry",
    )
}

// ---------------------------------------------------------------------------
// schedule_create / schedule_list / schedule_delete（宿主 schedule 域注入点）
// ---------------------------------------------------------------------------

/// `schedule_create` 工具（宿主 schedule 域注入；未注入 → `NOT_BOUND`）。
///
/// 输出 schema 保持 `json`（参考的 VIEW/错误 oneOf 联合体由宿主接线时以其
/// 领域 schema 覆盖——差异见回复）。
pub fn schedule_create() -> Result<M4Tool, ToolDefinitionError> {
    let render = schedule_render();
    define_bound(
        "schedule_create",
        "Create one reminder in the current session. Supply a non-empty prompt and exactly one \
         selector: a positive safe-integer after_seconds delay, at as a strict offset date-time \
         or local date/time object, or safe-integer every_seconds of at least 300. Fixed-rate \
         reminders stay creation-aligned, skip missed occurrences, and batch one latest \
         occurrence per overdue rule. Delivery is session-local: the reminder runs on time only \
         while this session is live and otherwise becomes overdue until the session is resumed."
            .to_string(),
        json!({
            "prompt": {
                "type": "string",
                "required": true,
                "description": "Reminder content to present when the target becomes due.",
            },
            "after_seconds": {
                "type": "number",
                "description": "Positive safe-integer delay in seconds.",
            },
            "every_seconds": {
                "type": "number",
                "description": "Fixed-rate safe-integer interval in seconds, at least 300.",
            },
            "at": {
                "description": "Absolute target as strict offset RFC 3339 or local date/time with an explicit IANA zone.",
                "oneOf": [
                    { "type": "string" },
                    {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "date": { "type": "string", "required": true },
                            "time": { "type": "string", "required": true },
                            "time_zone": { "type": "string", "required": true },
                        },
                    },
                ],
            },
        }),
        json!({ "type": "json" }),
        render,
        "schedule domain",
    )
}

/// `schedule_list` 工具（宿主 schedule 域注入；未注入 → `NOT_BOUND`）。
pub fn schedule_list() -> Result<M4Tool, ToolDefinitionError> {
    define_bound(
        "schedule_list",
        "List every active reminder in the current session in creation order, including its \
         exact id, UTC target, scheduled or overdue state, and session-local delivery mode."
            .to_string(),
        json!({}),
        json!({ "type": "json" }),
        schedule_render(),
        "schedule domain",
    )
}

/// `schedule_delete` 工具（宿主 schedule 域注入；未注入 → `NOT_BOUND`）。
pub fn schedule_delete() -> Result<M4Tool, ToolDefinitionError> {
    define_bound(
        "schedule_delete",
        "Delete one active reminder in the current session by the exact id returned by \
         schedule_create or schedule_list. Unknown or already-finished ids return deleted false."
            .to_string(),
        json!({
            "id": {
                "type": "string",
                "required": true,
                "description": "Exact session-local schedule id.",
            },
        }),
        json!({ "type": "json" }),
        schedule_render(),
        "schedule domain",
    )
}

/// schedule 输出 render：正则 JSON 序列化（对齐参考 renderValue）。
fn schedule_render() -> ToolRender {
    Rc::new(|_, value| {
        vec![ContentBlock::text(
            serde_json::to_string(value).unwrap_or_else(|_| "null".to_string()),
        )]
    })
}

// ---------------------------------------------------------------------------
// exit_plan_mode（宿主 plan-mode 服务注入点）
// ---------------------------------------------------------------------------

/// `exit_plan_mode` 工具（宿主注入「离开 plan mode + 写 command 事件」；
/// 未注入 → `NOT_BOUND`）。
pub fn exit_plan_mode() -> Result<M4Tool, ToolDefinitionError> {
    let render: ToolRender = Rc::new(|_, _| {
        vec![ContentBlock::text(
            "Plan approved — plan mode exited; carry out the plan starting with your next step."
                .to_string(),
        )]
    });
    define_bound(
        "exit_plan_mode",
        "Use only in plan mode. Present your plan for the user's review and, on approval, leave \
         plan mode. Send the COMPLETE plan as markdown, starting with a # heading that names it. \
         The user may approve (carry out the plan from your next step) or keep planning — their \
         feedback comes back in the tool result; revise and present again."
            .to_string(),
        json!({
            "plan": {
                "type": "string",
                "required": true,
                "description": "The complete plan, as markdown, starting with a # heading that names it.",
            },
        }),
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "approved": { "type": "boolean", "required": true, "const": true },
            },
        }),
        render,
        "plan-mode service",
    )
}

// ---------------------------------------------------------------------------
// workflow（M4 桩：meta 校验后恒 UNSUPPORTED_OPTION isError）
// ---------------------------------------------------------------------------

/// `workflow` 工具（M4 桩定义）。参数级 meta 校验走 `dsh-workflow::validate_meta`；
/// 校验失败 → `META_INVALID`；否则恒 `UNSUPPORTED_OPTION` 结构化 isError，绝不伪装
/// 成功。宿主无需注入（JS 引擎属后续里程碑）。
pub fn workflow() -> Result<Rc<ToolDefinition>, ToolDefinitionError> {
    let description = "Run a JavaScript workflow script that orchestrates subagents at scale. Use this for work that fans out across many independent pieces — an audit over many files, a migration, multi-angle research, adversarial verification of findings — where you write the orchestration as a script instead of delegating turn by turn.\n\nThe workflow's identity rides the `meta` parameter as JSON: required `name` (short kebab-case) and `description` strings, optional `whenToUse` string and `phases` array (`{title, detail?, provider?, model?}`). The `script` parameter is the plain JavaScript body ONLY (NOT TypeScript, and NO `export const meta` statement — meta is a parameter, not code), running with top-level await; end with `return <value>` — the value must be JSON-serializable and is this tool's result.\n\nScript-body hooks:\n- `agent(prompt, opts?): Promise<any>` — run one subagent to completion. Without `opts.schema` it resolves to the child's final text; with `opts.schema` (an object-rooted JSON Schema using ONLY type/properties/required/additionalProperties/items/enum/const/oneOf — no pattern/format/numeric bounds) it resolves to the validated object. Resolves `null` when the child fails (filter with `.filter(Boolean)`). Other opts: `label` (display), `phase` (progress group), and independent `provider`/`model` LLM target overrides (either may be provided alone). Anything else (`effort`/`isolation`/`agentType`) is rejected loudly.\n- `pipeline(items, ...stages): Promise<any[]>` — run each item through the stages independently with NO barrier between stages (prefer this for multi-stage work). Each stage receives `(prev, item, index)`. An ordinary stage throw drops that ITEM to `null` and skips its remaining stages.\n- `parallel(thunks): Promise<any[]>` — run zero-argument functions concurrently and await ALL of them (a barrier; use only when a stage genuinely needs every prior result together). A throwing thunk resolves to `null`.\n- `phase(title)` — start a progress phase; `log(message)` — narrate progress; `args` — the tool call's `args` input, verbatim.\n\nMisused hooks (bad arguments, unknown options, unsupported schemas, tripped caps) throw errors that ALWAYS kill the script — they never dissolve into a per-item `null`.\n\nConstraints: concurrency and total-agent caps apply; no filesystem, network, timers, or Node.js APIs are provided — the agents do the work, the script only coordinates them. The run executes in the foreground: this call returns when the whole script finishes.";

    let render: ToolRender = Rc::new(|args, value| {
        let name = args["meta"]["name"].as_str().unwrap_or_default();
        let started = value["agentsStarted"].as_i64().unwrap_or(0);
        let result = &value["result"];
        let formatted = serde_json::to_string_pretty(result).unwrap_or_else(|_| "null".to_string());
        let clipped = clip_chars(&formatted, WORKFLOW_MAX_RENDER_CHARS);
        let plural = if started == 1 { "" } else { "s" };
        vec![ContentBlock::text(format!(
            "workflow \"{name}\" completed ({started} agent{plural}).\nReturn value:\n{clipped}"
        ))]
    });

    let execute: ToolExecute = Rc::new(|args, _| {
        let meta = args.get("meta").unwrap_or(&Value::Null);
        match dsh_workflow::validate_meta(meta) {
            Err(e) => {
                let message = e.message;
                let code = e.code.as_str();
                Err(ToolFailureData::new(message, code, "WorkflowError"))
            }
            Ok(_) => Err(ToolFailureData::new(
                "workflow execution is not wired in this build (M4 stub definition; the JS engine lives behind the host)."
                    .to_string(),
                "UNSUPPORTED_OPTION",
                "WorkflowError",
            )),
        }
    });

    let def = define_tool(DefineToolOptions {
        name: "workflow".to_string(),
        description: description.to_string(),
        parameters: json!({
            "script": {
                "type": "string",
                "required": true,
                "description": "The plain-JS workflow script body (top-level await allowed; NO `export const meta` statement; end with `return <json-value>`).",
            },
            "meta": {
                "type": "object",
                "additionalProperties": true,
                "required": true,
                "description": "The workflow identity block (plain JSON — never code).",
                "properties": {
                    "name": { "type": "string", "required": true, "description": "Short kebab-case workflow name." },
                    "description": { "type": "string", "required": true, "description": "One-line description of what the workflow does." },
                    "whenToUse": { "type": "string", "description": "Optional guidance on when this workflow applies." },
                    "phases": {
                        "type": "array",
                        "description": "Optional phase declarations matched by phase() calls.",
                        "items": {
                            "type": "object",
                            "additionalProperties": true,
                            "properties": {
                                "title": { "type": "string", "required": true, "description": "The phase title phase() calls match by exact string." },
                                "detail": { "type": "string", "description": "Optional one-line description of the phase." },
                                "provider": { "type": "string", "description": "Optional provider override this phase is expected to use." },
                                "model": { "type": "string", "description": "Optional model override this phase is expected to use." },
                            },
                        },
                    },
                },
            },
            "args": {
                "type": "object",
                "additionalProperties": true,
                "description": "Optional JSON input exposed to the script as the `args` global (wrap a bare list as a field, e.g. {\"files\": [...]}).",
            },
        }),
        output_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "runId": { "type": "string", "required": true },
                "agentsStarted": { "type": "integer", "required": true },
                "result": { "type": "json", "required": true },
            },
        }),
        render,
        execute,
        ..Default::default()
    })?;
    Ok(Rc::new(def))
}

/// 渲染截断上限（对齐 tool-workflow Config.maxResultChars 默认 50000）。
const WORKFLOW_MAX_RENDER_CHARS: usize = 50_000;

/// 按字符数截断（尾部追加省略说明）。
fn clip_chars(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    let head: String = text.chars().take(max_chars).collect();
    format!("{head}\n… [truncated: {} more characters]", count - max_chars)
}

//! M5h: M5 执行工具的 web 接线（step7；M5-DESIGN §8）。
//!
//! 职责：把 M5 各 crate 的纯面（schema/parse/render）装配成 `dsh-tools` 可注册工具，
//! 并把宿主服务句柄（[`M5HostServices`]）bind 进对应工具的 execute 槽（同 `Rc` 生效）。
//!
//! 诚实接线原则（D-068）：工具一律先注册（定义可见、schema 可校验、模型可见 renderers
//! 单源），再按「宿主服务句柄是否在场」决定 execute 真实委托 vs 结构化 `NOT_BOUND`——
//! 绝不无句柄假装成功（M4 同款承诺，D-052）。真实绑定：terminal 六件套 + fs 六件套
//! （read/write/edit/glob/grep/str_replace_editor）+ bash 前台（resolve+run）；后台
//! run_in_background 诚实拒绝（jobs producer 桥/tick 后续轮）；read_image 待解码服务；
//! run_code 交注册表保留传输（D-068/D-069/D-070 记录待办）。

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use dsh_code_runtime::run_code::parse_run_code_args;
use dsh_code_runtime::{
    CodeRunFailure, CodeRunRequest, CodeRunResult, CodeRuntime, PythonCodeRuntime, PythonConfig,
};
use dsh_fs::grep::{
    format_grep_output, retain_grep_matches, GrepMatch, RetainedMatches, GREP_MAX_LINE_BYTES,
    GREP_MAX_MATCHES,
};
use dsh_fs::read_render::{
    build_window, format_read_output, parse_read_args, FileReadOutcome, ReadWindow, READ_LIMIT,
    READ_MAX_BYTES, READ_MAX_LINE_LENGTH,
};
use dsh_fs::{
    apply_insert, apply_str_replace, format_edit_output, format_file_view, format_write_output,
    glob_search_in, grep_search_in, parse_edit_args, parse_glob_args, parse_grep_args,
    parse_write_args, remediate_fs_error, FsEditRequest, FsError, FsTarget, FsWriteIntent,
    LocalFileSystem, Observation, ObservationGate, OwnerId, ReadTextOptions,
    DEFAULT_MAX_OUTPUT_CHARS,
};
use dsh_jobs::{
    JobRead, JobRegistry, JobRegistryConfig, JobSettlement, JobStartError, JobStatus,
    ProducerHooks, StartSpec,
};
use dsh_sandbox::SandboxMode;
use dsh_shell::{
    bash_tool_parameters, parse_bash_args, render_bash_result, BashConfig, LocalBashExecutor,
    ShellCollectedOutput, ShellError, ShellExecRequest, ShellExecSpec, ShellProcess,
    ShellProcessStatus, ShellRunResult,
};
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

/// fs 宿主：LocalFileSystem（root 解析）+ observation gate（owner 写/编守卫）+ agent→OwnerId
/// 稳定登记（Web 无 WeakMap；宿主会话结束时需清理——本轮接线不装会话清理钩子，D-069 记录）。
pub struct FsHost {
    pub fs: LocalFileSystem,
    pub root: PathBuf,
    gate: RefCell<ObservationGate>,
    owners: RefCell<HashMap<String, OwnerId>>,
    next_owner: Cell<OwnerId>,
}

impl FsHost {
    pub fn new(root: PathBuf) -> Self {
        Self {
            fs: LocalFileSystem::new(root.clone()),
            root,
            gate: RefCell::new(ObservationGate::new()),
            owners: RefCell::new(HashMap::new()),
            next_owner: Cell::new(1),
        }
    }

    /// agent → 稳定 OwnerId（memoize；时间序单调分配）。
    pub fn owner_id(&self, agent: &str) -> OwnerId {
        if let Some(id) = self.owners.borrow().get(agent) {
            return *id;
        }
        let id = self.next_owner.get();
        self.next_owner.set(id + 1);
        self.owners.borrow_mut().insert(agent.to_string(), id);
        id
    }

    /// 路径以宿主 root 为 cwd 解析。
    pub fn resolve(&self, file_path: &str) -> Result<FsTarget, FsError> {
        self.fs.resolve(
            file_path,
            dsh_fs::ResolveOptions {
                cwd: Some(self.root.clone()),
            },
        )
    }

    /// 写意图（observed-present → replace-if-version；否则 create-if-absent）。
    pub fn write_intent(&self, owner: &str, target: &FsTarget) -> FsWriteIntent {
        self.gate
            .borrow()
            .write_intent(self.owner_id(owner), target)
    }

    /// 编意图（未观察 → FS_NOT_OBSERVED 诚实拒绝）。
    pub fn edit_intent(
        &self,
        owner: &str,
        target: &FsTarget,
    ) -> Result<dsh_fs::FsVersion, FsError> {
        self.gate
            .borrow()
            .edit_intent(self.owner_id(owner), target)
            .map(|v| v.version)
    }

    /// 记录一次权威观察（读/写/编成功后）。
    pub fn record(&self, owner: &str, target: &FsTarget, obs: Observation) {
        self.gate
            .borrow_mut()
            .record(self.owner_id(owner), target, obs);
    }
}

/// shell 宿主：本地 bash 后端（root 为默认工作目录；`On Mac/Linux` 亦可）。
pub struct ShellHost {
    pub executor: LocalBashExecutor,
    pub root: PathBuf,
}

impl ShellHost {
    /// 构造并校验 bash 配置；cwd 锚定宿主 root（工作区）。
    pub fn new(root: PathBuf) -> Result<Self, String> {
        let executor = LocalBashExecutor::new(BashConfig {
            cwd: Some(root.clone()),
            ..BashConfig::default()
        })?;
        Ok(ShellHost { executor, root })
    }
}

/// bash 后台 jobs producer 桥（M5-DESIGN §8 jobs subprocess producer，D-049 形状闭合）。
///
/// `JobRegistry`（app 侧 job_read/job_list/job_kill 工具的石墨契约）与 `ShellProcess`
/// 后台句柄以 job id 关联：`start_bash` 把进程桥成 `ProducerHooks{on_cancel=kill}`；
/// 完成结算由**宿主合作泵**驱动（`pump()`：M5g tick/服务线程调之；测试直接调）——
/// 单线程注册表不自驱动 settle（D-004 诚实降级）。注册成功前不 spawn（producer 延迟
/// start 在 jobs.start 内）；`start_bash` 失败由调用方掐掉进程。
pub struct BashJobsBridge {
    registry: RefCell<JobRegistry>,
    processes: RefCell<HashMap<String, Rc<ShellProcess>>>,
    outputs: RefCell<HashMap<String, String>>,
}

impl Default for BashJobsBridge {
    fn default() -> Self {
        Self {
            registry: RefCell::new(JobRegistry::new(JobRegistryConfig::default())),
            processes: RefCell::new(HashMap::new()),
            outputs: RefCell::new(HashMap::new()),
        }
    }
}

impl BashJobsBridge {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一个已 spawn 的后台 bash 进程为 job（owner 归属 caller；label 缺省命令摘要）。
    /// 成功后 job 可见、可被 job_kill/job_read 操作。进程先在调用方 spawn：jobs.start
    /// 的 producer 只回喂 hooks，不重新触发执行。
    pub fn start_bash(
        &self,
        owner: &str,
        label: &str,
        process: Rc<ShellProcess>,
    ) -> Result<String, JobStartError> {
        let proc = process.clone();
        let id = self.registry.borrow_mut().start(StartSpec {
            kind: "bash",
            label,
            owner: Some(owner.to_string()),
            producer: Box::new(move || {
                let killer = proc.clone();
                ProducerHooks {
                    on_cancel: Box::new(move |_reason| {
                        killer.kill();
                    }),
                    read_output: None, // final-output 语义：终态 settle 携全文，不流式滚入
                }
            }),
        })?;
        self.processes.borrow_mut().insert(id.clone(), process);
        Ok(id)
    }

    /// 合作推进泵：滚动增量（终态走私全文）+ 探测终态 + settle + 移除。
    /// 返回本次结算条数。由宿主 tick（M5g）或测试循环调用；幂等。
    pub fn pump(&self) -> usize {
        let mut finished: Vec<(String, ShellProcessStatus, Option<i32>)> = Vec::new();
        {
            let procs = self.processes.borrow();
            for (id, proc) in procs.iter() {
                // 先 done() 等到退出（collector 已 join，管道缓冲已全部落盘），
                // 再 read_output() 收尾增量：终态轮拿到的是全文（含此前 running 轮
                // 已消费的首段——offset 消费性，累计即完整终态输出）。
                proc.done();
                let delta = proc.read_output().delta;
                if !delta.is_empty() {
                    self.outputs
                        .borrow_mut()
                        .entry(id.clone())
                        .or_default()
                        .push_str(&delta);
                }
                let st = proc.status();
                if st == ShellProcessStatus::Completed || st == ShellProcessStatus::Killed {
                    finished.push((id.clone(), st, proc.exit_code()));
                }
            }
        }
        for (id, st, code) in &finished {
            let status = if *st == ShellProcessStatus::Killed {
                JobStatus::Killed
            } else {
                JobStatus::Completed
            };
            let output = self.outputs.borrow_mut().remove(id).unwrap_or_default();
            let detail = code
                .map(|c| format!("exit code {c}"))
                .unwrap_or_else(|| "killed".to_string());
            self.registry.borrow_mut().settle(
                id,
                JobSettlement {
                    status,
                    detail: Some(detail),
                    output: Some(output),
                },
            );
            self.processes.borrow_mut().remove(id);
        }
        finished.len()
    }

    /// job 只读投影（caller 授权围栏由注册表执行）。
    pub fn read(&self, id: &str, caller: Option<&str>) -> Result<JobRead, dsh_jobs::JobOpsError> {
        self.registry.borrow_mut().read(id, caller)
    }
}

// ---------------------------------------------------------------------------
// M5g 定时推进（M5-DESIGN §8；M5-REQUIREMENTS 验收 #7）：服务层线程 tick → mpsc →
// 主线程 `m5g_tick_once`（ScheduleHost::dispatch_due 到期注入 + BashJobsBridge::pump
// 合作结算——非手工）。核心（折叠/到期/jobs 泵）留在主线程；线程只发 tick（Send 安全）。
// ---------------------------------------------------------------------------

/// 服务层 tick 发送器：每 `interval_ms` 向 mpsc 推一个 tick；`Drop` 置停（线程退出）。
pub struct M5gTick {
    rx: mpsc::Receiver<()>,
    stop: Arc<AtomicBool>,
}

impl M5gTick {
    /// 起服务层线程（间隔 ≥1ms；线程名 m5g-tick）。
    pub fn start(interval_ms: u64) -> Self {
        let (tx, rx) = mpsc::channel::<()>();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_t = stop.clone();
        let tx_t = tx;
        let _handle = std::thread::Builder::new()
            .name("m5g-tick".to_string())
            .spawn(move || loop {
                std::thread::sleep(Duration::from_millis(interval_ms.max(1)));
                if stop_t.load(Ordering::Relaxed) || tx_t.send(()).is_err() {
                    break;
                }
            });
        M5gTick { rx, stop }
    }

    /// 主线程阻塞等一个 tick（带超时）；false = 超时（避免无限等待）。
    pub fn wait_tick(&self, timeout: Duration) -> bool {
        self.rx.recv_timeout(timeout).is_ok()
    }
}

impl Drop for M5gTick {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // 线程 sleep 最迟 interval_ms 后退出；handle 已 detach（Builder::spawn 在线程退出
        // 时释放名字内存，join 由 stop 旗标保证有界）。
    }
}

/// 主线程 tick_once：ScheduleHost 到期注入（dispatch_due）+ jobs 桥合作结算（pump）。
/// 返回 (framing 文本, 派发的 schedule id)。由 M5g 主循环消费服务线程的 tick 调用。
pub fn m5g_tick_once(
    sched: &Rc<crate::web::dsh_cli_host::ScheduleHost>,
    bridge: Option<&BashJobsBridge>,
    now_epoch: i64,
) -> Result<(Vec<String>, Vec<String>), String> {
    let (framing, dispatched) = sched.dispatch_due(now_epoch)?;
    if let Some(b) = bridge {
        b.pump();
    }
    Ok((framing, dispatched))
}

/// M5h 宿主服务句柄集合：M5 工具组的 bind 目标。
///
/// `register_m5_tools_with_host` 接受可选的 `&M5HostServices`：有句柄 → 对应工具 bind
/// 到真实服务（fail loud 不再 NOT_BOUND）；无句柄 → 注册定义但保持 `NOT_BOUND`。
/// 装配 `terminal` + `fs` + `shell` + `bash_jobs`；code_runtime 传输/tick 后续轮
/// （D-068/D-069/D-070）。
#[derive(Default)]
pub struct M5HostServices {
    /// 终端会话注册表（terminal_open/send/read/signal/close/list 的真实句柄）。
    pub terminal: Option<Rc<RefCell<TerminalSessionService>>>,
    /// fs 宿主（read/write/edit/glob/grep/str_replace_editor 的真实句柄）。
    pub fs: Option<Rc<FsHost>>,
    /// shell 宿主（bash 工具前台执行的真实句柄）。
    pub shell: Option<Rc<ShellHost>>,
    /// bash 后台 jobs producer 桥（bash run_in_background 的真实句柄；缺省 → 诚实拒绝）。
    pub bash_jobs: Option<Rc<BashJobsBridge>>,
    /// code runtime（run_code 传输的真实 execute 覆盖；缺省 → 注册表占位桩诚实报错）。
    pub code: Option<Rc<PythonCodeRuntime>>,
}

/// M5 宿主生产装配（M5-DESIGN §8；验收 #9）：一次构造全部宿主句柄，root 为工作区。
/// terminal/fs/shell/bash_jobs 恒在场；code 仅 python 可用时装配（诚实——无 runtime 时
/// run_code 保持注册表占位桩）。会话清理钩子（fs owner 登记释放，D-069 记录）随宿主
/// 生命周期由装配方在会话结束调用（预留）。
pub struct M5Host {
    pub services: M5HostServices,
}

impl M5Host {
    pub fn assemble(root: PathBuf) -> Result<Self, String> {
        let root_abs = root.canonicalize().unwrap_or(root);
        let terminal = Rc::new(RefCell::new(TerminalSessionService::new()));
        let fs = Rc::new(FsHost::new(root_abs.clone()));
        let shell = Rc::new(ShellHost::new(root_abs.clone())?);
        let bash_jobs = Rc::new(BashJobsBridge::new());
        let code = dsh_code_runtime::python_available()
            .then(|| Rc::new(PythonCodeRuntime::new(PythonConfig::default())));
        Ok(M5Host {
            services: M5HostServices {
                terminal: Some(terminal),
                fs: Some(fs),
                shell: Some(shell),
                bash_jobs: Some(bash_jobs),
                code,
            },
        })
    }

    /// 便捷注册：全工具 + 全部在场句柄 bind 进目标 registry。
    pub fn register(&self, registry: &dsh_tools::ToolRegistry) {
        register_m5_tools_with_host(registry, Some(&self.services));
    }
}

// ---------------------------------------------------------------------------
// effectiveSandboxMode / sandbox:policy（M5-DESIGN §8；验收 #3 会话事件投影 + 系统提示段）
// ---------------------------------------------------------------------------

/// effectiveSandboxMode fold 结果：模式 + 来源（"session"/"delegation"/"default"）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectiveSandbox {
    pub mode: SandboxMode,
    pub source: &'static str,
}

/// 会话事件 → effective 模式（precedence：approved > session sandbox/mode > 默认
/// read-only）。本 fold 实现 session+default 两档（last-wins `sandbox/mode` 事件，未知
/// 模式忽略——log-only 语义）；approved 级联（approval/decided 事件落盘）留宿主接线
/// 的预留槽位，D-074 记录——不伪造 approved 来源。
pub fn fold_effective_sandbox_mode(events: &[Value]) -> EffectiveSandbox {
    let mut effective = EffectiveSandbox {
        mode: SandboxMode::ReadOnly,
        source: "default",
    };
    for e in events {
        if e["type"].as_str() != Some("sandbox/mode") {
            continue;
        }
        let Some(data) = e.get("data") else { continue };
        let Some(mode_str) = data.get("mode").and_then(Value::as_str) else {
            continue;
        };
        let Ok(m) = mode_str.parse::<SandboxMode>() else {
            continue;
        };
        effective.mode = m;
        effective.source = if data.get("source").and_then(Value::as_str) == Some("delegation") {
            "session-delegation"
        } else {
            "session"
        };
    }
    effective
}

/// `sandbox:policy` 系统提示段（order 110；验收 #3 系统提示注入）：有效模式 + 可写根
/// （仅 workspace-write 产名单，复用 dsh-sandbox::writable_roots）。
pub fn sandbox_policy_segment(
    mode: SandboxMode,
    workspace_root: Option<&std::path::Path>,
) -> String {
    let roots = dsh_sandbox::writable_roots(mode, workspace_root.map(|p| p.to_path_buf()));
    let roots_text = if roots.is_empty() {
        "(none — read-only)".to_string()
    } else {
        roots
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!("sandbox: policy — effective mode {mode}\nwritable roots: {roots_text}")
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

    // ---- bash：登记定义（纯面 schema + 渲染）；shell 宿主在场 → bind 真实前台执行。
    // 注：`run_code` 不在此登记——注册表保留该名注入 Code Mode 占位传输（诚实
    // "requires a code runtime" 桩）；真实运行面绑定属 registry/run_code binder 步（D-068）。
    let bash = bash_tool();
    if let Some(shost) = host.and_then(|h| h.shell.clone()) {
        let bridge = host.and_then(|h| h.bash_jobs.clone());
        bash.bind(bash_executor(shost, bridge));
    }
    registry
        .register_global(Rc::clone(&bash.definition()))
        .expect("register bash");

    // ---- fs 六件套 + 搜索 + sr-editor：纯面定义（schema + 渲染）+ 宿主 bind ----
    let fs_read = fs_read_tool();
    let fs_write = fs_write_tool();
    let fs_edit = fs_edit_tool();
    let fs_read_image = fs_read_image_tool();
    let glob = glob_tool();
    let grep = grep_tool();
    let sr_editor = sr_editor_tool();
    if let Some(fsh) = host.and_then(|h| h.fs.clone()) {
        fs_read.bind(fs_read_executor(fsh.clone()));
        fs_write.bind(fs_write_executor(fsh.clone()));
        fs_edit.bind(fs_edit_executor(fsh.clone()));
        glob.bind(glob_executor(fsh.clone()));
        grep.bind(grep_executor(fsh.clone()));
        sr_editor.bind(sr_editor_executor(fsh));
    }
    for (name, tool) in [
        ("read", fs_read),
        ("write", fs_write),
        ("edit", fs_edit),
        ("read_image", fs_read_image),
        ("glob", glob),
        ("grep", grep),
        ("str_replace_editor", sr_editor),
    ] {
        registry
            .register_global(tool.definition())
            .unwrap_or_else(|e| panic!("{name}: {e}"));
    }

    // ---- run_code 传输（验收 #6）：code runtime 在场 → 覆盖注册表 Code Mode 注入传输
    // 的 execute（真实 python 执行；D-073）。无 runtime → 占位桩保留（诚实报错）。
    if let Some(cr) = host.and_then(|h| h.code.clone()) {
        registry.set_run_code_executor(run_code_executor_with(cr));
    }
}

// ---------------------------------------------------------------------------
// run_code 传输 executor（验收 #6）：真实 python 后端；渲染词表由 dsh-tools registry
// run_code_def 单源（render_run_code_value 依规范化 value 产出可见文本）。
// ---------------------------------------------------------------------------

/// run_code executor：parse（code/description 必填）→ `PythonCodeRuntime::run` →
/// 规范化值 `{language, value?, logs[], error?}`。嵌套工具派发（bindings）本轮为空——
/// 诚实空命名空间（程序调 tools.* 得未注入错误），D-073 记录渐进。
fn run_code_executor_with(runtime: Rc<PythonCodeRuntime>) -> ToolExecute {
    Rc::new(move |args, _ctx| {
        let (code, _description) =
            parse_run_code_args(args).map_err(|m| invalid_args("run_code", m))?;
        let request = CodeRunRequest {
            program: &code,
            bindings: Vec::new(),
            signal: None,
        };
        let result = runtime.run(&request);
        Ok(run_code_canonical(&result))
    })
}

fn run_code_canonical(result: &CodeRunResult) -> Value {
    json!({
        "language": "python",
        "value": result.value,
        "logs": result.logs,
        "error": result.error.as_ref().map(code_failure_json),
    })
}

fn code_failure_json(e: &CodeRunFailure) -> Value {
    json!({
        "kind": e.kind.as_str(),
        "message": e.message,
        "detail": e.detail,
    })
}

// ---------------------------------------------------------------------------
// bash 工具：纯面定义（schema + 渲染）+ 宿主 executor
// ---------------------------------------------------------------------------

/// bash 工具定义：execute 产生规范化 value，render 依值重建 `ShellRunResult`（前台）或
/// job 启动说明（后台返回 jobId），走 `render_bash_result` 同词表（显式=值/可见性=渲染）。
fn bash_tool() -> M5Tool {
    define_m5_tool(
        "bash",
        "Run a shell command in the host workspace, returning its output, exit code, and sandbox status.".into(),
        bash_tool_parameters(true, &[]),
        json!({"type":"object","additionalProperties":true}),
        Rc::new(|_a, v| {
            if let Some(id) = v["jobId"].as_str() {
                // 后台启动：值只含 jobId（final-output 语义，job_read 消费终态输出）。
                return vec![ContentBlock::text(format!(
                    "bash: background job {id} started (collect via job_read; completion settled by host tick)"
                ))];
            }
            let result = ShellRunResult {
                exit_code: v["exitCode"].as_i64().map(|n| n as i32),
                signal: v["signal"].as_str().map(str::to_string),
                timed_out: v["timedOut"].as_bool().unwrap_or(false),
                aborted: v["aborted"].as_bool().unwrap_or(false),
                timeout_ms: v["timeoutMs"].as_u64().unwrap_or(0),
                stdout: collected_from_value(&v["stdout"]),
                stderr: collected_from_value(&v["stderr"]),
                // 本地 bash 后端恒无 SAND；escalation 词表为空集（本轮无提升参数）。
                sandbox: None,
            };
            vec![ContentBlock::text(render_bash_result(&result, &[]))]
        }),
    )
    .expect("bash defines")
}

/// 规范化 bash 执行结果（execute → value；render 只消费 value，不独走）。
/// 后台路径：`bridge` 在场 → 起 job（jobId）；否则诚实 `UNSUPPORTED_OPTION`。
fn bash_executor(shost: Rc<ShellHost>, bridge: Option<Rc<BashJobsBridge>>) -> ToolExecute {
    Rc::new(move |args, ctx| {
        let parsed = parse_bash_args(args).map_err(|m| invalid_args("bash", m))?;
        if let Some(perms) = &parsed.sandbox_permissions {
            if !perms.is_empty() {
                return Err(unsupported(
                    "bash: sandbox_permissions: non-empty requires sandboxed execution (SAND mode projection not yet wired; D-070)",
                ));
            }
        }
        let request = ShellExecRequest {
            command: parsed.command.clone(),
            workdir: parsed.workdir.map(PathBuf::from),
            timeout_ms: parsed.timeout_ms,
            stdout_max_bytes: None,
            signal: None,
            stdin: None,
            env: None,
            dsh_env: None,
        };
        let spec = shost
            .executor
            .resolve(&request)
            .map_err(|m| invalid_args("bash", m))?;
        if parsed.run_in_background == Some(true) {
            return bash_background(&shost, &spec, &parsed.command, ctx, bridge.as_deref());
        }
        let result = shost
            .executor
            .run(&spec)
            .map_err(|e| shell_failure("bash", e))?;
        Ok(bash_canonical(&parsed.command, &result))
    })
}

/// 后台路径：spawn ShellProcess → jobs producer 桥登记 → 返回 jobId。
/// 登记失败（配额等）掐掉刚 spawn 的进程（不产生孤儿），诚实报错。
fn bash_background(
    shost: &ShellHost,
    spec: &ShellExecSpec,
    command: &str,
    ctx: &dsh_tools::ToolRunContext,
    bridge: Option<&BashJobsBridge>,
) -> Result<Value, ToolFailureData> {
    let bridge = bridge.ok_or_else(|| {
        unsupported(
            "bash: run_in_background: true requires the jobs producer bridge (BashJobsBridge host handle) — not wired for this surface",
        )
    })?;
    let owner = required_agent(ctx.agent.as_deref(), "bash/run_in_background")?;
    let process = shost
        .executor
        .start(spec)
        .map_err(|e| shell_failure("bash", e))?;
    let process = Rc::new(process);
    let label = {
        let joined: String = command.trim().chars().take(60).collect();
        if joined.is_empty() {
            "bash background".to_string()
        } else {
            joined
        }
    };
    match bridge.start_bash(owner, &label, process.clone()) {
        Ok(id) => Ok(json!({ "jobId": id })),
        Err(e) => {
            process.kill();
            Err(ToolFailureData::new(
                format!("bash: start background job: {e:?}"),
                "JOB_START",
                "JobStartError",
            ))
        }
    }
}

/// execute 面规范化值（render 依此重建 ShellRunResult）。
fn bash_canonical(command: &str, result: &ShellRunResult) -> Value {
    json!({
        "command": command,
        "exitCode": result.exit_code,
        "signal": result.signal,
        "timedOut": result.timed_out,
        "aborted": result.aborted,
        "timeoutMs": result.timeout_ms,
        "stdout": collected_to_value(&result.stdout),
        "stderr": collected_to_value(&result.stderr),
        "sandbox": None::<Value>,
    })
}

fn collected_to_value(c: &ShellCollectedOutput) -> Value {
    json!({
        "text": c.text,
        "truncated": c.truncated,
        "spillPath": c.spill_path,
    })
}

fn collected_from_value(v: &Value) -> ShellCollectedOutput {
    ShellCollectedOutput {
        text: v["text"].as_str().unwrap_or("").to_string(),
        truncated: v["truncated"].as_bool().unwrap_or(false),
        spill_path: v["spillPath"].as_str().map(PathBuf::from),
    }
}

fn shell_failure(tool: &str, e: ShellError) -> ToolFailureData {
    ToolFailureData::new(format!("{tool}: {e}"), "SHELL_SPAWN", "ShellError")
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
// fs 六件套 + 搜索 + sr-editor：纯面定义（schema + 渲染）+ 宿主 executor
// ---------------------------------------------------------------------------

fn fs_read_tool() -> M5Tool {
    define_m5_tool(
        "read",
        "Read a file from the workspace into a numbered window (offset/limit line window honored)."
            .into(),
        json!({
            "file_path": {"type":"string","required":true},
            "offset": {"type":"integer"},
            "limit": {"type":"integer"},
        }),
        json!({"type":"object","additionalProperties":true}),
        Rc::new(|_a, v| {
            let outcome = FileReadOutcome {
                offset: v["offset"].as_u64().unwrap_or(1) as usize,
                lines: v["lines"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .map(|l| dsh_fs::read_render::FileTextLine {
                                number: l["number"].as_u64().unwrap_or(0) as usize,
                                text: l["text"].as_str().unwrap_or("").to_string(),
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                total_lines: v["total_lines"].as_u64().unwrap_or(0) as usize,
                truncated_by_bytes: v["truncated"].as_bool().unwrap_or(false),
            };
            let text = format_read_output(v["file_path"].as_str().unwrap_or("?"), &outcome);
            vec![ContentBlock::text(text)]
        }),
    )
    .expect("read defines")
}

fn fs_write_tool() -> M5Tool {
    define_m5_tool(
        "write",
        "Write or create a text file atomically in the workspace (UTF-8).".into(),
        json!({
            "file_path": {"type":"string","required":true},
            "content": {"type":"string","required":true},
            "description": {"type":"string"},
        }),
        json!({"type":"object","additionalProperties":true}),
        Rc::new(|_a, v| {
            let text = format_write_output(
                v["file_path"].as_str().unwrap_or("?"),
                v["operation"].as_str().unwrap_or("create"),
            );
            vec![ContentBlock::text(text)]
        }),
    )
    .expect("write defines")
}

fn fs_edit_tool() -> M5Tool {
    define_m5_tool(
        "edit",
        "Replace occurrences of old_string with new_string in a text file, version-guarded by read-before-edit observation.".into(),
        json!({
            "file_path": {"type":"string","required":true},
            "old_string": {"type":"string","required":true},
            "new_string": {"type":"string","required":true},
            "replace_all": {"type":"boolean"},
            "description": {"type":"string"},
        }),
        json!({"type":"object","additionalProperties":true}),
        Rc::new(|_a, v| {
            let text = format_edit_output(
                v["file_path"].as_str().unwrap_or("?"),
                v["replace_all"].as_bool().unwrap_or(false),
            );
            vec![ContentBlock::text(text)]
        }),
    )
    .expect("edit defines")
}

fn fs_read_image_tool() -> M5Tool {
    define_m5_tool(
        "read_image",
        "Read an image file and return it as inline media (PNG/JPEG/WebP/GIF) with dimensions."
            .into(),
        json!({"file_path": {"type":"string","required":true}}),
        json!({"type":"object","additionalProperties":true}),
        Rc::new(|_a, v| vec![ContentBlock::text(render_passthrough(v))]),
    )
    .expect("read_image defines")
}

fn glob_tool() -> M5Tool {
    define_m5_tool(
        "glob",
        "List files matching a glob pattern under the workspace, excluding VCS dirs.".into(),
        json!({
            "pattern": {"type":"string","required":true},
            "path": {"type":"string"},
        }),
        json!({"type":"object","additionalProperties":true}),
        Rc::new(|_a, v| {
            let matches: Vec<String> = v["matches"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|m| m.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let text = if matches.is_empty() {
                "(no matches)".to_string()
            } else {
                matches.join("\n")
            };
            vec![ContentBlock::text(text)]
        }),
    )
    .expect("glob defines")
}

fn grep_tool() -> M5Tool {
    define_m5_tool(
        "grep",
        "Search file contents with an ignore-aware regex under the workspace.".into(),
        json!({
            "pattern": {"type":"string","required":true},
            "path": {"type":"string"},
            "include": {"type":"string"},
        }),
        json!({"type":"object","additionalProperties":true}),
        Rc::new(|_a, v| {
            let items: Vec<GrepMatch> = v["matches"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|m| GrepMatch {
                            path: m["path"].as_str().unwrap_or("").to_string(),
                            line_number: m["line_number"].as_u64().unwrap_or(0),
                            line: m["line"].as_str().unwrap_or("").to_string(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            let retained = RetainedMatches {
                items,
                seen: v["seen"].as_u64().unwrap_or(0) as usize,
            };
            let text = format_grep_output(&retained, None);
            vec![ContentBlock::text(text)]
        }),
    )
    .expect("grep defines")
}

fn sr_editor_tool() -> M5Tool {
    define_m5_tool(
        "str_replace_editor",
        "View a file, apply a unique string replacement, or insert text at a line (all read-before-edit).".into(),
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
        Rc::new(|_a, v| {
            let text = format_file_view(
                v["file_path"].as_str().unwrap_or("?"),
                v["content"].as_str().unwrap_or(""),
                DEFAULT_MAX_OUTPUT_CHARS,
                None,
            )
            .unwrap_or_else(|_| "(unreadable view)".to_string());
            vec![ContentBlock::text(text)]
        }),
    )
    .expect("str_replace_editor defines")
}

fn fs_read_executor(fsh: Rc<FsHost>) -> ToolExecute {
    Rc::new(move |args, ctx| {
        let owner = required_agent(ctx.agent.as_deref(), "read")?;
        let input = parse_read_args(
            args["file_path"].as_str().unwrap_or(""),
            args.get("offset").and_then(Value::as_i64),
            args.get("limit").and_then(Value::as_i64),
            READ_LIMIT,
        )
        .map_err(|m| invalid_args("read", m))?;
        let target = fsh
            .resolve(&input.file_path)
            .map_err(|e| fs_failure("read", e))?;
        let text = fsh
            .fs
            .read_text(&target, ReadTextOptions { max_bytes: None })
            .map_err(|e| fs_failure("read", e))?;
        let window = build_window(
            &text.content,
            &ReadWindow {
                offset: input.offset,
                limit: input.limit,
                max_line_length: READ_MAX_LINE_LENGTH,
                max_bytes: READ_MAX_BYTES,
            },
            &target.display_path,
        )
        .map_err(|e| fs_failure("read", e))?;
        // 权威观察：后续 write/edit 以本次所见版本做 CAS 基础。
        fsh.record(
            owner,
            &target,
            Observation::Present {
                version: text.version,
            },
        );
        let lines: Vec<Value> = window
            .lines
            .iter()
            .map(|l| json!({ "number": l.number, "text": l.text }))
            .collect();
        Ok(json!({
            "file_path": target.display_path,
            "offset": input.offset,
            "lines": lines,
            "total_lines": window.total_lines,
            "truncated": window.truncated_by_bytes,
        }))
    })
}

fn fs_write_executor(fsh: Rc<FsHost>) -> ToolExecute {
    Rc::new(move |args, ctx| {
        let owner = required_agent(ctx.agent.as_deref(), "write")?;
        let input = parse_write_args(
            args["file_path"].as_str().unwrap_or(""),
            args["content"].as_str().unwrap_or(""),
        )
        .map_err(|m| invalid_args("write", m))?;
        let target = fsh
            .resolve(&input.file_path)
            .map_err(|e| fs_failure("write", e))?;
        let intent = fsh.write_intent(owner, &target);
        let outcome = fsh
            .fs
            .write_text(&target, &input.content, Some(intent), None)
            .map_err(|e| fs_failure("write", e))?;
        fsh.record(
            owner,
            &target,
            Observation::Present {
                version: outcome.version,
            },
        );
        Ok(json!({
            "file_path": target.display_path,
            "operation": outcome.operation,
        }))
    })
}

fn fs_edit_executor(fsh: Rc<FsHost>) -> ToolExecute {
    Rc::new(move |args, ctx| {
        let owner = required_agent(ctx.agent.as_deref(), "edit")?;
        let input = parse_edit_args(
            args["file_path"].as_str().unwrap_or(""),
            args["old_string"].as_str().unwrap_or(""),
            args["new_string"].as_str().unwrap_or(""),
            args.get("replace_all").and_then(Value::as_bool),
        )
        .map_err(|m| invalid_args("edit", m))?;
        let target = fsh
            .resolve(&input.file_path)
            .map_err(|e| fs_failure("edit", e))?;
        let saw = fsh
            .edit_intent(owner, &target)
            .map_err(|e| fs_failure("edit", e))?;
        let req = FsEditRequest {
            old_string: input.old_string,
            new_string: input.new_string,
            replace_all: input.replace_all,
        };
        let outcome = fsh
            .fs
            .edit_text(&target, &req, Some(&saw), None)
            .map_err(|e| fs_failure("edit", e))?;
        fsh.record(
            owner,
            &target,
            Observation::Present {
                version: outcome.version,
            },
        );
        Ok(json!({
            "file_path": target.display_path,
            "replace_all": req.replace_all,
        }))
    })
}

fn glob_executor(fsh: Rc<FsHost>) -> ToolExecute {
    Rc::new(move |args, _ctx| {
        let input = parse_glob_args(
            args["pattern"].as_str().unwrap_or(""),
            args.get("path").and_then(Value::as_str),
        )
        .map_err(|m| invalid_args("glob", m))?;
        let matches = glob_search_in(&fsh.root, &input).map_err(|e| fs_failure("glob", e))?;
        Ok(json!({ "matches": matches }))
    })
}

fn grep_executor(fsh: Rc<FsHost>) -> ToolExecute {
    Rc::new(move |args, _ctx| {
        let input = parse_grep_args(
            args["pattern"].as_str().unwrap_or(""),
            args.get("path").and_then(Value::as_str),
            args.get("include").and_then(Value::as_str),
        )
        .map_err(|m| invalid_args("grep", m))?;
        let matches = grep_search_in(&fsh.root, &input).map_err(|e| grep_failure("grep", e))?;
        let retained = retain_grep_matches(&matches, GREP_MAX_MATCHES, GREP_MAX_LINE_BYTES);
        Ok(json!({
            "matches": retained.items.iter().map(|m| json!({
                "path": m.path, "line_number": m.line_number, "line": m.line
            })).collect::<Vec<_>>(),
            "seen": retained.seen,
        }))
    })
}

fn sr_editor_executor(fsh: Rc<FsHost>) -> ToolExecute {
    Rc::new(move |args, ctx| {
        let owner = required_agent(ctx.agent.as_deref(), "str_replace_editor")?;
        let file_path = args["file_path"].as_str().unwrap_or("").to_string();
        if file_path.trim().is_empty() {
            return Err(invalid_args(
                "str_replace_editor",
                "file_path must be a non-empty string".into(),
            ));
        }
        let use_view = args.get("view").and_then(Value::as_bool).unwrap_or(false);
        let repl = args
            .get("old_string")
            .and_then(Value::as_str)
            .zip(args.get("new_string").and_then(Value::as_str));
        let insert = args
            .get("insert_line")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .zip(args.get("new_str").and_then(Value::as_str));
        // 读当前文本 + 版本（自动读 → CAS 基础；str_replace_editor 参考自读语义）。
        let target = fsh
            .resolve(&file_path)
            .map_err(|e| fs_failure("str_replace_editor", e))?;
        let text = fsh
            .fs
            .read_text(&target, ReadTextOptions { max_bytes: None })
            .map_err(|e| fs_failure("str_replace_editor", e))?;
        fsh.record(
            owner,
            &target,
            Observation::Present {
                version: text.version.clone(),
            },
        );
        let final_text = if use_view && repl.is_none() && insert.is_none() {
            text.content
        } else if let (Some((old_str, new_str)), None) = (repl, insert) {
            apply_str_replace(&text.content, old_str, new_str, &target.display_path)
                .map_err(|e| fs_failure("str_replace_editor", e))?
        } else if let (None, Some((line, new_str))) = (repl, insert) {
            apply_insert(&text.content, line, new_str)
                .map_err(|m| invalid_args("str_replace_editor", m))?
        } else {
            return Err(invalid_args(
                "str_replace_editor",
                "specify exactly one of view:true, old_string+new_string, or insert_line+new_str"
                    .into(),
            ));
        };
        if !use_view && (repl.is_some() || insert.is_some()) {
            let outcome = fsh
                .fs
                .write_text(
                    &target,
                    &final_text,
                    Some(FsWriteIntent::ReplaceIfVersion {
                        version: text.version,
                    }),
                    None,
                )
                .map_err(|e| fs_failure("str_replace_editor", e))?;
            fsh.record(
                owner,
                &target,
                Observation::Present {
                    version: outcome.version,
                },
            );
        }
        Ok(json!({
            "file_path": target.display_path,
            "content": final_text,
        }))
    })
}

fn fs_failure(tool: &str, e: FsError) -> ToolFailureData {
    let remedied = remediate_fs_error(&e);
    ToolFailureData::new(
        format!("{tool}: {}", remedied.message),
        remedied.code().as_str(),
        "FsError",
    )
}

fn grep_failure(tool: &str, e: dsh_fs::GrepError) -> ToolFailureData {
    ToolFailureData::new(
        format!("{tool}: {}", e.message),
        format!("{:?}", e.code),
        "GrepError",
    )
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

/// 允许外部（web.rs 测试 / 未来装配）复用本模块的专用输出 schema（permissive object）。
pub fn permissive_output_schema() -> Value {
    json!({"type":"object","additionalProperties":true})
}

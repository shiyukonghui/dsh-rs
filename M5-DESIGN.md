# M5 执行引擎与沙箱：系统设计（阶段二）

> 本文是 `M5-REQUIREMENTS.md`（阶段一，已验收，D-055）之后的设计工件。契约逐字取自
> 参考 TS 源码（`deepseek-harness/packages/{fs,shell,subprocess,terminal,sandbox,
> code-runtime}/`，只读）并经本会话独立交叉核实（round 7/8/9）。目标：Rust 实现阶段
> （阶段三 TDD）直接按本文件的契约与模块划分落地，不做二次勘察。
>
> 设计原则（继承方法论四）：① capability seam = Service Definition / Provider / Consumer
> 三段，缺一不算 seam；② 显式 > 隐式：defaulting 是 owning 实现里的 `resolve()` 步骤，
> 绝非 `run()` 里隐藏 `?? default`；③ opaque 跨界 id 一律 Branded；④ wire 逐字对齐参考，
> 分叉显式记录（本文件每处标 `DIV`）；⑤ 依赖引入遵循 D-054 清单（portable-pty 0.8 /
> globset / ignore / jiff / which / sysinfo / nix / windows-sys，均已缓存离线可用）；
> ⑥ 后台 = jobs 职责，进程句柄由工具包桥接成 `JobHooks`，jobs 不认识 subprocess。

---

## 1. 总览：crate 划分与依赖图

```
新增 crate（workspace members 追加）：
  crates/dsh-subprocess    底层执行原语（std::process + 平台树终止 + 有界收集/spill）
  crates/dsh-sandbox       沙箱策略缝（模式/升级/roots/denial marker/会话事件）
  crates/dsh-fs           文件系统能力缝（本地 provider + observation policy + tool-fs）
  crates/dsh-shell         shell 能力缝（request/spec 分裂 + bash 后端 + tool-bash）
  crates/dsh-terminal      终端会话注册表（PTY over subprocess-spawnTerminal + 6 tool）
  crates/dsh-code-runtime  code 执行缝（python 子进程后端真实 + TS worker 桩）

依赖方向（自底向上）：
  dsh-subprocess ← dsh-fs(search)、dsh-shell、dsh-terminal(spawnTerminal)、dsh-code-runtime(python)
  dsh-sandbox    ← dsh-shell、dsh-fs(sandboxed)、dsh-tools(escalation 审批)、dsh-session(sandbox/mode 事件)
  dsh-fs         ← dsh-session(事件词表)、dsh-tools(工具注册/输出 schema)、globset+ignore(搜索)
  dsh-shell      ← dsh-subprocess、dsh-sandbox、dsh-session(标签工具日志)
  dsh-terminal   ← dsh-subprocess(spawnTerminal)、dsh-jobs(pty-send 后台)、dsh-sandbox(PTY 活锁模式)
  dsh-code-runtime ← dsh-subprocess(python 子进程)、dsh-tools(run_code 真实接线)
  宿主接线 dsh-cli::web.rs：M4HostServices 扩展 + register_m5_tools_with_host
```

**外部依赖（D-054 已核离线）**：`portable-pty 0.8`（terminal/spawnTerminal）、`globset 0.4` +
`ignore 0.4`（fs 搜索）、`jiff 0.2 + jiff-tzdb`（P2 IANA 时区）、`which 6`（裸名可执行查找）、
`sysinfo 0.38`（进程存活探针）、`nix 0.30`（POSIX 进程组/信号）、`windows-sys 0.5x`（Windows
taskkill/token 平台面，按需 feature）。serde/serde_json 已有。

---

## 2. dsh-subprocess —— 执行世界最底层原语

### 2.1 缝：Service Definition 形状

```rust
/// 执行世界可执行查找 + 托管进程树 + 唯一终端原语。
pub trait SubprocessRuntime {
    /// 解析裸命令名为绝对/相对可执行路径（用 scrubbed PATH，含分隔符相对路径拒绝）。
    fn resolve_executable(&self, cmd: &str, env: Option<&Environment>, signal: Option<&CancellationToken>) -> Result<PathBuf, ProcessError>;
    /// spawn 一个非交互进程（零默认 spec）。
    fn spawn(&self, spec: SubprocessSpawnSpec) -> Result<SubprocessHandle, ProcessError>;
    /// spawn 一个终端进程（唯一 PTY 原语）。
    fn spawn_terminal(&self, spec: SubprocessTerminalSpec) -> Result<PtyHandle, ProcessError>;
    /// scrubbedParentEnv：父环境 − credential-shaped(/KEY|PASSWORD|SECRET|TOKEN/i) − 所有 DSH_*。
    fn scrubbed_parent_env(&self) -> Environment;
    /// dispose：终止全部存活进程并 await。
    fn dispose(&self);
}
```

### 2.2 `SubprocessSpawnSpec`（零默认——TS `SubprocessSpawnSpec` 逐字）

```rust
pub struct SubprocessSpawnSpec {
    pub argv: Vec<String>,          // 精确 argv（argv[0] 为可执行）
    pub cwd: PathBuf,               // 显式工作目录
    pub stdio: ChildStdio,          // 三态（见下）
    pub grace_ms: u64,              // SIGTERM→SIGKILL 宽限（≤ MAX_TIMER_DELAY_MS）
    pub signal: Option<CancellationToken>, // 取消 → terminate()
    pub env: Option<Environment>,   // 显式环境（缺省父亲 scrubbed）
}

pub enum ChildStdio {
    Stdin { mode: StdinMode },
    Stdout { mode: StdoutMode },
    Stderr { mode: StdoutMode },   // 复用 stdout 模式（inherit/pipe/collect）
}
pub enum StdinMode { Ignore, Pipe, WriteBytes(Vec<u8>) }   // 'ignore'|'pipe'|{data}
pub enum StdoutMode {
    Pipe,                            // 透传管道
    Inherit,                         // 继承宿主
    Collect(SubprocessCollect),      // 有界收集 + 可选 spill
}
pub struct SubprocessCollect {
    pub max_bytes: usize,
    pub spill: Option<SubprocessSpill>,  // 缺 spill = 只留内存 tail（诊断形）
}                                       // 带 spill = 可恢复完整流（bash 形）
pub struct SubprocessSpill { pub max_bytes: u64, pub dir: PathBuf /*0700 目录*/ }
```

### 2.3 句柄与收集

```rust
pub struct SubprocessHandle {
    pub pid: u32,                       // spawn 失败 = 0（TS 用 -1 语义，映射为 0）
    pub stdin: Option<Stdio>,           // pipe 时
    pub stdout: Option<PipeReader>,     // pipe/collect 时（offset-based 非消费 reader）
    pub stderr: Option<PipeReader>,     // 同上
    pub collected: Option<CollectedOutput>, // collect 时
    pub done: ProcessDone,              // settle 一次从不 reject；spawn 失败 settle 成 killed+stderr 带错
    pub started: Instant,
}
/// offset-based 整流 byte 坐标 reader：read_from(0)=batch 结果；lossy + spill_path。
pub struct CollectedOutput {
    pub read_from(&self, offset: u64) -> String,   // 非消费
    pub lossy: bool,
    pub spill_path: Option<PathBuf>,
}
pub type ProcessDone = Arc<dyn Fn() -> Outcome + Send + Sync>;
```

### 2.4 终止动词（唯一）

`terminate()`：**SIGTERM → grace → SIGKILL**，全平台**树级**（POSIX 进程组 / Windows
`taskkill /PID <pid> /T /F`），幂等；`wait_for_exit` 树级存活；pid+started 防复用
（process-inspector 校验）。`SubprocessTerminalSignal = SIGINT|SIGTERM|SIGKILL|SIGTSTP|SIGHUP`。

### 2.5 平台策略（cfg 门）

- **unix**：`nix` 进程组 `setpgid`/`killpg`；spill 0700 per-process；liveness 轮询 std。
- **windows**：spawn `cmd /c` 树托管 + `taskkill /T /F`；PID 复用防护用 `sysinfo`/`OpenProcess`。
- 平台 FFI 差异以 `#[cfg(target_os = ...)]` + 单测隔离的模块封装，Design 只定接口。

### 2.6 TDD 计划（红/绿/重构）

- 红→绿：spawn 真实 argv 回显；cwd 生效；stdin WriteBytes → 子进程 read；Collect max_bytes 截断 +
  spill 落盘；zero-default（漏填字段编译期或运行期即错）；`terminate` 树级（起一个会重启子进程的
  脚本，kill 后整树结束）；scrubbed env（KEY/SECRET 清除、DSH_* 清除）；spawn 失败 done settle 成
  killed+stderr。
- 重构：公共的 `terminate`/收集抽样抽成内部模块；平台格外提。
- 断言镜象 TS：`subprocess/tests/{spawn,terminal,process-inspector,windows-inspector}.spec.ts`。

---

## 3. dsh-sandbox —— 沙箱策略缝

### 3.1 核心类型（逐字 TS `sandbox/src/index.ts` + `escalation.ts`）

```rust
#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxMode { ReadOnly, WorkspaceWrite, DangerFullAccess }  // 'read-only'|'workspace-write'|'danger-full-access'

pub const ESCALATION_TARGETS: [SandboxMode; 2] = [SandboxMode::WorkspaceWrite, SandboxMode::DangerFullAccess];
pub fn wider_modes(mode: SandboxMode) -> &'static [SandboxMode] {
    match mode {
        SandboxMode::ReadOnly          => &[WorkspaceWrite, DangerFullAccess],
        SandboxMode::WorkspaceWrite    => &[DangerFullAccess],
        SandboxMode::DangerFullAccess  => &[],
    }
}  // 严格更宽阶梯；execution 时校验——只允许「严格更宽」target。

pub fn validate_escalation_args(sandbox_permissions: Option<&str>, justification: Option<&str>) -> Result<(), EscalationError> {
    // 同现（都缺或都有）+ justification 非空句子；否则 Err（fail-closed）。
}

pub fn sandbox_denial_marker(mode: SandboxMode) -> String {
    format!("[sandbox: file access denied under {mode} mode]")
}
```

### 3.2 `writableRoots(policy)`（逐字 `roots.ts`）

```rust
pub fn writable_roots(policy: &SandboxExecutionPolicy) -> Vec<PathBuf> {
    // [canonical(workspaceRoot), "/tmp", os::temp_dir()] 逐个 canonical + 去重保留序
}
/// read-only → []；workspace-write → writable_roots；danger-full-access → 直通。
/// 供 fs-sandbox 进程内围栏与（未来）平台 runner 共享的唯一来源。
```

### 3.3 策略解析优先级（fail-closed）

1. **approved 显式 mode**（调用者经审批提供的 escalation mode）> 2. **会话 `sandbox/mode` 事件
   （有效模式）** > 3. **部署默认 = read-only**。
`SandboxPolicy = { mode, workspace_root: Option<PathBuf> }`。
- `sandbox/mode` 会话事件：**log-only**（同 `approval/*`），payload `{ mode, source?: 'delegation' }`；
  最后一跳为 `effectiveSandboxMode`；事件词表**已预留**（dsh-session::EventKind::SandboxMode，
  映射 "sandbox/mode" 已存在，无需扩枚举）。
- 系统提示注入：`sandbox:policy` 段（order 110）输出当前模式 + 升级阶梯提示（对齐 reference）。

### 3.4 escalation 批准通道（复用 dsh-tools ApprovalProvider）

M4 已有 `ApprovalOutcome{AllowedOnce/Rejected/Cancelled/Unavailable}` + `ToolPreDecision`——
M5 escalation 的 `EscalationAsk` 落为：`sandbox_permissions` + `justification` 触发
`resolve_approval`（ApprovalProvider 回调；缺省无通道 ask 即 reject，fail-closed）。二者解耦：
sandbox 只定义 ask 数据；放不放行由既有的 approval 缝裁决。

### 3.5 TDD 计划

- 模式 kebab 序列化/反序列化 + `wider_modes` 严格更宽单向；`read-only` 永不可为 target。
- `validate_escalation_args`：缺一/空 justification/reject；同现且非空 → 放行。
- `writable_roots`：workspace-write → [canonical(root), /tmp, tmpdir] 去重；read-only → []。
- 解析优先级：无模式 → 默认 read-only；有会话事件 → 用它；再被 approved 覆盖。
- `sandbox/mode` 事件落会话 + fold 最后一跳 + log-only（不进 transcript）。
- 系统提示段内容快照。镜象 TS `escalation.spec.ts`/`roots.spec.ts`/`policy.spec.ts`。

---

## 4. dsh-fs —— 文件系统能力缝

### 4.1 缝（`ctx.fs: FileSystem` 抽象类，12 方法，逐字 TS `fs/types.ts`）

```rust
pub trait FileSystem {
    fn resolve(&self, path: &str, opts: ResolveOptions) -> Result<FsTarget, FsError>;
    fn stat(&self, target: &FsTarget) -> Result<Option<FsInfo>, FsError>;          // 高分辨率 identity + freshness
    fn lstat(&self, path: &Path) -> Result<Option<FsPathInfo>, FsError>;           // 不 follow symlink
    fn read_text(&self, target: &FsTarget, opts: ReadTextOptions) -> Result<String, FsError>;
    fn stream_text(&self, target: &FsTarget, opts: StreamTextOptions) -> Result<TextStreamHandle, FsError>;
    fn write_text(&self, target: &FsTarget, content: &str, intent: Option<FsWriteIntent>, opts: WriteOptions) -> Result<FsWriteOutcome, FsError>;
    fn edit_text(&self, target: &FsTarget, request: &FsEditRequest, intent: Option<FsWriteIntent>, opts: EditOptions) -> Result<FsEditOutcome, FsError>;
    fn list_dir(&self, target: &FsTarget) -> Result<Vec<FsDirEntry>, FsError>;
    fn read_image(&self, target: &FsTarget, opts: ReadImageOptions) -> Result<Vec<u8>, FsError>;  // 条件能力
    fn read_bytes(&self, target: &FsTarget, offset: u64, len: u64) -> Result<Vec<u8>, FsError>;
    fn get_size(&self, target: &FsTarget) -> Result<Option<u64>, FsError>;
    fn delete(&self, target: &FsTarget) -> Result<(), FsError>;
}
```

- **不透明 Branded**：`FsTargetKey` / `FsVersion`（`dsh-brand` 同源：`s!` newtype 或 re-export）。
- **FsErrorCode**（13，逐字）：`FS_NOT_FOUND|FS_NOT_DIRECTORY|FS_NOT_TEXT|FS_NOT_REGULAR_FILE|
  FS_TOO_LARGE|FS_PERMISSION_DENIED|FS_SANDBOX_DENIED|FS_IO_ERROR|FS_STALE_VERSION|
  FS_NOT_OBSERVED|FS_AMBIGUOUS_EDIT|FS_EDIT_NOT_FOUND|FS_ABORTED`。`FsError = { message, code, cause? }`
  （镜像 `HarnessError`）。
- **FsWriteIntent**：`createIfAbsent`（已存在 → FS_NOT_OBSERVED）| `replaceIfVersion{version}`
  （缺/版本不匹配 → FS_STALE_VERSION）；省略 = 无条件 create-or-overwrite（非第三分支）。
- **FsObservation**：`present{version}` | `absent`。

### 4.2 fs-local provider

- `std::fs` + 原子写（同目录 temp + rename，D-039 已有 `dsh-persistence::atomic_write` 可复用）。
- read 常量：`READ_LIMIT=2000` 行、`README_MAX_LINE_LENGTH=2000` 字符/行、`READ_MAX_BYTES=50KiB`、
  `STREAM_MIN_SIZE=10MiB`（超阈值走流式/服务线程）。
- mutation 前 **re-canonicalize** 收窄 TOCTOU；per-targetKey FIFO lock 串行化 read→guard→write。

### 4.3 fs-observation-policy（事件型，无服务）

- `WeakMap<owner, Map<FsTargetKey, FsObservation>>` 决策 read-before-edit + version CAS。
- 事件（waterfall 单槽）：`fs/write-intent` / `fs/edit-intent`（首个 return 者胜，next()=无条件）；
  `fs/observed`（emit 同步 recorder）。
- 写路径**无独立 stat**：靠 intent 事件（无条件或策略）。

### 4.4 fs-sandbox（进程内围栏，加在 writeText/editText 仅此两处）

`checked_target(target, sandbox_policy)`：danger → 直通；read-only → `FS_SANDBOX_DENIED`；
workspace-write → `is_path_under(writable_roots)` + 新鲜 resolve 防 TOCTOU；不落则
`FS_SANDBOX_DENIED` + escalation hint。读路径全放行（mutation 才围栏）。

### 4.5 tool-fs（model-facing，4 工具 + 搜索工具）

- `read(file_path, offset?, limit?)`：渲染截断加 `[output truncated; full output: <spillPath>]`。
- `write(file_path, content)`：输出值 `{path, operation:'create'|'update', before:string|null,
  after}`；模型面 `formatWriteOutput` 信封（`<path>/<type>/<content>` + `{verb} file`）——
  **DIV**：参考 render 是 `<path>…</path>\n<type>file</type>\n<content>\nCreated file\n</content>`。
- `edit(file_path, old_string, new_string, replace_all?)`：FS_AMBIGUOUS_EDIT（多匹配且非 replace_all）、
  FS_EDIT_NOT_FOUND（无匹配）、FS_STALE_VERSION → 「re-read the file, then retry」、FS_NOT_OBSERVED →
  「read the file, then retry」。
- `str_replace_editor`（view/create/str_replace 唯一 old_str/insert；`maxOutputChars=16_000`，
  `<response clipped>…`）。
- **glob/grep（D-054 可选落地）**：用 `globset 0.4` + `ignore 0.4`（ripgrep 同源引擎）实现
  `glob(pattern, path?)` / `grep(pattern, path?, include?)`；`GLOB_MAX_RESULTS=100`、
  `GREP_MAX_MATCHES=250`、`SEARCH_TIMEOUT_MS=30_000`；走 `ctx.subprocess`（打包引擎进程，
  非 model 可见 job）或进程内线程（单线程纪律下用服务层线程 + mpsc）。
- `read_image` 条件注册（有 attachments 能力才注册）。
- confining 后端下 write/edit 追加 `sandbox_permissions`(enum=escalation modes) + `justification`。
- 错误映射：FS_SANDBOX_DENIED → `[sandbox: <mode> mode]` + escalation hint（保留 code）。

### 4.6 TDD 计划

- provider：resolve/stat/read/write-atomic/edit-parse/list_dir；read 行数/字节上限 → FS_TOO_LARGE。
- policy：write-intent 决策（present→replaceIfVersion / absent→createIfAbsent）；edit stale→FS_STALE_VERSION；
  read-before-edit 未观察 → FS_NOT_OBSERVED；`fs/observed` 同步 recorder。
- sandbox：3 模式围栏透传/拒绝；writable_roots 逐条 is_path_under。
- tool：write 输出信封 + before/after；edit 三 error code；str_replace 唯一性 + clipped。
- 镜象 TS `fs/tests/service.spec.ts`、`fs-observation-policy/tests/policy.spec.ts`、
  `fs-sandbox/tests/*.spec.ts`、`tool-fs` 各。

---

## 5. dsh-shell —— shell 能力缝

### 5.1 缝（逐字 `shell/types.ts` + `index.ts`）

```rust
pub trait ShellExecutor {
    fn resolve(&self, request: &ShellExecRequest) -> ShellExecSpec;  // defaulting/capping 显式在此步
    fn run(&self, spec: &ShellExecSpec, signal: Option<&CancellationToken>) -> ShellRunResult;
    fn start(&self, spec: &ShellExecSpec) -> ShellProcess;           // 后台：无超时（spec.timeoutMs 被忽略）
}

pub struct ShellExecRequest<'a> {
    pub command: &'a str,
    pub workdir: Option<PathBuf>,        // 缺省 → 实现配置
    pub timeout_ms: Option<u64>,         // 缺省 → 实现默认；实现 cap
    pub stdout_max_bytes: Option<u64>,   // 前台 stdout 预算
    pub signal: Option<CancellationToken>,
    pub stdin: Option<String>,           // 写入后关闭
    pub env: Option<Environment>,        // credential-scrub 之后合入
    pub dsh_env: Option<DshEnvironment>, // 托管 DSH_*；最后合入且不可被 env 顶掉
    pub sandbox_policy: Option<SandboxPolicy>,
}
pub struct ShellExecSpec {               // resolve() 已填齐默认 + cap
    pub command: String,
    pub workdir: PathBuf,
    pub timeout_ms: u64,
    pub stdout_max_bytes: u64,
    pub signal: Option<CancellationToken>,
    pub stdin: Option<String>,
    pub env: Option<Environment>,
    pub dsh_env: Option<DshEnvironment>,
    pub sandbox_policy: Option<SandboxPolicy>,   // 可空（非 confining 后端忽略）
}
pub struct ShellRunResult {
    pub exit_code: Option<i32>,   // null = 信号死
    pub signal: Option<String>,   // e.g. "SIGTERM"
    pub timed_out: bool,          // 互斥 aborted：单一 fused deadline 先到者
    pub aborted: bool,
    pub timeout_ms: u64,          // 生效值（default/cap 后）
    pub stdout: CollectedOutput,
    pub stderr: CollectedOutput,
    pub sandbox: Option<ShellSandboxInfo>,  // {mode, denied, enforcement?, runner_failed?}
}
pub struct ShellProcess {
    pub status: ShellProcessStatus,   // running|completed|killed
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
    pub done: ProcessDone,            // settle 一次，从不 reject；spawn 失败 → killed+stderr 带错
    pub sandbox: Option<ShellSandboxInfo>,
    pub read_output(&mut self) -> ShellProcessRead,  // 增量消费（连续读不重复；lossy + spill_path）
    pub kill(&mut self) -> bool,      // 杀进程组；already-finished → false；幂等
}
```

- **request/spec 分裂是模板**（参考 `index.ts`：`abstract resolve(request): ShellExecSpec`；
  `run(spec)`/`start(spec)` 只收 spec，绝不收 raw request）。

### 5.2 bash-local 后端（`SHELL_SETTINGS_NAMESPACE` + 默认值）

- `bash -c <command>` 托管进程组经 `ctx.subprocess`。
- 默认：`timeoutMs=120_000`、`maxTimeoutMs=600_000`、`maxOutputBytes=64_000`、`maxSpillBytes=64MiB`、
  `graceMs=3_000`。
- `ENV_OVERRIDES = { NO_COLOR:'1', TERM:'dumb', PAGER:'cat', GIT_PAGER:'cat' }`（最先合入，信任方覆盖仍胜）。
- **会话 cwd 解析**：bear 场 `workdir ?? session.header.cwd ?? configuredRoot`（对齐参考 + DSH 插件
  经验文档「必须传递 sandboxPolicy/workspaceRoot，否则回退主目录」）。
- 后台输出 stdout+stderr 合并成 model-facing delta。

### 5.3 tool-bash（参数逐字，`DIV` 标注）

参数 schema：`command`(req)、`description`(req)、`timeoutMs`(camelCase，可选)、`workdir`(可选)、
`run_in_background`(可选 bool)、`sandbox_permissions`(可选 enum)、`justification`(可选)。
**DIV（P4 裁定）**：本 harness 既有 wire 用 `timeout_ms`（snake，见 `dsh-tools/src/m4.rs:321` 的
`job_output` schema）；M5 新工具一律按参考 camelCase `timeoutMs`，与既有 snake 分叉显式记录
（D 条目），避免模型学岔。

- 校验：`timeoutMs` 必须正有限数（否则 `invalid timeoutMs: expected a positive number, got …`）；
  escalate 两字段同现非空（`validate_escalation_args`）。
- 结果标记词汇（模型可见固定字符串，逐字）：
  - 正文 = stdout（截断追加 `[output truncated; full output: <spillPath>]`）
  - stderr 段 → `[stderr]\n…`
  - 空输出 → `(no output)`
  - sandbox denied → `[sandbox: file access denied under <mode> mode]`（+ escalation hint，保留 code）
  - 超时 → `[timed out after <timeoutMs>ms]`
  - 信号 → `[killed by signal: <sig>]`
  - 非零退出 → `[exit code: N]`（**报告而非 isError**；仅 spawn 错/abort 才 isError）。
- 后台 = `ctx.shell.start` 包成 `JobHooks{cancel, done, readOutput}` 交 `ctx.jobs.start({kind:'bash',…})`
  （producer 由工具包自桥，jobs 不认识 subprocess）。

### 5.4 TDD 计划

- resolve 默认+cap；timeoutMs cap 到 maxTimeoutMs；非正拒绝。
- run 真实 bash 回显；退出码/标记；超时 fused deadline（timedOut 且 !aborted）；abort → aborted 且
  !timedOut；stdin 写入后关闭；ENV_OVERRIDES 生效。
- start + readOutput 增量非重复 + lossy/spill；kill 幂等；done 永 reject；spawn 失败 killed。
- tool-bash：schema 逐字（含 camelCase）、标记词汇快照、后台经 JobRegistry（fake-loop 集成）。
- 镜象 TS `shell-bash/tests/*`、`tool-bash/tests/integration.spec.ts`。

---

## 6. dsh-terminal —— 终端会话注册表

### 6.1 缝与注册表（逐字 `terminal/*`）

```rust
pub type TerminalSessionId = Branded<TerminalSessionIdTag>;   // dsh-brand

pub struct TerminalSendRequest<'a> {
    pub text: &'a str,
    pub submit: bool,
    pub signal: Option<TerminalSignal>,
}
pub struct TerminalSendResult {
    pub viewport: String,
    pub wait_reason: TerminalWaitReason,   // stdin_read|inferred_idle|timeout|session_exit
    pub session_status: TerminalSessionStatus,
    pub truncated: bool,
}
pub struct TerminalSessionService {
    /// Branded 会话表；owner = 精确 Agent（非 session id）；每 session 仅一个 active send。
    pub open(&mut self, owner: &str, backend: BackendSpec, cfg: TerminalConfig) -> Result<TerminalSessionId, TerminalError>;
    pub send(&mut self, id, req) -> Result<TerminalSendResult, TerminalError>;   // SEND_ACTIVE 若忙
    pub read(&mut self, id) -> Result<String, TerminalError>;                    // 滚动缓冲 + maxReadBytes
    pub signal(&mut self, id, sig) -> Result<(), TerminalError>;
    pub close(&mut self, id) -> Result<(), TerminalError>;
    pub list(&self) -> Vec<TerminalSessionView>;
}
pub enum TerminalErrorCode {
    DuplicateBackend, DuplicateName, ForeignSession, NoBackend,
    NoSession, OwnerNotLive, SendActive, ServiceDisposing,
}
pub enum TerminalSignal { Sigint, Sigterm, Sigkill, Sigstp, Sighup }  // 同 SubprocessTerminalSignal
```

### 6.2 terminal-bash 后端（P1 裁定：portable-pty 已装 → 真实实现）

- 前台：`subprocess.spawn_terminal` → `portable_pty::PtyPair`；slave spawn bash；master 读滚动缓冲。
- `sandbox/mode` 变更锁：**PTY 存活期间锁定**（改模式 → 拒绝，建议先 close）。
- 后台 `pty-send` 经 `ctx.jobs.start({kind:'pty-send',…})`。
- 配置：`rows=40`、`cols=160`、`scrollback_lines=10_000`、`scrollback_max_bytes=4MiB`、
  `max_read_bytes=256KiB`、`poll_interval_ms=50`、`idle_silence_ms=3_000`（→ inferred_idle）、
  `timeout_ms=30_000`。
- 6 工具：`terminal_open` / `terminal_send` / `terminal_read` / `terminal_signal` / `terminal_close` /
  `terminal_list`（schema 对齐参考 tool-terminal）。

### 6.3 TDD 计划

- registry：open/send/read/signal/close/list + owner 授权（ForeignSession）+ SEND_ACTIVE + 崩溃回滚。
- backend：真实 bash PTY 会话（echo/read）；滚动缓冲上限；inferred_idle 判定；模式锁定。
- tool：6 工具 schema 快照 + 集成（fake-loop 端到端 open→send→read→close）。
- 镜象 TS `terminal-backend/tests/*`、`terminal/tests/*`。

---

## 7. dsh-code-runtime —— code 执行缝

### 7.1 缝（逐字 `code-runtime/types`）

```rust
pub trait CodeRuntime {
    pub language: CodeLanguage;         // 'typescript' | 'python'
    pub isolation: Isolation;           // 'worker-thread' | 'process'
    fn run(&self, request: &CodeRunRequest, signal: Option<&CancellationToken>) -> CodeRunResult;
}
pub struct CodeRunRequest<'a> {
    pub program: &'a str,               // async 函数体
    pub bindings: Vec<CodeBindingNamespace>,  // {global: string, functions: [...], error_class?: string}
    pub signal: Option<CancellationToken>,
}
pub struct CodeRunResult {
    pub value: Option<Value>,           // lossless-JSON 值
    pub logs: Vec<String>,
    pub error: Option<CodeRunFailure>,  // 失败是结果字段，不是 run() 的 reject
}
pub enum CodeRunFailureKind { Exception, Timeout, Abort, WorkerExit, InvalidOutput, OutputLimit }
pub struct CodeRunFailure { pub kind: CodeRunFailureKind, pub message: String, pub detail: Option<String> }
```

### 7.2 可移植契约常量（逐字）

- `RESERVED_BINDING_GLOBALS = { console, __dsh_main__, __builtins__, __name__, __debug__ }`
- `RESERVED_ERROR_MEMBERS = { name, message, stack, args, with_traceback, add_note }`
- `DUNDER_MEMBER`（`__x__` 校验）、`PORTABLE_RESERVED_WORDS`（ECMAScript ∪ Python 保留字并集）。

### 7.3 python 后端（真实；P1/P2 无依赖障碍）

- **fresh `python3` 子进程**（`D:\Anaconda\python.exe` 可定位）+ `PROTOCOL_FD=3` JSON-lines 帧
  （stdout/stderr 留给用户日志）。
- Host 视入站帧为**恶意**：`validate_child_frame` 校验字段 + **REBUILD**（不信任子进程结构）；
  lossless JSON（`checkDoneValue` 语义：over-budget / non-lossless 分类；整数精确跨跨界）。
- 帧：Boot / Run / BootAck / Call / Log / Done；`WIRE_FRAME_FIELDS` 逐字。
- 子进程资源：`RLIMIT_CPU`/`RLIMIT_AS`（unix，cfg 门）或 OS 等价 cap；超时/中止 → 树级
  `terminate()`。
- **lossless-JSON 跨界的 Rust 端**：serde_json `Number` 保持；检查非有限/-0 → `invalid-output`。

### 7.4 run_code 工具 + TS worker 桩

- `run_code`：参数 `code`(req) + `description`(req)；程序 `await tools.name(args)` 经**嵌套
  execution** 分发 registry 真实 tool（`CodeDispatchLog`，`<parent>:code:<n>` 确定性 id）；
  run_code 从不暴露给程序自身（无递归）。mode=code 时 SDK/系统提示段声明其余工具。
- **TS worker-thread 后端 → 诚实桩**（Rust 无 JS 引擎）：`run` 恒 `CodeRunFailure{kind:WorkerExit,
  message:"requires a code runtime"}`（替换 M4 的 `placeholder_run_code` 占位，但错误语义保留）；
  `peek_runtime` 语言回退保留（无 runtime 回退 TS）。

### 7.5 TDD 计划

- seam 契约：绑定额外/保留名/错误常量。
- python 后端：fresh 子进程 echo；用户日志捕获；返回值 lossless（含 2^53/2^60）；异常 → Exception；
  超时 → Timeout；abort → Abort；恶意帧被拒（伪造 fd3/非 JSON → invalid-output/worker-exit）。
- run_code 工具：code/description 必填；`tools.*` 嵌套派发真实工具（fake-loop 集成）；无递归。
- 镜象 TS `code-runtime-python/tests/*`、`tool-run-code` 集成。

---

## 8. 宿主接线（dsh-cli::web.rs）

- `M4HostServices` 扩展为 `M5HostServices`（或新增字段，保持向后兼容）：
  `subprocess: Option<Rc<SubprocessRuntime>>`、`sandbox: Option<Rc<SandboxPolicyService>>`、
  `fs: Option<Rc<FileSystemService>>`、`shell: Option<Rc<ShellExecutor>>`、
  `terminal: Option<Rc<TerminalSessionService>>`、`code_runtime: Option<Rc<CodeRuntime>>`。
- `register_m5_tools_with_host(registry, host)`：fs 4 工具 + glob/grep + str_replace_editor + bash +
  terminal 6 + run_code；每工具 `bind(executor)` + `Option<Rc>` 无 handle → 注册自包含定义
  （校验-only / NOT_BOUND / UNSUPPORTED 诚实，绝不伪装）——复用 `register_m4_tools_with_host` 模式。
- **sandbox/mode 会话事件投影**：`SessionHost` 已有事件日志；`effectiveSandboxMode` fold 直接复用
  dsh-session（如 M4 `ScheduleHost::fold` 同款）；`sandbox:policy` 系统提示段由系统提示装配补入。
- **M5g schedule 定时推进**：宿主起**服务层线程 tick**（1s 或配置间隔）→ `mpsc` 桥 → 主线程
  `ScheduleHost::dispatch_due(now_epoch)`（复用 `set_timer_clock`-style 注入 + Hmr 桥样板）；
  回调转 `job_due`/事件。定时器属服务层线程，核心不动。
- **jobs subprocess producer**：dsh-fs search / tool-bash 后台 / terminal `pty-send` 经
  `JobRegistry.start(StartSpec{ producer })`；producer 把进程句柄桥成 `ProducerHooks{on_cancel,
  read_output}`（D-049 形状已闭合，M5 只补 bridge 实现）。
- 全仓 `cargo test --offline --workspace` 全绿 + clippy `-D warnings`。

---

## 9. 实现顺序与每步验收（TDD 红→绿）

| 步 | crate | 关键验收（对应 M5-REQUIREMENTS §5 验收标准） |
|---|---|---|
| 1 | dsh-subprocess | spawn/collect/terminate/scrub（2） |
| 2 | dsh-sandbox | 模式/阶梯/审批/roots/事件（3） |
| 3 | dsh-fs | provider/policy/sandbox-fence/tool-fs（4）+ glob/grep（可选 4a） |
| 4 | dsh-shell | resolve/run/start + tool-bash + ENV_OVERRIDES + 标记（5 + 2a guard 真抢占） |
| 5 | dsh-terminal | registry + PTY backend + 6 tool（P1 已装 → 8 全真） |
| 6 | dsh-code-runtime | python 后端真实 + run_code 接线 + TS 桩（6） |
| 7 | web.rs 接线 | M5HostServices + register_m5_tools + sandbox/mode 投影 + 定时 tick + producer（7/9） |
| 8 | M5-ACCEPTANCE | 全绿 + clippy + DECISIONS 对应条目 + git 互查（10） |

> 每步走完即 `cargo test -p <crate>` 全绿 + clippy 零告警 + git 提交（提交信息引 DECISIONS 条目）；
> 逐步累积，绝不让测试长期红。

---

## 10. 未决/分叉清单（DIV）

| # | 项 | 参考 | 本设计取舍 |
|---|---|---|---|
| DIV-1 | bash/其他工具 timeout 参数名 | `timeoutMs`（camel） | 采纳 camel `timeoutMs`；既有 m4 job_output 的 `timeout_ms` snake 保持并记 D 条目分叉（P4） |
| DIV-2 | 平台沙箱 runner（Seatbelt/bwrap/Landlock/ACL） | TS 有 sandbox-local/windows-acl runner | Rust 不做内核 FFI；argv confiner 留 seam + fail-closed，真实边界 = fs 进程内围栏（P3(a)） |
| DIV-3 | TS worker-thread code 后端 | 真实 worker | Rust 无 JS 引擎 → 诚实桩（WorkerExit requires-a-runtime），python 后端真实落地（P1 已装） |
| DIV-4 | e2b / lsp / out-of-process provider | 参考有独立包 | 登记 + 诚实 unavailable，provider 归 M6（P5） |
| DIV-5 | schedule 定时器 | TS 宿主事件循环 | Rust 服务层线程 tick + mpsc 桥（单线程核心不动） |
| DIV-6 | IANA 全时区 | chrono/jiff 现代替代 | P2(a) jiff+jiff-tzdb（已提取离线验证） |
| DIV-7 | fs-search 引擎 | ripgrep 二进制 spawn | globset+ignore（ripgrep 同源 crate）进程内实现，避免二进制依赖 |

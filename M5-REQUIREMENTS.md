# M5 执行引擎与沙箱：需求结论文档（阶段一：需求分析）

> 本文件是 `PLAN-rust-full-harness-migration.md` §6「M5 范围」的实现工件，按瀑布流
> **阶段一（需求分析）** 产出 目标/非目标/假设/约束/边界/验收标准；**不下手写实现**。
> 契约事实来自逐行阅读参考源码（`packages/{fs,shell,subprocess,terminal,sandbox,
> sandbox-policy,code-runtime,e2b,lsp}/` 全部真实 `src`）与两份语义提炼报告
> （M5 执行契约 / M5 宿主侧遗留项），错误 code/字段名/wire 形状逐字对齐并记录差异。
> 决策编号待本文件通过验收后补记 `DECISIONS.md`，设计阶段（部分二）在需求验收后追加。

---

## 第一部分：需求分析（第一性原理 + 双视角）

### 1. 根本目标

M5 的目标：把 harness 从「能聊天/能编排」补成**能真实执行**——让模型在受控沙箱里能：
读/写/编辑工作区文件（fs）、跑 shell 命令（shell/subprocess）、打开持久终端会话
（terminal）、在沙箱边界内执行它自己写的程序（code-runtime）——并且这些执行被
**同一个三档 sandbox 策略 + 严格更宽升级阶梯 + 人的最终审批**统一约束，绝不绕过边界。

对应 `PLAN-rust-full-harness-migration.md:217`：

> | **M5** | fs + shell + subprocess + terminal + sandbox + e2b + code-runtime + lsp | 执行引擎与沙箱（服务层线程/进程隔离） |

本次需求分析把该范围**按第一性原理拆成「可单线程实做 / 须服务层线程/进程、真实代价 / 环境不可行、诚实桩」三档**（见 §2、§5 非目标、§6 决策），避免把里程碑当成「什么都要做完」的无边界泥潭，也不因环境问题偷偷降级架构（方法论四：环境问题是临时阻碍，不是改变设计决策的理由——真实能力必须能在一个有真实 OS 的环境里跑，只是本机编译/联调环境受限时用测试代偿）。

### 2. 第一性原理分解

M5 的能力面剥到不可再分的基础事实：

1. **subprocess 是执行世界的最底层原语**。fs/shell/terminal/code-runtime 全都建立在
   「spawn 一个精确 argv 的进程、给它显式 cwd/stdio/env、有界收集输出、树级终止、
   清洗环境」之上（参考 `dsh-subprocess` 是唯一「执行世界可执行查找 + 托管进程树 +
   一个终端进程原语」的包）。Rust `std::process::Command` 天然即是该原语；树级终止
   （POSIX 进程组 / Windows `taskkill /T`）与有界收集 + spill 是唯一要补的平台逻辑。
   → **先造 subprocess，一切执行能力都从它长出**。
2. **sandbox 的本质不是「通用隔离层」，而是 3 个文件效应模式的 allow-list + 严格更宽
   升级阶梯 + 人在执行前的最终批准**。参考（`dsh-sandbox`）**没有** insecure/adhoc/
   authenticated/wasm 这些模式；真实唯一词汇就是 `read-only / workspace-write /
   danger-full-access`（`SandboxMode`），语义是「这个调用允许触碰哪些文件效应」。
   escalation 只有两档 target（`workspace-write`、`danger-full-access`），且**只在
   execution 时校验严格更宽**；审批走结构化的 `EscalationAsk`（fail-closed：审批在
   一切执行之前）。→ sandbox 层 = 「策略解析 + 会话 `sandbox/mode` 事件 + 系统提示
   注入 + escalation 审批 + denial marker」这**一整段纯逻辑**全可进 Rust 单线程核心，
   与平台 runner（Seatbelt/bwrap/Landlock/Windows ACL）解耦。
3. **fs 是「不经过 shell 的文件访问」，双层守卫正交**：(a) presenter 后端（fs-sandbox）
   在 mutation 上做进程内围栏（canonicalize-then-contain + `writableRoots`）；(b)
   观察状态策略（fs-observation-policy）靠 `fs/*` 事件做 read-before-edit/CAS 版本
   守卫。两层独立——Rust 需分别实现（provider 内 `FS_SANDBOX_DENIED`，事件/策略层
   `FS_STALE_VERSION`/`FS_NOT_OBSERVED`）。本地后端 `std::fs` + 原子写（temp+rename）
   完全可单线程落地；大文件/流式走服务层线程桥。
4. **shell 是 subprocess 的 bash -c 消费者，request/spec 分裂是其模板**。`ShellExecRequest`
   （command/workdir?/timeoutMs?/…）→ `resolve()` 填默认+cap → `ShellExecSpec`（全填）
   → `run()/start()`。`timedOut/aborted` 互斥（单一 fused deadline 先到者）；`run` 只对
   基础设施失败 reject，非零退出/超时/中止都 resolve 成 `ShellRunResult`（非零退出是
   报告不是 isError）。→ Rust 侧同形：executor 层 + tool 层分离。
5. **background 不是 shell 的职责，是 jobs 的桥**。后台执行 = 工具包自己把
   `ctx.shell.start()` 的进程句柄包成 `JobHooks{cancel, done, readOutput}` 交
   `ctx.jobs.start(...)`（bash、terminal 的 `pty-send`、fs-search 都是同一模式，
   jobs registry 不认识 subprocess，只认 `JobHooks`）。Rust 的 `JobRegistry`（M4）
   已有完全同形的 `StartSpec{producer: FnMut()->ProducerHooks}` → **M5 直接复用**，
   只需把「subprocess/shell 的句柄桥接成 ProducerHooks」做出来。
6. **terminal 是 owner-scoped 的 PTY 注册表，长在 subprocess.spawnTerminal 之上**。
   三 层：subprocess 的 `spawnTerminal` 原语（分配/前台组/信号/整树清理）→ terminal-bash
   后端（会话/readiness/滚动缓冲）→ terminal 注册表（id/精确 Agent owner/单 active
   send）。PTY 是唯一「普通管道 stdio 无法重建」的原语，Rust 需 PTY 依赖（见 §6 决策 P1）。
7. **code-runtime = 宿主桥 + 双后端，程序是恶意对端**。`CodeRunRequest{program,
   bindings, signal}` → `CodeRunResult{value?, logs, error?}`；失败 6 类是结果上的字段
   （exception/timeout/abort/worker-exit/invalid-output/output-limit），不是 run() 的
   reject。TS 后端=worker_threads（Rust 无 JS 引擎 → 诚实桩）；Python 后端=python3
   子进程 + fd-3 JSON-lines 协议 + **host 视入站帧为恶意（validate+REBUILD）** + 
   lossless-JSON 精确跨越。→ Rust 可真实落地 **python 后端**（Host 端帧编解码 +
   子进程桥 + run_code 工具真实接线），TS worker 后端保持与 M4 相同的诚实桩。
8. **every model-visible ⟺ logged**（参考 AGENTS.md 纪律）：fs 的 read/write/edit、
   bash、terminal、code-runtime 的一切模型可见输入必要时落会话事件（`tool/…` 已有
   承载），`sandbox/mode` 是 log-only 会话事件（可回放、绝不进模型 transcript）。

### 3. 自顶向下（Top-down）：M5 交付物分解

```
M5a dsh-subprocess   <- 依赖：dsh-session（类型/事件词表，若跨界）==
                         std::process + 平台树终止 + 有界收集(spill) + scrub env
M5b dsh-fs           <- 依赖：dsh-session（事件词表）+ 原子写 + 版本 CAS + tool-fs
M5c dsh-sandbox      <- 依赖：dsh-session（sandbox/mode 事件）、dsh-tools（ApprovalProvider）
                         = sandbox-policy + escalation + writableRoots + 系统提示注入
M5d dsh-shell        <- 依赖：M5a + M5c（请求/规格分裂 + bash/pwsh 后端 + tool-bash）
M5e dsh-terminal     <- 依赖：M5a(spawnTerminal) + M5c + dsh-jobs（pty-send 后台）==
                         registry + bash 后端 + 6 tool（取决于 PTY 依赖决策 P1）
M5f dsh-code-runtime <- 依赖：M5a（python 子进程）+ dsh-tools（run_code 真实接线）
M5g jobs 子进程 producer + schedule 定时器 <- 依赖：M5a/M5d + dsh-jobs + dsh-schedule
M5h web.rs 接线      <- 依赖：上述全部（M4HostServices 扩展 + 工具注册 + 投影）
M5i M5-ACCEPTANCE    <- 依赖：上面全部（契约面 + 集成 + 全绿 + clippy）
```

> 依赖序遵循「能力缝三段式（Service Definition / Provider / Consumer）」：
> subprocess 是材质底座（最底层，无上层依赖），fs/sandbox 平行，shell 长在 subprocess
> 之上、terminal 长在 spawnTerminal 之上、code-runtime 长在子进程桥之上；tool 层永远
> 由各能力自己的 Consumer 包持注册 + 宿主 bind。

> 其中 dsh-fs/dsh-shell/dsh-terminal/dsh-code-runtime 各自的 tool 注册都走既有
> `dsh-tools::ToolRegistry` + 宿主 bind（复用 M4h 的 `M4HostServices`/`M4Tool` 模式：
> 有 handle → bind 真实执行器；无 handle → 诚实 `NOT_BOUND`/`UNSUPPORTED`，绝不伪装）。

### 4. 自底向上（Bottom-up）：现有资产核实

**参考契约（已逐字核对，来源：M5 执行契约报告）**：
- fs：`ctx.fs` 12 个 `FsErrorCode`、4 个事件（`fs/write-intent`/`fs/edit-intent`/
  `fs/observed`）、tool 各自独立注册（`read(file_path,offset?,limit?)`、
  `write(file_path,content)`、`edit(file_path,old_string,new_string,replace_all?)`、
  `read_image` 条件注册、`glob`/`grep` 独立包、`str_replace_editor`）；provider 词汇
  **不透明 Branded**（FsTargetKey/FsVersion）；版本守卫 + 原子变更。
- shell：`SHELL_SETTINGS_NAMESPACE`；`ENV_OVERRIDES = {NO_COLOR:1, TERM:dumb, PAGER:cat,
  GIT_PAGER:cat}`；默认 `timeoutMs=120_000 / maxTimeoutMs=600_000 / maxOutputBytes=64_000 /
  maxSpillBytes=64MiB / graceMs=3_000`；`bash -c` 托管进程组；模型可见标记
  `[exit code: N]`/`[timed out after …ms]`/`[killed by signal: …]`/`[stderr]`、
  `[sandbox: file access denied under <mode> mode]`。
- subprocess：`SubprocessSpawnSpec` **零默认**（argv/cwd/stdio/graceMs/env 全显式）；
  stdio 三态 `ignore/pipe/{data}`×`pipe/inherit/Collect{maxBytes,spill?}`；唯一终止动词
  `terminate()`=SIGTERM→grace→SIGKILL 树级；`spawnTerminal` 唯一 PTY 原语；
  `scrubbedParentEnv` 洗 KEY/PASSWORD/SECRET/TOKEN 与 `DSH_*`；`pid+started` 防 PID 复用。
- sandbox：`SandboxMode = 'read-only'|'workspace-write'|'danger-full-access'`；`WIDER_MODES`
  严格更宽（read-only→[ws-write, danger]；ws-write→[danger]）；`ESCALATION_TARGETS=[ws-write,
  danger]`；`validateEscalationArgs`（两字段同现 + justification 非空句子）；
  `writableRoots(ws-write)=[canonical(workspaceRoot),'/tmp',tmpdir()]`；`sandbox/mode` 会话
  事件（log-only，{mode, source?:'delegation'}）；系统提示 `sandbox:policy` 段（order 110）。
- code-runtime：`CodeRunRequest/CodeRunResult` 三件套；失败 6 类；`RESERVED_BINDING_GLOBALS`/
  `RESERVED_ERROR_MEMBERS`/`DUNDER_MEMBER`/`PORTABLE_RESERVED_WORDS` 常量；python 后端
  `PROTOCOL_FD=3` + JSON-lines + host 恶意帧重建；`run_code` 参数 `code`/`description`。

**Rust 侧已有资产**：
- `dsh-session::types`：`EventKind::SandboxMode` **已预留**（L115、映射 "sandbox/mode" L191）
  ——M5 只需实现模式状态与投影/事件落会话，无需扩事件枚举。
- `dsh-tools`：`ToolRegistry`/`define_tool`/`ToolRunContext`、`run_code` 保留名三层
  （register/restrict/view 注入）+ `placeholder_run_code()`（runtime.rs:966，execute 恒
  Err「requires a code runtime」= M5 code-runtime 要替换的占位）、`ApprovalProvider`/
  `ApprovalOutcome` 四态（runtime.rs:76-90）——escalation 审批通道**已有闸**。
- `dsh-jobs`：`JobRegistry` + `StartSpec{kind,label,owner,producer:FnMut()->ProducerHooks}`
  + `ProducerHooks{on_cancel, read_output}`——subprocess/terminal 后台 producer 直接复用。
- `dsh-cli::web.rs`：`M4HostServices{jobs,schedule,todo}` + `register_m4_tools_with_host`
  （Option<Rc<…>> bind / 无 handle → NOT_BOUND）——M5 加 host 字段复用同一模式。
- `dsh-schedule`：`ScheduleHost.dispatch_due(now_epoch)` 已有；M5 补**定时推进**（宿主自动
  tick，服务层线程 + mpsc 桥，复用 Hmr 样板）。
- 并发样板：`std::sync::mpsc` + 后台线程 + `try_recv`（Hmr notify / web SSE 已用）；
  `set_timer_clock`/`set_spawn` 注入钩子；PLAN §3.3/Q3 明确「IO/进程/网络放服务层线程/
  进程，经信道桥回单线程核心」。

**双视角校验**：自上而下「subprocess→fs/sandbox/shell/terminal/code-runtime 的依赖
层叠」与自下而上「std::process + mpsc 桥样板已有、SandboxMode 事件已预留、ApprovalProvider
已有、JobRegistry producer 同形、run_code 占位待换、host-bind 模式可复用」**在中点相遇**
——M5 真实工作量落在「平台树终止/收集/PTY/帧协议这些服务层真实实现」+「沙箱策略层」+
「工具/宿主接线」，几乎不需要重构既有核心。唯一从第一性原理判定让步的：
**JS worker code-runtime / e2b / lsp 平台级后端 → 诚实登记/桩，不伪装**（见 §5 非目标）。

### 5. 目标 / 非目标 / 假设 / 约束 / 边界 / 验收标准

**非目标（勿扩散；凡环境/平台不可行 → 诚实桩而非降级架构）**

- **JS（TypeScript）code-runtime worker-thread 后端**：Rust 无 JS 引擎，不复刻
  （与 D-051 workflow-JS 同理）。→ `run_code` 支持 **python 后端真实** + TS worker 保持
  M4 桩（执行给精确「requires a code runtime」错误），`peekRuntime` 语言回退保留。
- **e2b**：参考是「experimental POC」+ 外部 E2B 云沙箱（网络/key）——本环境不可跑，且
  属于外部部署面。→ 能力表登记 + `NO_START_CAPABILITIES` 全 false（M4 同款诚实处理）。
- **fs 搜索工具（glob/grep）**：参考 `tool-fs-search` 靠 **spawn 打包的 ripgrep 二进制**
  （`@vscode/ripgrep`）+ 固定 argv。**2026 环境实测修订（D-054）**：Rust 侧不引二进制——
  `globset` 0.4.18 + `ignore` 0.4.26（ripgrep 同源引擎 crate）已在本地 registry，可直接
  实现同名工具（glob 匹配 + 目录遍历 + 正则内容匹配，对齐 GLOB_MAX_RESULTS=100 /
  GREP_MAX_MATCHES=250 / SEARCH_TIMEOUT_MS=30_000 契约）。→ **推断入 M5 可选落地**：
  M5 交付 `read`/`write`/`edit`/`str_replace_editor` + **真实 glob/grep**（用同源 crate）
  或用户在裁定表放行时纳入；不再受「ripgrep 二进制不可得」限制。
- **lsp**：(a) 依赖语言服务器二进制 + fs/subprocess 执行世界；(b) 参考 4 操作
  (goToDefinition/findReferences/goToImplementation/hover) + stdio JSON-RPC，属周边协议
  集成面。→ M5 交付 subprocess 原语后**可做、但不做**；登记 seam + 明确 unavailable，
  连同 mcp/acp/hooks/skill 归 M6（PLAN 也如此分层）。
- **平台内核级沙箱 runner**（Seatbelt/bwrap/Landlock/Windows ACL restricted-token）：
  纯 Rust std 无此能力，做 runner 需要系统 FFI/外部二进制，且本机（Windows）无 bwrap。
  → sandbox 交付 **策略归属性（解析/事件/提示/escalation）+ fs 进程内围栏**（真实边界）；
  argv confiner 作为 **seam 声明 + 无 runner 时的 fail-closed 拒绝**（绝不静默返回原
  argv），待后续可引安全库/平台 runner 时填补。**诚实降级而非隐蔽绕过**。
- **IANA 全时区**（chrono-tz/jiff-tzdb）：**2026 spike 复核**——`chrono-tz` 不可离线，但
  `jiff`/`jiff-tzdb` 全家在本地缓存、**离线编译+运行已验证可行** → 需用户裁定（P2）是否
  纳入 M5；未裁定前保持 `invalid_time_zone` 诚实报错（继承 D-050）不负债。
- **workflow JS 引擎**：继承 D-051/D-053 = 保持桩（M5 不重开）。
- **out-of-process subagent provider**（acp/claude-code/codex/dsh-sdk）：M5 交付
  subprocess 原语后**可 spawn 外部进程**，但完整 provider 适配/网络协议是大块；
  只把「能力登记 + NO_START_CAPABILITIES」做实，真实 provider 归 M6。
- **凭据 .env 解析 / records half（grant/api-key）**：M3/M4 明确 M5 服务层——本 M5 交付
   `scrubbedParentEnv`（执行时清洗）作为**真实边界**；`.env` 文件解析仍留 M6 服务层
  （本机凭据文件不可验，且属配置面非执行面）。
- **真实浏览器 E2E**：本环境不可跑（沿用 D-022/D-036，以 `handle_rpc_host` 集成 + 单测代偿）。

**M5+ 遗留项完整性裁定（2026 复核：既往前兆逐条对照，不得静默遗漏）**

| # | 既有 defer 项（来源文件:行号） | 本 M5 裁定 |
|---|---|---|
| 1 | guard 真抢占（DECISIONS:1744「真正可抢占接线留 M4/M5 executor」、M3-ACC:57「真抢占留 M4/M5」） | **入 M5**：M3 只交付了「同步 wall-clock 后置度量」（无并发抢占的诚实降级）；M5 的 subprocess/`terminate()` 提供**真实树级终止**，shell/fs 工具的超时→kill 抢占成为真实可实现——补 `timeout` 预算触发树级终止 + `[timed out after …]` 标记（对齐参考 timedOut/aborted 互斥语义），收掉这条欠账 |
| 2 | OS 级 watch（M3-REQ:109「无 OS 级 watch，M5 可选轮询」） | **M5 可选、默认不做**：HMR 已用 notify 后台线程 + mpsc 桥（M35）实现真实 OS watch；「外部编辑热更新」依赖 fs 监听场景，非执行引擎核心 → 登记 seam，M5 不展开（避免与能力缝范围纠缠） |
| 3 | settings YAML 注释保真 leaf-diff（M3-REQ:45「M5 范围」） | **推 M6（配置面）**：settings 属配置持久化非执行面；M5 聚焦执行引擎，此条连同凭据 .env 服务层一并 M6 |
| 4 | ts-host 差分编排（DECISIONS:848/D-022「session-host.mjs 等属 M5」） | **推 M6（Cordis-equivalence 差分面）**：属 dsh-diff 差分基建而非执行引擎；M5 不重开 |
| 5 | SQLite backlog（DECISIONS:870 历史「M5 范围」 vs M1-REQ:103「M2+」归属不一致） | **推 M6（持久化面）**：M5 用 JSONL/临时文件（已有）；SQLite 是存储后端选型，与执行引擎正交，且归属历史不一致需在 M6 单独裁决 |
| 6 | 持久 ToolRegistry 注入点（DECISIONS:2101/D-052「M4i/M5 若宿主开放 registry 再真挂」） | **入 M5（web.rs 接线）**：M5h 在 `M4HostServices` 扩展后，fs/shell/code-runtime 工具以「定义 + 宿主 bind」挂进既有 registry（复用 M4Tool 模式）；即「宿主开放 registry」落定 |
| 7 | subagent 真实目录源（DECISIONS:2113 历史「M5 in-process driver」→ 已被 M4i 实驱动覆盖） | **已结**：M4i 已补 in-process 实驱动；M5 只剩 out-of-process（非目标，M6） |
| 8 | schedule 定时推进（DECISIONS:2170/D-053「定时推进属 M5 宿主调度」） | **入 M5**：M5g 宿主自动 tick（服务层线程 + mpsc 桥 + `dispatch_due`），非手工调用 |
| 9 | run_code 占位→真实（DECISIONS:956/996/D-024/025、runtime.rs:966） | **入 M5**：python 后端真实接线替换 `placeholder_run_code`；TS worker 桩保留 |
| 10 | bash/pwsh/terminal producer（DECISIONS:1808/1987/D-044/049、M4-REQ:146） | **入 M5**：M5g 把 subprocess/shell 句柄桥接成 `JobHooks` 进 `JobRegistry`（bash producer 真实；terminal producer 随 P1） |

**假设 / 约束**

- 单线程核心 `Rc<RefCell>` 不动（D-004）；服务层线程 mpsc 桥（Hmr 样板）是唯一并发面；
  不引入 tokio/async。
- `cargo/clippy` 一律 `--offline` + `$env:RUSTC_WRAPPER=''`（D-027）；中文文件只用 write/edit。
- 编译验证在 Windows 主机；bash/pwsh/python 均可执行（`C:\WINDOWS\system32\bash.exe`、
  `D:\Anaconda\python.exe`、node24LTS 存在）——集成测试能用真实子进程；无法抵达的
  平台路径（POSIX 进程组/BigSur seatbelt）以 cfg 门 + 单测中的平台逻辑隔离。
- 新依赖引入需先评估（方法论四）：`tempfile`/`regex` **已在 lock**；**2026 环境实测
  （D-054）**：网络真实可达（Node/Python 直连 rsproxy HTTP 200），此前「离线下拉受限」
  是沙箱 Schannel 假故障 + 缓存目录不可写的组合；`jiff` 全家 / `globset` 0.4.18 /
  `ignore` 0.4.26 / `which` 6.0.3 / `sysinfo` / `nix` / `windows-sys` 均已在本地 registry；
  仅 `portable-pty` 缺、`globset/ignore` 待一次普通 `cargo check` 提取（用户手动安装清单
  见 `M5-DEPENDENCIES.md`，D-054）。`chrono-tz` 不在本地（P2(a) 用 jiff 已够）。
- 字段名/wire 形状/错误码逐字对齐参考源码（本文件已列权威来源）；差异显式记录。
- `bash` tool 参数名：**参考源是 `timeoutMs`（camelCase）**，本 harness GUI 提示用
  `timeout_ms`（snake）——M5 以参考 wire 为准，并记录分叉（凡 dsh 工具参数 schema 就
  用参考命名，避免模型学岔）。

**边界（不变量）**

- subprocess：spec 零默认；`terminate()` 是唯一终止动词、树级、幂等；`done` 永不 reject
  （spawn 失败 → killed + stderr）；collect 是 offset-based 非消费 reader；scrubbed env
  只许显式覆盖。
- fs：version 守卫（CAS：`FS_STALE_VERSION`/`FS_NOT_OBSERVED`）；mutation 原子；
  双层守卫（进程内围栏 + 观察状态）独立；targetKey/version 不透明。
- sandbox：`read-only` 是地板永不可为 target；escalation 必须在 execution 时校验严格
  更宽 + 审批 fail-closed 先于执行；`sandbox/mode` 是 log-only 会话事件。
- shell：`timedOut`/`aborted` 互斥；非零退出不是 isError；后台经 `JobHooks` 桥接 jobs。
- code-runtime：程序是恶意对端（host 校验 + 重建帧 + lossless-JSON）；失败是结果字段。
- terminal（若做）：精确 Agent owner；单 active send（`SEND_ACTIVE`）；PTY 存活时禁改
  `sandbox/mode`。

**验收标准**

1. `cargo test --workspace` 全绿；clippy `-D warnings` 零告警。
2. **subprocess**：真实 spawn（argv/cwd/stdin data/stdout collect+spill）/ 树级 terminate
   （Windows taskkill /T 路径至少单测隔离）/ 有界输出 tail / scrub env（KEY/SECRET 清除）/
   `terminal` 原语 seam（PTY 依赖决策落地）。
2a. **guard 真抢占（M3 欠账收口）**：shell/fs 工具的超时预算触发**真实树级 kill**（非 M3
   后置度量）；结果带 `[timed out after …]` 标记 + `timedOut`/`aborted` 互斥分类（对齐
   参考 timedOut/aborted；账目对 D-042/M3-ACC:57）。
3. **sandbox**：模式解析优先级（approved > session `sandbox/mode` > 默认 read-only）/
   严格更宽阶梯（read-only→ws-write→danger 单向）/ 审批四态 fail-closed /
   `writableRoots` 唯一来源 / 系统提示注入 / `sandbox/mode` 会话事件 + 回放 fold。
4. **fs**：read/write/edit 三 tool 真实执行 + 输出 schema（write `{path,operation,
   before,after}`）+/ version CAS（stale/missed → 对应 FsErrorCode）/ 进程内 sandbox 围栏
   （`FS_SANDBOX_DENIED`）/ 原子写 / 流式与大小上限（FS_TOO_LARGE）。
5. **shell**：bash tool 真实执行（含 ENV_OVERRIDES、退出标记、超时/中止分类、
   `timedOut`/`aborted` 互斥）/ 后台经 `JobHooks` 进 `JobRegistry`（bash producer）/
   会话 cwd 解析。
6. **code-runtime**：python 后端真实跑（fd3 帧编解码 + hostile 帧重建 + lossless 回传）/
   `run_code` 工具桥接线真实执行（替换 `placeholder_run_code`）/ TS worker 后端诚实桩。
7. **jobs/schedule**：bash/terminal 子进程 producer 真实跑 + read/kill/wait；
   schedule **真实定时推进**（宿主 tick 线程 + mpsc → `dispatch_due` 自动触发，非手工）。
8. **terminal（若决策 P 落地）**：open/send/read/signal/close/list 6 tool + owner 授权 +
   单 active send + PTY sandbox/mode 锁定；否则 seam + 诚实 unavailable。
9. **web.rs**：M5 工具经 `M4HostServices` 扩展 + `handle_rpc_host` 集成真实驱动；无
   handle → NOT_BOUND/UNSUPPORTED 诚实；投影/事件经既有通道。
10. 每子步 DECISIONS 对应条目 + git 提交可互查。

### 6. 关键决策点（阶段关卡待用户裁定）

| 决策 | 选项 | 倾向/影响 |
|---|---|---|
| **P1 PTY 依赖** | (a) 引 `portable-pty` 库真实做 terminal(+spawnTerminal)；(b) terminal 推 M6，M5 交付 seam+unavailable | **2026 环境实测修订（D-054）**：`portable-pty` 是本环境唯一缺失 crate，但**网络真实可达**（Node/Python 200），用户装好即可（见 `M5-DEPENDENCIES.md`）→ (a) 重新可行；用户未装前 (b) 保底 |
| **P2 IANA 时区** | (a) 引 `jiff`+`jiff-tzdb`（**已提取+实测可离线**，见下方决策辅助）；(b) 保持 invalid_time_zone 报错 | **2026 复核（D-054 实查）**：chrono-tz 不在本地，但 `jiff-tzdb` 全套已**提取进 registry/src**（已离线编译+运行验证）→ (a) 无任何依赖障碍即可落地；仍需用户决议取 (a) 或 (b) |
| **P3 平台沙箱 runner** | (a) seam+失败闭； (b) 引入 Landlock/bwrap FFI（不可离线验证） | 倾向 (a)，真实边界由 fs 进程内围栏承担 |
| **P4 bash 参数名** | timeoutMs（参考）vs timeout_ms（GUI 现状） | 倾向参考 `timeoutMs`，记录分叉 |
| **P5 lsp/e2b/out-of-process/jobs Scope** | 全部 M6（登记+诚实桩）；subprocess 原语 M5 已交付其底座 | 倾向 M6（非目标已列） |

**决策辅助（供裁定参考，2026 复核）**

- **P1**：本机无 PTY 验证路径。**2026 环境实测修订（D-054）**：`portable-pty` 不在本地
  缓存，但网络真实可达——用户普通终端跑一次 `cargo fetch`/`check`（见 `M5-DEPENDENCIES.md`
  清单）即入缓存 + 提取；装好后选 (a) 真实 terminal（含 spawnTerminal）可落地。未装前
  选 (b) M5 交付 `spawnTerminal` 原语 **seam**（握法/信号/owner 语义进 types）+
  `terminal_open/send/read/signal/close/list` 工具的 `NOT_AVAILABLE` 诚实桩。
- **P2**：承 D-050 已把 `invalid_time_zone` 定为诚实报错并记 README Known Limitations。
  **2026 spike（实测）**：`chrono-tz` 不在本地缓存、不可离线；但 `jiff` 0.2.35 全家
  （jiff-core/jiff-static/jiff-tzdb/jiff-tzdb-platform/crc32fast/serde）**均在缓存内**，
  临时 crate `jiff { features=["tzdb-bundle-platform"] }` 的 `cargo check --offline` 编译
  通过、`cargo run --offline` 运行时 `TimeZone::get("America/New_York")` 返回 `ok: true`
  （IANA 全时区离线可用）。→ 若裁定 P2(a)，schedule 的 canonicalize_time_zone/local-at
  可真实扩展（替换 D-050 的 invalid_time_zone 降级），且无网络依赖；选 (b) 则零回归。
  建议 M5 取样 2(a)（成本低）或 2(b)（严格不改），两者均不负债。
- **P3**：稀缺真实边界（reference 的 Seatbelt/bwrap/Landlock/ACL 全平台 FFI），本机
  Windows 无 bwrap；纯 std 做不了。选 (a) 把「进程内 fs 围栏（canonicalize-then-contain +
  writableRoots）」作为 M5 的真实约束面，argv confiner 留 seam + fail-closed（无 runner
  绝不放行），后续引安全库时补真实 runner。
- **P4**：reference 逐字是 `timeoutMs`（camelCase，见 tool-bash index.ts L254）；本仓 GUI
  提示模板写 `timeout_ms`。M5 新增 tool 的 schema 若继续 `timeout_ms` 则与参考 wire 分叉；
  凡 dsh 工具参数 schema 统一按参考 camelCase，避免模型学岔两套命名。差异显式记 D 条目。
- **P5**：lsp/e2b/out-of-process subagent/jobs 均在参考里是独立大缝（进程协议 + 外部
  服务 + FFI），M5 已把 subprocess 原语 + JobHooks 桥 + 能力登记做实，真实 provider 全
  留 M6（与 PLAN 里程碑路线一致：M6 = mcp/acp/spill/hooks/skill）。选「M6」最省且不欠
  架构债。

> 裁定方式：可整体采纳（如「按倾向走」）或逐项指定。**本文件通过验收 + P1-P5 裁定
> 落定后**，才进入阶段二（系统设计）；决策记录随后补记 `DECISIONS.md`。

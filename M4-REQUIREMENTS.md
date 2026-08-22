# M4 长任务编排与子代理：需求结论文档 + 系统设计

> 本文件是 `PLAN-rust-full-harness-migration.md` §6「M4 范围」的实现工件：
> **阶段一（需求分析）** 产出目标/非目标/假设/约束/边界/验收标准；
> **阶段二（系统设计）** 产出 crate 划分、依赖序、模块结构与关键设计决策。
> 契约事实来自逐行阅读参考源码（packages/{goal,subagent,plan-mode,jobs,schedule,workflow,
> todo}/ + packages/host/apiproxy/src/api/{goals,subagents,jobs}.schema.ts + rpc-map.ts）
> 与两份语义提炼报告（M4 goal/subagent/plan、M4 jobs/schedule/workflow），
> 错误 code/字段名/wire 形状逐字对齐并记录差异。
> 决策编号追加记入 `DECISIONS.md`，git 提交可互查。

---

## 第一部分：需求分析（第一性原理 + 双视角）

### 1. 根本目标

M4 的目标：把 **web.rs 的长任务编排空桩做实**——让前端（复用现有 harness UI）能真实地：

- **goal**：为一个会话设置/编辑/暂停/恢复/完成/清除长期目标（带 CAS revision），目标状态
  以 `goal/change` 事件持久化于会话日志并通过 `goal` 投影呈现；长任务具备**自动续跑驱动**
  （goal-round-driver：active+armed+未超 cap+agent idle → 自动发起下一轮）。
- **subagent**：真实起用**进程内子代理**（in-process provider，含 spawn 全新子代理与 fork
  继承父已完成轮次前缀两种），可 list/history/prompt/interrupt，list 返回完整可达目录
  （child one-shot/continuable + diagnostic + parentAvailable 提示）。
- **plan-mode**：计划模式（`/plan <msg>` 进入 / `/plan off` 离开），`plan/mode` 事件 +
  `plan` 投影（active/pending），`exit_plan_mode` 工具在计划模式下提交完整计划（评审通道
  若缺席则诚实报错）。
- **jobs / schedule / workflow / todo**：作为宿主服务能力落地（jobs 注册表 + 定时器调度
  schedule + todo 工具 + workflow 能力登记），并按其真实边界决定「可单线程实做 / 必须补
  进程级桩」。

M4 的 web RPC 接线面（rpc-map.ts 权威）实为 **10 个方法**：`goal.*`（create/edit/pause/
resume/complete/clear，6）+ `subagent.*`（list/history/prompt/interrupt，4）——其余
（jobs/schedule/workflow/todo）**不在 web RPC 方法表上**，而以宿主服务/工具/事件/投影形式
承载。

| 交付 | 对应 TS 包 | 一句话职责 |
|---|---|---|
| `goal.*` 方法面 | goal/ + host/apiproxy/src/api/goals + goal-round-driver | 目标生命周期 CAS + 事件溯源 + 自动续跑驱动 |
| `dsh-goal` | goal/goal（domain/types/fold/runtime）+ goal-round-driver | 纯目标域：状态机/严格 fold/投影/轮次驱动 |
| `subagent.*` 方法面 | subagent/subagent + in-process driver + host/apiproxy/src/api/subagents | list/history/prompt/interrupt + 进程内 provider |
| `dsh-subagent` | subagent/{subagent,in-process-driver,spawn-in-process,fork-in-process} | 子代理域：目录/描述符/投影/continuable 管理 |
| plan-mode | plan/plan-mode | `plan` 投影 + `exit_plan_mode` 工具 + `/plan` 命令接线 |
| jobs | jobs/jobs + jobs-local | 后台任务注册表：start/list/get/read/kill/wait + 生命周期 |
| schedule | schedule/schedule | 持久化定时提醒：create/list/delete + fold 重放 + 到期注入 |
| workflow | workflow/workflow | 能力最前端：meta 校验 + 事件骨架；**JS 执行引擎保持桩** |
| todo | todo/tool-todo | `todo/write` 事件 + `todos` 投影 + todo 工具 |
| commands | interaction/commands | `/plan`/`/goal` 等斜杠命令清单扩展（M4 相关项） |

### 2. 第一性原理分解

1. **goal 的本质 = CAS 状态机 + 事件溯源**：目标是回放 `goal/change` 事件折叠而来（last-wins
   全量快照），revision 是精确递增的 compare-and-set 标识。一切读（projection、get_goal、
   roundsStarted 校验）都是纯重放；一切写（create/edit/pause/resume/complete/clear/block）都是
   同一条可靠事件日志上的 CAS 提交。→ **Goal 无外部 backend，可整块搬进 Rust 单线程**。
2. **「自动续跑」是调度不是状态**：goal 的持久状态只有 phase/objective/maxGoalRounds/revision；
   `armed/disarmed` 是进程内激活标志（session-start/fork 后自动 disarmed，resume 才 armed）。
   续跑驱动是一个「active ∧ armed ∧ roundsStarted<max ∧ agent idle ∧ 无竞争 prompt →
   排队下一轮」的可判定谓词，依赖 agent-loop 的 followup/inbox/whenIdle（Rust 已具备）。→
   只给「调度判定」和「驱动入口」，不把轮次执行逻辑塞进 goal 域。
3. **subagent 的进程内/外边界是硬边界**：in-process（spawn/fork）在同一事件循环内起 child
   Agent（复用 dsh-agent-loop 的 AgentLoopHost），纯内存可单线程落地；out-of-process
   （acp/claude-code/codex/dsh-sdk）必须真实 OS 进程（M5 subprocess 边界）。→ M4 只交付
   in-process 两 provider + 完整管理面；out-of-process 保持「能力登记 + 明确不可用」桩。
4. **read vs write 分离**：subagent.history 读取的是持久化转录（不激活 Agent）；只有
   subagent.prompt 走活父代把消息投进 child 的 inbox（followup）。continuable cold resume
   依赖会话持久化。→ Rust 侧复用 dsh-persistence + AgentLoopHost.followup。
5. **jobs/schedule/workflow/todo 是「宿主内部服务能力」而非 web RPC**：rpc-map 只有
   goal/subagent 两类方法；jobs 走 `session/jobs` 帧投影，schedule/todo 走工具 + session 事件，
   workflow 是工具 + 事件。→ 这些以 dsh-tools 工具注册 + dsh-session 事件/投影承载，
   不新增 web RPC 方法（保持 rpc-map 契约面不变）。
6. **workflow 的 JS 脚本执行无法低成本复刻**：workflow 的模型可见面就是「写一段 JS 脚本
   （agent()/pipeline()/parallel() 顶层 await）由 worker 线程执行」。Rust 无 JS 引擎、
   无 async worker 协议层，复刻成本远超收益。→ **保持桩**：meta 校验做实（shape 会错→
   结构化 `isError`）、事件骨架/致命 code 分类/result materialize 规则做实的「诚实桩」，
   不伪装成功。
7. **schedule 的持久化权威是会话事件日志**：schedule 记录以 `schedule/change` 事件
   （create/delete/dispatch）持久化，fold 重放给出 active 集合；到期注入走
   `agent.followup(userMessage)`。persistence 不确定（create/list/delete 落盘失败）→
   `persistence_uncertain` 错误。→ 复用 dsh-persistence 会话日志 + AgentLoopHost.followup，
   无新存储机制。

### 3. 自顶向下（Top-down）：M4 交付物分解

```
M4a dsh-goal          <- 依赖：dsh-session（goal/change 事件）、dsh-brand（GoalId）、serde
M4b goal-round-driver <- 依赖：M4a + dsh-agent-loop（followup/whenIdle/inbox/status）
M4c plan-mode         <- 依赖：dsh-session（plan/mode 事件）、dsh-tools（command/exit_plan_mode）
M4d dsh-subagent      <- 依赖：dsh-session（subagent/descriptor 事件）、dsh-agent-loop
                         （child Agent + followup + interrupt）、dsh-persistence（cold resume）
M4e jobs              <- 依赖：dsh-session（session/jobs 投影）、dsh-tools（job_* 工具）
M4f schedule          <- 依赖：dsh-session（schedule/change 事件）、dsh-agent-loop（followup 注入）
M4g todo + workflow   <- 依赖：dsh-session（todo/write）、dsh-tools（todo 工具）；workflow 桩 + meta 校验
M4h web.rs 接线 + 投影 <- 依赖：以上全部（Boot 装配 + dispatch + ProjectionRegistry 挂载）
M4i M4-ACCEPTANCE     <- 依赖：上面全部（契约面 + 集成 + 全绿 + clippy）
```

> 其中 M4h 同时把 goal/plan/subagent/jobs/todos 投影单元注册进既有的
> `dsh-session-query::ProjectionRegistry`，使 `session.history`/`subagent.history` 的
> projections block 真实携带这些键。

### 4. 自底向上（Bottom-up）：现有资产核实

- `dsh-session::types`：**已预留全部 M4 事件类型** `goal/change`、`plan/mode`、
  `subagent/descriptor`、`schedule/change`、`todo/write`、`command/run`、`command/done`、
  `tool-workflow/agent-start|end`、`session/end-seed` 等（EventKind 词表 + payload 变体已有）——
  M4 只需实现域逻辑 + 让域事件落会话，无需扩事件枚举。**这是最大的既有资产**。
- `dsh-session-query::projection`：`ProjectionRegistry`/`ProjectionUnit`（key/init/apply/view
  纯函数 fold + as_of_seq + snapshot/checkpoint + 持久化恢复）已完整（M1d 交付）。M4 注册
  goal/plan/subagent/jobs/todos 投影 unit 即可。
- `dsh-agent-loop`：`AgentLoopHost`（`agent(id)`/`followup(id,msg)`/`events()`/`teardown()`）、
  `ReactLoopAgent`（inbox/followup/pre-step/step 驱动）、`execute_tool_calls`（M2 全链已绿）。
  → goal-round-driver 的 followup/status/idle 判定、subagent 的 child Agent 创建与消息投递
  **全部就绪**。
- `dsh-llm::types::Message`：`Message::user(id, content)` / `Message::assistant(...)` 已有。
- `dsh-cli::web.rs`：M4 相关 10 方法目前是**空桩**（goal.* → `{ref:{id:"default",revision:1}}`、
  subagent.list → 空 entries、subagent.interrupt → `{accepted:true}`、subagent.prompt →
  `{messageId:"default"}`、commands/list → 硬编码三命令）。M4a-M4h 逐步替换为真实服务；
  `rpc_extended_method_surface` 形状测试会随实做调整（参照 M3a 对 host.* 的处理方式）。
- `dsh-tools`：`ToolRegistry`（register/register_global/get）+ `define_tool` + `ToolRunContext`
  + `ToolResult` 已就绪——M4 的 job_*/schedule_*/todo/exit_plan_mode/workflow 工具在此注册。
- `dsh-persistence`：`atomic_write`（M3b）+ `SessionPersistence`（JSONL 落盘/读回）已就绪——
  subagent cold resume / schedule 事件持久化直接复用。
- `rpc-map.ts`（参考权威）：M4 的 web RPC 只有 `goal.*` 6 + `subagent.*` 4，共 10；jobs 走
  `session/jobs` 帧（taskViewSchema），workflow/schedule/todo/plan 不走 web RPC。

**双视角校验**：自上而下「goal + subagent + plan + jobs/schedule/workflow/todo 六个域」与
自下而上「事件词表已是全的 + 投影注册表已是全的 + agent-loop followup 已有 + 工具注册表
已有」**在中点相遇**——M4 的真实工作量落在「域逻辑（状态机/fold/调度）+ RPC 接线 + 投影
注册 + 工具注册」，几乎不需要新建基础设施。唯一从第一性原理判定让步的是 **workflow JS
执行 → 保持桩**（复刻成本 >> 收益，且是 M5 subprocess/deploy 层决策）。

### 5. 非目标 / 假设 / 约束 / 边界 / 验收标准

**非目标（D-044 声明，勿扩散）**

- **out-of-process subagent provider**（acp/claude-code/codex/dsh-sdk）M4 不落地：真实 OS
  进程适配在 M5 subprocess/seam；M4 只做能力登记 + `NO_START_CAPABILITIES` 明确不可用。
- **workflow 的 JS 执行引擎**（node:vm / worker-thread 协议）M4 不落：只做 meta 校验、事件
  骨架、致命 code 分类、result materialize 规则的诚实桩。
- **goal 的实时跨进程续跑**不做：驱动走 Rust agent-loop 的 followup/whenIdle 单线程判定；
  不引入线程/后台调度器（D-004 单线程核心不破）。
- **schedule 的 IANA 全时区数据库**不内置：用 `chrono-tz`/轻量时区解析（引入成熟库评价；
  本地固定时区可降级）。
- **jobs 的宿主 subprocess producer**（bash/pwsh/terminal）M4 不落（M5）；只落子代理
  producer（进程内）+ 注册表状态机。
- 不新增 web RPC 方法（rpc-map 保持 10 个 M4 方法），jobs/schedule/workflow/todo 均在现有
  事件/工具/投影通道承载。
- 真实浏览器 E2E 本环境不可跑（无浏览器/无 key/网络阻断）——延续 D-022/D-036 策略，
  以 `handle_rpc_host` 集成 + 单测代偿。

**假设 / 约束**

- 单线程、无新 crate 依赖。Rust（dsh-*）侧不引入 tokio/async。
- 事件词表/投影键/错误 code/wire 字段名逐字对齐参考源码（本文件已列出权威来源）。
- cargo/clippy 一律 `--offline` + `$env:RUSTC_WRAPPER=''`（D-027）；中文写文件只用 write/edit。

**边界（不变量）**

- goal revision 严格递增；CAS 失败 → `GOAL_STALE_REVISION`，绝不静默覆盖。
- goal 投影 `goal: null` ⟺ 无目标或已 clear（含 clear 墓碑折出），非缺失键。
- subagent.history 永不激活 Agent；只有 subagent.prompt 投递活消息。
- subagent 深度预算：childDepth = max(header,runtime)+1，越界 → `SubagentDepthError`。
- jobs 授权围栏：owner/无主才可见；他人 session → 拒绝。
- workflow 桩对未知能力返回结构化 `isError`（不伪装成功）。
- wire 上 goal/subagent 可选字段（label?/detail?/projections?/blockedReason?）缺失即省略。

**验收标准**

1. `cargo test --workspace` 全绿；clippy `-D warnings` 零告警。
2. **goal**：create→edit→pause→resume→complete→clear 全生命周期 + CAS（stale revision →
   `GOAL_STALE_REVISION`、重复 create → existing）+ `goal/change` 事件落会话 + `goal` 投影
   fold（含 clear 墓碑 → null）+ 严格 fold 逐字段校验（revision 精确 +1、计数/时间戳守恒）。
3. **goal-round-driver**：active+armed+未超 cap 自动排队续跑（followup 驱动）、roundsStarted
   回放递增、超 cap → blocked `{code:"round-limit"}`、session-start/fork → disarmed。
4. **plan-mode**：`plan/mode` 事件 + `plan` 投影（active/pending）+ `/plan off` 判定 +
   `exit_plan_mode`（计划前置条件/评审通道缺失时明确报错）。
5. **subagent**：in-process spawn/fork 两 provider 真实 child Agent 跑一轮；list 完整可达目录
   （child one-shot/continuable + activity/hasChildren + parentAvailable 提示）；history 持久化
   转录分页；prompt 经 alive parent 投递 + `{messageId}` 回执；interrupt 收到即 `{accepted:true}`
   （fire-and-return）；descriptor 事件 + subagent 投影 + 深度预算。
6. **jobs**：注册表生命周期（running→stopping→终态 first-wins）+ id `<kind>-N` + 授权围栏 +
   list/read/kill/wait；子代理 producer 真实跑；`session/jobs` 投影帧。
7. **schedule**：create/list/delete + after/at/every 三类 + `schedule/change` 事件 fold 重放 +
   dispatch 推进 + 到期注入（followup 或 framing 文本落事件）。
8. **todo + workflow**：`todo/write` 事件 + `todos` 投影 + todo 工具；workflow meta 校验 +
   事件骨架 + 致命 code 分类的诚实桩。
9. **web.rs**：10 个 RPC 方法（goal 6 + subagent 4）经 `handle_rpc_host` 集成真实服务驱动（不再
   空桩），投影键经 history 响应携带。
10. 每子步 DECISIONS 对应条目 + git 提交可互查。

---

## 第二部分：系统设计（决策 + 模块结构）

### 6. 关键设计决策（对应 DECISIONS D-044 起）

| 决策点 | 结论 | 理由 / 差异记录 |
|---|---|---|
| goal 承载 | 新 crate `dsh-goal`（纯域）+ `dsh-agent-loop` 侧驱动 | 域与驱动分离：纯 fold/状态机可单测穷举；驱动只做 followup 判定 |
| goal 事件用语 | `goal/change`（版本 1）全量 last-wins；clear 写墓碑 | 复用已预留事件类型；投影天然 last-wins |
| goal 自动续跑 | `arm/disarm` 进程内，followup 同一轮或其他轮 | 无跨进程；单线程内 AgentLoopHost 驱动 |
| plan 承载 | `dsh-plan`（轻量，plan/mode 投影 + 工具）或塞进 dsh-session-query 折叠 | 事件/投影已支持；最小面：投影 unit + exit_plan_mode 工具 |
| subagent 承载 | 新 crate `dsh-subagent`（域/目录/投影）+ dsh-agent-loop 复用 | 域与 loop 分离；child Agent 复用 AgentLoopHost |
| subagent provider | 仅 in-process（spawn/fork）做实；out-of-process 登记但不可用 | 硬边界：OS 进程 M5；in-process 单一事件循环 |
| jobs 承载 | `dsh-jobs`（注册表状态机）+ dsh-tools job_* 工具 | 纯内存状态机可单测；producer 接口化（子代理 producer 即一例） |
| schedule 承载 | `dsh-schedule`（fold/调度规则）+ dsh-tools schedule_* 工具 | 事件重放权威 + followup 注入 |
| workflow 承载 | `dsh-workflow` 仅 meta 校验/事件骨架/致命 code；执行桩 | JS 引擎不可低成本复刻；诚实桩不伪装成功 |
| todo 承载 | `dsh-session-query` 投影 unit + dsh-tools todo 工具 | 事件已预留；极轻 |
| 投影挂载 | M4h 在 Boot/web 装配 `ProjectionRegistry` 注册 M4 各键 | 复用 M1d 注册表；history 响应带 projections block |
| subagent cold resume | 复用 `dsh-session` JSONL 持久化 + 激活边界 = seedLength | 无新存储；缺持久化 → `PERSISTENCE_UNAVAILABLE` |

### 7. 模块结构

```
crates/dsh-goal/
  src/lib.rs            # GoalId/GoalRef/GoalPhase/GoalSnapshot/GoalView/GoalBlockReason
  src/fold.rs           # decodeGoalChange + applyGoalChange（严格回放 fold，revision 校验）
  src/projection.rs     # GoalProjection fold（last-wins；clear → null）
  src/service.rs        # CAS 状态机：create/edit/pause/resume/complete/clear/block + 错误码
  src/round_driver.rs   # 自动续跑判定：active∧armed∧未超cap∧idle → followup 下一轮
  tests/m4_goal.rs
crates/dsh-plan/
  src/lib.rs            # PlanProjection 折叠（plan/mode last-wins + command 生命周期 → pending）
  src/exit.rs           # exit_plan_mode 工具前置校验
  tests/m4_plan.rs
crates/dsh-subagent/
  src/lib.rs            # SubagentListEntry/SubagentAddress/descriptor/深度预算
  src/provider.rs       # Provider 注册表（in-process spawn/fork；out-of-process 登记不可用）
  src/inproc.rs         # startInProcessRun：mint id → 捕获 delegation policy → child Agent 一轮
  src/catalog.rs        # listChildren/listDescendants + activity/hasChildren 派生
  src/control.rs        # history（持久化分页）/ prompt（followup）/ interrupt（fire-and-return）
  tests/m4_subagent.rs
crates/dsh-jobs/
  src/lib.rs            # JobRef/JobStatus/JobSnapshot/JobView（wire）
  src/registry.rs       # start/list/get/read/kill/wait + 生命周期状态机 + 授权围栏 + 限流
  tests/m4_jobs.rs
crates/dsh-schedule/
  src/lib.rs            # ScheduleRecord（after/at/every）+ ScheduleView
  src/fold.rs           # foldScheduleEvents（create/delete/dispatch 重放）
  src/rules.rs          # 校验/锚定/推进（纯函数；时区可选）
  tests/m4_schedule.rs
crates/dsh-workflow/
  src/lib.rs            # WorkflowMeta 校验 + 致命 code 分类 + 事件骨架（执行桩）
  tests/m4_workflow.rs
crates/dsh-cli/src/web.rs
  goal_* / subagent_*: 10 RPC 实做（替换空桩）
  projection 装配：Boot 注册 goal/plan/subagent/jobs/todos 投影 unit
  commands/list：/goal /plan 扩展
crates/dsh-tools/src
  todo / job_* / schedule_* / exit_plan_mode / workflow 工具注册
crates/dsh-session-query/src
  goal/plan/subagent/jobs/todos 投影 unit 定义（或引用对应 crate 的 fold 逻辑）
```

### 8. 依赖序与验证策略

- **先** M4a `dsh-goal`（纯域 + fold，无驱动依赖）→ 绿；
- **再** M4b goal-round-driver（钉 dsh-agent-loop followup/idle 判定，单测驱动入口）→ 绿；
- **再** M4c plan-mode（投影 + exit_plan_mode 工具）→ 绿；
- **再** M4d `dsh-subagent`（provider + inproc + catalog + control）→ 绿；
- **再** M4e `dsh-jobs`（注册表 + 子代理 producer）→ 绿；
- **再** M4f `dsh-schedule`（fold + 注入）→ 绿；
- **再** M4g todo + `dsh-workflow` 桩 → 绿；
- **再** M4h web.rs 接线 + 投影挂载（handle_rpc_host 集成）→ 绿；
- **最后** M4-ACCEPTANCE：workspace 全绿 + clippy + D-044 收口报告。

---

*依据：deepseek-harness packages/{goal,plan-mode,subagent,jobs,schedule,workflow,todo}/ +
packages/host/apiproxy/src/api/{goals,subagents,jobs}.schema.ts + rpc-map.ts；
M4 goal/subagent/plan 与 M4 jobs/schedule/workflow 两份语义提炼报告（会话子代理产出）。*

# M2 Agent 驱动链：需求结论文档 + 系统设计

> 本文件是 `PLAN-rust-full-harness-migration.md` §6「M2 成功标准」的实现工件：
> **阶段一（需求分析）** 产出目标/非目标/假设/约束/边界/验收标准；
> **阶段二（系统设计）** 产出 crate 划分、依赖序、模块结构与关键设计决策。
> 需求分析的契约摘要由并行子代理深读十个 TS 包生成，存入 `analysis/m2/*-report.md`
> （scope / tools / system-prompt / agent+system-prompt / agent-loop / interaction 五份，
> 逐行阅读 + 逐字错误消息 + 测试场景→期望行为 + 依赖面全 import）。
> 决策编号追加记入 `DECISIONS.md`，git 提交可互查。

---

## 第一部分：需求分析（第一性原理 + 双视角）

### 1. 根本目标

M2 的目标：把「真正的 agent 驱动链与审批链」落在 Rust 宿主上——让宿主拥有一套
**可替换、可测试、语义与 TS 逐字节对齐**的 agent 运行时：

| 交付 | 对应 TS 包 | 一句话职责 |
|---|---|---|
| `dsh-scope` | core/scope | 作用域注册原语（key 无关）：打标签/读标签/carrier 路由/ScopedLayers |
| `dsh-tools` | core/tools | 工具能力缝：注册表（全局+逐 agent 遮蔽）+ 执行管线 + 模型呈现模式 + Code Mode |
| `dsh-system-prompt` | core/system-prompt | 系统提示装配（sections/contexts/tools/variables）+ 渲染纯函数 |
| `dsh-agent` | core/agent | 活体 Agent 注册表 + initiator 作用域 + 模型选择快照 + Inbox + consumed-work |
| `dsh-agent-loop` | core/agent-loop | **核心运行时**：turn/step 驱动状态机 + llm 流 + tool 调度 + 取消 + request 重建 |
| `dsh-interaction` | interaction/*5 | 审批链：user-approval + permission-presets + commands + user-questions + tool-ask-user |

### 2. 第一性原理分解

1. **「agent 驱动」的本质** = 一个确定性状态机：`turn/start → step/start → (user/message
   → llm 流 → assistant/chunk… → assistant/message → tool/call → tool/result)× → step/end
   → turn/end`，每一步**可重放**、每事件**可审计**。→ 核心是纯函数 + 事件构造，
   必须可单测穷举（agent-loop spec 19 个文件就是状态机测试）。
2. **session log 是唯一事实源**（PLAN §1.2 第二原理）：模型请求 = `deriveMessages +
   foldRequestHeader` 的**字节重建**，任何模型可见输入先落日志。→ dsh-agent-loop 的
   `buildRequest` 与 dsh-system-prompt 的渲染是「重建不变量」的载体。
3. **能力缝 = Service Definition / Provider / Consumer 三位一体**（PLAN §1.2 第三原理）：
   tools 不是数据表而是缝——注册表 + staged 执行管线（pre-execute/execute/post-execute
   waterfall）+ 呈现模式（native/code/both）+ Code Mode 传输。
4. **模型可见 ⟺ 已记录**：max-tokens 粘性、取消的合成 tool result（`ABORTED_BEFORE_
   DISPATCH`）、request/header 锚点（initial/resume/change）都是这一不变量的推论。
5. **审批链是「策略先行、fail-closed」的并发小状态机**：`'never'` 策略在 dispatch 前
   判定不可旁路；无答案者/答案者抛错/越界值一律收敛 `unavailable`；`approval/asked ↔
   decided` 审计对必须 turn-enclosed 且一一对应。
6. **作用域是注册可见性不是权限**：`ScopeKey` 是纯内存身份（引用相等、从不序列化），
   父子链一条关系双向驱动（向下继承 layer、向上准入事件）。

### 3. 自顶向下（Top-down）：M2 交付物分解

```
M2a dsh-scope        <- 依赖：无（零依赖纯语义）
M2b dsh-tools        <- 依赖：dsh-scope（逐 agent 遮蔽）、dsh-llm（ToolSchema/Message）
M2c dsh-system-prompt<- 依赖：dsh-scope（assemble 是 scope-filtered 事件）、dsh-session、
                         dsh-llm、dsh-tools（tools 目录）
M2d dsh-agent        <- 依赖：dsh-scope（InitiatorScope）、dsh-session（事件/UserMessage）、
                         dsh-llm（LlmCallConfig/ToolSchema）
M2e dsh-agent-loop   <- 依赖：dsh-agent + dsh-scope + dsh-tools + dsh-system-prompt +
                         dsh-session + dsh-llm（依赖密度最高，最后编码）
M2f dsh-interaction  <- 依赖：dsh-agent(dsh-scope) + dsh-session + dsh-llm + dsh-tools +
                         dsh-api（remotes 形状）+ 依赖缝注入（sandbox/shell 为 M5 缺口）
M2g 集成（web/Boot） <- 依赖：上面全部（可交付的优先级最低，用于 E2E 冒烟）
```

### 4. 自底向上（Bottom-up）：现有资产核实

- `dsh-session`：`SessionEvent`/`EventKind`（48 词表，含 approval/asked、command/run、
  command/done、permission/preset、agent/inbox/spliced 等 M2 事件）+ `SessionStore`/
  `Runtime`/`deriveMessages`/`foldRequestHeader`。**齐全**。
- `dsh-llm`：`Message`/`ContentBlock`/`StreamChunk`/`TokenUsage`/`ToolSchema`/
  `GenerateOptions`/`LlmCallConfig`/`LlmRuntime`（prepareCall/stream/registerAdapter）。
  **齐全**。
- `dsh-brand`：`SessionId`/`CallId`/`MessageId`/`ProviderRequestId` 等 Newtype。**齐全**。
- `dsh-core`：Cordis 等价物（effect/on/emit/waterfall/serial/bail）+ `ToolRegistry`
  （薄注册表，M2 升级为 `dsh-tools` 真缝）。
- `dsh-api::spec`：方法面（approval.*、commands.* 等 remotes 形状在 M3 接）。

→ M2 只需**引用** M0/M1 类型；所有实现落在新增的 6 个 crate。

### 5. 需求结论（目标 / 非目标 / 假设 / 约束 / 边界 / 验收）

**目标（M2 内）**
- `dsh-scope`：ScopeKey 身份/父链/bindScopeParent 环检测/scopeTarget carrier/ScopedLayers
  （NamedEntries/AnonymousEntries + 全局+精确层聚合/effect 时序）+ 26 事件 subject 表。
- `dsh-tools`：ToolDefinition/schema 规范化/JSON Schema 生成/ts-types/py-types 代码生成/
  code-mode（run_code）/execution-mode/signals/presentation + 注册表（restrict/shadow/
  guard/staged scheduler [TOOL_RUNTIME_SCHEDULER]）+ pre/post-execute 决策。
- `dsh-system-prompt`：注册表 → assemble → PromptAssembly → render（稳定升序 section +
  tools 字典序/toolOrder + 严格 `{{var}}` 插值 + cloak 头 `Current runtime context…`）。
- `dsh-agent`：Agent 接口/注册表/AgentOptions/Inbox（splice 标准化算术）/model-selection
  （current+assembled 双快照）/consumed-work 折叠/initiator 作用域。
- `dsh-agent-loop`：AgentLoop（turn/step 状态机/llm 流/BlockAssembler 组装/request-header
  锚点/request-context 去重/tool 调度滚动池 `maxParallelToolCalls`(默认10)/取消合成/粘性
  max-tokens/resume）。
- `dsh-interaction`：approval 状态机（fail-closed 三定律 + `'never'` 不可旁路 + abort→
  cancelled 丢弃迟到答案）/permission-presets（sandbox+approval 旋钮折叠）/commands
  （admission miss 零日志 + run↔done 配对）/user-questions（唯一 provider + 7 错误码）/
  tool-ask-user（snake↔camel 翻译）。

**非目标（明确排除，防扩散）**
- 不做 `dsh-sandbox(-policy)`/`dsh-shell`/`dsh-settings`/`dsh-session-projection`/
  `dsh-attachment` 的实现——它们是 M5 范围；M2 只在 interaction 的依赖缝上**注入回调**
  或承载缝形状（如 permission-presets 读 sandbox.mode 的 seam）。
- 不改 dsh-session/dsh-llm 的既有语义类型面。
- 不建 ts-host 差分编排（M5 范围，继承 D-022 结论）；M2 语义以 in-crate golden /
  逐字文本锚定「可差分」。
- dsh-agent-loop 不引入异步运行时/多线程于核心——单线程 `Rc` 纪律 + 服务层线程桥
  （继承 D-004/D-006）。

**假设**
- 前端只经 /api 与 WS 感知宿主；agent 事件（`agent/status` 等）Rust 宿主可先内部派发，
  M3 再接 web 方法面。
- 宿主插件 = Rust/WASM；无第三方 JS 插件 → 作用域路由主要服务我们自己的消费方。

**约束**
- `cargo test --workspace` 全绿 + clippy 零告警（-D warnings）为每子步门禁。
- 错误消息/模型可见文本/wire 可选字段缺省即省略，必须逐字对齐（见各报告 §迁移要点）。

**边界（不变量）**
- 每次 `approval/asked` 恰一个 `approval/decided`；审计事件必在 open turn 内。
- request 可重建：模型请求（messages + system + tools + header）能从 session log 重建。
- tool 调度：abort 必为未启动 call 写合成 result；scheduler 内部失败不伪造结果。

**验收标准（M2 结束时逐条核对，见 M2-ACCEPTANCE.md）**
1. `cargo test --workspace` 全绿 + clippy 零告警。
2. 每个新 crate 的语义测试以 analysis/m2 报告为清单（目标：scope 23、tools 30+、
   system-prompt 12+、agent 12+、agent-loop 30+、interaction 30+）。
3. 逐字不变量：错误消息 / 系统提示模板 / constants（如 `TOOL_ABORTED_BEFORE_DISPATCH`、
   `DEFAULT_MAX_PARALLEL_TOOL_CALLS`）/ wire 形状以 in-crate 文本/JSON golden 锚定。
4. 集成：`dsh-cli` Boot 可构造 agent-loop 并驱动一次 turn 产生 typed 事件链
   （turn/start→…→turn/end），E2E 冒烟（prompt 仍工作、事件下链不回归）。
5. DECISIONS.md 追加决策条目（M2a 已 D-023；后续每子步一条）；git 提交可互查。

---

## 第二部分：系统设计

### 6. crate 划分与依赖序

```
dsh-scope（零依赖）
   └─ dsh-tools      dsh-system-prompt      dsh-agent
        │                  │                    │
        └──────────────────┴────────────────────┴───▶ dsh-agent-loop ──▶ dsh-interaction
```
- `dsh-scope` 不依赖任何包（纯语义身份 + 迷你派发）。
- `dsh-tools` 引用 dsh-scope（scopeKey 遮蔽）与 dsh-llm（ToolSchema/ContentBlock）+ serde_json。
- `dsh-system-prompt` 引用 dsh-session/dsh-llm/dsh-tools/dsh-scope。
- `dsh-agent` 引用 dsh-session/dsh-llm/dsh-scope。
- `dsh-agent-loop` 引用上面全部（含 dsh-agent）。
- `dsh-interaction` 引用 dsh-agent/scope/session/llm/tools，依赖缝（sandbox）用 `Rc<dyn Fn>` 注入。

### 7. 模块划分（每 crate）

- **dsh-scope**：`lib.rs`（ScopeKey/父链/scopeTarget/ScopedContext/Scope）、`store.rs`
  （NamedEntries/AnonymousEntries/ScopedLayers）、`invariant.rs`（26 事件表 + dispatch 检查）。
- **dsh-tools**：`schema.rs`（ValueSchemaSpec→JSON Schema + validateArgs）、`types.rs`
  （ToolDefinition/执行；注册表 web 面）、`runtime.rs`（ToolRuntime：restrict/shadow/
  guards/staged scheduler）、`code_mode.rs`、`presentation.rs`、`sdk_gen.rs`（ts/py 生成）。
- **dsh-system-prompt**：`types.rs`（PromptSection/PromptContext/PromptAssembly）、
  `assemble.rs`、`render.rs`、`variables.rs`、`cloak.rs`、`invariant.rs`。
- **dsh-agent**：`agent.rs`（Agent 接口/注册表）、`inbox.rs`、`model_selection.rs`、
  `consumed_work.rs`、`initiator.rs`、`invariant.rs`。
- **dsh-agent-loop**：`agent.rs`（ReactLoopAgent 相态机）、`turn.rs`（step 驱动）、
  `request.rs`（buildRequest/header 锚点）、`tool_calls.rs`（调度器）、`runtime_context.rs`、
  `constants.rs`、`invariant.rs`。
- **dsh-interaction**：`approval.rs`、`policy.rs`（permission-presets）、`commands.rs`、
  `user_questions.rs`、`tool_ask_user.rs`、`invariant.rs`。

### 8. 关键设计决策（预告；逐条记录入 DECISIONS.md）

- **D-023（已结）**：scope 以身份句柄 + 迷你派发适配，不动 dsh-core 内核。
- **D-024（M2b 预定）**：dsh-tools 与 dsh-core::tools::ToolRegistry 关系——dsh-core 的
  薄表保留给 WASM loop 缝；dsh-tools 是生产缝，Boot 侧以适配桥把 WASM-loop 的
  声明/执行代理到 dsh-tools（若 M2g 需要）。
- **D-025（M2c 预定）**：cloak 头/`{{var}}` 插值以**逐字节文本 golden** 锚定。
- **D-026（M2e 预定）**：agent-loop 用同步 llm 流（Box<dyn Iterator<Item=StreamChunk>>）
  + 取消令牌（Cell<bool>）模拟 AbortSignal；服务层线程桥在 M2g。
- **D-027（M2f 预定）**：interaction 依赖缝（sandbox/shell/settings/projection）以
  `Rc<dyn Fn>` 注入，M5 前不做实现。

### 9. 构建序（自底向上收口，每子步 TDD）

```
M2a: dsh-scope（✅ D-023，23 测试）
M2b: dsh-tools（schema → types → runtime → code-mode → presentation → sdk-gen）
M2c: dsh-system-prompt（assemble → render → variables → cloak → invariant）
M2d: dsh-agent（registry → inbox → model-selection → consumed-work → initiator）
M2e: dsh-agent-loop（constants → agent → request → tool-calls → turn → runtime-context）
M2f: dsh-interaction（approval → permission-presets → commands → user-questions → tool-ask-user）
M2g: 集成（Boot + web.rs 冒烟）→ M2-ACCEPTANCE 收口
```

> 每子步：先写失败测试（移植报告测试清单），再最小实现通过，保持测试全绿；
> clippy 零告警；提交 + DECISIONS 条目互查。差分门禁：逐字文本/JSON golden。

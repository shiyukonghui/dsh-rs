# M6 需求分析（第一性原理 + 双视角）

> **阶段一（需求分析）产出**：目标/非目标/假设/约束/边界/验收标准；**不下手写实现**。
> 方法论归属：第一性原理（剥到不可再分的基本事实与根本目的）+ 自上而下/自下而上双向
> 校验。范围经用户裁定：**P1 两者都要**（服务器执行闭环为主轴 + M5R §5 待办篮按子步穿插）、
> **P2 工作区 root = web 配置指定，缺省 CWD（canonicalize）**、**P3 无 LLM 凭据 → fail-loud
> 明确报错（不伪造）**，并注入真实测试 LLM 端点（见 §5 约束 ⑤ / §6 裁定）。

## 第一部分：需求分析

### 1. 根本目标

M6 的根本目的（第一性原理）：**让 dsh-rs 服务器真正能跑一个 agent 回合**——把已在 M4/M5
备齐的执行面接进 `dsh web` 的真实服务器执行环：前端发 `agent.turn`/`session.prompt` →
Rust `AgentLoopHost`（真实 LLM 签发文本/工具调用）→ `ToolRegistry` 解析并**真实执行**
M4/M5 工具（todo/jobs/schedule + fs/terminal/shell/bash/run_code）→ M5Host 宿主句柄 →
结果回注会话 → 沙箱/mode 投影 + 定时推进落事件 → 前端经既有 `session/event` 下链读到
同一读模型。这是 D-076 明指的「M6 serve 里程碑」。

为什么是它：M5 收口时（D-076）诚实记录了两个边界——「M5 工具进 CLI `serve()` 服务器执行
环属 M6」、且生产 `serve()` 的 `boot.agent_loop = None`（agent 循环工具仅测试注册）。不把
执行环接真，前端永远只能发会话语料、无法驱动真实工具。所以 M6 的第一性命题 = **从「会话语料
可收发」推进到「agent 闭环可驱动」，让 M1‥M5 的全部能力在服务器上变成可被真实 LLM 使用的
交付物**。

**并存（P1 裁定）**：M5R §5 明确推给 M6 的待办篮（settings 配置面 / .env 解析 / provider 能力
做实 / mcp·acp·hooks·skill / ts-host 差分 / SQLite）**按子步穿插**进主轴，凡服务于「执行闭环
的上游/下游」者优先（settings/.env 供 LLM 凭据与工作区配置、provider capabilities 供模型列录），
生态缝（mcp/acp/hooks/skill）与基建（ts-host 差分/SQLite）作为独立子步排在主轴之后。

### 2. 第一性原理分解

1. **服务器闭环 = 已有积木的组合，不是新引擎**。`AgentLoopHost{store, bus, registry, llm,
   tools, prompt, agents}`、react-loop driver、`run_rust_loop(boot, session, text)`、
   `M5Host::assemble`、M4/M5 `register_*_with_host`、M5g tick、EventSink 全部已存在。唯一缺口 =
   **服务器侧没有把 register_m4 + register_m5 + 宿主 + LLM 组装进 AgentLoopHost 并放进
   `boot.agent_loop`**。第一性判断：M6 主轴是「装配（wiring）」，工程风险在组合语义而非新算法。
2. **工具即注册表视图**。agent 能调用什么 = 注册进 `host.tools`（`Rc<ToolRegistry>`）什么。
   前端看到什么 = `view`。M6 要保证注册表在服务器与前端之间**单一事实源**（注册 + view 下 kebab
   schemas），无第二份工具清单。
3. **宿主必须随服务器生命周期**。fs/shell/bash 共享 `workspace_root`；bash 后台 job、
   terminal 会话、tick 线程都是带资源的宿主状态——服务器退出必须 disposer 清理（kill jobs、
   close terminals、stop tick），否则孤儿进程。第一性：宿主生命周期 = 服务器生命周期。
4. **定时推进必须落服务器主循环**（非手工）。M5g tick 线程只发信号；serve 请求循环 eat tick →
   `tick_once(schedule, bridge)`。这是验收 #7 语义在服务器面的自然延伸。
5. **沙箱/mode 必须是循环内事实**。`resolve_sandbox_mode(approved, session events)`（D-075）+
   `sandbox:policy` 系统提示段（order 110）注入 prompt——agent 决策与工具执行的围栏语义一致，
   不双轨。
6. **真实 LLM 需要诚实凭据路径**（P3）：key 经 `DEEPSEEK_API_KEY` 环境变量→服务层桥 HTTP 头；
   无 key → `agent.turn` fail-loud 明确报错（不伪造、不静默降级）；工具注册与 API 面照常可用。
   测试连接（base_url/model/key）由用户提供注入，**key 永不进 git**。
7. **every model-visible ⟺ logged**（沿 D 纪律）：agent 回合、工具调用、结果、模式投影全部落
   会话事件；前端读模型 = 会话事件投影，无旁路状态。

### 3. 自顶向下（Top-down）：M6 交付物分解

**主轴 1-6（服务器执行闭环，M6 验收硬项）**

| 子步 | 交付物 | 通过条件 |
|---|---|---|
| step1 服务器装配工厂 | `serve()`/`dsh web` 装配 `AgentLoopHost`（LlmRuntime + ToolRegistry: register_m4_with_host + register_m5_with_host(M5Host) + workspace_root）+ 置入 `boot.agent_loop`；`agent.turn/agent.run/session.prompt` 真驱动 | RPC 集成测试：携带真实工具注册的循环真执行一轮（stub LLM + 微工具） |
| step2 宿主生命周期 + 清理 | `WebConfig.workspace_root`（缺省 CWD canonicalize，P2）；M5Host disposer（kill jobs/close terminals/stop tick）挂 serve 退出钩子 | 退出无孤儿进程（bash bg/terminal/tick 全清） |
| step3 M5g tick 注入 serve | serve 请求循环 eat tick → `tick_once(sched, bridge)`：调度到期 + jobs 自动 settle | 真实定时推进：after-schedule 自动派发、bash bg 自动 settle（非手工） |
| step4 沙箱/mode 投影进循环 | `sandbox:policy` 段注入 prompt（order 110）+ `resolve_sandbox_mode` 生效；escalation 工具面接入（`sandbox_permissions`+`justification` 校验，缺省无审批通道 fail-closed） | 投影段内容快照 + escalation 校验用例 |
| step5 LLM 后端装配 + 诚实无 key | `DeepSeekConnection{base_url, model, ...}` 经配置/env 解析；key 仅 `DEEPSEEK_API_KEY`；无 key → `agent.turn` fail-loud（明确错误码/消息） | 无 key 路径 fail-loud 测试 + 真实端点冒烟（用户提供） |
| step6 前端最小闭环 | `agent.turn` → 事件（`/api/events.mux` host downlink）→ `session.history` 可读同店 | RPC 层集成测试：一轮真实循环后 history 含 turn/工具事件 |

**穿插篮 7-12（P1 裁定纳入，按相关性排后）**

| 子步 | 交付物 | 通过条件 |
|---|---|---|
| step7 settings/.env 服务层 | settings YAML 注释保真 leaf-diff（M5R §5 ③）+ `.env` 解析（M5R §5 ④）；供凭据/工作区/模型缺省读取 | settings 读改写保真测试 + .env 键注入测试 |
| step8 provider capabilities 做实 | 把「能力登记 + NO_START_CAPABILITIES」（M5R §5 ②）落地为真实 provider 列录（DeepSeek 已具备态模型 + 容量/重试/模式） | provider/models RPC 返回真实列录 |
| step9 hooks/skill 生态缝 | hooks（pre/post-execute 宿主钩子）+ skill（system-prompt 段）真实缝；mcp/acp 能力登记缝出口（真实 link 留生态） | 缝单测 + 熵销（NO_START_CAPABILITIES 保持诚实） |
| step10 ts-host 差分 + SQLite | ts-host 差分编排（M5R §5 ⑤）+ SQLite backlog 裁决与落地（持久化面；M5 用 JSONL 过渡） | 差分对齐测试 + SQLite 落盘/回读测试 |
| step11 M6-ACCEPTANCE | 全量 test + clippy + DECISIONS 互查 + git 闭环 + 真实 LLM 冒烟报告 | 验收全绿 + 冒烟证据 |

### 4. 自底向上（Bottom-up）：现有资产核实（M6 需求阶段实测）

- **AgentLoopHost 已备**：`dsh-agent-loop/src/host.rs` `with_store(config, llm, tools, store)` 持
  `store/bus/registry/llm/tools/prompt/agents`；`ensure_agent` 幂等装配 react agent；事件写共享
  store。**tools 由调用方传入**——服务器装配点即 M6 注册缝。
- **服务端正门存在**：web.rs RPC 面含 `agent.run/agent.turn/session.prompt/llm.models/
  subagent.prompt` 等；`agent-loop|agent.turn|agent.run` 分支：`boot.agent_loop.is_some() →
  run_rust_loop`，否则 `run_turn`（cordis loop 插件）。**生产 `dsh web` 的 boot.agent_loop=None**
  （lib.rs Boot 默认 None；仅测试装配）。
- **工具注册资产**：`register_m4_tools_with_host`（web.rs:1413）与 `register_m5_tools_with_host`
  （web/web_m5.rs）均公开；**当前只在测试调用**。生产装配点 absent。
- **宿主资产**：`M5Host::assemble(root)`（D-074）一次构造 terminal/fs/shell/bash_jobs/code；
  `BashJobsBridge` pump；`M5gTick` + `m5g_tick_once(sched, bridge, now)`（D-072）；`resolve_
  sandbox_mode`（D-075）；`sandbox_policy_segment`（D-074）。
- **LLM 资产**：`dsh-llm-deepseek` `DeepSeekAdapter{ resolve_connection, resolve_payloads }`；
  `DeepSeekConnection{ base_url(+ /chat/completions), defaults, max_tokens, default_context_
  window, models, retry_policy }`；**api_key 不在 connection、在服务层桥的 HTTP 头**（M1e 线程
  桥）→ key 天然不上 git。真实测试端点由用户提供（§6）。
- **事件下链**：serve 已建 `EventSink`（SSE/WS downlink）；`session.history`/`session/event`
  与 loop 共享 store 即前端读模型（M4h 已证）。
- **约束型事实**：Windows 主战场（树级 kill=taskkill /T /F + win_job；bash 门控 Git Bash；
  python 门控 `python_available()`）；workspace 全量 187 组测试绿；clippy `-D warnings` 零告警。

**双向相遇点**：自上而下要「服务器闭环」，现有积木齐备，唯一上线缺块 = 装配工厂 + 生命周期 +
tick/sandbox 挂入 dispose 通道；自下而上确认无一需要新引擎，全部是既有缝的落点。冲突点
调节：无——设计决策来自装配语义与清理语义。

### 5. 目标 / 非目标 / 假设 / 约束 / 边界 / 验收标准

**目标**
1. `dsh web` 服务器装配真实 AgentLoopHost（M4+M5 工具 + M5Host + workspace_root），
   `agent.turn/agent.run/session.prompt` 真驱动 agent 回合、工具真实执行。
2. 宿主随服务器生命周期（disposer 清理：bash bg/terminal/tick，无孤儿进程）。
3. M5g 定时推进落服务器主循环（调度自动派发 + jobs 自动 settle，非手工）。
4. 沙箱/mode 投影进循环（`sandbox:policy` 段 + `resolve_sandbox_mode` + escalation 校验）。
5. 真实 LLM 端点可测（用户注入 base_url/model/key@env），无 key → fail-loud。
6. 玩家待办篮按子步纳入（settings/.env → provider → hooks/skill → ts-host 差分/SQLite）。

**非目标**
- 不做新执行引擎/新工具集（M5 全部为可复用资产；新增仅装配/缝层的编排代码）。
- 不做 mcp/acp 的真实外部链接（仅能力登记缝出口，真实 link 留生态）。
- 不写前端新 UI（前端最小闭环以既有 RPC + 事件下链为准，UI 属 web 路线图阶段 1-4 的后续子步）。
- 不重开 tools/session/execution 的语义（差分对齐先行，不另立事实源）。

**假设**
- 服务器单线程宿主模型不变（tools/host 非 Send 留主线程；tick 线程仅发信号，沿用 M5g）。
- 前端 dist 存在（serve 已校验 index.html）；`dsh web` 为真实入口。
- 每次测试冒烟期间，用户注入的 LLM 端点可达；key 经环境变量注入（不落盘、不进 git）。
- workspace_root 目录服务器可读写（本地主机语义）。

**约束（硬性）**
- `DEEPSEEK_API_KEY` 为 key 唯一来源；**key 永不进入仓库/配置/DECISIONS/git history**。
- clippy `-D warnings` 零告警 + 全量 `cargo test --workspace` 绿（沿用验收 #1 纪律）。
- run_code 嵌套 tools 派发、read_image 解码仍为诚实渐进项（D-069/D-073），本轮不伪造。
- 无凭据时工具注册与 API 面照常用，仅 agent 回合 fail-loud。

**边界**
- 真实 LLM 冒烟 = 可选门控（端点可达才跑；不可达 → 记录 skipped，不阻塞验收）。
- M6 服务器装配工厂 = real host 的装配点；`serve()` 的请求循环语义不变。
- 沙箱真实围栏仍以 fs 进程内围栏为准（DIV，D-0xx）；M6 投影到循环，不重做围栏。

**验收标准**
1. `cargo test --workspace` 全绿；clippy `-D warnings` 零告警。
2. **服务器装配工厂**：RPC 集成测试驱动真实 AgentLoopHost——注册表含 M4+M5 工具（
   `register_m4_with_host` + `register_m5_with_host(M5Host)`），一轮循环执行真实工具
   （stub LLM 逐句驱动），事件落共享 store。
3. **宿主生命周期**：装配 factory 关停时 bash bg/terminal/tick 全清理（无孤儿进程断言）；
   `workspace_root` 缺省 CWD canonicalize + 可配置锚定（P2）。
4. **定时推进落服务器**：serve 请求循环 eat tick → `tick_once`：schedule after → 自动派发、
   bash bg → 自动 settle（非手工，句子级断言）。
5. **沙箱投影进循环**：`sandbox:policy` 段注入 prompt（order 110）+ `resolve_sandbox_mode`
   生效；escalation（`sandbox_permissions` + `justification`）工具面校验 fail-closed。
6. **诚实无 key**：无 `DEEPSEEK_API_KEY` → `agent.turn` 返回明确错误（code/message 可读），
   不伪造、API 面照常；有 key + 真实端点（门控）→ 冒烟一轮真实 agent 回合。
7. **穿插篮达成**：settings/.env、provider capabilities、hooks/skill 缝、ts-host 差分/SQLite
   各子步有实现 + 测试 + D 条目（P1 裁定纳入）。
8. 每子步 DECISIONS 对应条目 + git 提交可互查；M6-ACCEPTANCE 全量互查闭合。

### 6. 关键决策点（阶段关卡已裁定）

> 复盘 2026 用户裁定 + 既有 D 记录，全部采纳为最终裁定：
> - **P1 范围 = 两者都要**：主轴（服务器执行闭环 step1-6）+ 待办篮穿插（step7-10）。验收硬项
>   为主轴；穿插篮各子步独立验收。
> - **P2 workspace_root** = `WebConfig.workspace_root`，缺省 = 服务器进程 CWD canonicalize。
> - **P3 无凭据** = fail-loud 明确报错（不伪造）；工具注册/API 面照常。
> - **P4 测试 LLM 端点（用户注入）**：base_url `http://100.105.152.101:18080/v1`、model
>   `deepseek-v4-flash-0731-ext`、key 由 `DEEPSEEK_API_KEY` 环境变量提供（**key 本体不入库**）。
>   DeepSeekAdapter `DeepSeekConnection{base_url, models(catalog≥该 model), ...}` + 服务层桥
>   在 HTTP 头带 `Authorization: Bearer <key>`。
> - **D-024/D-068/D-069/D-073 沿用**：注册表保留名/诚实 NOT_BOUND/read_image 与嵌套派发渐进。
>
> 此文件为 M6 阶段一（需求分析）关卡产物；经此裁定的文档作为「需求结论」，进入阶段二（系统
> 设计）时不再重新发散需求。

---

## 第二部分：阶段结论

M6 需求结论文档成立：目标=服务器执行闭环（让 M1‥M5 全部能力在 `dsh web` 上被真实 LLM 驱动 +
宿主生命周期 + 定时推进 + 沙箱投影 + 诚实凭据），非目标明确（不造新引擎/不铺生态链接），待办篮
按子步穿插，验收 #1-8 可测，关键决策 P1-P4 已裁定。进入阶段二（系统设计）前，此工件经用户放行。

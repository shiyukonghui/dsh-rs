# PLAN：DeepSeek Harness 逻辑全量迁移规划（UI 复用）

> 目标：把 DeepSeek Harness 的**宿主侧逻辑**从 TypeScript 全量迁移到 Rust；
> **UI 与浏览器端 client 插件完全复用现有 harness**（`deepseek-harness/` 已构建前端 + `client/*` 40 个 web 插件）。
> 本文档是第一轮完整规划：先以**第一性原理**澄清需求（§1），再以**自顶向下**从 UI 所依赖的宿主契约面倒推（§2），
> 以**自底向上**从现有 Rust 资产（crates/）逐层扩展（§3–§4），给出 **M1 详细方案**（§5）与**里程碑路线**（§6）、
> **验证策略**（§7）、**架构决策记录**（§8）。决策点已向用户确认并于 §8.1 结案。

---

## 1. 第一性原理：需求澄清

### 1.1 最终目标的本质

「把 DSH 逻辑迁移到 Rust，UI 复用现有 harness」这句话拆开是三件事：

1. **DTO/实体与业务语义**（SessionEvent、Surface、Message、LlmFailure、CompactionResult…）——这些是**数据 + 纯函数**，
   是迁移的**可差分主体**（TS 与 Rust 各跑一遍，trace 逐行一致）。
2. **运行时**（Cordis fiber/effect 生命周期、loader 事务、event 分派）——**Rust 已迁移并差分验证**（dsh-core）。
3. **宿主对外契约**（/api RPC、WS/SSE 事件下链、`__DSH_BOOT__`、`/plugins/*` bundle）——**浏览器 UI 唯一能感知 Rust 宿主的接缝**，
   必须与 TS 宿主**逐字节兼容**，才能让现有前端不改一行就工作。

用户已确认：**只迁宿主侧，UI/client 全复用**（决策 Q1）。因此「等价」的精确判据是：
**同一 cordis.yml 配置 + 同一操作序列下，Rust 宿主经 /api 与 WS 对前端产生的可观察行为（响应 value、事件流、boot 面）与 TS 宿主一致。**

### 1.2 关键第一性原理（从 DSH 架构文档提炼）

1. **一切皆插件**：模型适配器、工具注册表、session 日志、agent loop 都以插件形式存在，可从配置替换。→ Rust 宿主必须是 Cordis 语义运行时 + 插件仓库，且宿主插件 = Rust 原生 / WASM（决策 Q2）。
2. **Session 日志是唯一事实源**：模型历史由日志派生（`deriveMessages()`），绝不单独存。任何新行为若影响模型可见输入，必须新增 session event。→ Rust 的 `SessionLog` 必须承载完整的 `SessionEventMap` 语义（§5.2）。
3. **能力缝 = Service Definition / Provider / Consumer 三位一体**：一个能力是三个角色，缺一不可。→ Rust 每个迁移包要以"能力缝"为单位交付，而非只搬数据。
4. **模型可见 ⟺ 已记录**：任何到达模型请求的内容必须能从日志重建，运行时不变量校验。→ Rust session/llm 需要对应的重建不变量（差分场景可固化）。
5. **持久化是缝不是实现**：`ctx.sessionPersistence` 抽象 seam，JSONL/SQLite 是可互换后端，`session/event` 同步通知 + flush 检查点做批量落盘。→ Rust 按缝搬，后端原生实现（决策 Q6）但仍保留 seam 抽象。
6. **API 面是生成契约**：TS 宿主用 `@Remote` 装饰器 + Typert type-graph 生成 Host/Client 契约与 codec；运行时走 `连接层(/api + WS)`。→ Rust 无法复用 TS 的 Typert 生成器（无 Node/TS 类型图），必须**以生成产物为契约规范**自建 Rust 侧 RPC 描述与校验（§3.4）。

### 1.3 需求边界（自顶向下倒推出的完整宿主面）

浏览器 UI 要向 Rust 宿主请求以下能力（按 `client/*` 插件与前端代码实际调用划分）：

| 前端调用面 | 宿主须提供 | 现状（Rust） |
|---|---|---|
| boot：`__DSH_BOOT__`、`/plugins/<id>/client.js`、`host.describe` | boot manifest 注入 + bundle 静态服务 + host 描述 | ✅ 已有（D-005，扫描 plugin_root 组装 manifest） |
| `/api/<ns>/<method>` 传输 | client-request/server-response 信封 + trust fence + 取消 | ✅ 信封/WS 已有；**方法面仍是空桩为主** |
| `events.mux`/`events.host` 下链 | session 事件实时推送（WebSocket） | ✅ 已有（WS/SSE 双轨，推 `session/event` 帧） |
| `ctx.remote.*` 生成方法（Typert Remote） | 大量命名空间方法（session/goal/subagent/settings/…） | ❌ 仅少量真实实现，多数空桩/non-implemented |
| session 事件流（turn/start…turn/end） | 完整 `SessionEventMap` 语义 + 投影 + 持久化 | ⚠️ 薄实现（kind+payload bytes），无生产语义 |

**结论**：前端契约面（传输、boot、事件下链）已就绪；**缺的是契约面背后的完整宿主逻辑**，这正是 M1 及后续里程碑的主战场。

---

## 2. 自顶向下：UI 所依赖的宿主契约面（顺序：从外壳到内核）

### 2.1 传输/安全层（已就绪，需回归保证）
- `POST /api/<method>` 信封、`GET /api/events.mux|host` WebSocket、trust fence（loopback Host）、`--web-root`/`--port`。
- **契约**：对齐前端 `clientRequestSchema`/`serverResponseSchema`/`muxFrameSchema`。已有 25 个方法 shape 单测 + 真实浏览器 E2E（D-008）。

### 2.2 Remote 方法面（契约规范源 = 生成产物）
TS 侧权威 = `packages/api/remotes` 收集的 `/remote` 贡献 + `@deepseek-ai/dsh-client-connection` 的 `UNARY_VALUE_SCHEMAS` + Typert 生成描述符。
Rust 侧要做的：
- **先建"契约仓库"**：把一个固定基线 DSH 版本的 `lib/typert.remote-client.*` 生成产物与 `UNARY_VALUE_SCHEMAS` 以 JSON 形式做进 `crates/dsh-api`（生成物转译，不手抄），作为 Rust RPC 描述的**单一权威**。
- Rust `dispatch` 按该仓库：方法名 → 参数 codec（zod→Rust 校验）→ lookup 解析（agent/session id → 对象）→ 业务方法 → 返回值校验。
- **用途对齐**：`agentId`/`sessionId` lookup、`signal: AbortSignal` 取消参数（Rust 用取消令牌）、`@RemoteScope` 的 agent-scoped 调用。

### 2.3 会话事件下链（M1 核心契约）
前端渲染依赖的会话事件流（`session/event` 帧）必须携带生产 `SessionEvent` 语义。Rust `SessionEvent` 目前是 `{kind, payload: bytes}`——这是**缝传输形态**，但**语义层必须是生产事件单词表**：
- 事件类型全集（core + 插件合并）：`turn/start,end`、`step/start,end`、`user/message`、`assistant/chunk,message`、`tool/call,result`、`request/header,context`、`session/end-seed`、`todo/write`、`compaction/*`、`hook/*`、`llm/retry`。
- Surface 语义（append/replace + sourceEventSeqs + provenance 校验 + shadowing）已有雏形（M36/M37），**须补**：`request/header` 折叠、`session/end-seed`、`deriveMessages()` 缓存/冻结语义。
- Turn/step 序号、中止原因（`TurnEndReasonMap`）、崩溃恢复 `interrupted` 标记。

### 2.4 前端"可交互"验收（每一里程碑的契约面验收）
真实浏览器 `--dump-dom` 断言：boot 无 loading 卡死 → 侧边栏/模型选择器/发送按钮 → `session.prompt` 驱动 → `session.history` 完整 turn 流 → `events.mux` 实时推帧。此验收贯穿每一里程碑（§7.3）。

---

## 3. 自底向上：Rust 资产盘点与分层架构

### 3.1 现有资产（可直接复用）
| crate | 能力 | M1 相关 |
|---|---|---|
| `dsh-core` | Cordis 运行时（fiber/event/reflect/logger/service)+ session 薄实现 + tools 注册表 + llm HTTP + 定时器 | **扩展为主战场** |
| `dsh-loader` | 配置驱动插件树 + include + HMR | 复用（loop 挂载/Cordis 插件加载） |
| `dsh-schema` / `dsh-eval` | Schemastery 移植 / `!!js` 求值子集 | 复用（配置解析/RPC codec） |
| `dsh-wasmrt` | WASM 插件（C ABI/组件模型/WASI）+ WasmLoopPlugin | 复用（宿主插件形态） |
| `dsh-diff` + `diff/ts-host` | **差分测试基建**（TS 生成 golden → Rust 校验） | M1 验证主引擎（§7） |
| `dsh-cli` | boot/CLI + `web.rs`（/api + WS + trust fence + boot 注入） | 复用（契约面外壳） |

### 3.2 目标分层（新增 crate 规划）
```
crates/
  dsh-session/        # SessionEventMap 语义、Session/SessionSurface、deriveMessages、SessionStore、fork
  dsh-session-query/  # 读模型投影、session-log-export
  dsh-llm/            # Message/ContentBlock、StreamChunk、BlockAssembler、LlmAdapter 缝、DeepSeek 适配器
  dsh-compaction/     # CompactionEngine 缝 + basic 后端 + tool-result-pruner
  dsh-api/            # Remote 契约仓库 + codec + lookup + dispatch 规范化
  dsh-persistence/    # SessionPersistence 缝 + JSONL 原生后端 + 导入工具（SQLite 视 M2+）
  （沿用）dsh-core / dsh-loader / dsh-schema / dsh-eval / dsh-wasmrt / dsh-cli
```
> 划分原则：**每个 crate 对应一个能力缝（Service Definition/Provider/Consumer）**，数据/纯函数与运行时解耦
> （语义类型放 `dsh-session::types` 等，纯函数可差分）；crate 间只经语义类型与 Cordis 服务 seam 通信，不互相 import 实现。

### 3.3 运行时模型（对应决策 Q3）
- **核心不变**：dsh-core 单线程 `Rc<RefCell>` + 收集-再执行纪律（Cordis 语义层），供应链面保持现有 `Arc<dyn Any + Send + Sync>` 服务值。
- **服务层隔离**：IO/进程/网络（fs/sandbox/subprocess/sqlite/HTTP）放独立线程/进程，经信道桥接回单线程运行时
  （现有 `set_spawn`/`set_timer_clock`/`Hmr` mpsc 已是此形态的样板）。
- 宿主插件 = Rust `Plugin` trait + WASM 组件（dsh-wasmrt），不加载第三方 JS（决策 Q2）。

### 3.4 API 契约的 Rust 化（关键工程点）
- **契约仓库转译**：一次性把固定基线的 Typert 生成产物（descriptor + schema）+ `UNARY_VALUE_SCHEMAS` 落成
  `crates/dsh-api/spec/*.json`（脚本生成，从 `lib/typert.remote-client.*` 提取）。
- **运行时**：Rust 侧 `dispatch` 按 spec 做：信封校验 → 参数 zod-schema 校验 → lookup 解析 → 业务方法 → 返回校验。
- **取消**：`signal: AbortSignal` → Rust 取消令牌（当前 `agent-loop` 驱动已有取消语义，扩展为通用）。
- **注意**：不要试图在 Rust 里复刻 TS 类型图/装饰器；**以生成产物为规范**，前端发的请求必须能被该产物校验，反之亦然。

---

## 4. 自底向上的依赖顺序：M0（契约基建）先行

每迁移一个包之前，先把该包依赖的**底层数据面**铺好。M1 依赖序：

```
M1 前置（M0）：
  dsh-session:types       # SessionEvent 语义类型全量（core + compaction + hook 合并扩展）
  dsh-llm:types           # Message/ContentBlock/StreamChunk/TokenUsage/LlmFailure/ToolSchema
  dsh-api:spec            # Remote 契约仓库（基线转译）
  dsh-persistence:seam    # SessionPersistence trait + SessionHeader/SessionInspection/SessionPreparation
```

---

## 5. M1 详细方案：会话/LLM/压缩链路

### 5.1 范围界定（决策 Q4）
**入 M1**：
- `core/session`（事件日志 + surface + deriveMessages + SessionStore + fork）
- `session/*` 持久化链（persistence seam + jsonl 后端 + 导入转换）
- `llm/llm`（Message/StreamChunk/BlockAssembler/LlmAdapter 缝/Runtime）+ `llm/llm-deepseek`（DeepSeek 适配器：流式 SSE、wire 序列化、重试策略）
- `compaction/*`（compaction 缝 + compaction-basic 后端 + compaction-tool-result-pruner）
- `session-query/*`（session-query 读模型 + session-log-export）
- `token-meter/*`（compaction 依赖的 token 估算）

**暂不入 M1（M2+）**：core/agent、core/agent-loop、core/system-prompt、core/tools（现有工具注册表是简化版）、
context/*、interaction/*、settings/credentials/identity/guard、fs/shell/subprocess/terminal/sandbox、goal/subagent/
schedule/jobs/workflow、mcp/acp/spill/hooks/skill/todo/plan/workspace/preset/bundle/attachment/feedback/storage/typert/util。
> 理由：M1 聚焦「前端聊天 + 持久化 + 压缩」能真实工作；agent loop 依赖 llm/session 就绪后再迁，编排类依赖 agent 就绪。

### 5.2 M1a：dsh-session（生产语义）
从现 `dsh-core::session.rs`（薄承载）扩展为：
- **`EventKind` 完整枚举 + 合并扩展点**：用 `serde` tagged enum + 插件扩展（`ignorable: true` 支持；未知必需事件 refuse）。
- **`SessionLog` → `Session` 语义**：`seq=`log.length 连续性契约、`time`、深冻结等价（不可变 `Value`）、
  `requestHeader()`/`requestContext()` 折叠、`deriveMessages()` 缓存 + 新数组快照语义。
- **Surface**：`SurfaceManager`（validate-then-commit）、`replaceGeneration`、provenance（引用唯一/含封面）、
  tool-result rewrite 校验（现有 `tool_result_only_content_changed` 扩展为生产规则）。
- **`SessionStore`**：create/prepare/enter/announce/flush/get/list/fork；`session/created`/`session/disposed`/
  `session/event`/`session/flush` 事件（Rust 内部 parallel/emit 分派）。
- **`TurnEndReasonMap`** + `TurnEndCancelCause`（含 backlog 'legacy' 导入）。
- **不变量 companion**：turn/step 序号、执行事件 enclosu、同 step tool call/result 配对（对应 `dsh-session/invariant`）。
- **差分**：新增场景覆盖 append→surface→deriveMessages→fork→resume 全链（TS 宿主 = vendored/npm `@deepseek-ai/dsh-session`）。

### 5.3 M1b：dsh-llm（流式 + 适配器）
- **类型**：`Message`/`ContentBlockMap`（text/reasoning/image/tool-call/tool-result）、`StreamChunk`（index 关联 + block-end +
  usage + finish）、`BlockAssembler`（增量组装、max-tokens 丢弃 tool-call、interruptedBlocks）、`TokenUsage`（disjoint 计数）、
  `LlmFailure`、`FinishReasonMap`、`GenerateOptions`、`ToolSchema`。
- **缝**：`LlmAdapter`（`stream()` 唯一必实现 + info/listModels/resolveModel/resolveCallConfig）、`LlmRuntime`
  （registerAdapter/listProviders/discoverModels/provideRetryPolicy/listModels/resolveModelInfo/prepareCall/stream）。
- **`llm/stream` waterfall** + `prepareCall` 一次性注册 + replayState 只透传同 adapter 实例。
- **DeepSeek 适配器**：SSE 流式解析（`llm-deepseek/src/sse.ts` 对应）、wire 序列化（`serialize.ts` 全量字段：
  reasoning_content、image 拒绝、stream/stream_options.include_usage、thinking/reasoning_effort、temperature/…）。
  现有 `dsh-core::llm_http` 只是 OpenAI 兼容非流模式——**升级为流式 chunk 语义**。
- **重试**：`llm-retry` 策略（maxRetries/backoff/jitter、EMPTY_RESPONSE、CONTEXT_WINDOW_EXCEEDED）。
- **差分**：chunk 流穷举（TS adapter mock-server 对照），wire 序列化逐字段（覆盖 `serialize.spec`）。

### 5.4 M1c：dsh-compaction（压缩缝）
- **事件扩展**：declaration-merge `compaction/start`/`summary`/`end`（log-only）+ replacement `user/message`（`SurfaceOp::Replace`）
  + `compaction/prune` shadow-price。
- **缝**：`CompactionEngine`（compactIfNeeded/compactNow/compactRegion）+ `toolPairingBalancedBefore/After`；
  `ToolResultPruner`（Unicode 码点裁剪、preserve rich-block 顺序）。
- **后端**：`compaction-basic`（阈值/retained-tail/overflow cap/trigger policy + routed summarization）。
- **关键语义**：锁在 `compaction/start` 先落盘、`compaction/end` 最后释放（crash 孤儿锁可检测）；`session/end-seed` 区分
  上一生命周期遗留的未配对 start。
- **差分**：normal → overflow → prun → summary replacement → 失败路径（busy/cancelled/changed/summary/commit/persistence）。

### 5.5 M1d：dsh-persistence + dsh-session-query（原生存储 + 读模型）
- **seam**：`SessionPersistence`（locate/create/append/prepare/load/inspect/readFrom/list/listSnapshots/readRaw），
  flush batching window（bounded）、crash 修复（interrupted turn/step/tool）、revision token。
- **JSONL 原生后端**：checksummed Zstandard 帧（默认）/raw 行（配置），原子写（tmp+rename+重试——移植生产语义，
  现 `include.rs` 缺失原子写是已知差异），torn-tail 容忍，`supportsRawArtifacts`。
- **导入工具**：`SessionImport`——读取 TS 侧 JSONL/SQLite 产物（基线格式一次转译），经 `Session.fromRestore` 语义导入
  Rust JSONL；SQLite 后端留 M2（决策 Q6：Rust 原生存储，旧数据用导入导出迁移）。
- **session-query**：读模型投影（watermark readFrom）、session-log-export（前端日志导出）。

### 5.6 M1 集成（自底向上收口）
- **agent loop 最小化**：M1 不迁 core/agent-loop，但为了「前端聊天闭环」复用现有 `WasmLoopPlugin`/`run_turn`
  （echo/llm/tool loop）投喂 M1 就绪的 dsh-session/dsh-llm：把当前 `Boot` 的 session 承载从薄 `SessionLog`
  升级为 `dsh-session::Session`（含持久化插件挂载 + flush）。
- **web.rs 方法面**：`session.list/create/history/models/prompt/fork/rename` 从空桩/硬编码升级为真实 dsh-session 驱动；
  `llm.providers/models` 由 dsh-llm 注册表驱动（不再是硬编码 echo/llm/tool）。
- **E2E 冒烟**：真实前端 boot → prompt → history 含 assistant 流式 → 事件实时推送 → 重启后 `--session-in` 恢复会话。

### 5.7 M1 验收标准（每条可验证）
1. `cargo test --workspace` 全绿、clippy `-D warnings` 零警告。
2. **差分**：M1 新增场景（session/llm/compaction/persistence）TS↔Rust trace 逐行一致；旧 16 场景零回归。
3. **契约面**：25+ RPC 方法 shape 测试扩到真实语义（不再空桩）；真实浏览器 `--dump-dom` 阶段验收通过。
4. **流式**：llm 流式 chunk 端到端（本地 mock server）与 TS adapter 输出一致。
5. **持久化**：会话 JSONL（zstd）落盘 → 重启恢复 → 事件/投影/表面一致；崩溃中断 turn 以 `interrupted` 收尾并可恢复读。
6. **压缩**：长会话 overflow → compaction-basic 选出范围 → replace 落 surface → 压缩后 deriveMessages 一致、shadowed seqs 覆盖校验通过。

---

## 6. 里程碑路线（M1 之后）

| 里程碑 | 范围（宿主侧） | 说明 |
|---|---|---|
| **M1** | session 全家桶 + llm + compaction + session-query + token-meter + JSONL 持久化 | 前端聊天/对比/压缩真实工作（本规划主文档） |
| **M2** | core/agent + core/agent-loop + core/system-prompt + core/tools + core/scope + interaction（permission/user-approval/commands） | 真正的 agent 驱动与审批链；tools 从注册表升级为能力缝 |
| **M3** | host/api 全方法面（apiproxy/webserver/frontend-static/plugin-inventory/directory-picker）+ settings/credentials/guard | 把 web.rs 空桩全部做实；配置/凭据持久化 |
| **M4** | goal + subagent + schedule + jobs + workflow + interaction/users + plan/todo | 长任务编排与子代理 |
| **M5** | fs + shell + subprocess + terminal + sandbox + e2b + code-runtime + lsp | 执行引擎与沙箱（服务层线程/进程隔离） |
| **M6** | mcp + acp + spill + hooks + skill + storage + attachment + feedback + bundle/preset/workspace + identity + util | 外部协议与周边集成 |

> 提示：M2 起可用 workflow 工具把多个能力缝并行摊给子代理按同一契约仓库迁移，差分场景作为合并门禁。

---

## 7. 验证策略（决策 Q5）

### 7.1 差分测试（核心语义包）
- **基建**（已就绪）：`diff/ts-host`（scenario-host/loader-host/include-host + verify-diff.mjs）+ `dsh-diff` + `scenarios/*.json/.golden`。
- **扩展**：新增 **session-host.mjs / llm-host.mjs / compaction-host.mjs / persistence-host.mjs**（npm/vendored 生产包权威执行 +
  printf `%C` 展开对齐），对应新场景目录。
- **原则**：纯数据/纯函数/确定性语义包（session/llm-wire/compaction surface/persistence 格式）强制差分；
  真实 IO/网络包（HTTP 客户端、zstd、导入读取）走集成测试（不差分不稳定）。

### 7.2 集成测试（IO/复用面）
- llm HTTP/SSE 真实 mock server 端到端；zstd 编解码往返；JSONL 原子写/读取/恢复；导入转换旧产物。

### 7.3 E2E（真实前端）
- 每里程碑：`dsh web <cordis.yml>` + 真实浏览器 `--dump-dom`/WS 客户端，断言 UI 可 boot、会话可交互、事件实时推送。

---

## 8. 架构决策记录（已确认）

### 8.1 决策摘要（2025 用户确认）
| 决策点 | 结论 |
|---|---|
| Q1 迁移边界 | **只迁宿主侧，UI/client 全复用**（40 个 web 插件 + 前端 dist 保持 TS/JS） |
| Q2 宿主插件形态 | **宿主插件 = Rust 原生/WASM**；不加载第三方 JS 插件（不留 JS 引擎） |
| Q3 并发模型 | **核心单线程 Rc<RefCell> 不动**；IO/进程/网络服务层线程/进程隔离 |
| Q4 M1 优先级 | **session 全家桶 + llm + compaction + session-query + token-meter + JSONL 持久化** |
| Q5 验证 | **核心语义包差分 + IO/集成包集成测试**（+ E2E 契约面验收） |
| Q6 存储 | **Rust 原生存储（JSONL/zstd/SQLite 后续）**，旧数据导入导出迁移 |

### 8.2 影响与回滚
- Q1：改动集中在 crates/；`deepseek-harness/` 只读引用，零回滚风险。
- Q3：M1 不引入多线程于核心；新增 crate 的线程仅在服务层，若崩坏可回退为纯单线程（无回归）。
- Q6：Rust JSONL 为新格式，TS 产物仅经导入工具单向进入；不破坏原版安装。
- 全程以差分/契约/E2E 三道闸守住「UI 不改一行」承诺；任一闸红即为阻断，先修复再合并。

---

## 9. 风险与开放问题

1. **Typert 生成契约转译的维护**：前端随版本演进 Remote 描述符会变。→ 固定基线 + 升级时重新转译（脚本），开放：版本策略。
2. **agent loop 最小化的边界**：M1 用现有 WasmLoopPlugin 实现聊天闭环，而 TS 生产 loop 在 agent-loop 包；两者语义可能渐行渐远。
   → 建议 M2 早迁 agent-loop；M1 只做承载升级，不做 loop 语义扩展。
3. **SQLite 后端时机**：TS sqlite schema 17 是物理分块编码，转译成本高；M1 先用 JSONL，SQLite 视需求 M2+（决策 Q6 已授权）。
4. **取消语义**：AbortSignal 贯穿 session/llm/compaction；Rust 侧需统一取消令牌，M1 必须落地。
5. **深冻结/不可变性**：TS `Object.freeze` 等价在 Rust 用不可变 `Value` + 快照语义复刻，注意 llm-wire/deriveMessages 的共享/拷贝权衡。

---

*本规划基于：deepseek-harness @ 47f943859b（docs/architecture、api-gateway、session、llm-streaming、compaction、persistence、subsystems）+ crates/ 现状 +
用户 6 项决策确认。后续里程碑文档（M2+）在 M1 验收后按同一模板细化。*

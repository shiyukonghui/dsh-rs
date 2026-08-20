# M0 契约基建：需求结论文档 + 系统设计

> 本文件是 `PLAN-rust-full-harness-migration.md` §4「M0（契约基建）先行」的实现工件：
> **阶段一（需求分析）** 产出本文件前半部分的目标/非目标/假设/约束/边界/验收标准；
> **阶段二（系统设计）** 产出本文件后半部分的 crate 划分、依赖序与关键设计决策。
> 决策编号 D-011/D-012/D-013 记录在 `DECISIONS.md`，git 提交可互查。

---

## 第一部分：需求分析（第一性原理 + 双视角）

### 1. 根本目标

M0 的唯一目的：**为 M1（会话/LLM/压缩/持久化链路）铺好底层「数据面」**——
把 M1 各迁移包依赖的**契约类型与缝（seam）**先固化下来，成为 Rust 侧可引用的单一权威，
使 M1 及后续里程碑只写业务实现、不再回头改类型。

### 2. 第一性原理分解（把需求剥到不可再分）

1. **TS 侧是权威**：DSH 的会话事件、LLM 消息、RPC 契约已经存在（`deepseek-harness/packages/`），
   Rust 侧必须与之**逐字节/逐字段等价**（"UI 不改一行" 承诺的根基）。→ M0 的可差分主体。
2. **类型先于实现**：`SessionEventMap`/`Message`/`StreamChunk`/`RpcMethodMap` 都是**数据 + 纯函数**，
   实现（SessionStore、BlockAssembler、dispatch）依赖类型，不反向依赖。→ 类型面可先行独立交付。
3. **合并可扩展（merge-extensible）是硬语义**：TS 的类型图是开放并集（插件可声明合并）。
   Rust 枚举是封闭的；要在 Rust 复刻"可扩展"必须显式建模 **未知分支（Unknown 扩展点）+ ignorable 语义**
   （`SessionEvent.ignorable`：未知必需事件在读取侧必须 **refuse**，未知可忽略事件可 **skip**）——
   这是持久化正确性（reconstruct 不静默错读）的闸门，不是装饰。
4. **缝是三位一体**：持久化是 "Service Definition / Provider / Consumer" 缝。M0 固化
   **Definition 层（trait + 数据结构）**，Provider（JSONL 后端）留 M1d。缝的契约必须能
   让任意后端实现（JSONL/SQLite/测试 mock）无歧义替换。
5. **RPC 契约走"生成物转译"**：Typert 生成产物 + `UNARY_VALUE_SCHEMAS` 是唯一权威；Rust 不为
   TS 复刻类型图/装饰器，而是**把固定基线的契约落成 JSON 仓库**（方法目录 + 错误目录 +
   消息模型 + JSON Schema），dispatch 按仓库校验。M0 只固化仓库，不写 dispatch（M1/M3）。

### 3. 自顶向下（Top-down）：M1 依赖的四块底层数据面

| 来自 PLAN §4 | 交付物 | 服务对象（M1+） |
|---|---|---|
| `dsh-session:types` | SessionEvent 语义类型**全量**（core 13 种 typed + 全 48 词表 EventKind + Surface + TurnEndReason + header 折叠类型） | session 语义包、持久化、compaction |
| `dsh-llm:types` | Message/ContentBlock/StreamChunk/TokenUsage/LlmFailure/ToolSchema + brand + call-config | llm 流式、assemble、session |
| `dsh-api:spec` | RPC 契约仓库（52 方法目录 + 39 错误码目录 + 四象限消息模型 + JSON Schema 转译） | web.rs dispatch、trust fence、前端校验对齐 |
| `dsh-persistence:seam` | SessionPersistence trait + Header/Inspection/Preparation/Location/Revision/Snapshot/RawArtifact + 错误 | JSONL/SQLite 后端、会话恢复 |

### 4. 自底向上（Bottom-up）：现有资产核实

- `dsh-core::session.rs` 已含 surface 折叠（M36）、provenance（M37）、fork（M49）、JSONL 初版（M47），
  但 `SessionEvent` 是 `{kind: String, payload: bytes}`——**缝传输形态**，无生产语义。
  M0 不改造它（M1 交接），但新 `dsh-session` 类型须可承载这些语义的**超集**。
- `dsh-core::llm.rs`/`llm_http.rs` 是非流式 OpenAI 兼容；M0 只立类型，不动运行。
- `deepseek-harness/packages/` 源码是权威参考（无已构建 `lib/`，故 `lib/typert.remote-client.*`
  的"生成物"在本 checkout 的对应物 = `.schema.ts` 模块 + `rpc-map.ts` + `fetch/client.ts`
  `UNARY_VALUE_SCHEMAS`；转译以其为基线）。
- 工具链：rustc 1.94.1、`serde`/`serde_derive` 已在 Cargo.lock；`RUSTC_WRAPPER=sccache`
  在沙箱不可用（见 §6 环境决策）。基线 `cargo check --workspace` 通过。

### 5. 需求结论（目标 / 非目标 / 假设 / 约束 / 边界 / 验收）

**目标（M0 内）**
- 新增 5 个 crate（`dsh-brand` / `dsh-llm` / `dsh-session` / `dsh-persistence` / `dsh-api`），
  全部加入 workspace，编译全绿。
- `dsh-session`：`SessionEventMap` 核心 13 种事件的 typed payload + `KNOWN_SESSION_EVENT_TYPES`
  全 48 词表（core + compaction + hook 合并扩展）的 `EventKind` 枚举（含 Unknown 扩展点）；
  `SessionEvent` 信封（type/seq/time/data + sourceEventSeqs/surfaceOp/ignorable）；
  `SurfaceEventType`/`SurfaceOp`/`SurfaceIntent`；`TurnEndReason`/`AgentCancelCause`/`TurnEndCancelCause`；
  `SessionHeader`/`CreateSessionOptions`/`PrepareSessionOptions`；`EpochHeader`/`RequestContext`/`RequestHeaderReason`；
  `TodoItem`；`SESSION_FORMAT_VERSION`；未知必需事件 refuse / 未知可忽略 skip 的校验函数。
- `dsh-llm`：`Message`/`UserMessage`/`AssistantMessage`/`ToolResultMessage`，`MessageSourceMap`
  （user/plugin/model/tool + Unknown），`ContentBlockMap`（text/reasoning/image/tool-call/tool-result + Unknown），
  `StreamChunk` 全变体，`FinishReasonMap`，`TokenUsage`，`LlmFailure`，`ToolSchema`，`GenerateOptions`，
  `LlmCallConfig` + `callConfigEquals`，`LlmCallConfigAdapterDefaults`，以及 provider 目录/发现类型；
  brand 新类型（MessageId/CallId/ProviderRequestId/ReasoningEffortId）。
- `dsh-brand`：品牌新类型（SessionId/MessageId/CallId/ProviderRequestId/ReasoningEffortId/RpcId/WorkspaceId/AttachmentIdType）
  的零依赖微型 crate（镜像 `@deepseek-ai/dsh-brand`，打破 dsh-session↔dsh-llm 环）。
- `dsh-persistence`：`SessionPersistence` 缝 trait（locate/supports_raw_artifacts/read_raw/create/append/
  prepare/load/inspect/read_from/list/list_snapshots），`PersistenceBackend` 后端契约，
  `SessionInspection`/`SessionPreparation`/`SessionLocation`/`SessionPersistenceRevision`/
  `SessionPersistenceSnapshot`/`SessionRawArtifact`，`SessionFormatUnsupportedError`/
  `SessionPersistenceCorruptionError`/`session_format_version_refusal`，常量（cache size=5 / delay=200）。
- `dsh-api`：`spec/` JSON 契约仓库（`methods.json` 52 方法、`errors.json` 39 错误码含 details 字段、
  `messages.json` 四象限消息模型 + error 体 + receipt、`schemas/` 各域 request/value 的 JSON Schema 转译
  含 session 全域 + host/workspace/skills/goals/settings/credentials/llm/subagents/agentPresets），
  以及读入并暴露这些仓库的 Rust 模块（方法存在性校验、错误码目录、消息模型类型）。

**非目标（M0 不做，明确留给后续里程碑）**
- 不写 dsh-session 的 `Session`/`SessionStore`/`deriveMessages`/`SurfaceManager` 运行时（M1a）。
- 不写 dsh-llm 的 `LlmAdapter`/`LlmRuntime`/`BlockAssembler`（M1b）。
- 不写 dsh-persistence 的 `PersistenceCoordinator` 编排 / `SessionWriteBehind` / JSONL 后端 / crash 修复（M1d）。
- 不写 dsh-api 的 `dispatch`/lookup/codec 运行时（M1/M3）。
- 不改 `dsh-core::session.rs` / `dsh-core::llm.rs` 现有实现（避免 M0 引入回归；M1 交接）。
- 不做差分测试基建扩展（session-host.mjs 等，M1 验收时按 §7.1 加入）。

**假设与约束**
- 权威参考 = 固定基线 `deepseek-harness @ 47f943859b` checkout（本仓库已 vendored，只读）。
- rust-version ≥ 1.85；edition 2021；保持单线程纪律（M0 全是数据面，无线程/IO）。
- crate 间只经 `dsh-brand` / 语义类型依赖，不 import 实现（§3.2 划分原则）。
- serde（derive）+ serde_json 为标准 codec；JSON 形状必须与 TS 序列化等价（字段名 kebab-case 对齐）。

**验收标准（M0 每项可验证）**
1. `cargo test --workspace` 全绿；`cargo clippy --workspace --all-targets -- -D warnings` 零警告。
2. 五个新 crate 的每个公开类型/函数/常量均有行为单测（TDD 红→绿产物）。
3. `dsh-session::EVENT_TYPES`（48 词表）与 `known-event-types.ts` 完全一致（逐字符串断言测试）。
4. `dsh-api::spec` 的 `methods.json`（52）与 `rpc-map.ts` 键集完全一致（逐项断言）；
   `errors.json`（39）与 `RpcErrorDetailsMap` 键集一致；消息模型与 `rpc.ts` 四象限一致。
5. JSON 往返测试：各种 `SessionEvent`/`ContentBlock`/`StreamChunk` 序列化→反序列化保真
   （未知类型进入 Unknown 扩展点后仍可无损回写）。
6. `dsh-persistence` 缝可被一个 mock 后端实现编译通过并跑通 create/append/load 形状。

---

## 第二部分：系统设计

### 6. 环境决策记录（先记，避免后续踩坑）

- **sccache 不可用**：沙箱 `RUSTC_WRAPPER=sccache` 无法启动（`Timed out waiting for server
  startup`）。**自修**：所有 cargo 命令前置 `$env:RUSTC_WRAPPER=''`（或永久的 `.cargo/config.toml`
  `[env]` 覆盖），并记入 DECISIONS.md（D-012）。此为本机环境问题，不改变任何架构决策。

### 7. crate 划分与依赖序（自底向上）

```
crates/
  dsh-brand/        # 零依赖：品牌新类型镜像 @deepseek-ai/dsh-brand（SharedIds）
  dsh-llm/          # 依赖 dsh-brand：Message/ContentBlock/StreamChunk/TokenUsage/LlmFailure/ToolSchema/GenerateOptions/call-config
  dsh-session/      # 依赖 dsh-brand + dsh-llm：SessionEventMap 语义类型 + SessionEvent 信封 + surface + TurnEndReason + header 折叠 + KNOWN 词表
  dsh-persistence/  # 依赖 dsh-brand + dsh-session：SessionPersistence 缝 trait + 类型
  dsh-api/          # 依赖 dsh-brand：spec/ JSON 契约仓库 + 消息模型类型 + 方法/错误目录
```

> `dsh-brand` 是唯一新增的"非能力缝" crate：TS 侧 `@deepseek-ai/dsh-brand` 就是零依赖类型包，
> 供所有跨界 id 的所有者使用而无需依赖拥有者 crate（避免 dsh-llm↔dsh-session 的品牌环）。
> 我们把它从"每个 crate 一个能力缝"原则中豁免——它承载 **SharedIds**（跨能力缝的共享标识），
> 是划分原则的显式例外，理由记入 DECISIONS.md（D-011）。

### 8. 合并可扩展语义的 Rust 建模（关键设计）

TS 的开放并集在 Rust 的等价做法（对齐 PLAN §5.2 "serde tagged enum + 插件扩展"）：

- **词汇（词汇表）**：`EventKind` 枚举覆盖 `KNOWN_SESSION_EVENT_TYPES` 48 项 + `Unknown(String)`；
  序列化为 `"kebab-case"` 字符串；`FromStr`/`is_known_event_type` 校验。
- **信封（Envelope）**：`SessionEvent { rpcId 无；type: EventKind 或 String?, seq, time, data: Value,
  source_event_seqs/surface_op/ignorable 可选 }`——对齐 TS `sessionEventSchema` 的
  **strict envelope + wide data**（TS 权威在 wire 层就只严格校验信封、data 为 `z.unknown()`）。
- **typed data**：核心 13 种事件各提供 typed payload struct（`turn/start`→`TurnStart{turn}` 等），
  经 `SessionEvent::as_turn_start()` 等访问子从 wide `data` 解析；扩展类型（compaction/hook/…）
  由 `EventKind` 识别、data 保持 `Value` 直至其归属包（M1c/M3/M4）迁移后补 typed 子面。
- **读取闸（read-side guard）**：`validate_readable(events) -> Result<(), ReadRefusal>`——
  未知 `EventKind`（不在  48 词表且非 Unknown 扩展）且 `ignorable != true` → **Err（refuse）**；
  可忽略未知事件 → 放行（skip）。对齐 coordinator `assertEventsSupported`。
- **词汇可扩展面（扩展类型）**：`EventKind::Unknown(String)` 承载"本 build 不认识但可能是
  插件/新版本"的类型字符串；`is_event_ignorable(event)` 由信封的 `ignorable: true` 判定。
  这样"未知必需事件 refuse / 未知可忽略 skip"的语义在 Rust 可判定。

`ContentBlock`/`MessageSource`/`FinishReason`/`TurnEndReason` 用同样手法：
serde tagged enum（`#[serde(tag="type"/"kind", rename_all="kebab-case")]`）+ `Unknown` 扩展点。

### 9. 持久化缝形状（对齐 session-persistence/src/index.ts）

`trait SessionPersistence`（同步版，M1d 桥接 IO）：
```
locate(&self, meta: &SessionHeader) -> Option<SessionLocation>
supports_raw_artifacts: bool
read_raw(&self, id, ...) -> Result<Option<SessionRawArtifact>>
create(&self, meta: ...) -> Result<()>
append(&self, id, events: &[SessionEvent]) -> Result<()>
prepare(&self, id, ...) -> Result<SessionPreparation>
load(&self, id) -> Result<SessionInspection>
inspect(&self, id, ...) -> Result<SessionInspection>
read_from(&self, id, from_seq) -> Result<SessionInspectionSuffix>
list(&self) -> Result<Vec<SessionHeader>>
list_snapshots(&self) -> Result<Vec<SessionPersistenceSnapshot>>
```
> TS 是异步 `Promise`；Rust 核心单线程纪律：M0 以同步 `Result` 定形（缝的形状=参数/返回类型），
> M1d 在服务层用线程桥接 IO 后仍保持该签名（决策 D-013）。`PersistenceBackend` 镜像
> coordinator.ts 的最小后端契约（loadStored/readStoredRevision/appendBatch/commitRepair/list/
> locate?/close?），供 M1d 的 `PersistenceCoordinator` 实现。

### 10. dsh-api 契约仓库（对齐 rpc-map.ts + 各域 .schema.ts + rpc.schema.ts）

- `spec/methods.json`：`[{namespace, method, wire, requestSchema, valueSchema}]`，52 项，
  键与 `rpc-map.ts` 完全一致；`has_method(wire)` / `iterate()` 供 dispatch 使用。
- `spec/errors.json`：`[{code, details: {field: required|optional}}]`，39 项，键与
  `RpcErrorDetailsMap` 一致。
- `spec/messages.json`：四象限（client-request/server-response/server-request/client-response）+
  `RpcResult`（ok/value 或 ok/error）+ `RpcReceipt` + error 体判别。
- `spec/schemas/*.json`：把各域 `.schema.ts` 的 zod 定义**机械转译**为 JSON Schema
  （每条记录源文件:行号，可回查），从 `sessions.schema.ts` 全域开始的逐方法 request/value。
  M0 转译范围 = 我能机械核对的域；以后随 dispatch 落地（M3）按同一模板补齐其余域。
  约定：契约断言只锁 JSON **内容**，不锁键序——差分统一走仓库规范序
  （`Object.keys().sort()`，决策 D-014），故 serde_json 用默认序、不用 preserve_order。

### 11. 验收对照（映射到本文件第 5 节）

| 验收条目 | 验证方式 | 落点 |
|---|---|---|
| workspace test/clippy 全绿 | cargo test/clippy | 里程碑收尾 |
| 每类型有行为单测 | TDD（红→绿） | 各 crate tests/ |
| 48 词表一致 | `assert_event_type_vocabulary_matches` | dsh-session tests |
| 52 方法/39 错误码一致 | 逐项断言 | dsh-api tests |
| JSON 往返保真 | 序列化/反序列化断言（含 Unknown 扩展） | dsh-llm/dsh-session tests |
| 缝可 mock 实现 | `MockBackend` 实现 trait + 形状测试 | dsh-persistence tests |

# M1 会话/LLM/压缩/持久化链路：需求结论文档 + 系统设计

> 本文件是 `PLAN-rust-full-harness-migration.md` §5「M1 详细方案」的实现工件：
> **阶段一（需求分析）** 产出目标/非目标/假设/约束/边界/验收标准；
> **阶段二（系统设计）** 产出 crate 内模块划分、构建序与关键设计决策。
> M0（`M0-CONTRACT-INFRA.md`）已交付契约基建（dsh-brand/dsh-llm/session/persistence/api 的类型与缝），
> M1 在这些数据面之上写**运行时语义**。决策编号追加记入 `DECISIONS.md`，git 提交可互查。

---

## 第一部分：需求分析（第一性原理 + 双视角）

### 1. 根本目标

M1 的目标：让「前端聊天 + 持久化 + 压缩」在 Rust 宿主上**真实工作**——
把 M0 铺好的数据面升级为**生产语义运行时**：
- `dsh-session`：从 `{kind,payload:bytes}` 薄承载（dsh-core::session.rs）升级为完整
  `SessionEventMap` 语义（append/surface/deriveMessages/SessionStore/fork/repair/invariant）。
- `dsh-llm`：从非流式 OpenAI 兼容（dsh-core::llm_http.rs）升级为流式 chunk 语义
  （BlockAssembler/LlmAdapter 缝/LlmRuntime + DeepSeek 适配器）。
- `dsh-compaction`（新增）：CompactionEngine 缝 + compact-basic 后端 + tool-result-pruner。
- `dsh-persistence`：JSONL 原生后端（zstd 帧） + crash 修复 + 导入转换（TS 产物 → Rust）。
- `dsh-session-query`（新增）：读模型投影 + session-log-export。
- `dsh-api`：web.rs 方法面（session.list/create/history/models/prompt/fork/rename、
  llm.providers/models）从空桩升级为真实语义驱动。

### 2. 第一性原理分解

1. **反复原成的判据**：同一操作序列下，Rust 宿主经 /api 与 WS 对前端产生的可观察行为与 TS
   宿主一致。→ M1 的每个纯语义包（session/llm-wire/compaction surface/persistence 格式）
   必须可**差分**（TS 权威执行 ↔ Rust 校验），IO/网络（HTTP 客户端、zstd、导入读取）走集成测试。
2. **session 日志是唯一事实源**（§1.2 第二原理）：任何模型可见输入必须能从日志重建。
   → `deriveMessages` 必须只从 surface 重投影，source=turn/step 序号 + tool call/result 配对
   必须可校验（invariant companion）。
3. **模型可见 ⟺ 已记录**：`Interrupted` 崩溃恢复、`request/header` 折叠、`session/end-seed`
   边界标记，都是「日志可重建模型请求」这一不变量的直接推论。
4. **持久化是缝不是实现**：M0 已定形 `SessionPersistence` 同步 trait（D-013）；M1 提供 JSONL
   前端后端（写盘/修复），编排（PersistenceCoordinator、SessionWriteBehind）按缝实现，
   不改变缝签名。
5. **API 面执行生成契约**：M0 已有 `dsh-api::spec`（52 方法/39 错误码/消息模型/JSON Schema）；
   M1 的 web.rs 方法面对齐 `spec` 的 request/value 形状，**不再手抄形状**。
6. **流式 chunk 是增量组装的关键**：DeepSeek 适配器产出 `StreamChunk`（block-start/delta/
   block-end/usage/finish），`BlockAssembler` 增量组装为 `Message`；`interruptedBlocks` 与
   `max-tokens 丢弃 tool-call` 是边界语义——必须差分穷举。

### 3. 自顶向下（Top-down）：M1 交付物分解

| 交付物 | 依赖 | 服务 |
|---|---|---|
| `dsh-session::runtime`（Session/SurfaceManager/deriveMessages/SessionStore/StoreThread） | M0 types | 前端历史、agent loop 承载、持久化事件源 |
| `dsh-session::repair`（interruptedTurnClosers） | M0 types + llm | 崩溃恢复 |
| `dsh-session::invariant`（turn/step 序号 + tool 配对校验） | M0 types + llm | 运行时一致性闸 |
| `dsh-llm::runtime`（BlockAssembler/LlmAdapter/LlmRuntime/retry） | M0 llm types | stream 组装、provider 注册 |
| `dsh-llm-deepseek`（适配器：SSE 流式解析 + wire 序列化 + 重试策略） | dsh-llm runtime | DeepSeek provider |
| `dsh-compaction`（缝 + basic 后端 + tool-result-pruner） | dsh-session + dsh-llm | 长会话压缩 |
| `dsh-persistence`（JSONL zstd 后端 + crash 修复 + import） | M0 seam + dsh-session | 落盘/恢复 |
| `dsh-session-query`（读模型投影 + log-export） | dsh-session + dsh-persistence | 前端日志导出 |
| `dsh-api`（web.rs 方法面真实语义） | spec + dsh-session + dsh-llm | 前端可交互 |

### 4. 自底向上（Bottom-up）：M0 资产核实

- `dsh-session/src/types.rs`（883 行）：SessionEvent 信封 + EventKind 48 词表 + typed payload
  访问子 + TurnEndReason + SessionHeader + SurfaceOp/SurfaceIntent + validate_readable。**齐全**。
- `dsh-llm/src/types.rs`（914 行）：Message/ContentBlock/StreamChunk/TokenUsage/FinishReason/
  MessageSource/ToolSchema/GenerateOptions，全部带 Unknown 扩展点。**齐全**。
- `dsh-llm/src/call_config.rs`：CallConfig + call_config_equals。**齐全**。
- `dsh-persistence/src/seam.rs`（298 行）：SessionPersistence trait + SessionPreparation +
  errors + SessionInspection.is_balanced。**齐全**。
- `dsh-api/src/spec.rs`：方法/错误/消息/JSON Schema 访问子。**齐全**。

→ M1 只需**引用** M0 类型，不必再改类型面；所有实现落在新增的 runtime 模块/新 crate。

### 5. 需求结论（目标 / 非目标 / 假设 / 约束 / 边界 / 验收）

**目标（M1 内）**
- `dsh-session` 运行时：Session（append/events/seq/firstLiveSeq/requestHeader/requestContext/
  deriveMessages/从 seed 创建/fork seed）、SurfaceManager（validate-then-commit + replace
  generation + provenance）、SessionStore（create/prepare/enter/announce/flush/fork），
  repair（interruptedTurnClosers 合成关闭器）、invariant 校验（turn/step 序号 + tool 配对）。
- `dsh-llm` 运行时：BlockAssembler（增量组装、max-tokens 丢弃 tool-call、interruptedBlocks、
  usage/finish 记账）、LlmAdapter 缝（stream 唯一必实现 + info/listModels/resolveModel/
  resolveCallConfig）、LlmRuntime（registerAdapter/listProviders/discoverModels/provideRetryPolicy/
  prepareCall/stream）、llm-retry 策略。
- `dsh-llm-deepseek`：SSE 流式解析（sse.ts）、wire 序列化（serialize.ts 全量字段）、
  DeepSeek 适配器实现 LlmAdapter。
- `dsh-compaction`：CompactionEngine 缝（compactIfNeeded/compactNow/compactRegion）、
  compact-basic 后端（阈值/retained-tail/overflow cap/trigger policy）、tool-result-pruner
  （Unicode 码点裁剪、保留 rich-block 顺序）；compaction/start|summary|end 事件 + user/message
  Replace + compaction/prune shadow-price；锁的「start 先落盘、end 释放」语义 + 孤儿锁检测；
  session/end-seed 区分上一生命周期遗留的未配对 start。
- `dsh-persistence`：JSONL 原生后端（zstd 帧默认/raw 行配置、原子写 tmp+rename+重试、
  torn-tail 容忍、supportsRawArtifacts）、crash 修复（interrupted turn/step/tool）、
  SessionImport 读取 TS 侧 JSONL/SQLite 产物导入 Rust JSONL。
- `dsh-session-query`：读模型投影（watermark readFrom）+ session-log-export。
- `dsh-api` 集成：web.rs 方法面（session.* 真实语义 + llm.providers/models 由注册表驱动），
  Boot 会话承载从 SessionRegistry/SessionLog 升级为 dsh-session::Session + 持久化挂载。
- 差分测试基建：新增 `session-host.mjs` / `llm-host.mjs` / `compaction-host.mjs` /
  `persistence-host.mjs`（npm/vendored 生产包权威执行 + printf `%C` 展开），对应新场景。

**非目标（M1 不做，留 M2+）**
- core/agent、core/agent-loop（M1 用现有 WasmLoopPlugin/run_turn 承载，不做 loop 语义扩展）。
- core/tools 升级为能力缝、system-prompt、scope、interaction/permission（M2）。
- SQLite 后端（决策 Q6：Rust 原生存储，SQLite M2+；旧数据走导入导出迁移）。
- host/api 全方法面（M3）；goal/subagent/schedule（M4）；fs/shell/sandbox（M5）；
  mcp/acp/hooks/skill/storage（M6）。
- 不在 Rust 里复刻 TS 类型图/装饰器（§2.2/3.4：以生成产物为规范）。

**假设与约束**
- 权威参考 = `deepseek-harness @ 47f943859b`（已 vendored，只读）。
- 单线程 `Rc<RefCell>` 纪律：核心不变；`dsh-session`/`dsh-llm` 运行时为纯语义（无线程），
  IO（zstd/HTTP/文件）放服务层线程桥（既有 `set_spawn`/`set_timer_clock`/`Hmr` mpsc 样板）。
- 新 crate 依赖序：`dsh-llm-deepseek` 依赖 `dsh-llm`；`dsh-compaction`/`dsh-session-query`
  依赖 `dsh-session`（+`dsh-persistence`）；`dsh-persistence` 的 JSONL 后端依赖
  `dsh-session`（修复合成事件）+ `dsh-llm`（消息构造）。crate 间只经 M0 语义类型通信。
- serde/serde_json 为标准 codec，JSON 键序默认（决策 D-014）。

**验收标准（M1 每项可验证，对应 PLAN §5.7）**
1. `cargo test --workspace` 全绿；`cargo clippy --workspace --all-targets -- -D warnings` 零警告。
2. **差分**：M1 新增场景（session/llm/compaction/persistence）TS↔Rust 一致；旧 16 场景零回归。
3. **契约面**：25+ RPC 方法 shape 测试扩到真实语义（不再空桩）；真实浏览器 `--dump-dom`
   阶段验收通过（boot → 侧边栏/模型选择器/发送 → prompt → history 完整 turn → mux 实时推帧）。
4. **流式**：llm 流式 chunk 端到端（本地 mock server）与 TS adapter 输出一致。
5. **持久化**：会话 JSONL（zstd）落盘 → 重启恢复 → 事件/投影/表面一致；崩溃中断 turn 以
   `interrupted` 收尾并可恢复读。
6. **压缩**：长会话 overflow → compaction-basic 选出范围 → replace 落 surface → 压缩后
   deriveMessages 一致、shadowed seqs 覆盖校验通过。

---

## 第二部分：系统设计

### 6. dsh-session 运行时模块划分（M1a）

```
crates/dsh-session/src/
  lib.rs            # pub mod surface; pub mod repair; pub mod store; pub mod invariant; pub use runtime::Session
  surface.rs        # fold_surface / SurfaceManager / derive_event_message / is_surface_*（对齐 surface.ts）
  runtime.rs        # Session（log + header + caches + append/events/seq/requestHeader/requestContext/deriveMessages）
  store.rs          # SessionStore（create/prepare/enter/announce/flush/get/list/fork）+ SessionForkError
  repair.rs         # interrupted_turn_closers（合成 tool/result/step/end/turn/end）
  request_header.rs # canonical_header / header_equals / fold_request_header（对齐 request-header.ts）
  invariant.rs      # SessionTrace validate/apply（对齐 invariant.ts）
```

关键设计（对齐 TS 语义）：
- Session 持有 `log: Vec<SessionEvent>` + `SurfaceManager`（增量） + `derived` 缓存 +
  `headerFold`/`contextFold` 增量折。`append(type, data, surface_op, source_event_seqs)`
  先 snapshot data → validate envelope → `surfaceManager.validateNext` → 入 log → 通知观察者。
- `deriveMessages` 只在 surface `nodes` 上投影 `deriveEventMessage`（每 node 一次缓存，
  replaceGeneration 变化时重建）；空 content assistant/message 跳过。
- SessionStore 用 `Rc<RefCell<HashMap<SessionId, Entry>>>`（core 单线程纪律）；观察者回调
  用 `Vec<Box<dyn Fn(&Session, &SessionEvent)>>`（M1 内无 Cordis 事件总线，用最小观察者表；
  `session/flush` 为同步 drain 回调，`session/created`/`disposed` 同步广播）。
- fork：校验 boundary（INVALID_BOUNDARY/OPEN_TURN/SESSION_NOT_FOUND/NOT_LIVE/ALREADY_EXISTS），
  默认边界=最后事件 seq（空源→空子）；子 session 带 `end-seed` 标记 seed 边界。

设计取舍（回滚点）：
- 观察者表是「M1 中最接近 Cordis 事件总线的形态」；M2 若把 agent-loop 迁入，可把观察者表
  换成真实 Cordis `emit/parallel` 语义而无需改 Session 内核对事件的构造。
- `time` 用宿主单调时钟 vs `Date.now()` 的差值：事件 `time` 是 epoch 毫秒（persistence 需
  稳定语义），M1 用 `SystemTime::now()` 的毫秒；差分场景固定 time 为常量（golden 含 time）。

### 7. dsh-llm 运行时模块划分（M1b）

```
crates/dsh-llm/src/
  assembler.rs    # BlockAssembler（增量组装、max-tokens 丢弃 tool-call、interruptedBlocks、usage/finish）
  runtime.rs      # LlmAdapter 缝（trait）+ LlmRuntime（注册表/prepareCall/stream/retry）
  retry.rs        # 重试策略（maxRetries/backoff/jitter、EMPTY_RESPONSE、CONTEXT_WINDOW_EXCEEDED）
```

```
crates/dsh-llm-deepseek/
  adapter.rs      # DeepSeek 适配器实现 LlmAdapter（provider='deepseek'）
  sse.rs          # SSE 行流解析 → StreamChunk（align llm-deepseek/src/sse.ts）
  serialize.rs    # wire 序列化（align serialize.ts：reasoning_content/image 拒绝/stream_options/effort/temperature/…）
```

### 8. dsh-compaction 模块划分（M1c）

```
crates/dsh-compaction/src/
  engine.rs        # CompactionEngine 缝（compactIfNeeded/compactNow/compactRegion）+ toolPairingBalanced*
  basic.rs         # compact-basic 后端（阈值/retained-tail/overflow cap/trigger policy + routed summary）
  pruner.rs        # ToolResultPruner（Unicode 码点裁剪、preserve rich-block 顺序）
  absorb.rs        # 生成 compaction/start|summary|end 事件 + user/message Replace + prune shadow（absorb 进 session）
```

### 9. dsh-persistence / dsh-session-query 模块划分（M1d）

```
crates/dsh-persistence/src/
  coordinator.rs   # PersistenceCoordinator（loadStored/readStoredRevision/appendBatch/commitRepair/list）
  write_behind.rs  # SessionWriteBehind（flush batching window、bound）
  jsonl.rs         # JSONL 后端实现 SessionPersistence（zstd 帧/raw 行、原子写、torn-tail、修复）
  import.rs        # SessionImport（读取 TS 侧 JSONL/SQLite 产物 → Session.fromRestore → Rust JSONL）
  io.rs            # 服务层线程桥（channel + worker，桥接同步 trait 与真实文件 IO）

crates/dsh-session-query/src/
  projection.rs    # 读模型投影（watermark readFrom）
  export.rs        # session-log-export（前端日志导出形状）
```

### 10. M1 集成收口（M1e）

- `dsh-cli/src/web.rs`：`session.*` 方法面（list/create/history/models/prompt/fork/rename）由
  `dsh-session::SessionStore` + `dsh-llm::LlmRuntime` 驱动；事件下链（WS mux）推
  SessionStore append 的 `session/event`。
- `dsh-cli/src/boot`：sessions 承载从 `SessionRegistry<Arc<Mutex<SessionLog>>>` 升级为
  `dsh-session` store；持久化插件挂载（`session/event` → `SessionPersistence.append`，
  `session/flush` → flush 批量落盘）。
- `dsh-agent-loop` 最小承载：现有 WasmLoopPlugin/run_turn 投喂 dsh-session/dsh-llm，
  不改 loop 语义（风险 §9.2）。

### 11. 验证策略

- **差分（M1 纯语义包）**：先补基建——`diff/ts-host` 加 `session-host.mjs`/`llm-host.mjs`/
  `compaction-host.mjs`/`persistence-host.mjs`（用 vendored/npm 生产包权威执行），
  对 M0 已转译的 `dsh-session`/`dsh-llm` 未来场景目录；printf `%C` 展开对齐键序/字节。
  Rust 侧 `dsh-diff` 已有场景驱动（scenarios/*.json/.golden），新场景照模板加入。
- **集成（IO/复用面）**：llm HTTP/SSE mock server 端到端；zstd 编解码往返；JSONL 原子写/
  读取/恢复；导入转换旧产物。
- **E2E**：`dsh web <cordis.yml>` + headless Edge `--dump-dom`/WS 客户端（每里程碑验收）。

### 12. 构建序（自底向上收口）

```
M1a: dsh-session 运行时（surface → runtime → store → repair → invariant）→ 差分基础场景
M1b: dsh-llm 运行时（assembler → retry → rt → adapter seam）→ dsh-llm-deepseek（sse/serialize/adapter）
M1c: dsh-compaction（engine → basic → pruner → absorb）→ 差分
M1d: dsh-persistence（coordinator/write-behind/jsonl/import）→ dsh-session-query（projection/export）
M1e: 集成（web.rs 方法面 + Boot 会话承载 + 持久化挂载）→ E2E 冒烟 → 全量验收
```

> 每一子步按 TDD（红→绿→重构）推进：先写失败测试（明确期望行为），再最小实现通过，
> 保持测试全绿；差分场景作为语义包的合并门禁。

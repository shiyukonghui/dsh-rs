# deepseek-harness（TS 源码）与 dsh-rs（Rust 迁移版）差异排查报告

> 排查范围：`deepseek-harness/`（TypeScript 原版源码仓库，HEAD `47f943859b`，2026-08-13）
> 与 `crates/`（Rust 迁移版，7 个 crate）之间的**结构、范围、行为、架构**差异。
> 依据：两侧源码精读 + 16 个子代理逐模块对比 + 差分测试基建核查。

---

## 0. 结论速览

1. **这不是"全量迁移"，而是"核心框架迁移 + 薄 DSH 服务层 + Rust 独有扩展"**。Rust 侧把 TS 的
   **vendored Cordis 插件框架**（约 4.6k 行 TS：cordis/loader/include/schemastery/timer/logger-console）
   行为等效移植为约 8.5k 行 Rust 核心（dsh-core + dsh-loader + dsh-schema + dsh-eval），并为 WASM 插件与
   CLI/Web 各新增一层（dsh-wasmrt、dsh-cli）。
2. **核心运行时行为等价**：已用差分测试（TS 生成 golden → Rust 逐行校验）证明——`scenarios/` 下
   **24 个场景**（9 核心 + 5 loader + 2 include + 若干 web 冒烟）Rust↔TS 逐行一致。
3. **DSH 生产包（49 个 packages）绝大多数未迁移**。Rust 只实现其中极小一部分概念（session 折叠/
   投影/JSONL、llm 的 OpenAI wire + HTTP 客户端、tools 注册表、web /api 子集），且 web 层的
   settings/credentials/llm/goal/subagent/agentPreset/workspace/skill 等大量方法只是**形状对齐的空桩**
   （`ok:true` 但返回硬编码/空值，只为通过前端 zod 校验）。
4. **Rust 独有而 TS 原仓库没有的层**：`dsh-wasmrt`（WASM C ABI + 组件模型 + WASI + loop 插件运行时，
   把"一切皆插件"扩展到可编译成 WASM 的第三方插件）、`dsh-cli` 的 `--watch` HMR、`--once` headless、
   `--session-in/out` 等 CLI。
5. **架构本质差异**：单线程 `Rc<RefCell>` + "收集-再执行"纪律 vs JS 事件循环/任意重入；显式方法调用
   vs Proxy 动态属性解析；`Value`(serde_json) 数据模型 vs 任意 JS 值；模块是"类型定义 + 运行时分散"
   的**合并式**组织 vs TS 分散在多个文件。

---

## 1. 规模对比

| 维度 | TS 原版（deepseek-harness） | Rust 迁移版（crates/） |
|---|---|---|
| 源码文件 | 2578 个 .ts/.tsx 文件（含测试/夹具） | 16 个 src/*.rs + ~44 个 tests/*.rs |
| 源码行数 | **517,739**（全量）；非测试 **238,550** | **≈11,400**（src 全部，含注释） |
| 包/模块数 | 49 个 packages + apps(cli,web) + vendor + native + website | 7 个 crate |
| 测试 | vitest 全仓（数百 spec） | 289 个 `#[test]`/`#[tokio::test]` 标记 |
| 差分验证 | — | 24 个 scenarios（*.json + *.golden） |
| WASM 插件 | 无此形态（node embedder 是宿主） | wasm-plugins/ 7 个（hello、hello-component、hello-net、hello-wasi、echo-loop、tool-loop、llm-loop） |

**对应关系粗览**（Rust 迁移目标明确写在 `PLAN-rust-cordis-equivalent-migration.md`）：

| TS vendor 部分 | 迁移到 | 等价性证据 |
|---|---|---|
| `vendor/cordis/src/*.ts`（2466 行） | `dsh-core`（context/runtime/fiber/events/reflect/registry/service/logger） | 差分场景 01–09 + m0/m1/m7/m40/m42/m43/m64 等单测 |
| `vendor/loader/src/*`（995 行） | `dsh-loader`（loader/entry/group/isolate/include/hmr） | 差分场景 loader-01/02/10/11/12 + m2/m3/m14/m15 单测 |
| `vendor/include/src/index.ts`（336 行） | `dsh-loader/include.rs` | 差分场景 include-01/02 + m3_include 单测 |
| `vendor/schemastery/src/index.ts`（817 行） | `dsh-schema/lib.rs` | m4_schema 单测 |
| `vendor/timer/`（136 行） | `dsh-core`（timeout/interval/debounce/throttle） | m40_timer 单测 |
| `vendor/logger-console/`（125 行） | `dsh-core/logger.rs` exporter | m1_logger 单测 |
| `vendor/hmr/`（cordis-plugin-hmr，549 行） | `dsh-loader/hmr.rs`（**仅 registerConfig 子集**） | m15_hmr 单测 |

---

## 2. 分层差异

### 2.1 L1 —— 核心框架（Cordis）：行为等价，结构不同

**行为等价（已被差分测试证实）**：插件生命周期（PENDING→LOADING→ACTIVE→UNLOADING→DISPOSED）、
effect 逆序幂等清理、依赖 gate 与两阶段延迟加载、四种同步分派（emit/bail/serial/waterfall）+ async
形态（parallel_async/serial_async）、waterfall `next()`/短路、provide/set/get/accessor/mixin、intercept
合并链、internal/get・internal/set・internal/listener・internal/dispatch 钩子、once/on 卸载语义、
logger 阈值过滤/printf 格式化、loader 7-case 自处置、update 四分支事务+回滚、group 嵌套/disabled/
isolate-intercept 等。

**关键结构差异（易误判为"不等价"）**：
- TS 把行为分散在 `events.ts`/`reflect.ts`/`fiber.ts`/`registry.ts`/`service.ts` 等文件；Rust 侧
  **`events.rs`/`reflect.rs`/`fiber.rs` 只是"类型 + 数据模型"**（DispatchMode/HookResult/Impl/Property/
  FiberData/EffectOutcome），运行行为集中在 `context.rs`（门面，1847 行）+ `runtime.rs`（转换逻辑，862 行）。
  若逐文件对比会得出"事件/反射/会话全没实现"的错误结论——必须按运行时整体对比。
- loader 的 `entry.rs`/`group.rs` 在 Rust 侧只是**带 serde 的 DTO**（含 isolate/intercept/disabled_expr
  覆盖字段），运行逻辑全部内联在 `loader.rs`（1502 行）。

`cordis-context` 子代理结论：**在 Rust 已实现的重叠公共 API 面上行为对齐**（并作差分验证）。

### 2.2 L2 —— DSH 生产包：极小覆盖 + 大量空桩

Rust 侧**只实现**了 DSH 概念层面的一小部分：

| TS packages 概念 | Rust 实现 | 状态 |
|---|---|---|
| session-projection / session-persistence-jsonl / Session surface | `dsh-core/session.rs`（SessionLog：surface 折叠、derive_messages、save_to/load_from、fork） | **概念对齐但范围不同**（见 §3 偏差） |
| llm-deepseek serialize | `dsh-core/llm_http.rs`（messages_to_wire + 手写 HTTP/1.1 客户端） | **消息序列化核心对齐**，请求级字段缺失（见 §3） |
| tools 注册表 | `dsh-core/tools.rs`（ToolRegistry + schema） | 精简实现 |
| web / api / host / client / boot / core / context | `dsh-cli/web.rs` **复用原版前端 dist** + /api RPC | 见 §2.3 |
| skill / workspace / goal / subagent / settings / credentials / agentPreset / commands | `web.rs` 的 RPC **空桩** | 只对齐 zod 形状 |
| 其余 30+ 个 packages | **完全无 Rust 对应** | 详见清单 |

### 2.3 L3 —— Rust 独有层（TS 原仓库没有）

- **`dsh-wasmrt`（WASM 插件运行时）**：C ABI 插件（alloc/dealloc/plugin_apply/…）、组件模型
  （wasmtime::component + WIT `dsh:plugin` + `dsh:dsh` 四缝 session/tools/llm/agent-loop）、
  WASI preview1/preview2 精细能力授予、CURRENT_CTX thread_local 桥接、WasmLoopPlugin 把 WASM loop
  挂进 Cordis。这是"一切皆插件可脱离 Node 运行 + 第三方插件编译成 WASM"的核心新增。
- **`dsh-cli`**：`dsh <cordis.yml>` 交互式/`--once` headless/`--watch` HMR/`--session-in|out`/
  `--dump-config`/`web` 子命令。
- **`dsh-eval`**：`!!js` 表达式子集的求值器（tokenizer + 递归下降，含 `?.`/`??`/`typeof`/模板字符串/
  `in`/`?.()`）。
- **`dsh-diff`**：差分场景 DSL 解释器 + golden 校验 CLI（这是验证工具，非 DSH 功能）。

---

## 3. 具体行为偏差清单（已迁移部分的真实缺口）

以下来自 16 个子代理逐文件深读，是最可信的"有则不同"清单：

### 3.1 Logger（dsh-core/logger.rs vs vendor/cordis/logger.ts）
- `Logger.level` 阈值回退从未被读取（TS `levels[name] ?? levels.default ?? this.level ?? INFO` vs Rust
  `?? default_level ?? 1`）。
- 单 Error 的 cause 链递归缺失；`%d`/`%i` 非数值输出 `"0"`（TS 输出 `"NaN"`）；`%f` 非数值输出 `"0.0"`。
- 截断：TS 按 `\r?\n` 分行 + UTF-16 码元截断 10240 vs Rust 仅按 `'\n'` 分行 + 字节 slice（多字节边界
  可能 panic）。
- exporter 的 `colors/formatters/maxLength` 缺失，`%C` 无 ANSI 着色；`ctx.logger()` 可调用服务形态缺失；
  WeakRef→强 FiberId。

### 3.2 Include（dsh-loader/include.rs vs vendor/include）
- 扩展名白名单不校验（未知扩展一律按 YAML）；原子写（`.tmp`+rename+EACCES/EBUSY/EPERM 重试）缺失，
  直接 `std::fs::write`；写防抖/合并/队列/`loader/config-update` 事件缺失；readonly/W_OK 保护缺失；
  `refresh()` 内容未变短路缺失；`internal/update` 配置热更订阅缺失；初始写恒用 serde_yaml；覆盖键按
  已知字段类型转换（TS 宽松 `target[key]=value`）。

### 3.3 HMR（dsh-loader/hmr.rs vs cordis-plugin-hmr）
- 只移植了 **registerConfig 配置刷新**这一条路径；`partialReload`/全量重启/externals/模块缓存管理、
  `hmr/change`/`hmr/reload` 事件、依赖图分析全部缺失。
- 重复注册：TS 抛错 vs Rust 静默覆盖（注释明示此偏差）；初始扫描时机语义错位（Rust 对齐
  `ignoreInitial:true`）；无 refresh 合并/排队；no realpath/父目录提升。

### 3.4 Schema（dsh-schema/lib.rs vs vendor/schemastery）
- `is(Class)` 只按 JSON 类型名映射（非 `instanceof`/constructor）；`date` 只收 RFC3339 且返回字符串；
  `regExp` 只校验可编译。**adapt 第二输出值被丢弃**（TS 写回 `data[key]=adapted`）。intersect 不强制
  strict；intersect 合并覆盖顺序不同（Rust `Map.insert` 后覆盖先）。default 直接短路不经过 resolver。
- 缺失：`Schema.from`/`arrayBuffer()`/`i18n()`/`simplify()`/`toJSON()`/`set()`/`push()`/`deprecated()`。

### 3.5 LLM wire（dsh-core/llm_http.rs vs llm-deepseek/serialize.ts）
- `reasoning_content` 回传缺失、图像内容拒绝校验缺失（静默丢弃非 text 块）、`stream`/
  `stream_options.include_usage` 缺失、`thinking`/`reasoning_effort` 缺失、`temperature`/`max_tokens`/
  `stop` 缺失、类型化 `LlmError` 缺失（返回 `{error, content:""}` JSON）。
- Rust 独有：手写 HTTP/1.1 客户端 + https(TLS)（TS 走 openai SDK/undici）。

### 3.6 Session（dsh-core/session.rs vs 各 session 包）
- 对齐了：SessionSurface 折叠/replace+shadow、provenance/tool-result 校验、deriveEventMessage 规则、
  JSONL 持久化（torn-tail 容忍）、fork。差异：`image` block 未支持（WIT 预留）、id 确定性生成非 uuid、
  surfaceOp 必填校验宽松、无 zstd/元数据、`restoreFloor`/增量 checkpoint 缺失。

### 3.7 DSH web /api（dsh-cli/web.rs）
- **真实实现**：version、host.describe、session.list/create/history/search/models/selectModel/rename/fork/
  prompt/cancel、workspace.list（硬编码）、**agent-loop**（驱动 WASM loop，真实功能）、commands/list
  （硬编码 3 条）。另实现 WebSocket/SSE 下链 + trust fence + `__DSH_BOOT__` 注入 + /plugins bundle。
- **空桩（`ok:true` 但硬编码/空值）**：session.attachment（假图片）、session.updateQueue、
  workspace.create/rename/delete/insertBefore/insertSessionBefore/archiveSession（恒 default）、skill.list
  （空）、agentPreset.*、settings.*、credentials.*、llm.providers/models/discoverModels（恒 echo/llm/tool）、
  goal.*（假 ref default）、subagent.*（空）、dynamicCordisRunner/inventory（空数组）+syncInspectManifest（null）。
- **其余方法 → `not-implemented` fail loud**（不伪装）。

### 3.8 Cordis 语义中 Rust 明确未移植
- `ctx.extend(meta)` 子上下文（原型继承遮蔽）、`ctx.isolate(name,label)` 公开服务隔离 API、`Context.is()`
  品牌检查、`ctx.root`/`ctx.baseUrl`、运行期 Proxy 动态属性解析（Rust 用显式 `get()/get_value()`）、
  `!!js` 任意 JS 表达式（收敛为子集）、`Config['simplify']` 写回前简化、`locate()` 反查、
  `internal/update` 的 'reload' 日志监听器。

---

## 4. 架构差异（设计层面）

| 维度 | TS 原版 | Rust 迁移版 |
|---|---|---|
| 线程/并发 | Node 单线程事件循环 + 任意重入 | 单线程 `Rc<RefCell<Runtime>>` + **收集-再执行**纪律（借用中绝不调用户代码）|
| 上下文 | `ctx` 是 **Proxy**，`ctx.xxx` 动态解析为服务/accessor | `Cordis` 结构体门面，显式 `get(name)/get_value()` |
| 服务值 | 任意 JS 值 | `Arc<dyn Any + Send + Sync>`，取回 downcast；`Value`(serde_json) 负责跨缝数据 |
| 错误模式 | JS 异常 + 类型化 `CordisError`/`ValidationError` | `CordisError` 枚举 + `Result`/`Option` |
| 异步 | Promise/async-await/allSettled | tokio current_thread + `LocalBoxFuture` + `join_all`/yield_now（模拟微任务交错）|
| 模块组织 | 每文件一个服务/概念，行为分散 | **类型层文件**（events/fiber/reflect…）+ **运行时合并**（context/runtime/loader 大文件）|
| 插件形态 | function/class/object 三种归一 | `Plugin` trait + C ABI + WASM 组件三种适配 |
| 确定性 | 依赖 `Math.random`/uuid/Date | 注入时钟 + 确定性 id（差分测试可复现）|

---

## 5. 未迁移到 Rust 的 TS 功能全景（49 包盘点）

**有对应实现/概念（8）**：session、llm、tools（以上 dsh-core / dsh-loader）、api、web、host、client、
boot、core、context（以上 web.rs 复用前端 + RPC）。

**空桩（仅形状，无功能）（8）**：skill、workspace、goal、subagent、settings、credentials、
agentPreset（+ commands 硬编码）——均返回 `ok:true` 但内容硬编码/为空，仅供前端 zod 校验通过，
**无真实后端能力**。

**完全没有 Rust 对应（33）**：
`acp`、`attachment`(真实实现)、`bundle`、`code-runtime`、`compaction`、`e2b`、`examples`、`extensions`、
`feedback`、`fs`、`guard`、`hooks`、`identity`、`interaction`、`jobs`、`lsp`、`mcp`、`plan`、`preset`、
`runtime-diagnostics`、`sandbox`、`schedule`、`sdk`、`session-query`、`shell`、`spill`、`storage`、
`subprocess`、`terminal`、`test-support`、`todo`、`typert`、`util`、`workflow`。
（其中 compaction（上下文压缩）、subagent、goal、schedule、workflow、mcp/lsp/sandbox/shell/fs/storage
等是 DSH 的核心生产力面，Rust 侧完全没有。）

> 注：Rust 的 `dsh-web` 阶段（M70–M71 及阶段 1–4, D-003~D-010）目标只是"复用现成前端 + 提供 /api 可交互"，
> 并不是重写这些包；这些包的功能在运行时仍由前端侧 cordis 插件（在浏览器/宿主中）承载——Rust 只提供
> 前端坐标系里所需的 API 外壳。

---

## 6. 建议结论

- 若以"核心框架（Cordis 语义）能否脱离 Node 运行"为判据：**已等价**（差分 + 289 单测 + wasmrt 冒烟）。
- 若以"完整 DSH 功能面"为判据：**远未覆盖**——Rust 侧是"运行时内核 + WASM 插件 + 薄 API 外壳"，
  约 33/49 个生产包无对应，web 层约 8 个方法组只是空桩。
- 下一步方向（如需继续）：带动 **compaction/session-query/session-persistence 全量**到 Rust；把空桩
  （settings/credentials/llm/goal/subagent/workspace/skill）做成真实后端；补齐 llm wire 的
  reasoning/stream/thinking 字段；补 include 原子写与 HMR 全链路。

---

*本报告由对两侧源码的自动化逐文件对比（16 个独立对比代理）+ 人工核验（结构/规模/差分基建/web 方法面）生成，
时间：当前会话。*

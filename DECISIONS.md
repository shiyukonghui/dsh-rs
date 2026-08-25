# dsh-rs 决策日志（Decision Log）

> 每一条记录对应一个关键决策点，使「为什么代码长这样」可追溯。改动 → git 提交 →
> 本日志 三者可互查（提交信息引用决策编号）。完整方案依据见
> `PLAN-rust-cordis-equivalent-migration.md` 与 `HANDOFF.md`。

---

## D-015（M1a）：dsh-session 运行时以「纯语义 + 最小观察者表」承载 Session/Surface/Store

**日期**：2025（本机时间）

**触发的问题**：M1 需把 `dsh-session` 从 M0 类型面升级为生产语义运行时（append/
surface/deriveMessages/Store/fork/repair/invariant）。TS 侧 `SessionStore` 依赖 Cordis
`context` 的事件总线（`session/created`/`session/event` 的 scoped emit、`session/flush`
的 drain）。Rust 侧核心是单线程 `Rc<RefCell>` 纪律（D-004/D-006 否定 async 运行时），
且 web.rs 集成要能订阅 `session/event` 推 WS。

**考虑的选项**：
1. **纯语义内核 + 最小观察者表（本次采用）**：`Session` 不依赖任何事件总线；`append`
   提交后同步调用可替换的 `on_event: Option<Box<dyn Fn(&SessionEvent)>>` 钩子；
   `SessionStore`（Rc<Self>）持 `on_created/on_disposed/on_event/on_flush` 回调表，
   `enter` 时用 `Weak<Self>` 把会话 append 钩子引向 store 的观察者表（避免循环引用）。
   surface 校验（validate-then-commit）与 TS `SurfaceManager` 逐字段对齐。
2. **在 dsh-session 内实现完整 Cordis 事件总线**：引入 fiber/emit 语义，偏离核心单线程
   纪律，且与 M2 的关联（agent-loop 承载）耦合过早。
3. **不做观察者，Store 暴露轮询读取**：持久化插件自行 diff log——违背
   「session/event 同步通知 + flush 检查点」缝隙形态（第一性原理 §1.2 第五点）。

**最终选择**：选项 1。`Session` 保持纯数据语义（可差分、可单测、可在任意线程上
以 `&self` 调用）；观察者是**挂载点**而非核心逻辑。store 用 `Rc<Self>` 形态以便在
闭包中捕获；`Weak` 引用避免 store→session→store 环。

**选择理由**：「模型可见 ⟺ 已记录」「日志是唯一事实源」等不变量在 Session 内核里，
与事件分发无关；观察者表是 M1 里最接近 Cordis 总线的最小形态，M2 迁 agent-loop 时
可把表换成真实 emit 而无需改 Session 内核对事件的构造。

**预期影响与回滚点**：新增 `surface.rs`/`runtime.rs`/`store.rs`/`repair.rs`/`invariant.rs`/
`request_header.rs` 六模块 + `tests/m1_session_runtime.rs`（61 项测试 + 17 项 surface 单测）。
回滚：撤提交即回到 M0 类型面，不影响 dsh-core 等旧 crate。

---

## D-011（M0）：契约基建的 crate 划分——新增 `dsh-brand`（SharedIds）与四块数据面

**日期**：2025（本机时间）

**触发的问题**：`PLAN-rust-full-harness-migration.md` §4 要求 M0 铺好四块底层数据面
（`dsh-session:types` / `dsh-llm:types` / `dsh-api:spec` / `dsh-persistence:seam`）。
但 TS 侧的品牌 id 分布会形成 Rust 依赖环：`SessionId` 在 `dsh-session`，而
`GenerateOptions.sessionId`（dsh-llm）与 RPC 的 `RpcId`（dsh-api）也引用它；`dsh-session`
的核心事件类型又必须 import `dsh-llm` 的 `AssistantMessage`/`StreamChunk`。直接搬会造成
dsh-llm↔dsh-session 循环依赖。而 §3.2 的划分原则是「每个 crate 对应一个能力缝」，品牌 id
不是一个能力缝，需要显式豁免。

**考虑的选项**：
1. **新增 `dsh-brand` 微型 crate 承载全部跨界品牌新类型（本次采用）**：镜像 TS 的
   `@deepseek-ai/dsh-brand`（零依赖类型包），把 `SessionId`/`MessageId`/`CallId`/
   `ProviderRequestId`/`ReasoningEffortId`/`RpcId`/`WorkspaceId`/`AttachmentIdType` 统一放一处；
   每个能力缝 crate 从它 re-export 自己拥有的 id（`dsh-session::SessionId` 等），保持
   「拥有者暴露自己的 id」的 TS 语义。零依赖、破环、单一品牌实现。
2. **在各拥有 crate 各自定义品牌新类型**：`dsh-session` 定义 `SessionId`，
   `dsh-llm` 自行定义 `session_id: String` 字段。— `GenerateOptions.sessionId` 失去类型锚，
   且 `SessionId` 会被复制定义，语义漂移风险。
3. **把 `dsh-brand` 合进 `dsh-llm`**：dsh-llm 是品牌的最小共享依赖（dsh-session 依赖它）。
   但 `SessionId`/`RpcId` 归属语义上不属于 llm 能力缝；dsh-api 不应依赖 dsh-llm 仅仅为了
   一个 id。— 贴合度最差。

**最终选择**：选项 1。`dsh-brand` = 零依赖纯类型 crate（`SharedIds`），作为「每个 crate 一个
能力缝」划分原则的**显式豁免**，理由：品牌类型是跨缝共享标识，非任何单一能力的实现。
这也正是 TS 侧 `@deepseek-ai/dsh-brand` 的定位，迁移保持同构。

**选择理由**：第一性事实是「跨界 id 必须被多个拥有者引用且带类型身份」，选项 1 以最小
结构满足它并消除依赖环；re-export 双保险维持拥有语义；Rust 侧 newtype 等价 TS 的
`Branded<B>`（编译期幻影类型）。

**预期影响与回滚点**：新增 5 个 crate（brand/llm/session/persistence/api），依赖序 brand →
llm → session → persistence / api 单向无环。回滚：撤 M0 提交即移除，不影响现有 7 个 crate。

---

## D-012（M0）：环境问题自修——沙箱内禁用 `RUSTC_WRAPPER=sccache`

**日期**：2025（本机时间）

**触发的问题**：基线 `cargo check --workspace` 失败：`sccache: error: Timed out waiting for
server startup`（沙箱无法起 sccache 服务）。环境变量 `RUSTC_WRAPPER=sccache` 是环境级预设。

**自修**：所有 cargo/clippy 调用前置 `$env:RUSTC_WRAPPER=''` 再运行；基线重跑通过
（`Finished dev profile ... in 7.54s`）。**环境问题不改变任何架构决策**（M0 是纯数据面，
无构建期对缓存的硬依赖）。

**后续动作**：若持续影响可写 `.cargo/config.toml` `[env] RUSTC_WRAPPER=""` 固化；本次以命令
前置解决，避免改动仓库环境配置。

---

## D-013（M0）：持久化缝以同步 `Result` 定形（TS 的 `Promise` 在 Rust 单线程纪律下的形态）

**日期**：2025（本机时间）

**触发的问题**：`session-persistence/src/index.ts` 的缝方法是 `Promise<T>`（async/await）；
Rust 核心是单线程 `Rc<RefCell>` 纪律，且 M1d 要把 IO 放服务层线程/进程再桥回。M0 的 seam
签名怎么定，直接影响 M1d 后端实现与 web.rs 集成。

**考虑的选项**：
1. **同步 `trait SessionPersistence`（方法返回 `Result<T, PersistenceError>`）（本次采用）**：
   M0 定形「参数 + 返回类型 + 错误」；IO 的异步落在 M1d 的服务层桥（channel + worker 线程），
   缝保持同步外观，与核心单线程一致，也便于 mock 后端与形状测试。
2. **引入 async trait（async-trait/tokio）**：贴合 TS Promise 但把 async 运行时拉进核心，
   与单线程纪律冲突（D-004/D-006 已两度否 async 运行时）。
3. **不上 trait，先只放类型**：seam 只剩数据。— 违背「能力缝 = 三位一体」、M0 要能让 mock
   后端按缝实现。

**最终选择**：选项 1。缝的形状（方法集、签名、`SessionInspection`/`SessionPreparation`/
`SessionLocation`/revision）与 TS 逐一对齐，仅把 `Promise` 换成同步 `Result`，错误用
`SessionFormatUnsupportedError`/`SessionPersistenceCorruptionError` 细分并保留
`session_format_version_refusal` 的方向感知文本。`PersistenceBackend` 同步版同此。

**选择理由**：同步 trait 保留单线程纪律与可测性，M1d 的 IO 桥是既有样板（`set_spawn`/
`set_timer_clock`/`Hmr` mpsc）的复用，不改变缝契约。

**预期影响与回滚点**：`dsh-persistence` 暴露同步 trait；M1d 实现层内部做 IO 桥。回滚：撤
M0 提交即移除；未来若确需 async 面，可在缝外加 async wrapper（不开新缝）。

---

## D-014（M0）：JSON 键序策略——契约不绑定键序，差分统一走规范序（弃用 preserve_order）

**日期**：2025（本机时间）

**触发的问题**：初版 M0 在 `serde_json` 上启用了 `preserve_order`（插入序），理由是「与 TS
`JSON.stringify` 插入序逐字节一致」。全量测试随即发现 `dsh-diff` 的既有 golden
`run_include_apply_patches_matches_ts_shape` 失败：其 `sorted_json`（lib.rs）把输出规范化为
**字典序**（注释明确对齐 TS host `canonical` = `Object.keys().sort()`），`preserve_order`
（feature 统一）把全工作区的 `Map` 从 `BTreeMap` 换成 `IndexMap`，键序变成插入序，破坏了该
规范序 golden。

**第一性分析**：① JSON 对象语义等价**不依赖键序**——前端以 zod 解析（键名访问），非字节
比较；② 仓库的差分测试本来就建立「两侧都规范化到同一键序」的机制（`sorted_json`），所以
「逐字节一致」在本仓库的实现路径 = 规范序，而非原封插入序；③ TS 的插入序只对**同一次
写入**的 byte-identity 有意义，Rust 是唯一 writer 时自洽即可，无需与 TS 的键序重合。

**考虑的选项**：
1. **弃用 preserve_order，保持 serde_json 默认 BTreeMap 规范序（本次采用）**：全工作区
   统一规范序；契约测试的精确字符串断言改为 **JSON 语义比较**（`serde_json::to_value` 对
   解析后的 `Value` 判等），并把 m0_llm/m0_session 测试中绑定键序的断言全部改掉；对
   消息目录等「集合而非序列」断言改为集合断言。
2. **保留 preserve_order，把 `sorted_json` 改成显式按键排序**：需要改动既有 dsh-diff 的
   序列化路径以恢复其规范序保证；且 preserve_order 是全局行为变化，可能波及更多既有
   trace/golden（本次全量跑只抓到 1 处，但 E2E/其它序列化点存在潜在漂移）。— 风险
   面大、收益（插入序）并无消费方。
3. **按 crate 隔离 serde_json 版本**：让 M0 crate 用带 preserve_order 的版本、其余默认。
   Cargo 每构建 feature 统一，需引入不同版本/rename 才可分，复杂且无谓。— 过度工程。

**最终选择**：选项 1。JSON 键序不进入契约；差分测试的「逐字节一致」语义由仓库既有的
`Object.keys().sort()` 规范序承担；M0 契约测试用语义等价断言。

**选择理由**：与 dsh-diff 已确立的 canonical 差分规范同构（D-001…M63 系一贯如此）；避免
全局行为变更的隐藏漂移；契约只锁 JSON 内容，不锁键序——这正是「serde_json 默认即契约」
的稳定选择。

**预期影响与回滚点**：五个 M0 crate 的 `serde_json` 恢复 `"1"`（无 feature）；m0_llm_types /
m0_session_types / m0_spec 的键序绑定断言改为语义/集合断言；dsh-diff 恢复全绿。回滚：
重新加 `preserve_order` 并同步改 `sorted_json` 与相关断言（不推荐）。

---
## D-001（M68）：WASM `tools::register` 桥接——先落地「声明登记 + 明确路由」

**日期**：2025（本机时间）

**触发的问题**：`dsh-loop.wit` 的 `tools::register(name, schema, handler: u32)`
此前是 no-op（返回 0）。WASM 插件（如 tool-loop）若想经缝注册自己的工具，
宿主既记不住声明、执行时又会误报「not registered」——把「未桥接」伪装成
「不存在」，掩盖真实缺口（handler 是 WASM 资源句柄，跨缝回调未实现）。

**考虑的选项**：
1. **完整桥接**：改 WIT `handler: u32` 为真实 `resource` 类型，让 wit-bindgen
   托管句柄表；`execute` 感知当前 `Caller` 以回调 WASM 导出；解决
   `call_run_turn` 期间的 **Store 重入借用**（`Rc<RefCell<Option<LoopRuntime>>>`
   需换成可重入访问模型）；宿主侧分流 host 工具 vs WASM 工具；并写一个导出
   handler 资源的 WASM 测试组件打通验证。— 涉及 WIT 契约 + Store 重入两处
   结构性设计变更，风险高、需新测试组件。
2. **声明登记 + 明确路由（本次采用）**：宿主记录 WASM 声明（name+schema，
   同名去重覆盖），`wasm_tools()` 可读回；`execute_tool` 路由命中顺序改为
   「宿主注册表权威实调 → 内存 add 回退 → WASM 声明（明确桥接未实现错误）→
   not registered」。不伪装缺口，为完整桥接留出干净的观察面。
3. **维持 no-op**：不做。— 把已知缺口藏起来，违背「诚实报缺口」原则。

**最终选择**：选项 2。声明登记 + 路由清晰化，作为 §7.13 的第一步落地。

**选择理由**：完整桥接（选项 1）是 DSH 层 WIT 缝的**跨资源桥接增强**，非 Cordis
内核语义、无测试组件覆盖，且牵动 WIT 契约与 Store 重入两处设计——不适合在
核心迁移已收尾时一次性强推。选项 2 以最小、无风险、可测试的方式先把「WASM
可声明工具」的语义显式化，并让执行路由不再误导：被声明但未桥接的工具给明确
错误，宿主注册表仍权威。这既补上了可观测性缺口，又为选项 1 预留了干净的
接入点（`wasm_tools` 即为将来 handler 桥接的登记表）。

**预期影响与回滚点**：
- 行为变化仅在「WASM 声明过某工具」时：执行返回明确桥接错误（此前是
  not registered），不破坏任何 host 注册工具路径（宿主注册表仍最先命中）。
- 新增公开方法 `WasmLoopPlugin::register_tool / wasm_tools / execute_tool`，
  均为纯增量。
- 回滚：撤掉本提交即可恢复 no-op 状态；不影响其他功能。

---

## D-007（阶段2/3）：`/api` 方法面对齐前端 `UNARY_VALUE_SCHEMAS`

**日期**：2025（本机时间）

**触发的问题**：前端每个 unary 方法的响应 value 都过 zod 校验（`callUnary` →
`UNARY_VALUE_SCHEMAS[method].parse(result.value)`）。M70 的 `dispatch` 返回的
形状与前端 schema 不符：`session.list` 返回 `{sessions:[...]}` 但 schema 要
`{items:[sessionSummarySchema]}`；`session.history` 返回 `{messages:[...]}` 但
schema 要 `{events:[{event}], hasMore}`。更关键的是 boot 必需的方法
`host.describe` 未实现——connection 插件 `start()` 里 `host.describe` 失败会
直接 throw，前端停在 loading。即使 WebSocket 通了，boot 后 UI 一调用这些方法
也会被前端 schema 校验拒绝。

**考虑的选项**：
1. **逐方法对齐 schema 形状（本次采用）**：重写 `dispatch`，让每个已实现方法
   的 value 通过对应 zod schema；补齐 boot 必需方法（host.describe /
   workspace.list / skill.list / agentPreset.list / session.search / commands）。
   未实现的仍 `not-implemented` fail loud。
2. **不校验形状，返回宽松 JSON**：前端校验直接拒。— 违背「boot 后 UI 可交互」
   目标。
3. **照搬前端 fixture 的完整方法面**：一次性实现所有方法（subagent/goals/
   settings/credentials/llm...）。— 方法面极大，超出本阶段范围，留待阶段 4。

**最终选择**：选项 1。以真实前端 schema 为准（自下而上核实 `client.js` 里的
`UNARY_VALUE_SCHEMAS`），对齐 boot 必需 + 阶段2 核心会话/工作区方法形状。

**选择理由**：第一性事实是「前端用 zod 严格校验响应」，所以形状对齐是 boot 后
可交互的前提，不是锦上添花。逐方法对齐是增量、可测的（每方法一个 shape 单测）；
补齐 boot 必需方法（尤其 host.describe）让前端能从 loading 走到真实 UI。
`cwd` 取 `std::env::current_dir()`（对齐 host.describe 语义——宿主进程 cwd）。

**预期影响与回滚点**：`dispatch` 返回形状改变——M70 的 `sessions`/`session.list`
单测更新为 `items[].sessionId`。新增 6 个 shape 单测 + 12 个 boot-critical 方法
E2E 冒烟全 ok。回滚：撤提交回到 M71 旧形状；不影响 WebSocket/静态。后续阶段 3
扩展 selectModel/rename/fork/prompt/cancel 语义，阶段 4 补全量方法面。

---

## D-010（阶段3）：多会话支持——web 层会话注册表

**日期**：2025（本机时间）

**触发的问题**：ROADMAP 阶段 3 验收含「多会话切换」。M70 的 `session.create`
恒返回 `"default"`、`session.list` 恒单会话——UI 无法新建/切换独立会话。但 WASM
loop 是单引擎（`boot.sessions` 单个 `SessionHandle`），`run_turn` 把事件写到该
单一 log。若硬改 loop 引擎为每会话一个实例，牵动 wasmrt 的 Store 模型，风险大。

**考虑的选项**：
1. **web 层会话注册表（本次采用）**：`SessionRegistry = Arc<Mutex<HashMap<String,
   SessionLog>>>`，seed `default`。`session.create` mint 唯一 id 注册空 log；
   `session.list` 返回全部；`session.history` 按 payload.sessionId 读对应 log；
   `session.prompt` 经共享 loop 驱动后，把 turn 新产生的事件追加到目标 session
   的 log（`run_turn` 前后 `boot.sessions.events().len()` 取差量）。loop 引擎
   保持单例（共享上下文），各 session 历史独立。
2. **每会话一个 WASM loop 实例**：wasmrt 层按 sessionId 建 Store。— 牵动
   `WasmLoopPlugin` 的单 Store 模型、HMR 换组件、CURRENT_CTX thread_local 桥接，
   改动大、回归风险高。
3. **维持单会话**：不满足阶段 3 多会话验收。

**最终选择**：选项 1。loop 仍是单一共享引擎（`boot.sessions`），但前端可见的
「会话」是 web 层独立 `SessionLog`——创建/切换/各自历史都成立，UI 多会话交互
可跑通，无需改 wasmrt 架构。

**选择理由**：第一性区分「loop 引擎状态」与「前端会话视图」——多会话是 UI 级
概念，不必然要求每会话独立 WASM Store。web 层 registry 以最小侵入实现多会话
切换与独立历史，符合「Rust 服务前端 + 提供 /api」的架构事实。`handle_rpc` 保留
单参包装（构造临时 default registry），既有测试签名不变；新增
`handle_rpc_registry` 供服务器路径用。

**预期影响与回滚点**：`session.create` 现在返回 `s2`/`s3`…（此前恒 default）；
`session.list` 返回多会话；`session.history`/`prompt` 按 sessionId 路由；新增
`session.fork` 复制事件到新会话。回滚：撤提交即恢复单会话（default），不影响
loop/静态/WebSocket。测试新增 `rpc_multi_session_create_list_prompt`。

---

## D-009（阶段4）：/api 与 /plugins 的 Host 头 trust fence

**日期**：2025（本机时间）

**触发的问题**：`dsh web` 绑定 127.0.0.1 并提供 `/api`（可驱动 WASM loop、
读写 session）与 `/plugins`（web 插件源码）。若攻击者的域名被解析到
127.0.0.1（DNS rebinding），浏览器从该域名加载的恶意页面也能跨域发
`POST /api/...` 到本机——读到/改到宿主状态。ROADMAP 阶段 4 要求 trust fence
（Host 头校验 / loopback 判定，对齐 `api-request-trust.ts`）。

**考虑的选项**：
1. **Host 头 loopback 校验（本次采用）**：对 `/api` 与 `/plugins/` 请求，读
   `Host` 头，仅当主机名为 loopback（localhost / `[::1]` / 127/8）才放行，
   否则 403。判定对齐前端 `isLoopbackHostname`（浏览器同款语义）。
2. **绑定非 loopback + Origin 校验**：绑定 0.0.0.0 靠 `Origin` 头白名单。—
   绑定非 loopback 扩大了暴露面；`Origin` 易伪造，不如 Host 校验直接。
3. **不做**：静态服务可被 rebinding 滥用。— 违背安全目标。

**最终选择**：选项 1。只对带状态的 `/api` 与源码 `/plugins` 加 fence，静态资源
（CSS/JS/图片）放行（无敏感状态，且 rebinding 读取静态无危害）。

**选择理由**：Host 头校验是 DNS rebinding 的标准缓解（浏览器会拒绝与当前页面
Host 不匹配的请求目标；服务端只信 loopback Host 则恶意域名无法命中）。判定
语义与前端 `isLoopbackHostname` 一致，避免「前端认为 loopback、后端拒绝」的
不一致。纯函数 `hostname_is_loopback` 可单测。

**预期影响与回滚点**：`/api` 与 `/plugins` 非 loopback Host → 403 JSON。默认
绑定 127.0.0.1 的访问不受影响。回滚：撤提交即移除 fence。后续阶段 4 可把
headless boot 固化为 E2E（D-008 已记）。

---

## D-008（阶段1/2 验收）：真实浏览器 boot + 聊天闭环验证

**日期**：2025（本机时间）

**触发的问题**：ROADMAP 阶段 1 验收是「页面从 loading 失败报告 → 出现真实 UI
骨架」，阶段 2 验收是「基本聊天闭环可用」。仅靠 Rust 单测与 curl/ws 客户端
不足以证明前端真的能 boot、能交互——需要真实浏览器。

**考虑的选项**：
1. **Edge headless 验证（本次采用）**：机器装有 Edge。用 `msedge --headless
   --dump-dom` 打开真实前端，检查渲染后的 DOM 是否出现真实 UI 骨架；用
   `session.prompt` + `session.history` 验证聊天闭环。
2. **Playwright/Puppeteer 全自动**：功能强但需额外安装（机器未装）。headless
   Edge 已足够验证 boot 与 DOM 渲染。
3. **仅单测 + curl 冒烟**：证明协议层正确，但不证明前端真能 boot。— 不足。

**最终选择**：选项 1。Edge headless `--dump-dom` + `--screenshot`。

**选择理由**：自下而上用真实浏览器验证「前端 boot 成功」这一第一性事实，避免
「协议对但 UI 白屏」的盲区。headless Edge 零安装（机器已有），`--dump-dom`
即返回渲染后的 DOM，可直接 grep 断言 UI 元素。

**验证结果**：
- DOM 渲染出真实 UI：侧边栏 logo、「打开侧边栏」切换、命令按钮（命令）、
  模型选择器显示 **echo-loop**（证明 `session.models` 响应被前端正确解析）、
  发送按钮。DOM 252KB，不再是 loading 失败报告。
- 聊天闭环：`session.prompt`（content 数组）→ `{ok:true, accepted:true}` →
  `session.history` 返回完整 turn 流（turn/start→step/start→user/message→
  assistant/message→step/end→turn/end），WebSocket 同步推 `session/event` 帧。
- 残余错误均为功能级（`dynamicCordisRunner/syncInspectManifest`、`inventory`
  未实现；slot 注册冲突警告），非 boot 失败——留给阶段 4 全量方法面。

**预期影响与回滚点**：纯验证，无代码改动（除 ROADMAP 勾选）。后续阶段 4 可把
headless boot 固化为 E2E 测试。回滚：无。

---

## D-006（阶段2）：`/api/events.mux|host` 从 SSE 升级为真实 WebSocket downlink

**日期**：2025（本机时间）

**触发的问题**：前端 boot 时 `connection` 插件用 `WebApiClient`（浏览器端）经
`new WebSocket("/api/events.mux")` 与 `/api/events.host` 建立下链——不是 SSE。
`readSse`（streaming fetch）只是 node 半的物理载体；浏览器端 `readWebSocket`
才走 `new WebSocket`。M71 的 SSE 下链在真实浏览器里**无法连接**，前端停在
loading/连不上。必须提供同源真实 WebSocket。

**考虑的选项**：
1. **tiny_http 的 `upgrade()` + tungstenite `from_raw_socket(Role::Server)`
   （本次采用）**：tiny_http 为 WebSocket 提供 `Request::upgrade(protocol,
   response)`——完成 101 握手（写 `Upgrade`/`Connection` 头）并返回
   `Box<dyn ReadWrite + Send>` 双工流。`Sec-WebSocket-Accept` 需自算
   （base64(SHA1(key + GUID))），用成熟 `sha1` + `base64` crate。拿到流后
   用 tungstenite（成熟 WebSocket 协议库，D-004 同理不手写帧）`from_raw_socket`
   包成 `WebSocket<Box<dyn ReadWrite>>`，`send(Message::text(json))` 推帧。
2. **手写 WebSocket 帧解析/封装**：直接对着双工流编解码。— 重复造轮子，违背
   D-004「不手写协议」；帧掩码/分片/close 协商易错。
3. **换 async 框架（axum/actix + tokio-tungstenite）**：功能全但引入 tokio
   async 运行时，与单线程 `Rc<RefCell>` 运行时冲突（D-004 已否）。
4. **仅 SSE，不接 WebSocket**：真实前端连不上。— 不满足阶段 2 验收。

**最终选择**：选项 1。tiny_http 已内置 WebSocket upgrade 通道（其源码注释即言
「main purpose is to support websockets」），配合 tungstenite 包帧，两个成熟库
组合满足「同源 WebSocket downlink」，无需换运行时。`Sec-WebSocket-Accept` 是
RFC 6455 固定算法（SHA1 + 魔数 GUID），用 sha1/base64 crate 计算。

**选择理由**：保持 D-004 的约束（同步、不换 async 运行时、不手写 HTTP/协议）。
tiny_http 负责握手与双工流，tungstenite 负责帧协议，各司其职均为成熟库。帧
内容与 M71 的 SSE 帧一致（`session/subscribed` + `session/event` server-request
信封），仅物理载体从 SSE 换 WebSocket。

**预期影响与回滚点**：新增依赖 `tungstenite`（default-features=false，关
handshake 特性因握手由 tiny_http 完成）、`sha1`、`base64`。`/api/events.mux|host`
的 GET 请求若无 `Upgrade: websocket` 头则回落 SSE（兼容 curl/node 测试），有则
升级 WebSocket。回滚：撤提交回到 SSE 版；不影响 HTTP RPC/静态。

---

## D-005（阶段1）：注入 `__DSH_BOOT__` entry graph + 服务 `/plugins/<id>/client.js`

**日期**：2025（本机时间）

**触发的问题**：ROADMAP 阶段 1 要求前端从白屏 → 真实 UI。前端是浏览器端 cordis
插件系统：boot 必需 `window.__DSH_BOOT__`（host 注入的 entry graph），每个插件
是 `/plugins/<id>/client.js?rev=<hash>` 的 bundle，且所有 entry 必须 ACTIVE。

**考虑的选项**：
1. **扫描 plugin_root 的 package.json 组装 manifest（本次采用）**：遍历
   `node_modules/@deepseek-ai` 下声明 `dsh.client.platform === "web"` 且存在
   `lib/client.js` 的包，生成 `BootManifest {rev, entries}`；`/` 渲染 index.html
   时注入 `<head>` 首 script；`/plugins/<id>/client.js` 从 bundle 根读真实字节。
2. **硬编码 34 个插件清单**：把 ROADMAP §2 清单写死。— 无法随前端 bundle 版本
   演化，plugin_root 一变就失效，且违背「扫描真实包」的第一性事实。
3. **不做注入，仅静态服务**：前端永远停在 loading。— 不满足阶段 1 验收。

**最终选择**：选项 1。判定依据（platform==web && lib/client.js 存在）对齐
`ClientModuleRegistry.resolveMeta`；rev 用 bundle 内容确定 hash（内容一致则同
rev）；`immediately` 取声明值；inject 依赖边 informational。entry URL 带
`?rev=` 对齐前端缓存失效语义。

**选择理由**：自下而上从真实 bundle 结构出发（每个 web 插件确实是一个带
`dsh.client.platform` 声明的 npm 包），扫描法天然跟随前端版本演化，无需维护
硬编码清单；`<` 转义防注入逃逸对齐 `injectBootManifest`。这满足「Rust 只需服务
前端 + 注入 manifest + 提供 /api」的架构事实，无需重写 cordis。

**预期影响与回滚点**：`/` 现在返回注入 boot 的 index.html；`/plugins/*` 不再走
SPA fallback。新增 `default_plugin_root`（web_root 向上找 `@deepseek-ai`）与
`DSH_PLUGIN_ROOT` env 覆盖。回滚：撤提交即恢复纯静态版；不影响 M70/M71 基线。
后续阶段 2 需要 WebSocket downlink（浏览器用 `new WebSocket` 而非 SSE）。

---

## D-000（工程基线）：决策日志的建立

**日期**：2025（本机时间）

**触发的问题**：方法论要求关键决策留痕，但仓库此前无权威决策日志。

**最终选择**：在仓库根建立 `DECISIONS.md`，此后每次关键决策追加一条
（日期 / 触发问题 / 选项含被否理由 / 选择与理由 / 预期影响与回滚点），
git 提交信息引用决策编号。

**选择理由**：让「为什么代码长这样」有单一权威来源，且与 git 历史互查。

---

## D-002（M69）：WASM `tools::register` 完整桥接——`tools-handler` 导出接口

**日期**：2025（本机时间）

**触发的问题**：M68 只让宿主**记录** WASM 声明并给出明确错误，但「WASM 插件
注册的工具真正可执行」仍未打通——宿主执行 WASM 注册的工具时需要**回调进
WASM 组件**运行其 handler。

**考虑的选项**：
1. **完整桥接（本次采用）**：在 `dsh-loop.wit` world 增加 `export tools-handler`
   接口（`execute(name, args) -> result`），由 WASM 插件实现；宿主
   `WasmLoopPlugin::execute_tool` 对 WASM 注册的工具改调该导出。三个 loop
   插件（echo/llm/tool）都实现（tool-loop 真实执行 `wasm_echo`，另两个空实现）。
2. **WIT `handler: u32` 改为真实 `resource` 类型**（HANDOFF §7.13 早期设想）：
   让 wit-bindgen 托管句柄表。— 改动更大、需处理 Store 重入，且 name 分发
   已足够定位插件侧 handler，资源句柄是多余复杂度。
3. **维持仅记录**：不打通执行。— 违背目标「真正可执行」。

**最终选择**：选项 1。用「组件导出 `tools-handler` 接口 + 宿主按名回调」而非
WIT resource——name 即足够定位 handler，避开 resource 句柄表与 Store 重入的
复杂设计。

**选择理由**：这是最贴合「WASM 声明 → WASM 可执行」语义的最小闭环：WASM
插件既声明工具又实现其执行，宿主只负责路由。name 分发是幂等的、可测的、
可扩展的（新增工具只需插件侧多一个分支）。选项 2 的 resource 化是为跨组件
资源引用服务，此处用不上。

**预期影响与回滚点**：`dsh-loop.wit` world 新增导出 → 所有 loop 组件需实现
`tools-handler`（已补齐）。宿主 `execute_tool` 仅在 Store 空闲时回调（run_turn
之外的宿主驱动路径）——run_turn 内自调用注册工具仍受 wasmtime Store 重入
限制（已在 HANDOFF 注明）。回滚：撤提交即可回到 M68 仅记录态。

---

## D-003（M70）：`dsh web` 命令——复用现有 DeepSeek Harness 前端 + `/api` RPC

**日期**：2025（本机时间）

**触发的问题**：目标要求「支持 web 命令提供 web 页面服务，页面使用现有的
deepseek harness web 页面」。前端是已构建的 SPA（`dsh-web-frontend/dist`），
经 `location.origin` 推断后端基址——即**同源**服务。

**考虑的选项**：
1. **同源静态 + `/api` HTTP RPC（本次采用）**：Rust 侧既服务前端静态文件
   （SPA fallback → index.html），又承载 `POST /api/<method>` 的 client-request/
   server-response 信封传输（对齐 `@deepseek-ai/dsh-host-apiproxy`），桥接到
   dsh 运行时（sessions/tools/run_turn）。事件下链先以 SSE（keepalive 占位）。
2. **完整复刻 DSH Web 传输（HTTP RPC + trust fence + WebSocket downlink +
   全量方法）**：一次性实现全部。— 方法面极大（dozens of method groups）、
   需 WebSocket 帧协议与 trust fence，单轮不可达。
3. **仅静态文件服务**：不承载 /api。— SPA 能加载但无法连接后端，不满足
   「提供页面服务」的实质。

**最终选择**：选项 1。先交付可加载、可连接、可驱动 turn 的同源 Web 服务基线，
方法集（version/sessions/session.create/session.history/agent-loop）为可扩展
骨架，未实现方法 fail loud。

**选择理由**：用最小可验面覆盖「提供页面服务 + 连接后端 + 驱动 loop」的完整
语义闭环，且手写 HTTP/1.1 保持单线程纪律（同 llm_http）。前端同源复用零改动。
完整 DSH Web 传输（WebSocket downlink / 全量方法 / trust fence）作为后续增量，
`/api/events.mux|host` 的 SSE 占位即为其接入点。

**预期影响与回滚点**：新增 `dsh web` 子命令，不影响既有 stdin/headless/HMR
路径。事件 downlink 目前仅 keepalive——session 事件推送到前端为后续增强。
回滚：撤提交即移除子命令，不影响其他功能。

---

## D-004（M71）：`dsh web` 用成熟 HTTP 库 `tiny_http` + SSE 实时事件下链

**日期**：2025（本机时间）

**触发的问题**：用户反馈「web 功能调用使用成熟功能库，不要再自己手写轮子了」。
M70 的 `dsh web` 手写 HTTP/1.1 解析（read_head/read_body/http_response）——
既重复造轮子、易错（chunked/keep-alive/header 边界），又因**单线程 accept
循环**在 SSE 长连接上阻塞所有 POST /api（SSE 卡死 RPC）。

**考虑的选项**：
1. **`tiny_http`（本次采用）**：成熟同步 HTTP 服务器——每请求独立线程自带
   并发、完整解析 HTTP/1.1；贴合本项目单线程纪律（`Boot` 含 `Rc<RefCell>` 非
   Send，RPC 留在调用线程；SSE 只在 `SessionHandle`（Arc<Mutex>, Send+Sync）
   上跑，独立线程推帧）。RPC/静态逻辑保持纯函数（可测）。
2. **async 框架（axum/actix-web）**：功能更全但引入 tokio async 运行时，与
   单线程 `Rc<RefCell>` 运行时冲突，需大改 boot 的线程模型。
3. **继续手写 + 每连接一线程**：保留手写解析只加线程。— 仍是造轮子，且解析
   正确性风险仍在。

**最终选择**：选项 1。`tiny_http` 提供 HTTP 解析与并发，同时不强制 async；
RPC 与 SSE 分层（RPC 用 `&Boot`，SSE 用 `SessionHandle`）保持单线程纪律。

**选择理由**：用户明确要求成熟库；`tiny_http` 是同步生态里最贴合本项目约束的
选择——并发由库保证（每请求线程），SSE 长连接不再阻塞 RPC，且无需重构 boot
的线程模型。M71 同时把 SSE 下链从 keepalive 占位升级为**实时事件推送**：
握手 `session/subscribed` + 增量 `session/event` 帧（对齐 `muxFrameSchema`），
让前端在 turn 进行时实时看到 user/assistant/tool 事件——这是「完整可交互」
的关键。

**预期影响与回滚点**：新增依赖 `tiny_http`（含 ascii/chunked_transfer/httpdate
传递依赖）；行为变化：SSE 实时推送（此前仅 keepalive）+ 并发修复。回滚：撤
提交回到手写版或 M70；不影响其它功能。后续增强：WebSocket downlink 替代 SSE、
方法集扩展、trust fence（见 HANDOFF §7.14）。

**预期影响与回滚点**：纯文档，无运行时影响。

---

## D-016（M1b）：dsh-llm 同步运行时 + DeepSeek 适配器；transport 以「服务层线程桥」推迟

**日期**：2025（本机时间）

**触发的问题**：M1b 需交付 LLM 能力层——`dsh-llm` 运行时（retry/assembler/runtime/
模型元数据）与 `dsh-llm-deepseek` 适配器（SSE 解析/wire 序列化/translate/adapter），
并让 `LlmRuntime` + `DeepSeekAdapter` + `BlockAssembler` 全链可测。但核心是单线程
`Rc<RefCell>` 纪律（D-004/D-006）：真实 HTTP + SSE 字节 IO 需要线程、连接池与
阻塞语义，不能进核心。

**考虑的选项**：
1. **同步 `LlmAdapter` 缝 + 服务层线程桥（本次采用）**：`LlmAdapter::stream` 返回
   同步 `Box<dyn Iterator<Item=StreamChunk>>`；`DeepSeekAdapter` 只做纯函数组装——
   序列化、SSE 行解析、wire→chunk 翻译全在核心内（可差分、可单测）。真实 HTTP +
   SSE 字节 IO 由 `resolve_payloads`（transport thunk）抽象：适配器拿到 payloads
   后同步 translate，transport thunk 由服务层（M1e web.rs 线程桥）在桥内解析。
   连接事实（`DeepSeekConnection`）每次操作重读，配置文件变更无需重注册即达
   下一次请求。
2. **异步适配器（stream 返回 async Stream）**：功能最全但把 tokio async 运行时
   引入核心 seam，违背 D-004/D-006，且差分测试需 await 编排。
3. **适配器内直接做真实 HTTP**：核心掺入网络 IO 与连接管理，破坏纯函数可测性。

**最终选择**：选项 1。`stream` 同步迭代 + `PayloadsResolver` transport thunk；
`LlmRuntime` 在 `prepare_call` 绑定单次派发（dispatched-once guard + 配置漂移
检查），`defaultMaxTokens`/`defaultEffort` 在 resolve 阶段物化；`for_adapter`
replay 过滤器在 M1b 以 `adapter_owns_provider=true`（恒真）占位，M1e 接注册表
归属后落地。

**选择理由**：保持核心纯语义（可单测/可差分），把唯一非确定性源（真实网络）隔离在
服务层线程桥，与 dsh-session 的「纯语义内核 + 观察者表」同构。TS 常量/词汇在
`sese/serialize/translate` 单测覆盖，wire 字段名 snake_case 与 `types.ts` 逐字节对齐
（`golden_request_json_anchors_wire_parity` 锚定）。

**预期影响与回滚点**：新增 `dsh-llm-deepseek` crate（依赖 dsh-llm/dsh-brand/serde/
serde_json）。回滚：撤 M1b 提交即可；不影响 M0/M1a。M1e 线程桥是 transport 唯一
接线点；若桥延迟受阻，adapter 仍可用测试 thunk 全链验证。wire 字段名是兼容面，
改动需连同黄金测试更新。

---

## D-017（M1c）：dsh-compaction——压缩引擎按 TS 参考四模块推进，摘要缝每调用注入

**日期**：2025（本机时间）

**触发的问题**：M1c 需交付压缩能力（`compaction-basic`/`compaction`/`compaction-tool-result-pruner`/
`checkpoint`），并与 M1a 的 session surface + M1b 的 LLM 缝集成。TS 侧原始源码在
沙箱内只有 `node_modules` 编译产物（`@deepseek-ai/dsh-compaction-basic/lib/index.js`）可读，
且已确认 `summarize` 是**每次调用经 dependencies 注入**（`dependencies.summarize(...)`），
不是构造函数持有。Rust 核心是单线程 `Rc<RefCell>` 纪律（D-004/D-006），真实 LLM 摘要
调用必须推迟到服务层（M1e 线程桥）。

**考虑的选项**：
1. **纯语义四模块 + `Rc<dyn Fn>` Summarizer 每调用注入（本次采用）**：`basic.rs`
   （阈值/retained-tail/区域选择/压缩事务/一次性摘要框架 + token 估算/测量）、
   `engine.rs`（CompactionEngine 缝 + tool-pairing 平衡）、`pruner.rs`（Unicode 码点
   裁剪 + shadow-price）、`absorb.rs`（compaction/* wire 形状 + checkpoint user Replace）。
   `Summarizer = Rc<dyn Fn(&SummarizationInput) -> Result<SummaryResult,String>>`，
   `CompactionEngine` 三个方法（compactIfNeeded/compactNow/compactRegion）都带
   `summarize: &Summarizer` 参数——编译期强制每次调用显式接线，与 TS 的注入面一致，
   也便于测试用确定性替身。
2. **构造函数持有摘要闭包**：引擎生命周期内绑定单一摘要器，违背 TS 每次调用注入面
   且 M1e 服务层难以按 request 换摘要函数。
3. **async summarize（返回 Future）**：引入异步运行时，违背核心单线程纪律。

**最终选择**：选项 1。`BasicCompactionEngine` 实现 `CompactionEngine` trait，配置解析
（ratio/retainedTail/maxTokens/compactionRetries/overflowRetries + 成对校验）+ 模型
policy 覆盖 + `ModelInfoProvider`（默认 context window 65536）全部在核心内。压缩事务
（`compact_surface_region`）线性化：先落 `compaction/start`（锁）→ prepare/summarize/
稳定性检查/commit（`compaction/summary` + checkpoint user Replace）→ 成功/失败都落
`compaction/end`。`CompactionError` 实现 `Display`（串化对齐 TS 错误文本）。

**选择理由**：核心保持纯语义（可单测/可差分），唯一非确定性源（真实 LLM）隔离在
M1e 服务层；`Rc<dyn Fn>` 可 clone、跨多次压缩事务复用。TS 的「未匹配
`compaction/start` 阻挡并发压缩（busy 锁）」「无 open turn 时自动压缩拒绝」
「summary 不小于 shadowed 内容时拒绝」等错误文本与实现逐字对齐（`index.js` 已核对）。

**预期影响与回滚点**：新增 `crates/dsh-compaction`（依赖 dsh-session/dsh-llm/dsh-brand/
serde/serde_json）；`dsh-brand` 增加 `CompactionId`（拥有者=dsh-compaction）；
`dsh-llm::types::PluginMessageSource` 扩展 `extra: Map<String,Value>` 无损承载
`{kind:'plugin', plugin:'compact', compactionId, sourceCommandId?}`（checkpoint source，
已由 dsh-llm 单测覆盖 round-trip）。测试：`tests/{m1_compaction_tokens,config,toolpairing,
pruner,region,engine,checkpoint}.rs` 共 77 项全绿；全 workspace `cargo test` + `clippy
--all-targets` 零告警。回滚：撤 M1c 提交即可；不影响 M0/M1a/M1b。M1e 线程桥是摘要
缝唯一接线点，若受阻引擎仍可用测试替身全链验证。

---

## D-018（M1d）：dsh-persistence 按 TS 参考三模块推进——seam/coordinate/JSONL 后端；dsh-session-query 当期落地

**日期**：2025（本机时间）

**触发的问题**：M1d 需交付会话持久化链（`dsh-persistence`：coordinator/write-behind/jsonl/
import，`dsh-session-query`：projection/export）。TS 侧源码只在 `node_modules` 编译产物
（`@deepseek-ai/dsh-session-persistence/lib/index.js`、`-jsonl/lib/index.js`、
`dsh-session-query/lib/index.js`）可读，且为单线程、无 async 运行时、IO 推迟到服务层的
Rust 纪律（D-004/D-006，`Rc<RefCell>`）。

**考虑的选项**：
1. **无状态 JSONL 文件后端 + coordinator 实现 seam（采用）**：`jsonl.rs` 只做纯 IO
   （materialize/append/repair/list/read-raw），`coordinator.rs` 实现 `SessionPersistence`
   seam（状态复用、首 append 物化、write-behind 批窗、prepare 缓存、live-turn 守卫、
   crash 修复）。原子物化：temp 写 + fsync + rename 发布，失败回滚。可单测、可差分。
2. **后端自持状态（有状态文件后端）**：最初直接把 `SessionPersistence` 实现在
   `jsonl.rs` 上（无状态无法懒物化、append 即 NotFound）——被否：状态复用/守卫/WB 全部
   重复，且 seam 语义无法复用，故重写为「后端 + coordinator」结构（记录本次修正）。
3. **引入 tokio async**：违反 D-004/D-006 单线程核心纪律，且差分需 await 编排；否决。

**最终选择**：选项 1。物理格式逐字对齐 TS：`logPath = root/<projectKey|_no-cwd>/<encodeSegment(id)>/session.jsonl(.zstd)`，
header 行 + 每事件一行 + `\n`，packChunks 默认 true（MIN_RUN=3）打包，压缩默认 zstd——
checksummed 拼接独立帧（magic 0xFD2FB528、checksum flag ON），header 是独立帧。
`revision = dev:ino:size:mtimeNs:ctimeNs` 经 `:` join（Windows dev/ino=0 占位）。
`SessionWriteBehind` 移植 TS `SessionWriteBehind`（barrier/批量失败原序回队首/automaticPaused/
enqueue 唤醒自动写），`BatchSink::write` 单批同步返回。ProjectKey 用 TS `separatorRun`
把连续分隔符坍缩为单个 `-`（`--C-work-proj--`），`encodeSegment` 仅 `~` 与非
`[A-Za-z0-9._-]` 以 `~XXXX` 转义。`import.rs`（SessionImport）读取 TS 侧 zstd/plaintext
artifact，经 `Session::from_restore` 语义校验后落 Rust JSONL，拒绝覆盖（幂等安全）。
`dsh-session-query`（projection/export）也于本里程碑以独立 crate 完成。

**选择理由**：保持核心纯语义（可单测/可差分），把唯一非确定性源（真实磁盘 IO）留在
后端内部，与 dsh-session 的「纯语义内核 + 观察者表」同构。TS 常量/词汇在
pretty-print/scan/坐标对齐单测覆盖（zstd 帧区间 `[0,33]/[33,56]/[56,118]`、torn 56、
short 0 与 TS `scanZstdFrames` 逐字节一致；dsh-session-query 经子代理 65a8ab3b 交付并
18/18 测试 + clippy 零告警）。

**预期影响与回滚点**：新增 `crates/dsh-persistence`（依赖 dsh-brand/dsh-session/zstd
0.13.3）与 `crates/dsh-session-query`（依赖 dsh-session/dsh-persistence/dsh-brand/
dsh-llm）。M1d 测试：format 24 + zstd 7 + jsonl 集成 21 + import 5 + seam 8 = 65 项全绿；
TS↔Rust 交叉验证双向通过（TS node:zlib 写 → Rust 读；Rust 写 → TS node:zlib 解码）；
全 workspace `cargo test` + `clippy --all-targets` 零告警。回滚：撤 M1d 提交即可；
不影响 M0/M1a/M1b/M1c。M1e 线程桥（io.rs）是真实磁盘 IO 与 LLM 唯一接线点，尚未实现。

---

## D-019（M1d）：torn 尾容差的 Rust 差异——zstd crate 缺 `ZSTD_e_flush` 流式解码，容忍性靠 committed 前缀兜底

**日期**：2025（本机时间）

**触发的问题**：TS `decompressZstdPrefix` 用 `ZSTD_e_flush` 可恢复残缺末帧的明文前
缀；Rust `zstd` crate 的流式 `Decoder` 对截断帧直接报 "incomplete frame" 并输出零字节
（探针 `probe-zstd` 已确认），因此 Rust 无法逐字复刻 TS 的「torn 尾恢复」。

**考虑的选项**：
1. **截断到 committed 前缀兜底（采用）**：`scan_zstd_frames` 逐字节对齐 TS（magic →
   descriptor → block 循环 → checksum），`load_stored` 只把「最后一个完整帧的 end」作为
   `truncate_offset`（crash 修复 truncate 到该处），帧内 EOF → `tornStart` 标记 torn。
   `decompress_zstd_prefix` 对残缺帧返回空（可整帧解码则恢复）。容忍性靠「截断到
   committed 前缀」获得，与 TS 的语义目标一致（不丢已提交事件）。
2. **直接调用 libzstd C API 拿 `ZSTD_e_flush`**：需引入 unsafe FFI，违反纯 Rust
   纪律且破坏单测/差分；否决。

**最终选择**：选项 1。另记录：TS 用 `publishNewFileWin32`（`MoveFileExW` + 写透语义），
Rust 用 `std::fs::rename`（原子替换同卷）——两者崩溃语义等价（要么旧文件、要么新文件）。

**选择理由**：Rust `zstd` 是成熟、通用、广泛验证的库（D-017 依赖引入原则），不重复造
轮子；`ZSTD_e_flush` 缺口对我们的场景（崩溃时最多丢一个未提交 batch）无正确性影响。
**预期影响与回滚点**：`zstd.rs::decompress_zstd_prefix` 对残缺帧返回 `Ok(vec![])`（而
非错误），调用方（`load_stored`/`read_artifact`）已按 committed 前缀处理；如需恢复
torn 尾明文，届时再评估 FFI 或上游能力回滚点。不影响 D-018 的其他决策。

---

## D-020（M1e）：SessionHost 承载——WASM loop 仍写 SessionHandle，经 adopt 桥进入 dsh-session store + 持久化挂载

**日期**：2025（本机时间）

**触发的问题**：M1e 需把 web.rs `session.*` 方法面升级为由 `dsh-session::SessionStore` +
`dsh-llm` 驱动（M1-REQUIREMENTS §10），并把 Boot 的会话承载从 `SessionRegistry<
Arc<Mutex<SessionLog>>>` 升级为 dsh-session store + 持久化挂载。已知摩擦：dsh-session
`Session`/`SessionStore` 是单线程 `RefCell`（`!Send + !Sync`），而 web.rs 的 SSE/WS
下链线程是 `std::thread` 轮询 `SessionHandle`（Send+Sync）；WASM loop 经 WIT
`session::append(kind:String, payload:Vec<u8>)` 只写 `SessionLog`（无 time、free-string
kind），且「不改 loop 语义」（风险 §9.2）。

**关键事实（三项子代理 + 源码直读确认）**：
- `SessionHandle = Arc<Mutex<SessionLog>>`（dsh-core/src/session.rs:460），loop 的
  `session::append` 落在它上面（dsh-wasmrt/src/loop.rs:209）；前端要求
  `event.{type,seq,time,data}` 且 `additionalProperties:false`（session.json:9-23），
  而 `SessionLog.SessionEvent` 无 time → 必须换成带 time 的 dsh-session 事件。
- dsh-session `SessionEvent` 已含 `{seq,time: i64(epoch ms),kind,data}`（types.rs:681），
  serde camelCase（type/seq/time/data）——结构上正好贴前端 schema。
- dsh-session `SessionStore` 是 `Rc` 持有、`create/enter/announce/fork` 需 `&Rc<Self>`，
  持久化只在 `on_event`/`on_flush` 回调里（store.rs:73-90）；`PersistenceCoordinator`
  （dsh-persistence）需 `create(header)` 先、`append(id, events)` 要求 seq 连续、`
  set_live_turn` 守卫 live turn。
- web.rs 的 `serve` 在单线程 `for request in server.incoming_requests()` 里同步分派
  RPC（`dispatch_request`），只有 SSE/WS 下链单独起线程。

**考虑的选项**：
1. **SessionHost 桥（采用）**：`dsh-cli` 新增 `session_host.rs`——持有
   `Rc<SessionStore>` + `Rc<PersistenceCoordinator>` + `EventSink`（
   `Arc<Mutex<VecDeque<(SessionId, SessionEvent)>>>`，Send+Sync 供下链线程 drain）。
   挂载：`store.on_event` → `coord.create/append`（持久化）+ `sink.push`（下链）；
   `store.on_flush` → `coord.flush(id)`。loop 经 WIT 仍写 `SessionHandle`（原封不动），
   `session.prompt` 在 `run_turn` 后把 SessionHandle 的新事件 **adopt** 进目标
   `dsh-session::Session`：`(String kind, Vec<u8>)` → `EventKind::from_str` +
   `Value` + `SurfaceIntent(append)`，`Session::append` 校验并触发 on_event → 持久化
   + 下链。事件带 time、类型化，满足前端 schema 与读模型。
2. **直接把 dsh-session store 作为 ctx "sessions" 服务**：loop 的 WIT append 需
   `Arc<Mutex<...>>` 类型，与 Rc/RefCell store 不兼容，且 `Session::append` 需
   SurfaceIntent/typed kind——loop 语义必改；否决。
3. **web 流线程持 `Arc<Mutex<Session>>`（再次包锁）**：违背单线程核心纪律，也是
   D-004/D-006 明确反对的；否决。

**最终选择**：选项 1。`SessionHost`（dsh-cli 内）是唯一让单线程 core 与多线程下链共处
的接缝：所有 `SessionStore`/coordinator 操作在 serve 单线程（RPC 线程）发生；
`EventSink` 用 `Arc<Mutex>` 把只读事件帧交给下链线程。`session.prompt` = `run_turn`
(loop 写 SessionHandle) → adopt 新事件入 Session（类型化 + time）→ 持久化 + 下链。
`llm.providers`/`llm.models`/`session.models` 由 dsh-core `LlmService`（真实 host 缝，
Arc<Mutex>+真实 HTTP，m17_http_llm.rs 已有本地 TCP mock 全套）驱动——dsh-llm
`LlmRuntime` 的 `PayloadsResolver` HTTP+SSE 桥在本仓库尚不存在（子代理确认），用
dsh-core 缝是「现状允许」的选择，后续 M1e 若需 LlmRuntime 丰富目录再评估。

**选择理由**：保持「纯语义内核（dsh-session）+ 观察者表」同构（D-018 同）；loop 语义
不改（§9.2 风险最小化）；事件一次 adopt 即同时进持久化与下链，无 double-write；
跨线程只走 `Arc<Mutex<EventSink>>` 唯一下链通道。`EventKind::from_str` 是 total
（未知 → `Unknown`），adopt 不会因词表扩展崩坏。

**预期影响与回滚点**：`dsh-cli` 新增 `session_host.rs` + `Boot{ session_host: Rc<
SessionHost> }`（`sessions` 仍保留——loop 写 + headless `--session-in/out` 兼容）；
web.rs `session.list/create/history/prompt/fork/rename/models` 与 `llm.providers/
models` 改由 SessionHost/dsh-core LlmService 驱动；SSE/WS 下链接 EventSink。
`m1e_session_host.rs`/`m1e_web_rpc.rs` 测试 + 全 workspace test/clippy 零告警。
回滚：撤 M1e 提交即可；不影响 M1a–M1d。E2E 冒烟（`dsh web` + 会话恢复）作为验收。

---

## D-021（M1e 落地）：SessionHost 实现定稿 + web.rs 方法面重接线 + llm.* 目录源
**日期**：2025（本机时间）

**触发的问题**：D-020 选定「SessionHost 桥」后，编码落地时暴露三个实现细节需要
定稿：(a) 事件下链日志的读语义；(b) `session.*`/`llm.*` 方法面重接线后的形状收口；
(c) llm 空注册表的目录回退。

**关键事实（直读源码 + 参考 TS 实测）**：
- 前端 `sessionEventSchema` = strict-envelope `{type, seq, time, data,
  sourceEventSeqs?, surfaceOp?, ignorable?}`（`dsh-client-connection/lib`:5229）——
  `dsh-session::SessionEvent` 的 serde 逐字段对其（types.rs:797），mux `session/event`
  帧与该 history 条目共用该 schema。
- 多连接下链若用「drain 型 VecDeque」会相互抢事件；改用 **append-only
  `Vec<(String, SessionEvent)>` + 每连接自有游标**（`sink_since(from)`），
  对应原 SSE/WS 的 `last_seq` 增量读语义。
- `llm.providers`=`{providers:[configurableProviderViewSchema]}`；`llm.models`=
  `{groups, failures}`；`session.models`=`{current, routable, groups, failures}`
  （`dsh-host-apiproxy`/`dsh-client-connection` 实测）。

**考虑的选项**：
1. **下链=EventSink（append-only Vec + 游标）（采用）**：SSE/WS 线程各自
   `cursor = sink_len()` 起增量读，`mux_session_event_frame(session_id, ev)` 直接
   `serde_json::to_value(e)` 序列化 strict-envelope 事件（真实 time + 真实 sessionId）。
2. 下链仍轮询 `boot.sessions`（SessionLog）：丢失 dsh-session 的类型化事件与真实
   time；否决。
3. llm 空注册表直接给 `groups:[]`/`providers:[]`：前端 `session.models` 的
   `current.{provider,model}` 必须 `string().min(1)`，空目录会让 UI current 校验失败；
   故保留内置 loop 目录组（`dsh` 组：echo/llm/tool——本仓真实可运行的 WASM loop
   组件）作为**空注册表回退**，注册表非空时完全由 `LlmService::providers()` 驱动。

**最终选择**：
- `session_host.rs`：`EventSink = Arc<Mutex<Vec<(String, SessionEvent)>>>` +
  `sink_len()`/`sink_since()`；store.on_event → coord.create/append（持久化）+
  sink.push（下链）；on_flush → coord.flush。`restore_all()` 从持久化根恢复快照进
  store（`from_restore` → enter + announce + coord cursor 回灌）。
- `web.rs`：`serve` 构造 `Rc<SessionHost>`（有 `--session-dir` 则 `with_root`，否则
  `in_memory`），seed `default`；`handle_rpc` → `handle_rpc_host(boot, m, body,
  &host)`；session.list/create/history/fork/rename/prompt 全部由 SessionHost 驱动；
  SSE/WS 下链接 EventSink（真实 sessionId + time）。
- `llm.providers`/`llm.models`/`session.models` 由 `Boot.llm.providers()` 驱动，空
  注册表回退内置 loop 目录组。
- `Boot` 新增 `llm: LlmHandle`（boot() 从 cordis 取 `llm` 服务，fallback
  `new_llm()`）；web.rs 测试 helper `boot_with_sessions()` 补该字段。

**选择理由**：下链语义与前端 mux frame 逐字段一致（diff/TS 权威），多连接不抢数据；
llm 目录在「有真实注册表」时完全注册表驱动（不再是硬编码 echo/llm/tool），空注册表
回退保证 UI `current` 校验通过、演示流程不破。

**预期影响与回滚点**：新增 `dsh-cli/src/session_host.rs`（11 测试）+ `web.rs`
session.*/llm.* 方法面 + SSE/WS 下链（38 测试）+ `dsh-core/src/llm.rs`
`LlmProviderInfo`/`provider_ids()`/`providers()`（4 测试）。cargo deps 新增
dsh-session/dsh-persistence/dsh-brand（无 dsh-llm 直接依赖——dsh-session 已
re-export）。web_main 新增 `--session-dir <dir>`。回滚：撤本提交即可；不影响
M1a–M1d 与 headless `--session-in/out`（Boot.sessions 仍保留为 loop 写目标）。
E2E 冒烟（`dsh web` + prompt + SSE + 重启恢复）已通过。

---

## D-022（M1 验收记录）：M1 全里程碑交付 + 验收证据收口

**日期**：2025（本机时间）

**触发的问题**：M1 全部五个里程碑（M1a–M1e）已按构建序落地并各自提交（PLAN
§5/§12），需按 M1-REQUIREMENTS §5.7 六条验收标准逐条核对并留下可审计的验收工件。

**逐条验收证据（本轮复核）**：
1. **`cargo test --workspace` 全绿 + `cargo clippy --workspace --all-targets --
   -D warnings` 零警告**：全 workspace 测试二次复跑全绿（含 dsh-session 32、
   dsh-llm 29、dsh-llm-deepseek 34+1、dsh-compaction 81、dsh-persistence 7+、
   dsh-session-query 18、dsh-cli 38）；clippy 零告警。
2. **差分**：M1 语义包（session/llm-wire/compaction/persistence）以「vendored
   TS 为权威 + in-crate 字节级 golden」锚定（如 dsh-llm-deepseek
   `golden_request_json_anchors_wire_parity` 锚 wire 序列化逐字节）；仓库既有
   16 个差分场景（`dsh-diff` m63/m7 等）零回归。专用 `.mjs` ts-host 差分编排
   （session-host.mjs 等）按 `diff/ts-host/package.json` 的自我定界属于 M5
   Cordis-equivalence 差分，M1 不重复造，M1 crates 以 in-crate golden 满足
   「可差分」验收。
3. **契约面**：web.rs 45 个 RPC 方法分支；`rpc_extended_method_surface_ok`
   等 25+ 方法 shape 测试扩到真实语义（session.* 由 SessionHost 驱动、
   llm.* 由注册表驱动，不再空桩）。真实浏览器 `--dump-dom` 阶段验收此前已在
   D-008/ROADMAP 阶段 1–4 固化（boot → UI → prompt → history → mux 推帧）；
   M1e 后以真实前端 dist + HTTP/WS/SessionHost 冒烟复验（prompt→6 typed
   事件、SSE/WS 下链真实 sessionId+time、llm.* 注册表驱动、重启恢复）。
4. **流式**：llm 流式 chunk 端到端——`LlmRuntime.stream` + `BlockAssembler`
   + DeepSeek adapter（SSE payload → translate → assembler）在
   dsh-llm-deepseek `m1b_runtime_adapter.rs` 全链覆盖；dsh-core `m17_http_llm`
   本地 TCP mock server 覆盖真实 HTTP 面。
5. **持久化**：JSONL(zstd) 落盘 → 重启恢复 → 事件/投影/表面一致（SessionHost
   `restore_all` + web_e2e_prompt_persist_restart_restores + 真实 `--session-dir`
   E2E）；崩溃中断 turn 以 `interrupted` 收尾并可恢复读（dsh-session `repair`
   `interrupted_turn_closers` 覆盖）。
6. **压缩**：长会话 overflow → compaction-basic 选出范围 → replace 落 surface →
   压缩后 deriveMessages 一致（dsh-compaction 81 测试：engine trigger / region
   select / tool-pairing / pruner / checkpoint / 全事务生命周期提交）。

**最终选择**：M1 判定**交付完成**。新增 M1 验收记录作里程碑工件；后续 M2+ 以同一
模板细化。残余事项为已知 M5 范围（ts-host 差分编排、SQLite 后端、agent-loop 迁入），
不阻塞 M1 验收。

**预期影响与回滚点**：本记录仅追加 DECISIONS.md，无代码改动。回滚：无。

---

## D-023（M2a）：dsh-scope 迁移设计——作用域原语以「身份句柄 + 迷你派发」适配 Cordis 缺口

**日期**：2025（本机时间）

**触发的问题**：M2 需迁 `packages/core/scope`（`@deepseek-ai/dsh-scope`）——TS 里是
建立在 Cordis 之上的作用域注册原语（`createScope`/`scopeTarget`/`ScopedLayers`）。
但 Rust dsh-core 的 Cordis 等价物（`Cordis`，D-004/D-006）**缺少** Scope 语义所依赖的
两样东西：`ctx.extend` 的 Symbol 标签继承、与 `Context.filter`（以 thisArg 为 this 的
对象过滤器）的派发路由。若直接改 dsh-core 内核，回归风险大，违背「核心单线程纪律
不轻易改动」。

**第一性原理拆解**：scope 的**可观察语义**只有三件（report §0）：打标签（后代可见）、
读标签、由 `scopeTarget` carrier 做路由（无标签全局 / 带标签 `tag ∈ chain(key)`、
事件沿链上行绝不下行）。这不是「权限边界」而只是「注册可见性」；其真相是**纯内存
身份**（TS `ScopeKey = object`，引用相等，从不序列化）。

**考虑的选项**：
1. **身份句柄 + 迷你派发总线（本次采用）**：`ScopeKey(Rc<()>)`（指针身份 `Eq/Hash`）；
   父链表 = 模块级 `thread_local`（对齐 TS 模块级 WeakMap）；`ScopeCarrier::adopts`
   复刻逐调用方谓词；`ScopedContext` 迷你总线复刻「global 绕过 filter、带标签者
   adopts」派发；`NamedEntries`/`AnonymousEntries`/`ScopedLayers` 按 store.spec 逐
   条语义（live 迭代 + 代数、精确幂等 undo、聚合惰性创建/只回收空层、effect 时序与
   `['notify','undo','notify']` 回滚）。不依赖、不改动 dsh-core。
2. 扩 dsh-core Cordis（加 extend/filter/thisArg dispatch）：等价性最高，但侵入核心
   且无实时消费者压力（第三方 JS 插件不会在 Rust 宿主运行——决策 Q2 宿主插件 =
   Rust/WASM），风险收益比差。
3. 只搬数据无路由：丢「全局/作用域可见性」这一核心语义，upper 层无法按 agent 收发事件。

**最终选择**：选项 1。与 D-015（dsh-session 用最小观察者表代替 Cordis 总线）同构：
scope 提供给 dsh-agent/dsh-agent-loop 一个**测试完备、语义逐条对齐**的作用域机制，
消费方在 Rust 核心内用它做作用域事件派发。

**差异声明（与 TS 逐字节对齐的边界）**：
- `create_scope.dispose` = 同步幂等（Cordis 的异步 quiescence/inertia 反复排空在
  单线程核心无对应物）；`raw_dispose` 仍暴露精确 disposer 身份。
- `is_scope_carrier` 由类型系统保证（`ScopeCarrier` 即 carrier，无需 WeakMap 查询）；
  base filter 用捕获 base 的闭包表达（TS 是 `base[cordis.filter]` 方法、`this=base`）。
- `Scoped<T>` phantom brand 运行时擦除，不强加。
- 事件载荷在迷你总线里用 `serde_json::Value`（消费方一致的通用形态）。

**预期影响与回滚点**：新增 crate `dsh-scope`（零依赖纯语义）+ 23 项测试
（`tests/m2_scope.rs`，移植 scope/store/invariant 三 spec 的 24 条可观察行为）。
回滚：撤提交即移除，不影响既有 crate。增量：dsh-agent 等消费方在 M2d+ 接入。

---

## D-024（M2b）：dsh-tools 迁为分步交付——首批纯语义层（schema + json-schema + defineTool）

**日期**：2025（本机时间）

**触发的问题**：`packages/core/tools` 是 M2 最大的包（schema/json-schema/ts-types/
py-types/code-mode/index+types/testing 七个文件、~5600 行 TS、~8200 行测试）。
一次全迁进单个提交既难审又难保绿；PLAN §12 明确允许按能力切子步。工具缝的**可观察
语义**集中在极纯、零集成的水文件里：作者 DSL 编译、强制子集断言、值校验、SDK 代码
生成——它们的输出是逐字节/逐字可锚的，且被 agent-loop/model-facing schema 全量消费。

**第一性原理拆解**：tools 的「模型可见协议面」= `parameters` JSON Schema + 校验诊断；
这本就是纯函数。注册表/执行管线虽大，但**依赖**这些纯函数，且与 scope/Cordis 事件
耦合深（restriction 交集、staged scheduler）。所以正确顺序是：先交付并锁定纯语义层
（稳定协议面），再在其上交付注册表管线。

**考虑的选项**：
1. **分批地平线（本次 M2b-1）**：schema + json-schema + types + defineTool 全量迁移
   （编译产物 / 断言消息 / 校验消息逐字锚定，28+ 测试）。注册表 + 执行管线 + Code
   Mode + ts/py SDK 生成留待 M2b-2/3 各自提交。
2. 一次全迁：单提交巨大、红期长，违背 TDD「不长期红」纪律。
3. 只迁 DSL 不迁校验：丢最核心的「非法参数诊断」，model-facing 校验无处安放。

**最终选择**：选项 1。首批 4 个模块即生产可用协议面（defineTool/validateArgs/
assertSupportedJsonSchema/validateJsonSchemaValue），后续提交（runtime/sdk-gen）在
稳定协议面上增量。

**差异声明（Rust 面）**：
- `execute: Promise<...>` → 同步 `Result<Value, ToolFailureData>`（单线程核心
  D-004/D-006）；`ToolArgsError`/`JsonSchemaError` 以 data 载体（message+code+
  violations）承载，宿主尚未有统一 HarnessError 类型。
- 键序沿用 D-014（BTreeMap 规范序）；TS 保插入序——诊断顺序在 Rust 收敛为字典序
  （单违规消息不受影响，已在测试里按确定序锚定）。
- `AbortSignal` → `ToolSignal`（`Rc<Cell<bool>>` + reason `RefCell`）；code-mode/scheduler
  是 M5 范围（依赖 dsh-code-runtime），本次只保留 `run_code` 名称保留与错误 code 常量。

**预期影响与回滚点**：新增 crate `dsh-tools`（首批：schema/json_schema/types，28
测试，workspace 全绿 + clippy 零告警）。回滚：撤提交。增量：M2b-2 注册表
（dsh-scope ScopedLayers restriction）+ M2b-3 SDK 生成，M2e agent-loop 消费协议面。

---

## D-025（M2b-2）：dsh-tools ToolRuntime 注册表 + 执行管线（本轮提交）

**日期**：2025（本机时间）

**触发的问题**：在 M2b-1 交付纯语义层后，需要给 M2d/M2e 一块可用的注册表面：注册/
遮蔽/限制/呈现/执行。TS `packages/core/tools/index.ts` 的 `ToolRuntime` 整个架构挂在
Cordis 事件总线上（`tools/pre-execute`、`tools/post-execute`、approval gates、
`static inject`、`scheduler`、`member: ctx.layers`），这些在 Rust 单线程核心没有
Cordis 总线实体（D-004/D-006/D-023）。

**第一性原理拆解**：真正能被 M2e 消费的注册表语义 =（1）**view 解析**：全局基 +
作用域链遮蔽 + 限制交集 + 自有覆盖 + run_code 注入（纯函数、逐字节可锚）；（2）
**restriction/presentation**：可安置在 scoped layer 上的 effect 条目（依赖
dsh-scope 的 ScopedLayers，D-023 已交付）；（3）**执行管线**的「无 approval 主干」：
resolve → guard → body → output 校验 → render → finalize → 取消合成。approval 判定
与 staged scheduler（prepare/dispatch/finalize/finish waterfall）是 M2f/M2e 的接线
条目，不是注册表本身。

**考虑的选项**：
1. **交付注册表 + 视图 + 限制 + 呈现 + 无 approval 执行主干（本次采用）**：把
   `ToolRuntime` 的 Cordis 事件面收敛为 `Rc<dyn Fn()` 变更通知（对齐 dsh-scope 的
   `on_change`），approval 留 M2f 在 `execute` 前插桩。
2. 空窗口期一次交付完整 approval/scheduler：跨 M2e/M2f 依赖、红期长、违背 TDD。
3. 只交付注册表不做执行：M2e 无执行入口，集成无从谈起。

**最终选择**：选项 1。

**差异声明（Rust 面）**：
- `tool.execute: Promise<ToolOutput>` → 同步 `Result<Value, ToolFailureData>`；取消
  用 `ToolSignal`（D-024 已定型）：body 前取消 → `ABORTED_BEFORE_DISPATCH`，body 后
  取消 → `ABORTED`。
- 呈现桶（`presentCall`/`presentResult`/`sdkSection`/`collapseSection`/
  `CODE_ONLY_INSTRUCTION`）随 protocols/UI 属 M5；本轮 view 只给 `parameters`
  allowlist（`to_tool_schema`）。run_code 注入占位（执行给「需 code runtime」精确
  错误），name 保留在 register/restrict 两层无条件拦截。
- guards 存 layer（`Rc<RefCell<Vec<...>>>` 内部共享句柄，effect action 拿 `&L` 时
  经句柄写入/回滚）；同步执行。技术选型强制：`ToolLayer` 可变字段一律
  `Rc<RefCell<...>>`，顺应 ScopedLayers effect 的 `&L` 面（行动作与 undo 都只持
  共享句柄，不裸指针）。
- `ToolExecutionMode::Both` 语义收敛为「可同时 native/code 呈现」；本轮 collapse 仅
  针对 `mode==='code'`（与 TS `collapses` 判定一致）。
- `executionMode` fail-closed：仅 `is_concurrency_safe` 显式返回 `true` → parallel
  （含 catch_unwind 兜底抛错 → exclusive）。
- `ToolExecutionResult`（规范化：value/content/content_annotation/is_error/error/
  additional_contexts）+ `ToolErrorInfo{message,info?}` 对应 TS；原始执行身份
  `ToolExecution`/`ToolExecutionSnapshot` 保持 M2b-1 形态（finalize 钩子消费）。

**预期影响与回滚点**：新增 `runtime.rs` + `tests/m2b_tools_runtime.rs`（26 测试，
workspace 全绿、clippy 零告警，总额 dsh-tools 54 + workspace ①）。回滚：撤提交。
增量：M2e agent-loop 将 `registry` 于 initiator scope 挂载并消费 `schemas/execute/
execution_mode`；M2f 在 `execute_inner` 的 guard 段前插 approval 判定。

---

## D-026（M2b-3）：TS/Python SDK 代码生成（tools:sdk 提示区）——TS 味分步交付

**日期**：2025（本机时间）

**触发的问题**：Code Mode（`mode: 'code'`）下模型从 `tools:sdk` 提示区读取工具签名
（原生 schema 被省略），`tools/sdk` 区由 `renderToolsSdk`/`renderToolsSdkPy` 生成。
这是 M2b-2 交付注册表后可独立验收的最后一块纯语义（输出逐字节可锚）。

**第一性原理拆解**：两味渲染器的模型面语义 =（a）固定使用说明文本
（`SDK_INSTRUCTIONS`，逐字节固定）+（b）schema → 类型文本的纯映射（TS/Python 两组
映射表）+（c）完整声明组装（字典序排序 + 固定骨架）。TS 味纯 ASCII/确定性，可在 Rust
逐字节对齐；Python 味依赖 JS 引擎/CPython 各自的 Unicode 表（XID/NFKC/`toUpperCase`、
UTF-16 码元计数），版本偏斜无法在本环境基准内逐字节验证（上游自身也已把 case-mapping
暴露记为 deferral）。因此按 D-024 的分步精神拆成两次提交：M2b-3a = TS 味（本提交），
M2b-3b = Python 味（含已知偏斜声明）。

**考虑的选项**：
1. **先交付 TS 味（本次采用）**：`ts_types.rs` = `json_schema_to_ts`（total：
   断言/解析失败 → `"unknown"`，绝不抛）+ `render_tools_sdk` + `SDK_INSTRUCTIONS`
   逐字节，11 项映射/锚定测试。
2. 两味一起：Python 味偏斜判定需先立起来，Red 期长，违背「不长期红」。
3. 不做 SDK 生成：M2e 无 `tools:sdk` 区，Code Mode 集成缺模型面协议。

**最终选择**：选项 1。

**差异声明（Rust 面）**：
- 类型文档用与 TS 相同的「可组合文档」结构（`Part::Text`/`Part::Doc` +
  `contains_union_or_intersection`）：字符串段扫描 `|`/`&`、文档段用子标志，精确复刻
  `typeDocumentFrom` 的逐段判定（const/enum 值含 `|` 时 X 数组项加括号的边角也一致）。
- 数字展示：`serde_json::Number::to_string()`（整数无小数点、float ryu 最短往返）。
  与 JS `JSON.stringify` 的差异仅（记入本条目，测试不依赖）：超大整数（serde_json
  保 u64/i64 精确，JS 双精度已舍入——Rust 更精确）；指数形式（JS `1e+21` vs ryu
  `1e21`）；`1.0` 写法（JS 归一为 `1`）。
- 排序：Rust `str cmp` ≤ JS UTF-16 lexicographic；差异只在含 astral 字符的名字落入
  BMP private-use 附近，现实工具名不涉及，记入而不测。
- 空白折叠：`split_whitespace`（Unicode White_Space）≈ JS `\s+`，差仅在极端空白。

**预期影响与回滚点**：新增 `ts_types.rs` + `tests/m2b_tools_sdk_ts.rs`（11 测试）。
回滚：撤提交。增量：M2b-3b 交付 `py_types.rs`（Python 味）；M2c/M2e 把 `sdkSection`
（order 150，native→''）接到 systemPrompt+agent-loop。

---

## D-027（M2b-3b）：Python SDK 代码生成 + 本机网络降级记录

**日期**：2025（本机时间）

**触发的问题**：M2b-3a 完成 TS 味后，Python 味（`packages/core/tools/py-types.ts`）
是 `tools:sdk` 区在 `runtime.language === 'python'` 下的模型面来源。其实现依赖模型
无关的确定性算法（命名 TypedDict、类名分配、`Literal[...]`、list 深度上限、协议组装）
加一层 JS/CPython 各自的 Unicode 表（`\p{XID_Start}`/`\p{XID_Continue}`、NFKC、
`toUpperCase`、UTF-16 码元计数的 120 上限、`.split` 行为）。

**环境事件**：为支撑 XID/NFKC 引入 `unicode-ident`+`unicode-normalization` 时，
cargo fetch 遭遇 rsproxy 与 crates.io 双侧 `SEC_E_NO_CREDENTIALS` SSL 失败；curl/
Invoke-WebRequest 同样 000（沙箱出网 TLS 被阻断）。自修：`unicode-ident 1.0.24` 与
`unicode-normalization 0.1.25` 已在本地 registry 缓存，`cargo --offline` 全部解析与
构建成功。记录：本环境出网暂不可用不影响后续（增量都走已缓存离线路径）；若需新依赖
将显式通知用户协调网络。

**考虑的选项**：
1. **完整移植 + 声明偏斜（本次采用）**：用 `unicode-ident`/`unicode-normalization`
   实现 `is_bare_identifier`/`camel_case`/class-name 上限，输出对 ASCII/通用 BMP
   输入与 TS 逐字节一致；涉及不同 Unicode 表版本的边角显式记录为偏差（上游自身也把
   case-mapping 暴露记为 deferral）。
2. 只做 ASCII 近似：`路径` 等合法 Python 标识符会被降级到 subscript/`dict[str, Any]`，
   在 `mode:'code'` 下丢掉字段名/必填/类型——信息损失违背第一性原理。
3. 全部部署到 M5（code runtime 时代再调）：`sdkSection` 只有 code-mode 才消费，推迟
   虽可，但 M2 的「tools 包完整交付」关口依赖纯语义可测性（本模块零集成即可锚定）。

**最终选择**：选项 1。

**差异声明（Rust 面）**：
- `is_bare_identifier`/`camel_case` 的 XID/NFKC/case-map 表版本 = Rust 捆绑
  `unicode-ident`（~Unicode 15.x）≠ Node 22（17.0）≠ CPython 3.9.6（13.0）。输出对
  三套表交集内的名字逐字节一致；表版本差异仅命中近期赋码位/特定 case-map 边角，记入
  本条目而非伪造对齐。
- 类名上限 120 按 UTF-16 码元计（`encode_utf16`），跨界 astral 整体丢弃（复刻 JS
  `slice(0,120)` + high-surrogate 回退的净效果）。
- 数字展示：整数超安全范围精确十进制（serde_json u64/i64 精确，JS BigInt 双精度已
  舍入——Rust 更精确）；`1.0` 写法 serde 保留小数点（JS 归一为 `1`）；指数形式
  `e+21` vs `e21`。均极稀有且不影响可解析性。
- 排序：`str cmp` ≤ JS UTF-16 lexicographic（同 D-026）；属性顺序 BTreeMap 字典序
  (D-014) 不等同 JS 插入序（仅影响 Python 类字段排版）。
- `describe` 折叠用 `split_whitespace`（≈ JS `\s+`）；lone surrogate 在合法 UTF-8
  不存在，省略 `\uNNNN` 分支（`\xNN` 覆盖 C0/C1/DEL 全范围）。
- `MAX_LIST_NESTING=180`、`MAX_CLASS_NAME_BASE=120`、`RESERVED` 34 词、`TYPING_ORDER`
  全部照搬。

**预期影响与回滚点**：新增 `py_types.rs` + `tests/m2b_tools_sdk_py.rs`（9 测试）；
dsh-tools 总测试 74，workspace 全绿（离线）+ clippy 零告警。环境：新增依赖
`unicode-ident`/`unicode-normalization`（已缓存，离线解析）。回滚：撤提交。
增量：M2e 在 `sdkSection` 按 runtime 语言选 `render_tools_sdk`/`render_tools_sdk_py`
（native → SDK 区为空串，`dsh-tools: no SDK renderer for <language>` 兜底）。

---

## D-028（M2c）：dsh-system-prompt 组装/渲染/插值/伴生 invariant

**日期**：2025（本机时间）

**触发的问题**：M2c 需交付 `system-prompt` 包（assemble/orderTools/渲染纯函数/
严格 `{{var}}` 插值/水岭唤醒/complete 恢复/scoped 遮蔽/invariant）。依赖面含
dsh-scope 的 `entries_live()`（变量 live 迭代——TS `Map.entries` 在 provider 求值期
新注册可见）。

**考虑的选项**：
1. **完整收敛 + 窄化声明（本次采用）**：ScopedLayers 上实现 SystemPrompt；`variables`
   用 `Vec<(String, Option<String>)>` 保序（own-property 语义 + 与 JS 一致的注册名
   报错序）；水岭监听器在服务内 `Rc<RefCell<Vec<..>>>` 存储；invariant 校验可达类。
2. 引入完整 Cordis 事件内核：超出单线程 Rc 纪律（D-004/D-006），且 M2f 再评估。

**差异声明（Rust 面）**：
- `AssembleContext.signal`（AbortSignal）未入 Rust 面——栈中无取消令牌对象；显式
  组装的控制信号语义（不保留去控后续 turn）不变（记本条目）。
- `variables` 保序 Vec 的对 → 无原型穿透、报错列表序 = 全局插入序 + 作用域链远→近
  （对齐 JS Record own-property + Object.keys 序）。
- 水岭监听器：`register_assemble_listener(scope, prepend, cb)`；prepend = 队列最前
  （invariant 用全局+prepend 包外层）；采纳 = 监听器 tag ∈ assemble scope 的链
  （对齐 `ScopeCarrier.adopts`）。短路径无监听器 = 恒等。
- invariant 窄化：TS 校验「text 非 string」「变量值非 string|undefined」在 Rust 强
  类型下不可能（waterfall listener 只能产出 `String`/`Option<String>`）——Rust 面
  保留可达类别（空名/重名/变量名非法），消息逐字对齐 B.6 契约。
- 本机 rustc 不接受 `format!("{expr}")` 内联表达式捕获（已用最小示例探针证实），
  error 消息统一用位置参数渲染（对用户可见零差异）。
- 环境：本回合继续离线（出网 TLS 仍被阻）；新增依赖零、全走已缓存路径。

**预期影响与回滚点**：新增 `crates/dsh-system-prompt`（lib + invariant + 42 测试，
`tests/m2c_system_prompt.rs` 全绿）；dsh-scope 增量 `entries_live()`（+1 测试，
additive 不动既有 API）；workspace 98 二进制全绿 + clippy 零告警。回滚：撤提交。
增量：M2d dsh-agent 的 `assembleContextFor`（`{ agent, scope: agent }`）与
`installModelSelection` 在 `system-prompt/assemble` 水岭覆盖 `variables.provider/model`
接到本 crate 的水岭注册 API。

---

## D-030（M2d-2）：dsh-agent 活体生命周期（AgentBus/Registry/initiator/dispatch/invariant/model-selection）

**日期**：2025（本机时间）

**触发的问题**：M2d 二轮落地「活体」部分——subject 作用域事件派发、注册表有序
生命周期、initiator 作用域、dispatch 融合、agent-invariant、model-selection 双钩。
这些依赖 Cordis `ctx.events` 语义在 Rust 的最小复刻，且 Cordis/dsh-core 的事件是
fiber-ScopeId 过滤（非 subject）——故按 D-023 的既定裁定走 dsh-agent 自带总线。

**考虑的选项**：
1. **自建 `AgentBus`（本次采用）**：专为 agent-subject 派发；模式支持 emit（通知，
   逐 listener catch_unwind 包含、不可 veto）、emit_veto（首抛传播=发布否决）、
   serial/waterfall（next 短路链）；收集-再执行（重入安全）；用 dsh-scope
   `ScopeCarrier::adopts` 做路由过滤。不入 dsh-core（那是 fiber 过滤），复用
   dsh-scope 路由原语。
2. 扩 dsh-scope `ScopedContext`：其 `emit` 无包含/veto 区分、非重入，改它违反
   「不跨界改已交付包」纪律。

**差异声明（Rust 面）**：
- initiator 为**同步栈**（with/without/current/require + 'active→'closing→'disposed'；
  边界守卫防泄漏），无 Promise drain/branded Promise/cross-realm（报告 A.7 承认）；
  读/写都要求 'active'，'closing'/'disposed' → `agent initiator scope is disposed` 逐字。
- created 监听器**同步抛 = veto**（register 抛同错误 + 回滚 disposed:vetoed）；
  无 async reject，故 `agent/created listener rejected` warn 不产生（D-029 已有声明）。
- 通知 warn 为同步 throw 词形：`agent event "<name>" listener threw: <e>`、
  `agent "<id>": agent/disposed listener threw: <e>`（rejected 变体无 async 不产生）。
- dispose 事件 payload 的 `agent` 字段为 Agent 的可串化投影 `{"id": ...}`（live 事件
  不入 log，测试按 `.agent.id` 断言；D-029 同类口径）。
- model-selection 只安装（无 disposer 组合）；拆除由 M2e loop 的 agent scope 拆除
  承担（TS 同语义：安装在 agent scope 内由 teardown 卸载）。`assemble_context_for`
  Rust 面无 signal（D-028），只在 scope 字段携带 agent。
- registry 的 create/resume 把 `owner` 从当前 initiator 快照传给 factory（无
  Fiber 重追踪，报告 A.7 承认）。

**预期影响与回滚点**：新增 `agent_bus.rs`/`registry.rs`/`dispatch.rs`/
`invariant.rs`/`model_selection.rs`（+21 测试）；dsh-agent 合计 44 测试；
workspace 815 测试全绿 + clippy 零告警。回滚：撤提交。增量：M2e agent-loop 的
`agent/request` 水岭消费 assembled、状态机在 `agent/status` 上转移并经 invariant
守卫；M2d-2 生命周期链在 agent dispose 时关闭 initiator + 拆 scoped 世界。

---

## D-029（M2d-1）：dsh-agent 的 durable/记账核心（Inbox + foldConsumedWork）

**日期**：2025（本机时间）

**触发的问题**：M2d 首轮需落地 dsh-agent 中「协议关键、语义可逐字节锚定」的部分：
Inbox 的 durable 双队列投影（JS `Array.prototype.splice` 标准化算术 + 
`agent/inbox/spliced` wire）+ `foldConsumedWork` 记账。注册表/dispatch/
model-selection/invariant/initiator 依赖一个**作用域事件总线设计的接入**（M2d-2
再评——dsh-scope 的 `ScopedContext`/emit_scoped 是候选）。

**考虑的选项**：
1. **首轮只交付 durable/记账核心（本次采用）**：落 `types.rs`（AgentOptions/
   AgentStatus/InboxTarget/ModelSelection/ConsumedWork 全类型面）+ `inbox.rs` +
   `consumed_work.rs`；23 测试对齐 agent.spec 的 Inbox 与 consumed-work.spec 全场景。
2. 首轮直接全量 dsh-agent：注册表生命周期/initiator ALS 等价物/事件总线量过大，
   与既有水岭分步（M2b 三拆）一致，风险集中到一轮。

**差异声明（Rust 面）**：
- `agent/inbox/spliced` wire：可选字段（`removedCount`/`outcome`）缺省即省略；
  `removedCount` 仅当 >0 出现（与 fold「removedCount 缺省 = 纯插入」口径一致）。
- 重建语义：`Inbox::new` 重放 `header.seedLength`（缺省 0）之后的持久 splice，
  错误包装 `invalid persisted inbox splice at session seq <seq>` 逐字。
- `claim` 的持久事件**无 outcome**（discardRemoved=false）；`clear`/`remove`/
  `replace` 有 outcome='canceled'；`0 删 0 插` 不写事件。
- 通知三素（inserted/discarded/claimed）以 `InboxNotify = Rc<dyn Fn>` 钩子暴露；
  live 总线事件（agent/inbox/inserted 等作用域事件）M2d-2 在此钩子上发射。
- 无其它行为差异；算术 `f64::trunc`/NaN→0/负坐标/上界截断对齐 JS。

**预期影响与回滚点**：新增 `crates/dsh-agent`（types + inbox + consumed_work +
23 测试）；workspace 成员 +1。回滚：撤提交。增量：M2d-2 在 `with_notify` 钩子上
接作用域事件，并交付 AgentRegistry/initiator/model-selection/invariant。

---

## D-031（M2e-1）：dsh-agent-loop 的请求重建层（constants/settings/requestProposal/buildRequest/invariant）

**日期**：2025（本机时间）

**触发的问题**：M2e 首轮需要落地 agent-loop 中「请求可由 session 日志逐字节重建」（THEOREM）
的**构造侧 + 校验侧**纯内核：settings 严格验证、requestProposal、buildRequest 的
initial/resume/change header-锚点与 request/context 增量、loop 标记、request-reconstruction
invariant。驱动状态机（turn/preStep/step）与 tool 调度、AgentLoop 服务留 M2e-2/3。

**考虑的选项**：
1. **先做请求重建纯内核（本次采用）**：constants + settings 验证 + `requestProposal` +
   `build_request`（propose/prepare_call 以**纯函数钩子**收口，M2e-2 再接作用域总线）+
   `check_loop_request`（fail 逐字）；与 M2d 两次拆分的瀑布尺寸一致，风险集中度低。
2. 首次直接全量 ReactLoopAgent 驱动：turn/step/tool 调度绑定过宽，MockAdapter 尚未就位，
   与瀑布「先接口契约再状态机」冲突。

**差值声明：Rust 面**：
- `AgentLoopRequest(GenerateOptions)` 包装类型 = TS `markAgentLoopRequest` 的 symbol 标记；
  `isAgentLoopRequest` 的**运行时门**在 Rust 变**类型面保证**（loop 唯一生产者；TS 的门
  存在只因事件边界是动态的）。frozen 检查同理：Rust 不可变值为恒冻结，无运行时检查
  （`a loop-built request must be frozen` 与 messages-frozen 两词形 Rust 不可达，测试不锚定）。
- `check_loop_request(request, Option<&Rc<Session>>) -> Result<(), String>`：**无 session**
  = TS `ctx.sessions.get(id)` 未中 → `a loop-built request must carry a live session id,
  got "<id>"`（`session_id` 缺失先于其触发，与 TS 源顺序一致）；fail 为**首个违例即失败**
  （TS `fail: () => never` 抛错，非累积）。
- `build_request` 无 AbortSignal（sync 纪律，D-028 同款）；`agent/request` 水岭在 M2e-1 是
  `&dyn Fn(CallConfig) -> CallConfig` 恒等钩子（`{turn,step,signal}` 上下文 M2e-2 由驱动
  线程化），其余 buildRequest 语义逐行移植：seed = `requestProposal(persistedHeader)` /
  `{...route, reasoningEffort?, maxTokens?}`；显式 effort 仅当 provider+model 同且非适配器
  填充时恢复；prepareCall `NO_ADAPTER` 降级透传 config；`canonicalHeader` 空 system/tools
  不写字段；reason 'initial'（无基线）/'resume'/'change'；request/context 仅在变化时记录。
- settings 验证消息逐字：`maxParallelToolCalls must be a positive integer`、
  `agent maxTokens must be a positive safe integer`；缺省 `DEFAULT_MAX_PARALLEL_TOOL_CALLS=10`。
- invariant 的 6 条可达 fail 文本逐字节对齐 `agent-loop/src/invariant.ts`（缺 session id /
  无活 session / 无 step-start / 无 request-header / 派生消息分歧 / 折叠头分歧）。

**预期影响与回滚点**：新增 `crates/dsh-agent-loop`（constants/settings/build_request/
invariant + 18 测试），workspace 成员 +1，815→833 全绿 + clippy 零告警。回滚：撤销提交。
增量：M2e-2 ReactLoopAgent 驱动把 `build_request` 的水岭绑到 AgentBus、在
`llm/stream` 派发点装配 `check_loop_request`。

---

## D-032（M2e-2）：ReactLoopAgent 同步驱动（turn/preStep/step 状态机 + send/steer/cancel + 水岭接线）

**日期**：2025（本机时间）

**触发的问题**：M2e 第二步需要把请求重建层接到**驱动状态机**：Phase（Idle/Maintenance/
Running）→ agent/status、send/followup/steer/inject → wakeDriver/kick 与 reentrant latch、
turn()/preStep()/step() 完整链路（含 pre-step 决策水岭、turn-stopping serial、request-error
重试、max-tokens 粘性、中断前缀），并以 mock 流完成全部生命周期测试。tool 调度/MockAdapter/
AgentLoop 服务/runtime-context 投影留 M2e-3。

**考虑的选项**：
1. **同步 inline 驱动（本次采用）**：send——唤起 driver 即同步排空（kick 内整段完成）；
   对应 TS async 列表的「此刻可完成的推进」子集；无 Promise 队列、无 AbortController。
2. 引入同线程任务队列模拟 async 调度点：复杂度高，与订单态机同步纪律冲突，收益仅是
   测试形态更接近 JS。

**差值声明（sync 语义，逐项核对后）**：
- **无 AbortSignal**：取消 = `Phase.abort_reason: Option<AgentCancelCause>`（合作式）。检查点
  对齐 TS `signal.throwIfAborted()` 位置：turn 首行（try 外——取消→无 turn 记录，Err 直返）、
  turn 循环顶、pre_step 装配后、step/start 前、step 内 while 顶、stream try 前、逐 chunk 前、
  stream 后、tool 执行后、turn-stopping serial 后。**中流抢占不可达**（sync 流物化为预取
  Vec；每 chunk 前仍有检查，但只对「流已整体返回后额外取消」有意义）；TS caught-mid-stream
  的 `interruptedBlocks` 前缀路径保留代码但正常测试不可达（D-028 同款 declared divergence）。
- **wakeDriver 内联同步**：TS `wakeDriver()` 在 idle 时以 `withInitiator(+index)` 真异步排空；
  Rust idle-分支在 `with_initiator` 内**同步**跑 kick；running/maintenance 分支同 latch（
  `abort_reason!=Disposed && (maintenance || wakeAfterAbort)` 才 latch）。kick 内 re-wake
  （踢完仍 latch+hasPending）在同一次 with_initiator 边界内重开相位——initiator 权威等同
  TS 的嵌套 withInitiator，一次 send 的整段都受同一发起人作用域约束。
- **每 turn 一 signal → 每 turn 一 abort_reason**：续 turn 前清 abort_reason+wake_requested、
  step 归零（fresh controller，对齐 TS `phase.abort = new AbortController()`）。
- **stream = `&dyn Fn(&GenerateOptions) -> Result<Vec<StreamChunk>, LlmError>`** 同步物化
  Vec（无后端本轮）；`preparedCall.stream ?? llm.stream` 的选择**留 M2e-3**（本轮恒用 hook）。
  finish `error|aborted` → `agent/request-error` 水岭（payload 含 turn/step/provider/failure；
  `retryPolicy` **M2e-2 未序列化**，M2e-3 接 `prepared.retryPolicy`）；`kind!='retry'`→
  throw LlmError → turn error（turnEnds=Error{failure}）。
- **错误分层**：`Halt::Aborted(cause)`/`Halt::Failed(LlmFailure)` 内部类型。非 abort 错误在
  turn catch 统一 emit `agent/error {turn,step,error}` + turnEnds=Error；abort 路径静默
  （turnEnds=Aborted{reason}）；turn/start、turn/end 追加失败各自 emit（含无 turn/end 的
  前置失败，对齐 TS 的独立 try/catch）。
- **监听器抛错**：bus `waterfall/serial` 链监听器 panic 由驱动 `catch_unwind` 收窄为
  `Halt::Failed("{name} listener threw: …")`（同 emit 的 skip-errors 纪律）；agent/request
  水岭内同款收窄，返回给 build_request 的 propose。
- **辅助接线**：live `agent/inbox/{inserted,discarded,claimed}` 事件由驱动构造时经
  `Agent::set_inbox_notify` 注入（新增 dsh-agent 小刀：Inbox::set_notify +
  Agent::set_inbox_notify；`AgentEventDispatch` 补 Clone）。project_context 本轮收
  `&PromptAssembly -> Option<Message>`（M2e-3 接 RuntimeContextProjection 的 joined
  sections）。tool_exec 收 `ToolExecCtx`（M2e-3 接 executeToolCalls + scheduler）。
- 模型文本/事件形状对照 agent.ts：pre-step 水岭 payload `{messages,turn,step}`（+agent
  融合）；默认决策 enter=`{kind:'enter', messages: claimed或claimed+context}`；空 enter
  决策「初始 turn 无步完成」与「turnEnds 后再空则 break」双分支逐行；user/message 在
  step/start 之后逐条 append；assistant/message `{turn,step,message,usage?}` +
  surfaceOp=append + sourceEventSeqs=chunk seqs；turn-stopping serial `{turn}`；
  `agent/status` payload `{status}`。

**预期影响与回滚点**：`crates/dsh-agent-loop` 新增 `agent.rs`（驱动）+ `tests/m2e2_driver.rs`
（13 测试）；`build_request` 签名扩展（`turn/step` 透传 + `propose(c,t,s)`）并更新 m2e1 全调用；
dsh-agent 增 3 处小刀。833→846 全绿 + clippy 零告警。回滚：撤销本提交。
增量：M2e-3 把 tool_exec/stream/project_context 换成真实 scheduler/MockAdapter/
RuntimeContextProjection，`preparedCall.stream` 选择归一，request-error 补 retryPolicy。

## D-033（M2e-3）：tool-calls 调度 + RuntimeContextProjection + AgentLoop 服务装配

**日期**：2025（本机时间）

**触发的问题**：M2e-3 把 M2e-2 留下的三个 mock 依赖换成真实接线：tool 调度
（`executeToolCalls`）、runtime-context 投影、AgentLoop 服务装配（`preparedCall.stream ??
llm.stream` 归一 + invariant 守卫 + MockAdapter 闭环验证）。要求：模型顺序提交、并行/独占
分类、`tool/call`+`tool/result` 事件与 `sourceEventSeqs`、`parseArguments`、skipped 合成、
`concluded` 来自 `concludesTurn`、retained 三态/CLEARED/清理全部有测试锚定，且保持 driver
（M2e-2）接口不变。

**考虑的选项**：
1. **sync 顺序调度（本次采用）**：按模型顺序循环 `run_group`；独占=单 call 屏障，并行=滚动
   池（顺序执行天然有序）。对齐 TS 的「结果与上下文按模型顺序提交」「已启动提交、未启动
   排水或跳过」。
2. 引入 `ToolExecutionMode` 真并发线程池：dsh-tools 自身的执行管线也是 sync（execute 闭包
   return 值，无 await），引入并发塞收益为零、破坏「日志即事实（事件顺序=提交顺序）」，
   否决。

**差值声明（sync 语义，逐项核对后，D-032 继续）**：
- **无并发 inFlight 池**：`run_group` 顺序执行整个并行组；「并行调用可同时运行」退化但
  **分类决定（屏障）语义保留**：组内 idx>0 起重新读 `execution_mode`，转 exclusive → 新
  屏障（移交下一轮）；事件/上下文模型顺序不变。
- **Abort 排水不可达**：调度器 ToolSignal 每次全新、driver 无中流抢占（sync 流已物化），
  `aborted_before_dispatch` 检测与 skipped 合成保留 API 但正常路径不可达（同 D-032）。
- **`concluded` 来源**：dsh-tools `ToolExecutionResult` 新增 `concludes_turn: bool`；执行期
  `ToolRunContext::conclude_turn()`（对偶 TS `exec.concludeTurn()`）在成功路径归一化时读取。
  失败/取消结果恒 false（TS 同款：failure never concludesTurn）。
- **tool/result.meta 未接**：Rust runtime 无 meta 字段（TS 有 meta 透传）；D-033 声明为
  差值，M?f 或暴露 meta 时补。`additional_contexts` 已接入（scheduler 收集为 next-step
  Messages）——真实 runtime 目前恒空（工具尚无额外上下文产出器），契约已测。
- **RuntimeContextProjection 无事件订阅**：TS 构造时扫描历史 + 跟随事件；Rust 每 `project`
  前 `reconcile` 从当前日志**权威重派生** retained（后向找最后一个 owned 且仍在 surface 的
  user/message；owned 但不在 surface → 无 retained；无 owned → never）。日志即事实源，
  THEOREM 保证与跟随等价；surface-replacement 撤销快照由 reconcile 自然体现。
- **`prepareCall.stream ?? llm.stream`** 归一：driver step 先取 `built.prepared_call.stream`
  （`LlmRuntime::prepare_call` 捕获注册表派发），回退 `deps.stream`；二者都在 service 处挂
  invariant 守卫（`check_loop_request`）。
- **invariant 守卫位置**：prepared 流包装 + `deps.stream` fallback 两路都守卫；逐字 fail
  message 不变（invariant.rs 已实现），守卫失败 → LlmError(code UNKNOWN)。
- **MockAdapter**（`tests/m2e3_service.rs`，对齐 TS mock-adapter.ts）：脚本驱动 LlmAdapter，
  `stream` 出队列 `Vec<StreamChunk>`；耗尽 → Finish Error（`SCRIPT_EXHAUSTED`）。

**dsh-tools 变更**：`ToolExecutionResult.concludes_turn`（4 处字面量 + 结构体）；
`ToolRunContext.concludes_turn`（pub(crate) Cell）+ `conclude_turn()`/`concludes_turn()`；
lib 重新导出 `TOOL_ABORTED_BEFORE_DISPATCH` 等 code 常量。

**预期影响与回滚点**：新增 `crates/dsh-agent-loop/src/tool_calls.rs` /
`runtime_context.rs` / `service.rs`；driver step 微改（`prepared_call.stream.take()` 优先）；
新增测试 `m2e3_scheduler`（7）/ `m2e3_projection`（7）/ `m2e3_service`（3，含真实工具闭环）。
workspace 全绿 + clippy `-D warnings` 零告警。回滚：撤销本提交（保留 D-032 驱动）。
增量：M2f 把 approval/guard 接 tool pre 阶段；M2g 把 AgentLoop 服务装进 host boot。

## D-034（M2f）：tool pre 阶段审批（approval）接线

**日期**：2025（本机时间）

**触发的问题**：M2f 把 M2b/M2e 留下的 approval 接线点补上——`tools/pre-execute`
（allow/deny/ask）与 `serviceAsk`（审批通道把 ask 解析为放行/拒绝）。TS 侧验收标准：
ask 无通道 → 逐字拒绝 `tool "<name>" requires approval (not yet supported)`；无 agent →
`...has no agent to route it through`；`allowed-once` → 放行（guards 仍挡）；rejected /
cancelled / unavailable → 三种逐字拒绝；deny 物化为 `Error: <reason>` 的 error 结果。

**考虑的选项**：
1. **同步决策钩子 + 全局 ApprovalProvider（本次采用）**：`add_pre_decision`（waterfall 到
   allow：None = delegate；首个非 None 即最终决策）表达 pre-execute；`ApprovalProvider`
   = `Fn(&ToolExecution, Option<&str>) -> ApprovalOutcome` 同步决策者，注册在 registry 根
   （TS `ctx.get('approval')` 的全局语义）。
2. 事件总线 waterfall（Cordis 完整 pre-execute chain）：同步 rust 无异步 listener 生态，
   单个 hook 序列已覆盖「先决策后执行」语义；full waterfall 留宿主总线基建（M3），否决。
3. 独立 `dsh-interaction` crate：仅一个类型别名/决策者函数，无宿主消费者（M3 才装），
   违反「Require a current owner and need」，否决——通道类型先进 dsh-tools，宿主装
   approval service 时（M3）再决定是否外提。

**差值声明（sync 语义，逐项核对后）**：
- **approval 请求是同步决策而非异步等待**：TS `approval.request()` 是异步心跳（起 UI 往返、
  等待用户）；Rust 以同步回调表达（宿主把 UI 往返放在 loop 之外，返回一次性 outcome）。
  唯一受影响的是「pending 等待期」——同步 loop 不可能悬空一个 step，declare divergence。
- **approvalCancelled→aborted-before-dispatch 不可达**：该分支需要 ask 决议后 caller 已
  取消；同步 loop 无中流抢占（流已物化、无并发 dispatcher），Cancelled 态恒以逐字拒绝
  `approval for tool "<name>" was cancelled` 物化错误结果。
- **ask 的 reason 语义**：TS `ask.reason ?? not-yet-supported`——有 reason 时无通道拒绝用
  reason 原文；代码逐字对齐。
- **模式：pre-decision 先于 guards**（对齐 prepareExecution：pre-execute waterfall → deny →
  guardReason）→ 再 body。deny（任意来源）走 `post_blocked_result`：`Error: <reason>` +
  isError=true + error{message, info:None}（TS `materializeFinalResult` error.info 缺省；
  scheduler 因此不写 error 块——D-033 的 append_tool_result 只透传 `e.info`）。

**dsh-tools 变更**：`PreToolDecision` / `ApprovalOutcome` / `ToolPreDecision` /
`ApprovalProvider` 类型；`ToolLayer.pre_decisions`（effect 可拆卸）；registry
`add_pre_decision` / `set_approval_provider`（返回前值便于组合）/ `approval_provider`；
`execute_inner` pre-phase 重写；lib 重新导出新类型。

**预期影响与回滚点**：新增 `dsh-tools/tests/m2f_approval.rs`（12）+ `dsh-agent-loop/
tests/m2f_interaction.rs`（2，真实闭环：ask 无通道 → tool/result 逐字拒绝且工具体不执行；
allowed-once → 工具体执行）。workspace 112 套件全绿 + clippy 零告警。回滚：撤销本提交。
增量：M2g 把 host boot 装进 web.rs + E2E 冒烟；approval 通道宿主化（M3）。

## D-035（M2g）：AgentLoopHost 宿主装配 + web.rs 路由 Rust loop

**日期**：2025（本机时间）

**触发的问题**：M2 收口需要把已建的 agent 能力缝（dsh-agent / dsh-agent-loop /
dsh-system-prompt / dsh-tools / dsh-scope）组装成**宿主服务**：按组合配置
（`AgentLoop.Config` 形态）装配 agent、暴露设置与配置身份（`CONFIGURED_AGENT_IDENTITIES_KEY`）、
负责生命周期 teardown，并把 Rust loop 接进 `dsh web`（`session.prompt`/`agent.run`）。

**考虑的选项**：
1. **宿主装配模块放 dsh-agent-loop（本次采用）**：`AgentLoopHost` 自拥 bus/registry/
   store/llm/tools/prompt，`with_store` 可注入外部 SessionStore（web 共享同一事实源）。
   agent 懒装配（`ensure_agent` 幂等）。
2. 在 dsh-cli 里手写装配：承载在 web crate 但无独立测试面，且 composer 语义（配置校验）
   被埋进 web；否决——配置形态/校验/teardown 属于 agent-loop 服务契约，需 crate 内测试。
3. 独立 `dsh-interaction` crate：M2f 已判定 approval 通道类型留在 dsh-tools；M2g 同样
   不新建（宿主装配是 agent-loop 的服务职责），否决。

**决策与语义**：
- **配置形态**：`AgentLoopConfig{ maxParallelToolCalls?, agents[] }`（serde，对齐
  `AgentLoop.Config` 逐字段）；`ConfiguredAgent { id, sessionId?, provider?, model?,
  maxTokens?, cwd?, resumeSessionId? }`。`validate()` 复用 settings.rs
  `resolve_max_parallel_tool_calls`（逐字 `maxParallelToolCalls must be a positive
  integer`）+ `validate_configured_agents`（逐字 `agents "a" and "b" use duplicate exact
  session identity "s"` / `agent "a": sessionId and resumeSessionId are mutually
  exclusive`）。
- **身份 key**：`CONFIGURED_AGENT_IDENTITIES_KEY`（`configuredAgentIdentities`）经
  `configured_identities()` 暴露（id + 精确 sessionId launcher 身份）。
- **装配**：`ensure_agent`（幂等）——会话 mint（已在 store → 复用现有 live 会话，续接/
  挂载既有历史）→ `Agent` → `create_loop_agent`（真实 service 装配）。
- **生命周期**：`teardown()` 执行宿主登记的 disposer + 清空装配表；registry/store/llm/
  tools 为宿主 Rc，随宿主 drop 释放。
- **web 集成**：`Boot.agent_loop: Option<Rc<AgentLoopHost>>`（`boot()` 默认 None——M1
  WASM loop 路径不变）。`session.prompt`/`agent.run` 在 Some 时改驱 `run_rust_loop`
  （按 `sessionId==session_id` 或默认 `agent-{id}==session_id` 匹配配置 agent），事件直接
  落共享 store（前端读模型 + EventSink 下链 + 持久化同一事实源）。
- **agent 懒装配差值**：TS 在 plugin apply 期热切创建配置 agent；Rust 懒装配（首用建）。
  语义等价（首用即建、幂等），声明差值（无「启动期空跑预热」）。

**sync/环境差值声明**：
- **`resumeSessionId` 恢复未做**：持久化 host（dsh-persistence SessionHost）在 M3 宿主化
  时接续；M2g 仅复用 store 既有 live 会话（续接同进程历史）。
- **浏览器 E2E 不可跑**：本环境无浏览器/无 `DEEPSEEK_API_KEY`/out 网络阻断；dump-dom 验收
  以 `handle_rpc_host` 集成测试替代（session.prompt → Rust loop → 共享 store 事件 +
  session.history 读模型 + EventSink 下链断言全部真实执行），真实浏览器阶段验收留 M3
  （web.rs 全方法面 + 宿主持久化）。

**预期影响与回滚点**：新增 `crates/dsh-agent-loop/src/host.rs` + `tests/m2g_host.rs`（9）；
dsh-cli 增加 dsh-agent/agent-loop/llm/tools/system-prompt/scope 依赖 + `Boot.agent_loop`
+ `run_rust_loop` + web.rs 两分支路由 + 集成测试
`rpc_prompt_routes_to_rust_agent_loop_shared_store`。workspace 113 套件全绿 + clippy
零告警。回滚：撤销本提交（M1 WASM 路径不受影响——agent_loop=None 分支不变）。

## D-036（M2 验收收口）：里程碑验收清单

**日期**：2025（本机时间）

**触发的问题**：M2 里程碑按 PLAN §6「core/agent + core/agent-loop + core/system-prompt +
core/tools + core/scope + interaction（permission/user-approval/commands）」收口；按
§7.1 差分核心语义包 + §7.2 集成 + §7.3 E2E 三道闸验收。逐项核对：

**验收清单（每条 + 证据）**：
1. **capability spine 迁移**：dsh-agent（M2d-1/D-029、M2d-2/D-030：AgentBus/Registry/
   initiator/dispatch/invariant/Inbox）、dsh-agent-loop（M2e-1/D-031 请求重建、M2e-2/D-032
   驱动状态机、M2e-3/D-033 调度+投影+服务装配）、dsh-system-prompt（M2c/D-028）、
   dsh-tools（M2b/D-025..027，注册表+pre/post 语义+SDK codegen）、dsh-scope（M2a）——
   均带 TDD 单测。**证据**：提交 91a8bda / 3b1f253 / f8823ae / 9b54c27 / 0fcec03 /
   42b5823 / 7d0004b。
2. **interaction（approval）**：M2f/D-034——`PreToolDecision`/`ApprovalProvider` 接入
   tool pre 阶段，四种逐字审批结果 + 无通道/无 agent 退化 + 真实闭环验证（工具体
   执行/不执行）。**证据**：`m2f_approval.rs`（12）+ `m2f_interaction.rs`（2），提交 100fb4d。
3. **宿主装配 + web 收口**：M2g/D-035——`AgentLoopHost`（设置/身份/teardown）+ web.rs
   `session.prompt`/`agent.run` 路由 Rust loop + 共享 store 集成测试。**证据**：`m2g_host.rs`（9）
   + `rpc_prompt_routes_to_rust_agent_loop_shared_store`，本提交。
4. **workspace 全绿 + clippy 零警告**：110→113 套件全绿；clippy `--workspace --all-targets
   -- -D warnings` 零告警。
5. **差分/契约**：D-028/D-031/D-032/D-033/D-034 内逐字 wire 形状（模型文本/拒绝原因/
   事件 payload/sourceEventSeqs）均经测试锚定（TDD 红绿绿）；核心语义包差分基建沿用
   M1 doctest + integration 快照。
6. **E2E**：本环境无浏览器/无 key/out 网络阻断 → 以 handle_rpc_host 集成测试代偿（D-035
   声明）；真实浏览器 `--dump-dom` 阶段验收 + 前端逐帧断言正式收口于 M3（web.rs 全方法面）。

**验收结论**：M2 能力缝与宿主装配按计划完成，可进入 M3（host/api 全方法面 + settings/
credentials/guard 宿主化 + 真实 provider + 浏览器 E2E）。M1 既有 WASM loop 路径零回归
（agent_loop=None 分支不变）；web Rust loop 路由为可选叠加。

---

## D-037（M3 需求分析）：web.rs 空桩方法面做实 + settings/credentials 宿主化 + guard 最小切片

**日期**：2025（本机时间）

**触发的问题**：M3 按 PLAN §6 = host/api 全方法面（directory-picker / frontend-static /
plugin-inventory / webserver）+ settings/credentials/guard，把 web.rs 的 `host.*` 目录方法、
`settings.*`、`credentials.*` 空桩全部做实并落 `$DSH_HOME` 持久化。需求经第一性原理 +
双视角（参考源码逐行核对 wire schema / 服务缝 / 文件提供者）收敛，工件见
`M3-REQUIREMENTS.md`。

**考虑的选项 / 关键决策**：
1. **schema 求值复用 dsh-schema（采用）**：settings 的 `resolve(defaults→base→user)` 直接包
   M4 移植的 Schemastery `resolve()`；只在 `dsh-schema` 补 `to_json()`（`{uid, refs}` wire
   形状）。不重写 schemastery。
2. **文件格式 YAML 默认（采用）**：settings.yaml/`.credentials.yaml` 对齐 TS 默认；原子写复用
   dsh-persistence 的 write_tmp_then_publish 形态（抽出 `atomic_write` 公共小函数）。
3. **hot-reload 不引入 OS watch（采用）**：无 chokidar 等价跨平台依赖；写路径自一致 + 启动读，
   外部编辑热更新留后续（差异记录：M3 不做 OS 级 watch）。
4. **凭据分层 env→file 两层（采用）**：.env（project/user）解析留 M5 服务层；web gui 主要用
   file 层，shadowed 拒绝语义保留。
5. **guard 最小切片（采用）**：timeout wrapper（TOOL_TIMEOUT 逐字）+ repeat-reminder 阈值
   pure 逻辑（[3,5,8] 逐字）；完整 agent-loop 接线依赖 M5 通道，留 seam。
6. **浏览器 E2E 代偿（沿用 D-022/D-036）**：无浏览器/无 key/out 网络阻断 → handle_rpc_host
   集成测试为 M3 阶段验收主通道；真浏览器收口顺延 M4+。

**选择理由**：M3 的重点是把「前端可感知的方法面」从空桩推成真实语义——settings/credentials
是纯数据语义（可单测、可差分），directory-picker 是纯 fs 函数 + 围栏（可测），guard 是事件
语义不 new 状态机；全部不触碰核心并发模型。

**预期影响与回滚点**：新增 `crates/dsh-settings`、`crates/dsh-credentials` 两 crate +
`dsh-schema::to_json` + `dsh-persistence::atomic_write` 抽取 + web.rs host/目录方法做实 +
guard 切片。回滚：撤相关提交即可，不影响既有 crate（dsh-schema/persistence 的修改是纯增量）。

---

## D-038（M3a 落地）：host 目录方法面（host_dir）真实 fs 实现

**日期**：2025（本机时间）

**触发的问题**：M3a 把 web.rs 的 `host.listDirectory`/`host.createDirectory` 从空桩推成真实
fs 实现，对齐 `@deepseek-ai/dsh-host-directory-picker-browse`。

**实现要点（每个 = 一处参考对齐）**：
1. **`fully_qualified` 平台敏感**：Windows 只收盘符限定（`C:\…`/`C:/…`）或完整 UNC
   （`\\server\share…`，server 与 share 两级非空）；POSIX 用 `Path::is_absolute`。**红线**：
   TS 的 win32 `isAbsolute('/a')` 为 true 但 browse 的 `fullyQualified` 正则拒之——Rust
   侧显式实现等价判定（非 `/` 根相对误判）。
2. **有界窗口**：`bounded_insert(window, cand, keep=max_entries+1)`——满窗且 name≥尾 O(1) 拒，
   否则二分插入；驱逐 = truncated 证据。对齐 browse maxEntries=1000 默认。
3. **symbolic link stat 探针**：dirent 命中目录直接放行；symlink `std::fs::metadata` 探针，
   可进入（is_dir）才出 row，broken/循环静默跳。
4. **hidden 前缀**：`name.starts_with('.')`（POSIX 习惯；Windows hidden 属性 dirent 不暴露，
   差异记录）。
5. **createDirectory**：父 fully-qualified 围栏 + 段名校验（空/`.`/`..`/含 `/\` 拒，文案逐字
   `"…" is not a single path segment`）→ `std::fs::create_dir` 非递归；`AlreadyExists →
   directory-exists`（`{path} already exists`）、其余 → `directory-create-failed`
   （`cannot create {path}: {err}`）。
6. **web.rs 接线**：listDirectory 错误 code 直透（directory-unreadable 等）；host.describe 补
   `home`。pickDirectory 保持 `{path:null}`（无 native dialog 诚实降级）；openPath 记录并按
   payload 校验（空 path → bad-request），openPath 真实 OS opener 留 M4（无桌面 opener）。

**过程中的测试修正（平台假设错误，非行为缺陷）**：
- `fully_qualified` 初始 POSIX 测试在 Windows 上误把 `/a/b` 当 absolute——测试改为 `cfg!(windows)`
  分岐（Windows 语义 + POSIX 语义各自断言），对应 TS 平台的 fullyQualified。
- `normalize_without_fs` 输出为 OS 原生分隔；测试期望改为 `cfg!(windows) ? "\\a\\c\\d" : "/a/c/d"`
  （原先用 `Path::new("/a").join(...)` 在 Windows 产生混合分隔符怪癖）。
- `host.createDirectory`/`host.openPath` 从「空 payload 恒 ok」的 extended-method-surface 测试
  移除（做实后空 payload 是合法错误），由专用集成测试覆盖。

**验证**：`host_dir` 12 测试 + web.rs 27 测试全绿（新增 list_directory_real_fs /
create_directory_real_fs 集成）；dsh-cli lib 53/53；clippy `-D warnings` 零告警。

**预期影响与回滚点**：新增 `crates/dsh-cli/src/host_dir.rs`（独立可测模块）+ web.rs 接线。
回滚：撤提交仅影响 dsh-cli 方法面，其余 crate 零改动。

---

## D-039（M3b 落地）：dsh-schema::to_json + dsh-persistence::atomic_write + dsh-settings

**日期**：2025（本机时间）

**触发的问题**：M3b 把 settings wire 面做成真实语义——describe/update/replace/mutate +
redact + 文件持久化。依赖两段基建（D-037 决策的兑现）：dsh-schema 缺 `to_json`；
dsh-persistence 的原子写是 jsonl 私有方法。

**实现要点（对齐 TS 语义）**：
1. **`dsh_schema::Schema::to_json`**：输出 `{uid, refs:{uid-str: node}}`；嵌套 schema 引用以
   uid 数字占位；`callback`/`builder` 函数不上 wire（对齐 JSON.stringify 跳函数，preserve 布尔
   保留）；lazy 只在 resolved 时输出 inner；uid 从 0 自增（前端 `new Schema({uid,refs})` 按
   refs[uid] 重建，数字本身无关）。**M3 差异**：TS 的 `__schemastery_refs__` 全局共享态 + 同一
   schema 实例只序列化一次（DAG 去重）未复刻——每次 `child()` 重新分配 uid → 共享 Rc 节点产生
   两份拷贝（wire 冗余但语义相等）。settings registry 无语义循环，故递归有界。
2. **`dsh_persistence::fs_atomic::atomic_write`**：从 jsonl `write_tmp_then_publish` 抽取的公共
   小函数，差异是 **可覆盖既有文件**（settings 反复改写）；temp create_new + sync + rename，
   失败清理 temp；带 `Other` 错误与父目录 mkdir。
3. **`Schema::extra(key,value)` / `secret()` 组合子**：对齐 TS `Schema.prototype.extra`
   （meta 顶层键）+ `role('secret')` 快捷，供 settings namespace schema 构造。
4. **dsh-settings crate**：`SettingsProvider::{memory,file}`；register（不预插空 section →
   user 层省略对 TS `section()` undefined）；describe 三层 redact + schema.to_json + revision；
   update=merge / replace=wholesale / mutate=path ops（set 建中间对象、unset 递归删除、空路径
   指根）；revision conflict → `SETTINGS_CONFLICT`（expected/actual）；commit 前 schema
   resolve 校验拒绝；file 模式把 `{ns: section}` 全量 YAML 原子写（**M3 差异**：不做 TS 的
   comment-preserving leaf-diff，非目标 D-037）。

**redact 移植要点**：role 在 Schema.meta 而非 kind → 全程 `&SchemaRef` 走；object 属性即使
值缺失也枚举 slot（`set:false`）但**不输出该键**（TS `undefined` 哨兵 → Rust `Option` 全链路
`None`＝不呈现）；未声明到 schema 的额外键保留。dict/array 只在拥有该位置时枚举。

**过程中的测试修正**：
- `Map` 不实现 `Index<String>` → `refs[key.as_str()]`；索引返回 `&Value` 需 `&refs[...]`。
- redact 首版以 `SchemaKind` 为递归载体漏掉 meta.role 判定 → 改 `&SchemaRef`；缺失键初版输出
  `Null`（错）→ Option 全链路修平，与 TS undefined 语义一致。
- `mutate_path_ops` 循环构造 Value 的借用问题 → 退用「wrong-typed patch → Invalid」断言
  （serde_json::Value 无法表达真循环，M3b 该层本就无循环输入；真循环在 M3d wire 层 Zod 拒）。
- `replace_wholesale_and_reset` 暴露 register 预插空 section 的 user 呈现错误 → 改为不预插。

**验证**：dsh-schema to_json 11 测试 + persistence atomic_write 3 测试 + settings lib/集成
11 测试全绿；dsh-schema/dsh-persistence/dsh-settings 三 crate 全量绿；clippy `-D warnings`
零告警。

**预期影响与回滚点**：新增 `crates/dsh-settings` + `dsh-schema::to_json`/`extra`/`secret` +
`dsh-persistence::fs_atomic`。回滚撤相关提交即可；dsh-schema/persistence 改动为纯增量
（新增 pub API + 模块，未改既有行为）。

---

## D-040（M3c 落地）：dsh-credentials 能力缝（env→file 两层，refs only）

**日期**：2025（本机时间）

**触发的问题**：M3c 把 credentials wire 面做成真实语义——describe/resolve/set/unset 分层
（进程 env 只读 wins > 本地文件 provider-managed 可写）+ 文件持久化。records half
（grant/api-key）留 M5。

**实现要点（对齐 credentials-local，D-037 决策 4 兑现）**：
1. **REF 语法**：`/^[A-Za-z_][A-Za-z0-9_]*$/`；`is_credential_ref_name` 非语法名读作
   「未配置」而非抛错（对齐 seam）。
2. **分层**：`inherited(env) > 文件 refs`；resolve 每次调用读 current（M3 无 OS watch——
   写路径自一致 + 启动读，与 settings 同纪律 D-039）。
3. **writable 规则**：仅继承 env 不可写（`{configured:true, source:'env', writable:false}`）；
   文件层 writable:true；未配置 `{configured:false, writable:true, source 缺席}`。
4. **空值 seam-wide 规则**：空串 = 未配置（resolve 跳过、describe unconfigured）；set 空值
   拒绝（`Empty`）。
5. **shadowed 拒绝**：env 设了某 ref → set/unset 都 `Shadowed`（写会看起来成功而 resolve
   仍返回遮蔽值）；unset absent 是幂等成功。
6. **文件布局**：`version: 1` + `refs: {REF: value}`；未知顶层键/非 mapping/错类型/空值/
   非语法 ref 全拒绝（boot-invalid）；空文档（或 comment-only）无需 version 即空 store。
   `try_file`（严格，损坏 fail loud）vs `file`（便捷式宽松）双构造。
7. **持久化**：复用 `dsh-persistence::fs_atomic::atomic_write`（D-039）；YAML 渲染
   `refs:` 下双空格缩进，值为引号字符串。

**验证**：dsh-credentials 8 测试全绿（grammar / env readonly / unconfigured / 文件
set-resolve-unset roundtrip / 空值拒绝 / 损坏文件 fail loud / versioned 布局 / describe 批量）；
clippy `-D warnings` 零告警。

**预期影响与回滚点**：新增 `crates/dsh-credentials`（依赖 dsh-persistence + regex +
serde_yaml）。回滚撤提交即可，其余 crate 零改动。

---

## D-041（M3d 落地）：web.rs 接线——settings/credentials/host 全方法面真实服务驱动

**日期**：2025（本机时间）

**触发的问题**：M3d 把 web.rs 的 `settings.*`（describe/update/replace/mutate/openDocument）、
`credentials.*`（describe/set/unset）从空桩改为真实服务驱动，`host.describe` 补
provider/model；完成 M3-ACCEPTANCE 标准 5 的 12 方法集成。

**实现要点**：
1. **Boot 承载可变态**：`Boot.settings: Rc<RefCell<dsh_settings::SettingsProvider>>`、
   `Boot.credentials: Rc<RefCell<dsh_credentials::CredentialProvider>>`——web RPC 只持
   `&Boot`，跨请求共享可变状态（对照 sessions/llm 的既有 handle 形态）。`boot()` 与
   测试 `boot_with_sessions` 构造同步补字段。
2. **boot() 注册默认 `llm` namespace**：provider/model（带默认）/baseURL/apiKey(secret)；
   `applies: restart`。设置页有可渲染表单且写入落到本地文档（对齐 TS `llm` 插件注册集）。
3. **dispatch 接线**：
   - `settings.describe` → `describe_all()` + `namespace_view`（wire 按钮：schema 用
     `schema.to_json()`、value/base/user redact、secrets `{path,set}`、revision、applies、
     可选字段缺省省略）；`writable:true`、`hasDocument: document_path.is_some()`。
   - `settings.update/replace/mutate` → 对应 provider 方法；错误映射
     `SETTINGS_CONFLICT`（消息逐字 toJSON 文案）/ `settings-rejected`。
   - `settings.openDocument` → `{opened:true}` 诚实降级（无桌面 opener，D-037 差异）。
   - `credentials.describe` → 按 refs 批量（invalid ref → `bad-request`；unknown 合法 ref
     → unconfigured+writable:true）；`credentials.set/unset` → provider + shadowed/empty
     映射 `credential-rejected`（含 `details:{ref}`）。
   - `host.describe` → provider/model 从 `llm_catalog(boot).0` 取，可选字段缺省省略。
4. **缺失的 Provider 方法补上**：`SettingsProvider::describe_all()`（保序列出）/
   `has_document()`。

**验证**：web.rs 29 测试全绿（新增 `rpc_settings_full_wire_real_driver` 7 段断言 +
`rpc_credentials_full_wire_real_driver` 8 段断言经 handle_rpc_host 真实驱动）；
dsh-cli lib 55/55；clippy `-D warnings` 零告警。

**预期影响与回滚点**：dsh-cli Cargo.toml 加 dsh-settings/dsh-credentials/dsh-schema 依赖；
lib.rs web.rs 接线。回滚撤提交即可；既有方法面（session/workspace/llm 等）零改动。

---

## D-042（M3e 落地）：guard 切片——timeout-policy + repeat-tool-reminder seam

**日期**：2025（本机时间）

**触发的问题**：M3e 把 `guard` 能力（M3-REQUIREMENTS 标准 6）做成可交付切片：TOOL_TIMEOUT
结构化替换结果逐字 + repeat-tool-reminder 阈值检测（默认 `[3,5,8]`）gentle/detailed 消息逐字。
完整 agent-loop 接线（依赖 fs/shell M5 通道）留 M5——M3 交付纯 seam + 最小 executor 路径。

**实现要点（对齐 TS** `packages/guard/{timeout-policy,repeat-tool-reminder}` **）**：
1. **dsh-tools `guard` 模块**（新增 `src/guard.rs`，纯逻辑，无新依赖——wildcard 用迭代
   实现避免 regex 依赖）：
   - `TOOL_TIMEOUT='TOOL_TIMEOUT'`；`tool_timeout_message(ms)` = `tool call timed out after
     {ms}ms` 逐字；`tool_timeout_result(exec, ms)` → `{content:[text:'Error: {message}'],
     isError, error:{message, info:{name:'ToolTimeoutError', code:'TOOL_TIMEOUT'}}}`。
   - `timeout_exceeded(declared, elapsed)`：有正有限预算且 `elapsed >= budget` 才超时
     （`None`/非正/非有限 → 永不超时，对齐 TS `timeoutMs undefined → delegate`）。
   - repeat-reminder：`canonicalize`（深键排序后 stringify，对齐 sortJsonValue）；`wildcard`
     （`*` 通配、其余含 `.` 逐字、锚定全串）；`validate_thresholds`（非空/整数>=2/无重复/
     升序，fail-loud）；`preview_arguments`（`{}… (+N more chars)`，只截展示文本，链 key
     恒用完整 canonical）；`GENTLE_REMINDER` 与 `detailed_reminder` 逐字。
   - `RepeatTracker` 状态机：`observe(agent, name, args)` 推进链（key=`[{name},{canonical}]`，
     命中阈值返回 `Reminder{text,count,summary}`，gentle@thresholds[0]、其余 detailed）；
     无 agent / 未 tracked → 不计数不重置；`reset`（用户插话）、`drop_agent`（dispose）。
2. **最小 executor 路径**（`runtime::execute_inner`）：工具声明 `timeoutMs` 时以 wall-clock
   量 body 耗时，返回后 `elapsed >= 预算` → 以 `tool_timeout_result` 替换工具自身结果。
   **差异记录**：TS 用 `deadline + AbortSignal` 抢占（异步管线）；Rust 核心同步（D-004）无法
   并发抢占，故后置度量 = 诚实降级——工具体已执行完但结果被结构化替换（模型面语义一致：
   同一 `Error:` 文本 + 独有 code 防嵌套外层误读）。真正可抢占接线留 M4/M5 executor。

**过程中的测试修正**：`wildcard` 初版尾段/首段切分逻辑有误（`*probe`/尾段判定）→ 改为
「首段前缀 + 中段顺序 find + 尾段后缀」标准 glob 三段式；`preview_arguments` 算术先用
字节长、改字符数；`tracker_default_threshold_escalation` 等 3 个测试把「非阈值计数
（4/3）」误写成 `.expectSome` → 改 `.is_none()` 断言（勿把 None 当 Some）。

**验证**：dsh-tools `tests/m3_guard.rs` 22 测试全绿（消息逐字 / executor 替换 / 保留快
结果 / 未预算委托 / canonical / wildcard / thresholds / preview / gentle+detailed /
chain 五态 / include-exclude / per-agent / reset / drop）；dsh-tools 全量回归
（m2b/m2f 等）绿；clippy `-D warnings` 零告警。

**预期影响与回滚点**：新增 `crates/dsh-tools/src/guard.rs` + runtime.rs execute_inner
加 8 行 wall-clock 判定。回滚：撤提交即还原 runtime；guard 模块独立可测。

---

## D-043（M3f 收口）：M3 验收通过 + 差异归档（M3-ACCEPTANCE.md）

**日期**：2025（本机时间）

**触发的问题**：M3 六子步（M3a host 目录 / M3b settings+基建 / M3c credentials / M3d web
接线 / M3e guard）全部落地，进入 M3f 验收——按 M3-REQUIREMENTS §5 七条验收标准逐项核对，
并生成阶段关卡工件（M3-ACCEPTANCE.md）。

**结论**：7/7 验收标准全部满足。
1. `cargo test --workspace` 全绿（0 failed）+ clippy `-D warnings` 零告警。
2. host 目录真实 fs 测试（temp 目录/点文件/大目录/hidden/truncated/文案逐字）。
3. settings 注册→describe(redact)→update(merge)→mutate(path-op)→replace(reset)→
   revision conflict + 文件落盘重启恢复。
4. credentials resolve/describe/set/unset + shadowed/空值/幂等 unset + `.credentials.yaml`
   落盘恢复。
5. web.rs 12 方法（settings 5 + credentials 3 + host 4）全部 handle_rpc_host 真实服务驱动。
6. guard TOOL_TIMEOUT + gentle/detailed 消息逐字。
7. 每子步 D-038…D-043 ↔ 提交 f7c698f/b61a1fa/3da232b/bd5e853/c81cf18 互查。

**差异归档**（M3 内对 TS 语义的有意取舍，非缺陷）：无 OS watch / 无 YAML 注释保真
leaf-diff / credentials 两层 / revision 不持久化 / guard 同步 wall-clock（无抢占）/
pickDirectory & openDocument 诚实降级 / discoverModels 空。

**预期影响与回滚点**：M3 是独立里程碑；M4 M2 规划内后续里程碑（agent-loop guard 完整
接线、credentials records、settings 文件 provider 缺省路径、真浏览器 E2E）在遗留清单。

---

## D-044（M4 需求分析+M3 差异）：M4 长任务编排需求结论 + 双域语义分界

**日期**：2025（本机时间）

**触发的问题**：M3 验收通过后进入 M4（长任务编排与子代理：goal + subagent + schedule +
jobs + workflow + plan/todo + interaction/users）。按瀑布流先做需求分析，产出
`M4-REQUIREMENTS.md`。关键难点：M4 范围大且混合「纯逻辑可单线程」与「硬依赖外部进程/JS
引擎」两类子域，需要第一性原理划界，避免把不可避免的桩伪装成实现。

**结论（需求结论，契约事实全部来自逐行阅读参考源码 + 两份子代理语义报告）**：
- **web RPC 方法面只有 10 个**：`goal.*`（create/edit/pause/resume/complete/clear）6 +
  `subagent.*`（list/history/prompt/interrupt）4（权威：rpc-map.ts）。**jobs/schedule/
  workflow/todo 不在 web RPC 上**，以宿主服务/工具/事件/投影承载。
- **可完整单线程落地（纯域 + 复用既定基础设施）**：goal（CAS 状态机 + 事件溯源，无外部
  backend）、plan-mode（log 派生投影）、subagent in-process（spawn/fork 两 provider）、
  jobs 注册表（状态机 + 授权 + 子代理 producer）、schedule（fold 重放 + followup 注入）、
  todo（todo/write 事件 + 投影 + 工具）。
- **必须保持诚实桩**：workflow JS 执行引擎（node:vm/worker 不可低成本复刻）、out-of-process
  subagent provider（acp/claude-code/codex/dsh-sdk，真实 OS 进程属 M5）、jobs 的宿主
  subprocess producer（bash/pwsh/terminal，M5）。
- **最大既有资产（自底向上核实）**：`dsh-session::EventKind` 词表 + payload 变体**已预留
  全部 M4 事件**（goal/change、plan/mode、subagent/descriptor、schedule/change、todo/write、
  command/run、command/done、tool-workflow/*）；`dsh-session-query::ProjectionRegistry` 已
  完整；`dsh-agent-loop::AgentLoopHost.followup/events/teardown` 已就绪；`dsh-tools::ToolRegistry`
  已就绪；`dsh-persistence`（atomic_write + SessionPersistence）已就绪 → M4 工作量集中在域
  逻辑 + RPC 接线 + 投影注册，几乎不新建基础设施。

**决策展开**：
- 新增 crate：`dsh-goal`（纯域 + round-driver 判定）、`dsh-plan`（轻投影 + exit 工具）、
  `dsh-subagent`（provider/inproc/catalog/control）、`dsh-jobs`（注册表 + job 工具）、
  `dsh-schedule`（fold/规则）、`dsh-workflow`（meta 校验 + 事件骨架）。
- subagent cold resume 复用 JSONL 持久化（激活边界 = seedLength）；无持久化 →
  `PERSISTENCE_UNAVAILABLE`。
- goal/plan/subagent/jobs/todos 投影挂载到既有 ProjectionRegistry（M4h 装配）。
- workflow 差异明确：只做 meta 校验/事件骨架/致命 code/result materialize 规则的诚实桩。
- 非目标重申：不新增 web RPC 方法、不引入多线程、不破 D-004 单线程核心。

**预期影响与回滚点**：M4 是独立里程碑；各子步独立 crate + 提交可回滚。M4a-M4i 八个子步
按文档依赖序推进，每步绿 + DECISIONS + git。真浏览器 E2E 留 M4+（环境不可跑，延续
D-022/D-036 handle_rpc_host 代偿）。

---

## D-045（M4a dsh-goal 纯域）：CAS 状态机 + 事件溯源 fold + 投影基础

**日期**：2025（本机时间）

**触发的问题**：M4 进入编码，首块 M4a = goal 纯域（无驱动依赖）。目标：CAS 状态机 +
严格回放 fold + 投影基础，逐字对齐 `packages/goal/goal/src/{types,domain,fold,runtime}.ts`
并复用 `dsh-session` 已预留的 `goal/change` 事件 / serde_json 宽载荷。

**结论**：
- 新增 crate `dsh-goal`（只依赖 dsh-session + serde + serde_json）。
- `types.rs`：GoalId/GoalRef/GoalPhase/GoalActivation(armed|disarmed)/GoalBlockReason/
  GoalSnapshot/GoalProjection/GoalOperation/GoalChangeMeta（snapshot|clear 墓碑）——wire
  字段 camelCase（`maxGoalRounds`/`roundsStarted`/`createdAt`/`updatedAt`/`clearedAt`）serde
  rename 对齐 TS；`GoalView` 含派生 rounds_started/created_at/updated_at/activation。
- `fold.rs`：`decode_goal_change`（非 goal 事件 → None；malformed → fail loud）+ 严格
  apply（revision 精确 +1、跨目标 create 必须 rev1、id 不重用【clear 墓碑后同 id recreate
  拒】、blocked 须带 blockedReason、非 complete 前 goal 的 create 拒、updatedAt/roundsStarted
  守恒）+ `fold_goal_events_strict`（fail loud）+ 宽容 `fold_goal_events`。
- `service.rs`：进程内 GoalService（单当前目标语义）——create（前 goal 须 absent 或
  complete、默认 256 轮）/edit/pause/resume/complete/block（host-only + lower-kebab code
  校验）/clear + `admit_round`（round 1..maxGoalRounds，递增 roundsStarted）+ `disarm`。
  错误码 9 个逐字（GOAL_AGENT_NOT_LIVE/GOAL_NOT_FOUND/GOAL_ALREADY_EXISTS/
  GOAL_STALE_REVISION/GOAL_INVALID_OBJECTIVE/GOAL_INVALID_MAX_ROUNDS/
  GOAL_INVALID_BLOCK_REASON/GOAL_INVALID_EDIT/GOAL_INVALID_TRANSITION）。CAS 前置以自由
  函数 `cas_ref` 剥离（避免 borrow checker 冲突）。
- 测试 27 绿（fold 10 + service 17）：生命周期/边界/错误码逐字 + 严格 fold 不变量。
- clippy -D warnings 零告警（fold bool 逻辑收敛为 is_none_or/is_some_and 组合）。

**差异记录**：GoalActivation 只在服务进程内（不持久化）；service 是进程内单目标镜像，
跨会话多目标由 caller 按会话实例化（web.rs M4h 装配）。clear 后用新 id create 允许、
同 id 复用拒。

**预期影响与回滚点**：纯域不接 agent-loop（驱动在 M4b）。回滚：撤提交即还原；service
已不依赖 dsh-session 事件落盘（caller 自己落 `goal/change`）。

---

## D-046（M4b goal-round-driver）：自动续跑判定 + 一轮提示驱动

**日期**：2025（本机时间）

**触发的问题**：M4a 纯域完成后，goal 需要「自动续跑」能力 —— active+armed+未超 cap+
agent idle 时自动发起下一轮（goal-round-driver），对齐
`packages/goal/goal-round-driver/src/index.ts`。

**结论**：
- `dsh-goal/src/round_driver.rs`：`StatusPort` trait 抽象宿主（`status_idle`/
  `has_pending_inbox`/`followup`），driver 不持有 agent-loop → dsh-goal 保持纯域零
  agent-loop 依赖，宿主在 M4h web.rs 装配实现。
- `round_driver_outcome(&service,&id,&port)` 只读判定：phase==active ∧ armed ∧
  roundsStarted<max ∧ idle ∧ 无竞争 inbox → `Continue`；`drive_once` 判定 + 准入本轮
  （admit_round(next)）+ 渲染 `<goal_round>…Round: N/M…` 提示 + followup 投递。
- service 补访问子：`phase()/rounds_started()/activation()/max_goal_rounds()/objective()`
  （driver 判定与提示渲染用）；`get(id)` 作 id 一致性校验。
- 测试 9 绿：续跑/不续跑（disarmed/pending inbox/running/非 active/到 cap）、提示渲染
  （Round: 3/3 逐字）、resume 后保持 armed。总计 dsh-goal 36 测试绿 + clippy 零。
- 超 cap 的自动 `block {code:"round-limit"}` 语义由 caller（agent-loop 轮次收尾）判定，
  driver 层面先以「到 cap 不续跑」表达（M4h 的 status 事件收到 cap 后由宿主调 block）。

**差异记录**：driver 是「每轮结束调用一次」的拉式驱动（宿主事件循环驱动），不是 TS 的
独立 round-loop 进程——单线程 D-004 下以拉式判定对齐，无后台线程。

**预期影响与回滚点**：driver 是纯函数 + trait seam，回滚撤提交即可。M4h 将与 agent-loop
的 AgentLoopHost 装配（status + inbox + followup 实配）。

---

## D-047（M4c dsh-plan）：plan/mode 折叠 + plan 投影 + exit_plan_mode 前置

**日期**：2025（本机时间）

**触发的问题**：M4 第三个子块 = plan-mode（计划模式，`/plan <msg>` 进入 / `/plan off`
离开，`plan/mode` 事件 + `plan` 投影 active/pending，`exit_plan_mode` 工具）。对齐
`packages/plan/plan-mode/src/index.ts`。

**结论**：
- `fold.rs`：`fold_plan_mode`（最后 `plan/mode` 胜出；无事件 → inactive）+ 前缀折叠 +
  `has_open_turn`（turn/start…turn/end 配对）。
- `projection.rs`：PlanUnitState（active/wanted/running）+ `plan_unit_apply`（command/run
  name=='plan'→running 记录 wanted=args.trim()!="off"；配对 command/done 仅 success 且
  wanted≠active 落 wanted；plan/mode→active 落定 + wanted 清；args 缺省不动）+ 视图
  `{active, pending: (running?.wanted ?? wanted) !== null && !== active}`。纯事件重放 →
  投影可从日志恢复（无 live mirror）。
- `exit.rs`：`exit_plan_mode_check(events, plan, review_channel)`——非 plan mode →
  `NotInPlanMode`；plan 不带 `# 标题`（`^#\s+\S`）→ `NeedsHeading`；无评审通道 →
  `NoReviewChannel`；全过 → `Ok`。宿主 M4h 装配 user-questions 通道后才发评审。
- 测试 12 绿：折叠 last-wins/前缀、open-turn、投影单元全路径（run/done success|error/
  commit/pending）、exit 前置四态。
- clippy -D warnings 零告警（prefix 用 `.take(end)` 消循环计数器；test bool 断言规范化）。

**差异记录**：user-questions 评审通道（ask_user_question）是本仓 Rust 侧缺失面——M4c
只做前置校验，真实评审交互由 M4h 决定是否提供最小内存通道或按文档报错（README 明确）。

**预期影响与回滚点**：纯日志派生 + 投影 unit（M4h 挂 ProjectionRegistry）。回滚撤提交即可。

---

## D-048（M4d dsh-subagent 纯语义）：描述符 + 深度 + provider 边界 + 目录 + 控制基础

**日期**：2025（本机时间）

**触发的问题**：M4 第四子块 = subagent（in-process spawn/fork 进程内子代理 + 目录/描述符/
深度/控制）。M4d 交付纯语义层（不接 agent-loop），对齐
`packages/subagent/subagent/src/{descriptor,depth,list-children,projection}.ts` +
各 provider capability 表。

**结论**：
- `descriptor.rs`：`SUBAGENT_DESCRIPTOR_VERSION=2`；`snapshot_descriptor`（one-shot 可选
  label / continuable 必填 label + 可选 agentProvider/agentModel/persona/toolFilter，
  前置校验）；`fold_descriptor_from_events`（首条 descriptor 权威、版本不符 → None、
  当前版本未知字段/类型错 → fail loud、toolFilter 必须 allow 和/或 deny）。
- `depth.rs`：`resolve_child_depth`（=max(header,runtime)+1，header 单调下限）+ 
  `resolve_child_depth_bounded`（attempted>max → `DepthError::Overflow{attempted,max}`）+
  `validate_max_depth`。
- `provider.rs`：capability 表——spawn/fork 全 true（outputSchema/depthLimit/toolFilter/
  persona；fork inheritsParentContext=true、spawn=false）；acp/claude-code/codex/dsh-sdk →
  `NO_START_CAPABILITIES` 全 false（out-of-process 边界，M5）。
- `catalog.rs`：`category_child`（child one-shot/continuable + activity + hasChildren）+
  `diagnostic_row`（corrupt/unsupported/unavailable）。
- `control.rs`：`prompt_gate`（仅 continuable 可 prompt）+ `interrupt_receipt`（fire-and-
  return 恒 accepted，absent 目标 no-op 也 accepted）。
- 测试 13 绿：descriptor snapshot/fold 全路径、深度递增/越界、provider 两边界、目录分类、
  diagnostic。clippy -D warnings 零告警（doc 列表项 lint 修正）。

**差异记录**：真实 child Agent 创建/投递（in-process driver + followup + persist resume）
由 M4h 接 dsh-agent-loop 实配；本子步是纯判别与数据面。out-of-process provider 保持
能力登记 + 明确不可用（非伪装成功）。

**预期影响与回滚点**：纯数据面 + 独立 crate。回滚撤提交即可；M4h 在其上装配活子代理驱动。

---

## D-049（M4e dsh-jobs）：任务注册表状态机 + 授权围栏 + first-wins 结算

**日期**：2025（本机时间）

**触发的问题**：M4 第五子块 = jobs（后台任务注册表）。web RPC 上不暴露 jobs——以宿主
`session/jobs` 帧 taskViewSchema 投影承载。本子步交付注册表纯语义（单线程拉式结算）。

**结论**：
- `registry.rs`：`JobRegistry` 状态机——`running` →（可选 `stopping`）→ 恰一终态
  （completed|killed|failed）；id = `<kind>-N`（每 kind 独立计数器）；`start` 前置校验
  （空 kind/label→EmptyKind/EmptyLabel、owner 活跃上限→OwnerQuota）→ producer() →
  登记（startedAt）；`kill` running→停 + on_cancel + 置 stopping（terminated →
  AlreadyFinished）；`settle` first-wins（终态已定即忽略后到，finishedAt 落）；`read`
  幂等 final-output（从不消费）+ 承诺 reported；`get/list` 授权围栏（owner 只见自己 +
  无主）；`view` 输出 JobView wire（id/kind/label/status/detail?/startedAt/finishedAt?，
  绝无 owner/reported 泄漏）。
- producer 是同步 `run() -> Hooks`（on_cancel + read_output），单线程模型不引入后台线程
  （D-004）；结算由宿主在 settle 时机显式调用。
- 测试 11 绿：id 分配、start 拒空、owner 配额、生命周期 kill→stopping→killed first-wins、
  completed 直结、授权围栏、read 幂等、ops 错误、reported 抑制、view wire 形状、list 隔离。
- clippy -D warnings 零告警（doc 列表项 + Default derive）。

**差异记录**：子代理 producer（subagent job）在此只占 kind 命名空间；真实 in-process
driver 投递由 M4h 提供。bash/pwsh/terminal producer 为 M5 subprocess。view 即 web
`session/jobs` 帧的 taskViewSchema。

**预期影响与回滚点**：纯内存单线程注册表，独立 crate。回滚撤提交即可；M4h 接宿主事件循环
驱动 settle + session/jobs 帧。

---

## D-050（M4f dsh-schedule）：schedule/change 域 + 创建规则 + every 锚定 + 时区范围决策

**日期**：2025（本机时间）

**触发的问题**：M4 第六子块 = schedule（计划提醒）。web RPC 不暴露 schedule——以工具 +
`schedule/change` 事件承载。本子步实现 durable 域纯语义（对齐
`deepseek-harness/packages/schedule/schedule/src/domain.ts`）。

**结论**：
- `domain.rs`：
  - 协议 `SCHEDULE_CHANGE_VERSION=1`；`MIN_EVERY_INTERVAL_SECONDS=300`（对齐 TS）。
  - 错误联合：`LogError`（durable 损坏，码 corrupt_schedule_log）+ `ScheduleInputError`
    `InputCode` 六码（InvalidPrompt/InvalidRule/InvalidTimeZone/NotFuture/
    TimeOutOfRange/FrequencyTooHigh）。
  - `decode_schedule_change`：create/delete/dispatch（±acceptedAt）精确键集合 + 版本校验 +
    规范 UTC instant（自研 `is_utc_instant`：四位数年、真实日历日、字段范围）。
  - `decode_record`：after/at/every 精确键 + prompt 已 trim 非空 + afterSeconds>0 safe +
    everySeconds>=300。
  - `fold_schedule_events`（+ `_seeded` fork 分界 seedLength）：create id 不重用、delete/
    dispatch 非活跃拒、one-shot dispatch 拒 acceptedAt、every dispatch 必须 acceptedAt 且
    锚定推进 scheduledAt（耗尽则移除）。`allocate_id_from_seen` 前扫不重 id（对齐 TS 从
    size+1 起）。
  - 创建：`create_after_record`（prompt trim/after>0/strict future）、
    `create_at_record_from_offset`（显式 ±HH:MM 或 Z → UTC，含 1-3 位小数秒）、
    `create_every_record`（>=300）。
  - `resolve_every_occurrence`（锚定对齐：acceptedAt 前最新 occurrence + 首个未来目标）+
    `schedule_view`（据真实 now 判 overdue/scheduled，deliveryMode=session-local）。
- **时区范围决策（D-044 预设的「eval then introduce」触发）**：IANA 本地时区
  `time_zone`（如 Asia/Shanghai）——离线 cargo 缓存有 index 元数据但无 chrono-tz crate
  本体（仅 iana-time-zone 0.1.65 存在且属系统探测库，不携带 tzdb 数据），`--offline`
  无法引入；评估后决定：M4 范围 `time_zone` 仅接受 `UTC`/`GMT` 与数值偏移，IANA local-at
  按 `invalid_time_zone` 报错并记入 README Known Limitations（defer 到可联网引入
  chrono-tz 或 jiff-tzdb 之时）。不因环境限制伪装支持，亦不降级架构（错误码留足 wire）。
- 测试 16 绿：decode/fold 全路径、seed 分界、create 全错误码、偏移→UTC、every 锚定、
  view、id 分配。clippy -D warnings 零告警（doc 列表项 + 冗余 cast + range contains）。

**预期影响与回滚点**：纯 durable 域 + 独立 crate；M4h 接到 schedule 工具/事件投影。回滚
撤提交即可。IANA 时区是显式 deferred 而非遗漏——一旦引入 chrono-tz 只需补
`canonicalize_time_zone`/local-at 解析并重跑测试。

---

## D-051（M4g todo + dsh-workflow 桩）：todos 投影 unit + todo 工具校验 + workflow meta/桩

**日期**：2025（本机时间）

**触发的问题**：M4 第七子块 = todo（`todo/write` 事件 + `todos` 投影 + todo 工具）+
workflow（meta 校验 + 事件骨架 + 致命 code 分类 + 诚实执行桩）。

**结论**：
- todo 承载（需求行 210/254 的「dsh-session-query 投影 unit + dsh-tools 工具」拆解）：
  在 `dsh-session-query` 新增 `todo.rs`——`to_todo_list`（trim 非空唯一 content；
  allowParallelInProgress=false 时至多一个 in_progress → TooManyInProgress；空/重复 →
  EmptyContent/DuplicateContent）、`todos_projection_unit()`（init=null，apply:
  todo/write → 整表 / turn/start → null / 其余保持，view=state；stateVersion=2 对齐 TS）
  + `into_unit()` 供 M4h 注册进 ProjectionRegistry、`todo_counts`（pending/inProgress/
  completed）。利用既有 `dsh_session::TodoItem/TodoStatus/TodoWrite EventKind`。
- dsh-workflow（补「上一步留下的桩」）：
  - `error.rs`：`WorkflowErrorCode` 11 码全列（SCRIPT_PARSE…CANCELLED，wire 对齐 TS
    大写下划线）+ `WorkflowError`（默认 fatal=true）+ `violations` 字段（META_INVALID 逐条）。
  - `meta.rs`：`validate_meta` 逐 violation 列出（meta 未知键 / name/description 非空 /
    whenToUse 可选 / phases 数组每项只认 title/detail/provider/model 且 title 非空），
    code=META_INVALID，成功返回规范化副本（不别名调用方对象）。
  - `event.rs`：WorkflowRunInfo / WorkflowAgentInfo（seq 1-based）/ AgentEndInfo wire 构造。
  - `stub.rs`：`run_stub` 恒 Err（UNSUPPORTED_OPTION，isError）——JS 执行引擎 M4 不落地，
    诚实桩不伪装成功（D-044/需求行 166）。
- 测试 12 绿（session-query 6 + workflow 6）：todo 规范化/重复/并行纪律/投影折叠/view/
  counts；meta 合法+全 violation/META_INVALID、错误码路由、事件 payload、桩 isError。
  clippy -D warnings 零告警。

**差异记录**：todo 工具注册本身（`todo_write` 工具 + 描述文案/输出 render）留 M4h 在
dsh-tools 完成；本子步交付其校验与投影数据面。workflow 桩仅覆盖 meta/事件/isError——
`parallel/pipeline` 组合器与 worker 协议均为 M5（真实 JS 引擎）。

**预期影响与回滚点**：纯数据面 + 独立模块/crate。回滚撤提交即可；M4h 注册 todo 工具并
挂 `todos` 投影 unit 进 Boot。

---

## D-052（M4h web.rs 接线）：10 RPC 实做 + todos 投影挂载 + M4 工具注册点

**日期**：2025（本机时间）
**触发的问题**：M4 收口集成——把 M4a-M4g 纯域接到 `crates/dsh-cli/src/web.rs` 的
`handle_rpc_host`，替换 goal.*/subagent.* 空桩，挂投影（todo），扩展 commands/list，
注册 M4 工具。

**结论**：
- `Boot` 增字段：`goal: Rc<RefCell<GoalService>>`（GoalService::new(ServiceOptions::default)）
  + `projections: Rc<RefCell<ProjectionRegistry>>`；boot() 组装时注册
  `todos_projection_unit().into_unit()`（注册失败静默容忍，投影是可选能力）。dsh-cli
  Cargo.toml 增 dsh-goal/dsh-subagent/dsh-session-query path 依赖。
- **goal.\* 6 RPC**：全部经 boot.goal.borrow_mut() 真实状态机。create（objective 空 →
  GOAL_INVALID_OBJECTIVE；maxGoalRounds 报 0 哨兵走 InvalidMaxRounds）/ edit（须 ref +
  objective 或 maxGoalRounds 至少一，否则 bad-request；CAS 错 → GOAL_STALE_REVISION）/
  pause/resume/complete（须 ref）→ 响应 `{ref:{id,revision(递增)}}`；错误统一
  GoalServiceError::code()。clear 须 ref，**NotFound → 幂等 `{cleared:true}`**（对齐 TS
  clear 无 current goal 的 no-op），其余错误透传。全部带 sessionId 必填校验。
- **subagent.\* 4 RPC**：list 经 catalog 纯函数投影为空目录 `{entries:[], parentAvailable:
  true}`（subagent_entry_wire 走 ChildEntry → camelCase hasChildren/label/reason；无真实
  子代理运行时 → 诚实空 + wire 形状留好供真实源接入）；history 诚实空 `{events:[],hasMore:
  false}`；prompt 走 control::prompt_gate（非 continuable → bad-request）+ 诚实合成
  messageId；interrupt 走 interrupt_receipt → `{accepted:true}`。
- **commands/list**：保留 compact/plan/goal + 新增 subagents 项。
- **M4 工具注册点**：`pub fn register_m4_tools(&ToolRegistry)` 注册 todo_write（描述 +
  执行经 to_todo_list 校验 Empty/Duplicate/TooManyInProgress → 拒）。**差异**：harness 无
  持久 ToolRegistry 注入点（Boot 不持有）→ 不强制挂 boot 链，以独立注册函数 + 单测证明
  可注册与校验路径（M4i/M5 若宿主开放 registry 再真挂）。jobs/schedule/workflow：M4e/f/g
  已为纯域，不在 web RPC 方法表（D-044 已定），本子步不新增 RPC 分支。
- **测试 9 个**：goal.create 真实 ref（id=goal-1, revision=1）、缺 objective 拒、缺
  sessionId 拒、complete→clear 全链路（含 ref 缺失 → bad-request 分叉）、subagent.list
  空目录、prompt 门（非 continuable 拒）、commands/list 含 subagents、Boot 挂载 todos
  投影、register_m4_tools todo_write。原有 `rpc_extended_method_surface_ok` 冒烟用例改为
  按方法给最小合法 payload（对齐 M3a 对 host.createDirectory 的处理——真实方法需要入参）。
- 全仓验证（父会话复核）：143 组 test-result 全 ok（0 failed，1000+ 测试）+ 
  `cargo clippy --workspace --all-targets -D warnings` exit 0 零告警。

**差异记录**：
- subagent.list/history 为诚实空实现（无真实子代理运行时），非伪装数据；wire 形状完备，
  真实目录源（M5 in-process driver 或持久化日志）接入时只换 rows 源不换 wire。
- clear 的 NotFound→cleared:true 是无当前 goal 时的幂等 no-op（对齐 TS），与
  create/edit/pause 的 NotFound→GOAL_NOT_FOUND 错误语义分叉——这是刻意对齐参考的行为
  （TS GoalService::clear 对无 current goal 返回 cleared:true），如实实现。

**预期影响与回滚点**：Boot 字段新增、10 RPC 真实现、投影/工具注册——均由 M4a-M4g 纯域
驱动，回滚撤 1ccd261 即可整体回退到 20497c6（M4g 桩后状态）。M4h 是 M4 全部编码的集成
收口，M4i 将据 D-044 验收标准做 M4-ACCEPTANCE。

---

## D-053（M4i 补齐收口）：子代理真实驱动 + 宿主工具 bind + todo 事件 + round-driver 实配

**日期**：2025（本机时间）

**触发的问题**：M4i 验收复核发现 M4h 的多处「诚实空/桩」实为**欠实**（非环境受限）：subagent
list/history/prompt 为空实现、M4 工具仅注册 todo_write、round-driver 未接真实 agent-loop、
todo 工具不落事件。经与用户确认（裁决 B：「全部补齐再做验收，含 fake-loop 驱动链路」），
回编码阶段逐项补实，禁止以环境受限收口、禁止文过饰非。

**结论**：
- **补4 subagent 真实 in-process 驱动**（`crates/dsh-cli/src/subagent_runtime.rs` 新建）：
  子代理 = store 里真实 `Session`（header `origin=Subagent` + `parentSession` +
  `delegationDepth`）；身份 = `subagent/descriptor` 事件经 dsh-subagent fold。`spawn_child`
  mint `sa-N` id / resolve_child_depth / store.create 带 meta / append descriptor（data=
  snapshot_descriptor）；`fork_child` seed=源 events + seed_length meta；`list_children`
  只读枚举折叠 category_child/diagnostic_row（不激活 Agent）；`history` 严格 `e.seq<beforeSeq`
  分页 + has_more；`prompt` 经 `AgentLoopHost.ensure_agent`（`sa-agent-<child>`，provider/
  model 从描述符析出）+ followup 驱动一轮（同步单线程；fake-loop = mock LlmAdapter 装配
  真实 Rust loop），messageId=`pmsg-{child}:{seq+1}`，无 agent_loop → Internal fail loud；
  `interrupt` fire-and-return 收据。gates：one-shot → bad-request。
- **Descriptor 序列化修复**（`dsh-subagent/src/descriptor.rs`）：`#[serde(rename_all_fields =
  "camelCase")]`——snapshot→to_value 产 camelCase（agentProvider/agentModel），与
  fold_descriptor_from_events 期望一致；此前 snake_case 致 round-trip 失败（既有 fold 测试
  用手写 camelCase JSON 未暴露此缺口）。subagent 投影 view 直接返回 identity。
- **补5 M4 工具全注册 + 宿主 bind**（web.rs）：`M4HostServices{jobs,schedule,todo}` +
  `dsh_cli_host` 内嵌模块（`ScheduleHost`：以会话 `schedule/change` 事件为权威，fold/
  create/list/delete/dispatch_due）+ `register_m4_tools_with_host`（注册 todo_write +
  job_output/list/kill + schedule_create/list/delete + exit_plan_mode + workflow，共 9 工具；
  有 host 则 bind 到真实 JobRegistry/ScheduleHost，无 host 保持 `NOT_BOUND` fail loud）。
  job_output 输出 `{text,job}`、job_kill 输出 `{outcome,job}`（对齐 SA-4 schema 精确键）。
  schedule create 事件 `{version,operation:"create",schedule:<record>}`（decode 强制精确键
  集合：after={id,kind,prompt,afterSeconds,scheduledAt}、at 无秒数键、every 用 everySeconds）；
  `dispatch_due` 到期注入 → dispatch 事件（one-shot 无 acceptedAt / every 带规范 acceptedAt）
  + framing 文本；schedule 工具输出 schema `{"type":"json"}`、workflow 恒桩。
- **补6 todo 事件落会话**：`TodoWriteHost`（SessionHost + agent→session 登记）——todo_write
  校验/规范化后把整表落为 `todo/write` 事件到属主会话；`todos` 投影据此折叠；无宿主时保持
  SA-4 自包含定义（校验-only，不伪称已持久化）；无 agent 调用者 → 拒绝。
- **补7 round-driver 实配**：`GoalRoundPort`（web.rs）把 `Rc<ReactLoopAgent>` 实配到
  `dsh_goal::round_driver::StatusPort`（status_idle / has_pending_inbox / followup）；armed
  目标 + 空闲 + 空 inbox + 未超 cap → `drive_once` admit 本轮 + followup 驱动真实 Rust 轮次，
  到 cap 后不再续跑。
- **测试**：补4 subagent_runtime 7 绿 + web.rs fake-loop 端到端；补5 注册/bind/schedule 注入
  测试；补6 todo 事件 + 投影折叠测试；补7 fake-loop round-driver 端到端（两轮 + cap）。
  dsh-cli `--lib` 81 绿；全仓 1130 测试 0 失败；`cargo clippy --workspace --all-targets -- -D
  warnings` exit 0 零告警。

**差异记录**：schedule 到期注入在本仓同步单线程下由宿主显式调用 `dispatch_due`
（frame/轮次钩子），非真实定时器——事件与 fold 语义一致，定时推进属 M5 宿主调度。
workflow 恒桩 / out-of-process provider / IANA 全时区仍为 M5+（D-044 非目标不扩散）。

**预期影响与回滚点**：本提交为 M4i 补齐收口，之后 M4-ACCEPTANCE 验收。回滚撤本提交即回
M4h 桩后状态；subagent_runtime.rs / dsh_cli_host / M4HostServices 均为本提交新增，撤提交
即整体移除。改动 → 提交 → 本条目互查（提交信息引用 D-053）。

---

## D-054（M5 需求分析·环境实测修正）：M5 依赖清单——沙箱 Schannel 是唯一障碍，网络真实可达

**日期**：2026（M5 需求阶段）

**触发问题**：前几轮把「portable-pty 不在缓存 → P1(a) 不可行」「globset/ignore 无、
fs-search 归 M6」归因于**离线受限**，依据是此前 cargo fetch/check 的 TLS 失败（历史曾
记为出网被阻）。用户提示「网络可达，只是受限执行环境假故障，需给清单手动安装」。需用
实测修正需求结论，并把安装清单交付用户。

**实测（本日，全部直接验证）**：
1. `rsproxy.cn:443` TCP 可达（`Test-NetConnection` True）；镜像源运行态无代理变量。
2. **Node `fetch('https://rsproxy.cn/index/config.json')` → 200（len 81）**；**Python
   `urllib` 同 URL → 200（len 81）**——Node/Python 用自家 TLS 栈，穿透正常。
3. **cargo / git / PowerShell 全部失败于同一根因**：`schannel: AcquireCredentialsHandle
   failed: SEC_E_NO_CREDENTIALS (0x8009030E)`——沙箱把「凭据/证书存储」从 TLS 上下文
   剥掉，凡走 Windows Schannel 的传输（cargo、git、PowerShell curl）都挂；Node/Python
   底层 OpenSSL/自栈不受影响。→ **这是沙箱运行环境假故障，非用户机器网络问题**。
4. cargo registry `cache/` 目录在本沙箱**不可写**（写测试被拒）→ 我无法在沙箱内自补
   依赖；必须由用户在无受限沙箱环境跑一次 fetch/build。

**依赖现状盘点（registry 实查）**：
- **已提取（registry/src 存在，可离线编译）**：jiff 全家（0.2.35/jiff-core/jiff-static/
  jiff-tzdb/jiff-tzdb-platform）、globset 0.4.18、ignore 0.4.26、which 6.0.3、
  sysinfo 0.38.4、nix 0.30.1、windows-sys 0.45~0.61、winapi、walkdir、memchr、
  glob、regex、tempfile、chrono 0.2/0.4、portable-atomic 等——**M5 绝大多数候选在列**。
- **缓存 .crate 但未提取 src**：globset/ignore 等需**一次普通 cargo check** 即入 src
  （用户无沙箱环境跑一次即可）；portable-pty 连 .crate 都不在（indexMeta=False）。

**最终选择**：M5 级依赖釐清如下，给出用户手动安装清单（command 见 M5-DEPENDENCIES.md）：
**round6 复核（实测）**：`nix 0.30 + which 6 + sysinfo 0.38` 临时 crate `cargo run --offline`
编译+运行通过（output `which_ok=true cpu=true`）→ M5a subprocess 的树级终止/裸名查找/
liveness 依赖栈全就绪；globset/ignore 提取仍被沙箱写 registry/src 拒绝（留用户一次
`cargo check`），portable-pty 仍缺（装机未进行）。

| 依赖 | M5 用途 | 状态 | 需用户操作 |
|---|---|---|---|
| portable-pty 0.8 | terminal PTY / spawnTerminal（P1(a) 落地前提） | **缺（无 .crate 无 meta）** | `cargo fetch`/build 一次（离线路径随后可用） |
| globset 0.4 / ignore 0.4 | fs glob/grep 引擎（参考 ripgrep 二进制 → 用同源 crate） | 已缓存待提取 | 一次 cargo check 即提取 |
| jiff 0.2 + jiff-tzdb | P2 IANA 全时区（2026 spike 已验证离线可编译运行） | 已提取 | 无 |
| nix/windows-sys/libc/winapi | subprocess 树级终止/PTY 平台 FFI | 已提取 | 无 |
| which/sysinfo/tempfile/regex | 裸名可执行查找 / 进程存活 / spill / 正则 | 已提取 | 无 |
| strip-ansi-escapes | shell 输出 ANSI 剥离（参考 shell machine 用） | **不在本地** | 需求决策 P4 后按需 fetch |

**理由**：环境结论要 `DECISIONS` 权威化——「离线受限」是环境假象，真实约束 = ①沙箱
Schannel 无凭据 ②沙箱不可写 cargo 缓存；两者都可通过用户在普通环境的一次 fetch/build
消除。依赖引入符合方法论四（成熟库直接引入：portable-pty 是 terminal 标准选择、globset/
ignore 是 ripgrep 同引擎、jiff 是 IANA 时区现代替代）。

**预期影响与回滚点**：本条目不改代码，只修订需求结论与新增依赖随 M5 开关；回滚即删除
本条目与 M5-DEPENDENCIES.md、M5-REQUIREMENTS.md 对应句。改动 → 提交 → 本条目互查
（提交信息引用 D-054）。after 需求结论启动设计后，P1(a)（真实 PTY）与 fs-search
（globset/ignore 引擎）重新变为可选落地而非推定排除。

---

## D-055（M5 阶段关卡裁定）：P1-P5 全部按推荐倾向锁定，进入阶段二设计

**日期**：2026（M5 round 8）

**触发问题**：M5-REQUIREMENTS.md §6 的 P1-P5 决策点此前阻塞在人工裁定；用户回「已经缓存
好依赖可以继续了…继续开发吧」——依赖已装（portable-pty 0.8.1 + globset 4.20 + ignore
4.33 均已入 registry src），权限升级为 danger-full-access，审批关闭。

**最终选择**（逐项，理由见 M5-REQUIREMENTS §6）：P1=(a) 引 portable-pty 0.8.1 真实做
terminal（round8 离线端到端实测 `openpty`+`spawn_command` 通过）；P2=(a) jiff+jiff-tzdb
IANA 全时区（已提取+离线运行验证）；P3=(a) 平台 runner seam+失败闭、真实边界由 fs 进程内
围栏承担；P4=参考 `timeoutMs` camelCase（凡 dsh 工具参数 schema，差异记 D）；P5=lsp/e2b/
out-of-process/jobs 真实 provider 全归 M6（登记+诚实桩）。

**理由**：用户明确放行且依赖实测可用，五条均无环境或架构障碍；P2/P4/P5 与 wire 对齐纪律
一致，P1 由用户手动安装解锁。

**预期影响与回滚点**：阶段关卡通过 → 进入阶段二系统设计（M5-DESIGN.md）；回滚 = 改回
M5-REQUIREMENTS §6 表 + 撤本条目。改动 → 提交 → 本条目互查（提交信息引用 D-055）。

---

## D-056（M5 阶段二·系统设计收口）：M5-DESIGN.md 六 crate 设计 + DIV 分叉清单 + 子代理降级接管

**日期**：2026（M5 round 9）

**触发问题**：① 设计子代理 f672179d 两个回合零产物——任务是「读全部参考 TS + 写详尽
M5-DESIGN.md」，超大单轮被中断且未写盘，遂由主线程独立交叉核查后直接接管撰写（中断子代理
是停掉无产物的 in-flight 轮，不丢已存工件；此为本会话对「大交付物委托」的降级处置，记档）；
② 设计定稿需要把六 crate 的契约、DIV 分叉、实现顺序落成可验收工件。

**考虑的选项**：
1. **主线程独立撰写 M5-DESIGN.md（本次采用）**：round 7/8 已对六缝做了逐字的独立交叉核查
   （tool-fs write 输出信封、tool-bash camelCase 参数与全部标记词汇、shell types 逐字、
   code-runtime python 协议 PROTOCOL_FD=3/lossless、fs 13 错误码 + 12 方法、sandbox 阶梯/
   roots/审批优先级、jobs ProducerHooks、ScheduleHost tick 点、M4 host-bind 模板），足以直接
   产出可信设计。
2. **再等/重启子代理**：已触发过增量写盘导引仍无产物，重开有同样卡死风险，且主线程核查已
   完备，重启不增信息。
3. **跳过设计直接编码**：违反瀑布流阶段关卡（阶段二验收工件缺失）。

**最终选择**：选项 1。设计文档 `M5-DESIGN.md` 含 10 节：crate 划分与依赖图、subprocess /
sandbox / fs / shell / terminal / code-runtime 六缝逐字契约 + 各自 TDD 计划、宿主接线
（M5HostServices/register_m5_tools/sandbox-mode 投影/定时 tick/jobs producer）、实现顺序
8 步 + 逐步验收、DIV-1..7 分叉清单。

**选择理由**：瀑布流要求阶段二产出可验收设计工件，主线程已具全部契约证据，接管是唯一可
推进路径；DIV 清单使 wire 分叉（camelCase、诚实桩、seam+失败闭）可审计。

**预期影响与回滚点**：阶段二工件落盘（M5-DESIGN.md）→ 提交易验收；回滚 = 撤 M5-DESIGN.md
提交（设计本身不改代码，无执行风险）。改动 → 提交 → 本条目互查（提交信息引用 D-056）。

---

## D-058（M5 编码·dsh-subprocess）：有界收集必须 drain-to-EOF；溢出即落盘则内存 tail 清空

**日期**：2026（M5 round 11）

**触发问题**：`tests/spill.rs` 红测（5000 行 echo 溢出 200B）暴露真 bug——初版 `drain_pipe`
在字节数达 `max_bytes` 时 `break` 停止读管道，导致管道满后子进程写阻塞/写失败
（stderr 反复 `The process tried to write to a nonexistent pipe`，退出码 1）。

**考虑的选项**：
1. **始终 drain 到 EOF（本次采用）**：收集器不断读管道直至 EOF；内存只保留尾部 ≤ max_bytes；
   发生溢出且有 spill → 完整流写盘、内存 tail **清空**（`readFrom(0)` 经 spill 路径恢复）；
   无 spill → 内存回绕只留最后 max_bytes 诊断 tail。
2. 初版「到预算即停读」：错误——管道是背压通道，停读必然连累子进程。
3. 溢出后继续读但全量保留内存：违背有界内存纪律（max_bytes 即上限）。

**最终选择**：选项 1。为「从不阻塞子进程」的不可违反不变量，即使 spill 未配置也必须读尽
管道；溢出丢弃受 `max_bytes` 约束。

**选择理由**：OS 管道是固定容量背压通道——任何「到阈值就停读」的收集器都会使子进程写侧
阻塞或崩溃（本测试实证 exit 1）；参考 TS collect 语义即「完整流恢复 or 有界 tail」二选一。

**预期影响与回滚点**：`drain_pipe` 重写；`tests/spill.rs` 锁定溢出落盘 + 内存 cap + 子
进程正常退出；回滚 = 撤 057af4f 恢复初版。改动 → 提交 → 本条目互查（提交信息引用 D-058）。

---

## D-059（M5 编码·dsh-fs 收尾）：glob 用 ignore::overrides（rg `--glob` 同源）+ grep 进程内 regex + tool-fs/sr_editor 纯面

**日期**：2026（M5 round 13）

**触发问题**：step3 最后一块（tool-fs：read/write/edit/read_image + glob/grep 搜索，
str_replace_editor）收尾。上一轮留下的 `fs_search.rs`（未提交在制品）用 `globset::GlobSet`
直接匹配相对路径，红测暴露四处语义偏差：① globset 默认 `literal_separator=false` 使 `*`
跨 `/`（`src/*.rs` 也会匹配 `src/sub/x.rs`，锚定失效）；② pattern 前导 `./` 使 GlobSet
恒不匹配；③ globset 无 `!` 取反过滤语义；④ walker `hidden(true)` 原以为收录隐藏文件，
实测 `.hidden.rs` 仍缺失。

**考虑的选项**：
1. **glob 匹配改用 `ignore::overrides::OverrideBuilder`（本次采用）**：这正是 rg
   `--glob` 的落地机制——无 `/` 的 glob 自动变 `**/*`（任意深度匹配 basename），带 `/`
   的 glob 锚定相对路径且 `*` 不再跨 `/`，`./` 前缀正常解析，`Whitelist`=命中 /
   `Ignore(UnmatchedIgnore)`=未命中。隐藏收录需 `hidden(false)`（ignore crate 语义是
   `hidden(yes)` 开启「忽略隐藏文件」，默认开启；`--hidden` = 关闭忽略）。VCS 剪枝沿用
   `filter_entry`。
2. 修补 GlobSet（剥 `./`、设 `literal_separator(true)`、手搓 `**` 变体）：与 rg 语义仍
   有细微出入，纯属再造 rg 已解决的轮子。
3. glob/grep 整个放弃（D-054 标为可选）：违背 step3 验收与模型面工具契约。

grep 引擎与 glob 相反（参考 argv 无 `--no-ignore --hidden`）：遍历保持默认——隐藏忽略、
`.gitignore`/`.ignore` 生效、仅 git 仓库内 gitignore 生效（`require_git` 默认 true，临时
目录须有 `.git` 才触发，rg 同语义）。匹配用 `regex::bytes::Regex` 按原始字节逐行，非
UTF-8 行显示为占位 `(line is not valid UTF-8)`（参考 `parseRecord`），不令整个搜索失败。
tool-fs 纯映射面（`parseWriteArgs`/`formatWriteOutput`/`parseEditArgs`/`formatEditOutput`/
`remediateFsError`/`imageMediaTypeForPath`/`formatImageReadOutput`）与 sr_editor 纯面
（`maybeTruncate`/`matchOffsets`/`lineNumbersAt`/视渲染/`str_replace` 唯一性/`insert` 行
插入 + 全部错误消息）逐字对齐参考；工具注册与宿主接线留 step7 web.rs。

**最终选择**：选项 1 + 上述 grep 引擎；tool-fs/sr_editor 以纯函数落「映射面」。

**选择理由**：Override/ignore/regex 都是 rg 同源 crate（D-054 已核离线），匹配语义逐字
对齐参考，避免自研疯狂重造轮子；`hidden(false)`/`require_git`/bytes-regex 三处反直觉点
经探针（`tests/globset_probe.rs`，用后即删）实证，不留猜测。

**预期影响与回滚点**：`src/fs_search.rs` 重写为 Override 引擎、新增 `src/grep.rs`/
`src/tool_fs.rs`/`src/sr_editor.rs` + 四组测试（fs_search 9 + grep 23 + tool_fs 13 +
sr_editor 15，dsh-fs 合计 98 全绿，clippy 零告警，workspace check 绿）。DIV：`maybe_truncate`
按 Unicode 标量截断（参考按 UTF-16 码元）；BMP 平面一致、星面字符 ±1，接受。回滚 = 撤本
提交即回 D-056 设计的 provider/policy/fence/read 基线。

---

## D-060（M5 编码·dsh-subprocess 扩展）：Windows 树终止改 Job Object；新增 wait_timeout/settle 读取/offset 读取原语

**日期**：2026（M5 round 13）。
**触发问题**：dsh-shell（step4）的前台超时杀、后台句柄读取、被 kill 终态都需要如下原语：
`wait_timeout`（同步超时轮询，不杀）、settle 后完整读取（collector drain-to-EOF 再缓存终态）、
`terminate` 写终态、增量 `read_stdout(offset)`/`read_stderr(offset)`。改完 `terminate` 后
`terminate_kills_running_child` 挂 29s：此沙箱拒绝 `taskkill`（Access denied），只有 taskkill
杀树时孙进程（`ping`）存活并握着收集管道直到自然结束，`finish_settle` join collector
被撑满。
**考虑的选项**：1. **Windows Job Object 树终止（本次采用）**——spawn 后立即
`AssignProcessToJobObject`（后代自动继承 job 成员），设 `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`，
`terminate` 用 `TerminateJobObject` 整树杀；taskkill 降为静音兜底，`std child.kill()` 仍做
确定性兜底。2. taskkill 保持 + `child.kill()` 兜底：能终止直系，但受限环境下孙进程存活、
collector join 被撑满（就是触发问题的样子）。3. 为 join 设超时/跳过：牺牲「终止后必拿完整
流」的正确性，掩盖树终止缺失。
**最终选择**：选项 1（`crates/dsh-subprocess/src/win_job.rs`，`#[cfg(windows)]`）。
**选择理由**：Job Object 是 Windows 整树终止的稳健原语（无 PID 复用竞态、后代自动继承、
`TerminateJobObject` 即时整树杀，强于 `taskkill /T /F`，且受限环境下仍有效），符合
M5-DESIGN §2.5 树终止语义；`win_job.rs` 侧已经验证 terminate 测试从 29s 降到 0.04s。
**预期影响与回滚点**：子进程被赋入带 `KILL_ON_JOB_CLOSE` 的 job——句柄 Drop 即整树终止
（进程已结束则为 no-op）；无对外 API 变化。新增原语：`wait_timeout(Duration)->Option<outcome>`
（10ms 轮询 try_wait，不杀）、`finish_settle`（join collectors + 缓存终态）、
`read_stdout/read_stderr(offset)` + `stdout_len/stderr_len/stdout_lossy/stderr_lossy/...spill_path`、
`CollectedOutput::data_len()`；`dsh-shell` 仅依赖这些面。环境注记：`taskkill` 的 stderr 被静音
（它是兜底）；回滚 = 撤本提交即回 taskkill 版。

---

## D-061（M5 编码·dsh-shell 核心）：request/spec 分裂 + resolve clamp + bash-local 后端与沙箱内 bash 不可用的测试门控

**日期**：2026（M5 round 13）。
**触发问题**：step4 第一块——dsh-shell 能力缝：需要 `flake` 参考 `shell/shell + bash-local`
的 request/spec 分裂与 resolve（缺省兜底：timeout 120s / max 600s / maxOutputBytes 64KiB /
spill 64MiB / grace 3s，`clampTimeout` 算术）、bash `-c` 后端与后台句柄。同时发现本 DSH
沙箱（Windows）**无法启动任何 bash**：Git Bash(msys) 创建 signal pipe/共享内存被拒
（Win32 error 5，exit 0xC0000142），WSL 启动器 `CreateInstance/E_ACCESSDENIED`。
**考虑的选项**：1. **bash 程序解析 = config.bash_path 显式 > Windows 候选（Git Bash 优先）>
裸 `bash`（本次采用）**：避开 system32\bash 这个 WSL 启动器。2. 测试改走 `cmd`/假后端：
背叛「真实 bash 语义」的验收目标。3. **Executor 集成测试用一次性可用性探测自跳**
（`tests/executor.rs`）：探测失败则打印明确原因（非实现缺陷）并跳过，在 Linux/正常
开发机/CI 上真实跑。4. `ShellRunResult.aborted` 恒 false：dsh-subprocess 信号面尚未接线
（step5+ 再补）。5. `ShellExecRequest` 暂不携带 `sandbox_policy` 字段：非 confining 后端
忽略之，sandboxing executor 落地时再随其类型加入。
**最终选择**：选项 1 + 选项 3 + 4、5 记录在案。
**选择理由**：bash 解析与统一 baseline 直接复刻参考；探测门控是「环境不可用则显式跳过、
可用则真实跑」的诚实门控（方法论四：环境问题是临时阻碍，不降级架构也不假绿）；
aborted/sandbox_policy 是 D-054 已标注的延后面，不提前造类型。
**预期影响与回滚点**：`crates/dsh-shell/` 新增 `types/resolve/executor` 三模块 +
`tests/resolve.rs`（6 纯面）+ `tests/executor.rs`（7 集成，本箱 7 用例探测到 bash 不可用
即跳过，全绿）；decode_errors 引用 `DEFAULT_MAX_SPILL_BYTES=64MiB` 等常量集中导出。
回滚 = 撤本提交。

---

## D-062（M5 编码·tool-bash 纯面）：camelCase `timeoutMs` schema + 标记词汇逐字；binding 留 step7

**日期**：2026（M5 round 13）。
**触发问题**：step4 最后一块——模型面 `bash` 工具：参数 schema（camelCase `timeoutMs` /
`run_in_background` / 可选 escalate 两字段）+ execute 校验（command/description 非空 →
`invalid command/description: …`；timeoutMs 正有限 → `invalid timeoutMs: expected a positive
number, got …`；escalation 两字段同现） + 模型面标记词汇。参考 `tool-bash/src/{index,render}.ts`
已逐字比对。
**考虑的选项**：1. **纯函数面落地（本次采用）**——`bash_tool_parameters`（schema DSL）/
`parse_bash_args`（校验）/`render_bash_result`/`render_bash_process_read`（标记词汇），
binding（define_tool / 宿主接线 / 后台 JobRegistry）留 step7 web.rs，与 dsh-fs 的 tool-fs
纯面先例一致。2. 现在就接 ToolRegistry.execute：引入 dsh-tools runtime 循环尚早，且后台
需 jobs 宿主，属 step7 验收面。
**最终选择**：选项 1。
**选择理由**：schema/校验/渲染都是纯函数、可独立 TDD 锁定逐字契约；执行与注册是对宿主
环境的组合，放到所有 seam 就位后的 step7 一次性接线，避免半接状态。
**预期影响与回滚点**：`src/tool_bash.rs` + `tests/tool_bash.rs`（19 用例，含 schema 逐字
快照、校验文案、标记顺序 denial→hint→exit、`[stderr]` 段、`(no output)`、截断 spill、
SIGTERM 优先于退出码）；dsh-shell 合计 32 测试全绿 + clippy 零告警。DIV（P4）：`timeoutMs`
camelCase 与既有 m4 snake `timeout_ms` 分叉（M5-DESIGN §5.3 已裁定）。回滚 = 撤本提交。

---

## D-063（M5 编码·dsh-terminal 注册表核心）：按 owner 隔离会话 + DUPLICATE_BACKEND=类型重复注册；PTY 后端/6 工具留后续

**日期**：2026（M5 round 13 末）。
**触发问题**：step5 第一块——终端会话注册表：Branded 会话 id、owner=精确 Agent 授权、
每会话单 active send、可分类错误码。M5-DESIGN §6.1 列出的 `DuplicateBackend`/`DuplicateName`
较为简略，初次实现把 DuplicateBackend 误解为「同名后端只能开一个会话」，导致两位 owner
无法各自开会话（红测暴露）。
**考虑的选项**：1. **逐字参考 `terminal/src/index.ts`（本次采用）**——`registerBackend`
重复类型 → `DUPLICATE_BACKEND`；会话**按 owner 隔离**（不同 owner 可各用同名后端各开
会话；同 owner 内 `name` 唯一 → `DUPLICATE_NAME`，覆盖已发布 + 在途保留）；spawn 失败
→ close 残留回滚；`SERVICE_DISPOSING`/`OWNER_NOT_LIVE` 在发布前判定；`NO_SESSION`/
`FOREIGN_SESSION` 统一 `expectOwned`。2. 维持「后端全局单会话」：违反 owner 隔离语义，
参考源码也证实是误读。
**最终选择**：选项 1。`TerminalSessionService.open(owner, backend_id, name, cfg)`；提供者
注册 = 类型表（`BackendProvider: Fn(TerminalConfig) -> Box<dyn TerminalBackend>`）；
**同步 send**（后端 `send()` 内完成等待与 wait_reason 判定后返回，注册表只守卫 `busy`
标志，SEND_ACTIVE 保留为 API 契约，单线程服务员不可并发重入）。
**选择理由**：owner 隔离与按 owner 命名是参考的行为契约（多 agent 并发的核心保证）；
崩溃回滚与本 suite 的「绝不半发布」纪律一致；同步 send 是本项目单线程服务员模型的
直接映射（异步主动操作由 jobs 层在 step7 包）。
**预期影响与回滚点**：`src/types.rs`（词汇）+ `src/registry.rs`（服务 + `TerminalBackend`
trait + `BackendDefinition`/`OwnerLiveness`）+ `tests/registry.rs`（12 用例：全生命周期、
ForeignSession、OwnerNotLive、NoBackend、DuplicateBackend、DuplicateName、回滚、dispose、
list）；clippy 零告警。真实 bash-pty 后端（portable-pty）与 6 工具（terminal_*）下一轮
继续（本箱 bash 不可用 → 集成门控）。回滚 = 撤本提交。

---

## D-064（M5 编码·dsh-terminal PTY 后端 + 6 工具）：ConPTY 输出管道在 ClosePseudoConsole 前不 EOF → close 先丢 pair 再 join；shell 程序参数化；工具面渲染词汇逐字

**日期**：2026（M5 round 13 末/14）。
**触发问题**：step5 后半——真实 PTY 后端（`portable-pty` 0.8，Windows=ConPTY）与 6 模型面
工具。首跑挂死（300s 超时）：面包屑定位到 `close()` 的 `reader_thread.join()`——本环境
ConPTY 在 `TerminateProcess(child)` 后**输出管道不 EOF**（伪控制台仍持有写端），读线程
永久阻塞，join 卡死；同样自然 exit 也不触发 EOF。另外 msys bash 起动即崩（D-061 已记），
但 ConPTY 单元可行。
**考虑的选项**：1. **close 先 `child.kill()` → 丢弃 pair（关 ConPTY → 管道 EOF）→ join
（本次采用）**；`wait_for_delivery` 同时轮询 `child.try_wait()` 判 `SessionExit`（不再只靠
reader EOF）。2. 读线程 detached（弃 join）：杀后/exit 后泄漏阻塞线程，不干净。3. 全量
非阻塞异步读：portable-pty 0.8 无 readiness API，需自研，成本高。
**最终选择**：选项 1 + shell 程序参数化（`PtyBackend::new(label, program)`，设计默认 bash；
本沙箱集成测试注入 `cmd.exe`——ConPTY 可用；Linux/正常机换 bash）。6 工具纯面
（terminal_open/send/read/signal/close/list schema + parse + 渲染词汇逐字对齐
`tool-terminal/src/render.ts`：`started terminal session … [type: …]`、`[wait: role]`
`[session: running|exited code=… signal=…]`、`[lines: a-b of c]`、`delivered … to
foreground process group …`、`closed terminal session …`、list 逐行 / `(no terminal
sessions)`；UTF-8 边界 head/tail 封顶 + `\n[output truncated]`）。
**选择理由**：ConPTY 生命周期明确要求「关闭伪控制台才释放输出管道」，丢弃 pair 是
portable-pty 语义下的正确顺序；轮询 try_wait 让 SessionExit 判定摆脱对 EOF 的依赖；
程序参数化是可控注入缝（与 shell 层 test_bash 同型）；工具面纯函数 TDD 锁定逐字契约。
**预期影响与回滚点**：`src/backend.rs`（`PtyBackend` + 后台读线程 + 有界滚动缓冲 +
idle 推断）+ `tests/backend.rs`（4 集成，cmd/ConPTY 实跑，7s）+ `src/tool_terminal.rs` +
`tests/tool_terminal.rs`（12 用例：schema 逐字、渲染快照、字节封顶）；dsh-terminal 合计
28 测试全绿 + clippy 零告警 + workspace check 绿。DIV：`signal` 仅「不能机器可发送
（ConPTY/portable-pty 0.8 无机敏信号），best-effort kill」；step7 接线映射 registry view
→ RenderedTerminalSession（pid/exit 元数据届时补全）。回滚 = 撤本提交。

---

## D-065（M5 编码·dsh-code-runtime 缝 + 纯面）：可移植排除集逐字、lossless-JSON 三层防线、run_code 纯面、TS 诚实桩

**日期**：2026（M5 round 14）。
**触发问题**：step6 前半——code 执行缝契约（`code-runtime/src/{types,index}.ts`）与
`run_code` 工具、TS worker 桩的 Rust 移植。参考仓无 python code-runtime 包（
`WIRE_FRAME_FIELDS` 在 M5-DESIGN §7.3 只署名未给出正文）→ python 协议由我们自设计（见
D-066）；本 D 只锁缝与纯面。
**考虑的选项**：1. 缝常量逐字 `&[&str]` + `is_dunder_member`（无正则：`len>=5 && 双下划线
两端`，与 TS `^__.+__$` 等语义：`__`/`____` 空中间非真 dunder）。2. lossless 三层防线：
parse 层 serde_json 拒绝 `NaN`/`Inf` 字面量 → invalid-output；`Number` 保持（2^60 精确）；
validate 层拦截 `-0.0`。serde_json `Number` 无法承载非有限（`from_f64(NaN)→None→Null`），
故非有限检查是防层不变式，真实防线在 parse 层（本文档果断修掉「构造 NaN 值断言」的
不可达测试，不留掩耳盗铃）。3. `run_code` 由 `code-runtime-python/tests`/`tool-run-code`
（M5-DESIGN §7.4 引用但参考仓不存在）→ 按 §7.4 孤立实现：code/description 必填、
`<parent>:code:<n>` 确定性嵌套 id、`exclude_run_code` 无递归。
**最终选择**：选项 1+2+3。`CodeRuntime` trait：`language()/isolation()/run(&request)`
（sync；signal 在 request 内）；`CancellationToken` = Arc<AtomicBool>；`CodeBindingFunction`
= `Arc<dyn Fn(Value) -> Result<Value,String> + Send + Sync>`。TS 诚实桩
`WorkerExit{"requires a code runtime"}`（DIV-3，M4 placeholder 语义保留）。
**选择理由**：可移植承诺要求一套共享排除集、按语言拆分即失效；有机 sync 后端 + 单线程
核心，无 async 收益；诚实桩取代假装，避免「绿但假」。
**预期影响与回滚点**：`src/{types,seam,json_lossless,tool_code,worker_thread_stub}.rs` +
`tests/{seam,json_lossless,tool_code}.rs` = 22 测试全绿 + clippy 零告警。python 后端接入
后 `validate_binding_namespace` 在 boot 前逐 namespace 校验。回滚 = 撤本提交。

---

## D-066（M5 编码·dsh-subprocess Pipe 端原语扩展）：Pipe 三态端在句柄上真正可见

**日期**：2026（M5 round 14）。
**触发问题**：code-runtime python 后端需要「spawn 后持续写 stdin + 裸读 stdout」的协议式
交互，而 `SubprocessHandle` 的 `StdinMode::Pipe`/`StdoutMode::Pipe` 注释已预告「宿主保留
句柄」却从未暴露端——收集模式才有读。这是设计语义与本体的缺口，不是新需求。
**考虑的选项**：1. **补齐 handle 端暴露（采用）**：`stdin_writer()`/`take_stdin()` +
`take_stdout_reader()`/`take_stderr_reader()`；spawn 后单点 take（`child` 每字段至多 take
一次，避免跨 match 条件移动被借检器拒绝；stderr 端类型是 `ChildStderr` 非 `ChildStdout`）。
2. python 后端自建 `std::process`：失去 job-object 树杀（timeout/abort 只杀直系）→ 违反
§7.3「树级 terminate()」。
**最终选择**：选项 1。`tests/pipe_ends.rs` 2 用例（写→取走 stdin→EOF→退出→裸读回显；
非 Pipe 无写端）。`Cmd /c more`（Windows）与 `cat`（Unix）做回显子进程。
**选择理由**：协议式后端是 dsh-subprocess 面向的一等使用形态；复用树杀/有界收集/scrub，
不为临时后端开旁路。**预期影响**：dsh-subprocess 24+ 测试全绿；回滚 = 撤本提交。

---

## D-067（M5 编码·dsh-code-runtime python 后端）：std 无法在 Windows 建额外 fd → 协议走
stdin/stdout JSON-lines，用户输出经 `log` 帧回流；顶层 null = 无完成值

**日期**：2026（M5 round 14）。
**触发问题**：§7.3 指定 `PROTOCOL_FD=3` JSON-lines，但 `std::process` 在 Windows **无法给
子进程创建额外 fd**（无 STARTUPINFOEX 句柄注入；Unix 也缺 pre_exec 公开稳定面）。参考仓
又无 python worker 可抄（`WIRE_FRAME_FIELDS` §7.3 只署名未成文）。
**考虑的选项**：1. **协议走 stdin/stdout（fd 0/1），用户 print() 在进程内捕获、按行回流
`log` 帧（采用）**；stderr 只承载引导级诊断。帧：host→child `boot|run|reply`，
child→host `boot_ack|call|log|done`——六帧名逐字但传输入口因平台约束重定义。2. 自研
Win32 句柄注入（CreatePipe+STARTUPINFOEX+bHandleInherit）建 fd3：违背「环境问题不改架构
意图也无必要给平台四处加成本」，且不安全面大。
**最终选择**：选项 1 + 核心决定：**宿主视入站为敌**——`validate_child_frame` 字段校验 +
REBUILD（未知/畸形帧 → WorkerExit）；`lossless` 三层（D-065）+ 完成值
`classify_admission`（over-budget / non-lossless 独立分类）；python None → 顶层 null →
**无完成值**（worker 无法区分 `return None` 与函数落到尾，统一按缺省，D 文档化）；
worker 自检非序列化完成值（`return float("nan")`）→ done error invalid-output；超时/中止 →
dsh-subprocess 树级 terminate；命名空间 boot 前 `validate_binding_namespace` 预检（契约
误用 → 诚实 WorkerExit，不伪装）。
**选择理由**：单条有序协议流（`log` 帧保序）比 fd3+stdout 双流还要稳；worker 单线程
多路复用 stdin（call 阻塞等 reply）自然支持绑定往返；python 真实执行（D:\Anaconda 可
定位）兑现「TS 桩 + python 真后端」的 DIV-3 设计。
**预期影响与回滚点**：`python_worker/worker.py`（stdlib 自含、编译期打包）+ `src/python_backend.rs`
（`PythonCodeRuntime` + `locate_python` + `validate_child_frame`）+ `tests/python_backend.rs`
11 集成用例（回显/大整数 2^60/日志/异常/binding 往返与拒绝/超时 500ms/中止/output-limit/
nan→invalid-output/契约误用）全绿 + clippy 零告警；`run()` 总值返回（错误是结果字段）。
回滚 = 撤本提交。

---

## D-068（M5 编码·step7a）：M5 工具 web 接线——slot-bind 定义器 + terminal 六件套真实绑定；
bash/fs/搜索/sr-editor 诚实 NOT_BOUND；run_code 交注册表保留传输

**日期**：2026（M5 round 15）。
**触发问题**：M5 各 crate 只有纯面（schema/parse/render），要把它们装配成 `dsh-tools`
可注册执行工具、并把宿主服务句柄 bind 进去（M5-DESIGN §8 的 register_m5_tools_with_host）。
**考虑的选项**：1. **dsh-tools 新增通用 slot-bind 定义器 [`M5Tool`]+[`define_m5_tool`]（采用）**
：参数/输出/渲染由调用方（web.rs 接线）传入，execute 从共享槽读入——已 bind → 委托真实
服务，未 bind → 结构化 `NOT_BOUND`（复用 `m4::not_bound_failure`，M4 同款诚实承诺）；不
引用任何 M5 crate（dsh-tools 是被依赖基座，避免依赖环）。2. 复制 m4.rs 的 define_bound
进 web.rs（重复 + 没实体）；3. 强行让 dsh-tools 依赖 M5 crate（依赖环）。
**最终选择**：选项 1。web.rs 增 `web/web_m5.rs`（独立模块承载，web.rs 已 4262 行）+ 顶层
re-export `M5HostServices`/`register_m5_tools_with_host`。
**接线语义**：`M5HostServices { terminal: Option<Rc<RefCell<TerminalSessionService>>> }`；
terminal 六件套真实绑定（owner=agent 必填；send/read/signal/close 走注册表授权，open→
list→send→read→signal→close 全生命周期被 `M5FakeBackend` 端到端测试覆盖；foreign-owner
拒绝；closed 会话后访问报错）。`bash/read/write/edit/read_image/glob/grep/str_replace_editor`
登记定义（schema+校验）+ NOT_BOUND（对应宿主句柄后续轮接入）。fs/搜索 schema 定义在
接线层 web_m5.rs（单消费者 = register_m5_tools_with_host），D 记录此归属。
**诚实偏差（D 记录）**：(a) **run_code 不登记**——`ToolRegistry::register_global` 无条件保留
该名（RUN_CODE_NAME，Code Mode 呈现传输，dsh-tools runtime 注入占位桩）→ 重复登记被拒；
真实运行面绑定属 registry/run_code binder 步（后续轮），其间占位桩诚实报
"requires a code runtime"。(b) terminal signal 渲染不声称虚构前台进程组
（ConPTY/Windows 无 PGID，D-064 DIV）→ 输出 `delivered SIGx` 而非 reference 的
"to foreground process group N"。(c) terminal_open 的 cwd 参数当前解析但忽略（backend
暂无 cwd 通道，后续轮）。测试替身 `M5FakeBackend` 复制了 dsh-terminal 测试内
FakeBackend 语义（其不导出，D 记录此重复）。
**预期影响与回滚点**：dsh-tools m5.rs + dsh-cli web/web_m5.rs + web.rs（mod + 2 接线测试）
+ dsh-cli/ dsh-tools Cargo 依赖；dsh-cli 83 测试全绿 + workspace clippy `-D warnings` 零告警
+ workspace check 绿。回滚 = 撤本提交。
**待办（后续 binder 轮）**：bash→LocalBashExecutor + job producer；fs 4/glob/grep/sr-editor→
LocalFileSystem+SandboxPolicy+Observation；run_code→registry 传输替换 + python runtime +
嵌套 tools 派发；M5g 定时 tick。

---

## D-069（M5 编码·step7b）：fs 六件套真实绑定——FsHost（LocalFileSystem + ObservationGate
+ agent→OwnerId 登记）；read/write/edit/glob/grep/str_replace_editor 全生命周期端到端

**日期**：2026（M5 round 16）。
**触发问题**：D-068 的 fs 组仍 NOT_BOUND；本地 provider（LocalFileSystem）与观察策略
（ObservationGate）已备好，现把它们 bind 进工具 execute 槽。
**考虑的选项**：1. **`FsHost` 组合宿主（采用）**：`LocalFileSystem(root)` + `ObservationGate`
+ `agent→OwnerId` 稳定登记（单调递增 Cell；Web 无 WeakMap 自动回收语义差异——宿主会话
结束需 `drop_owner` 清理，本轮接线未装会话清理钩子，D 记录为下次轮事项）。2. 每个
executor 自持本地 gate/owner map（重复 + 无组合归宿）；3. 借用 reference 的 per-session
宿主（web 装配后置，超出本轮）。
**语义落地**：read——`parse_read_args`→`build_window`（READ_LIMIT/READ_MAX_LINE_LENGTH/
READ_MAX_BYTES）→ **记录 Present{version} 观察**（后续 write/edit 以所见版本 CAS）；
write——`write_intent`（observed-present→ReplaceIfVersion，否则 CreateIfAbsent）→ 未读
写既有文件诚实 `FS_NOT_OBSERVED`（对齐 reference read-before-write）+ 写后记录观察；
edit——`edit_intent`（未观察 → FS_NOT_OBSERVED）→ `edit_text` CAS → 记录；
glob/grep——`parse_glob_args`/`parse_grep_args`（校验-only 语义同纯面）→ 进程内搜索体 →
保留上限（GREP_MAX_MATCHES/GREP_MAX_LINE_BYTES）→ 纯面 render（Found N of M matches）；
str_replace_editor——三模式（view / str_replace 唯一 / insert），自读 + 版本 CAS 回写
（参考工具 self-read 语义），渲染 `format_file_view` 编号视图。错误统一 `remediate_fs_error`
（STALE→「re-read the file, then retry」；NOT_OBSERVED→「read the file, then retry」）。
**诚实限制（D 记录）**：read_image 仍 NOT_BOUND（需图像解码服务取宽高；本轮不引图像
解码依赖，D 留待）；bash 仍 NOT_BOUND（需 LocalBashExecutor+producer 桥，下一轮）；
`FsHost` 无 ro 沙箱 SAND MODE 投影（SAND 模式入 web 装配时加，step7 收尾）。
**预期影响与回滚点**：web_m5.rs + web.rs（fs 端到端测试：write/read-before-write 拒绝/
read 观察/edit CAS/owner 隔离/glob/grep/sr_editor 替换落盘）；dsh-cli 84 测试全绿 +
workspace clippy 零告警 + check 绿。回滚 = 撤本提交。

---

## D-070（M5 编码·step7c）：bash 前台真实绑定——ShellHost（LocalBashExecutor，root 锚定
cwd）；run_in_background/sandbox_permissions 诚实 `UNSUPPORTED_OPTION`；Git Bash 真跑
端到端（Windows 候选探测）

**日期**：2026（M5 round 17/18）。
**触发问题**：D-068/D-069 的 bash 仍 NOT_BOUND；`LocalBashExecutor`（resolve+run）已备好，
本箱 Git Bash（C:\Program Files\Git\bin\bash.exe）可真实执行，现把前台路径 bind。
**考虑的选项**：1. **`ShellHost{executor, root}` 组合宿主（采用）**：root 锚定 BashConfig.cwd
（bash 默认工作目录 = 宿主 root，等同 reference workspace cwd）。2. 后台立刻接入 jobs
producer 桥（需宿主 completion tick 驱动 settle——单线程 JobRegistry 无自驱动，
tick/装配不在本轮，诚实拒绝而非半接）。3. 逐 token 伪造沙箱（否定：无 SAND 投影，
`result.sandbox` 恒 None）。
**语义落地**：bash_tool 定义 execute→规范化 value（command/exitCode/signal/timedOut/
aborted/timeoutMs/stdout{text,truncated,spillPath}/stderr/sandbox:null），render 由 value
重建 `ShellRunResult` 走同词表 `render_bash_result`（显式=值/可见性=渲染单一真相，与
terminal/fs 组同纪律）。executor：`parse_bash_args`（schema 硬校验，description 必填如参考）
→ workdir 覆盖（缺省 root）→ `resolve`（timeout clamp/预算兜底）→ `run`（非零/超时 resolve
成结果）→ 规范化值。**诚实拒绝**：`run_in_background:true` → `UNSUPPORTED_OPTION`（jobs
producer 桥 + completion tick 未接线，次轮落地）；`sandbox_permissions` 非空 → `UNSUPPORTED_OPTION`
（SAND 投影未接线）。agent 无需 owner（参考 bash 无属主语义）。
**诚实限制（D 记录）**：后台 / SAND / read_image / run_code 传输均未接，均为明确
`UNSUPPORTED_OPTION`/NOT_BOUND/保留占位，无伪造；`result.sandbox` 恒 None（本地后端零
SAND 事实但渲染不会假装 denied）。
**预期影响与回滚点**：web_m5.rs + web.rs（bash 真跑端到端测试：echo exitCode 0 / pwd cwd
锚定 MSYS 规范化路径 / exit 3 非零 resolve / bg+sandbox UNSUPPORTED；bash 不可用平台门控
跳过——诚实）；dsh-cli 85 测试全绿 + clippy 零告警 + workspace check 绿。回滚 = 撤本提交。
**待办**：jobs producer 桥（bash 后台 + completion tick 驱动 settle）、SAND 投影、read_image
解码、run_code 传输替换、M5g 定时 tick——step7d + step8。

---

## D-071（M5 编码·step7d·part1）：bash 后台 jobs producer 桥——BashJobsBridge（JobRegistry
+ ShellProcess 以 job id 关联；`ProducerHooks{on_cancel=kill}`；宿主合作泵 `pump()` 驱动 settle；
final-output 终态携全文）；真实 Git Bash 端到端（jobId → pump → job_read completed 全文 + 授权围栏）

**日期**：2026（M5 round 19/20）。
**触发问题**：D-070 的后台仍 `UNSUPPORTED_OPTION`；M5-DESIGN §8 jobs subprocess producer
（D-049 形状闭合）要求 tool-bash 后台经 `JobRegistry.start(StartSpec{producer})` 桥进程句柄。
**考虑的选项**：1. **`BashJobsBridge{registry:RefCell<JobRegistry>, processes, outputs}`（采用）**：
进程在调用方先 spawn（jobs.start 的 producer 只回喂 hooks，不触发执行——`producer 先跑再分配
id`（D-049）语义下不产生孤儿：start 失败由调用方 `process.kill()` 掐掉）；`start_bash(owner,
label, process)` 登记 job，`ProducerHooks{on_cancel=kill, read_output=None}`（final-output 语义）。
2. produce-time spawn（jobs.start 内 spawn：OwnerQuota 失败前不 spawn，但进程句柄无法回传
bridge/无需 registry 序——增加 executor 借入复杂度，弃）。3. 流式 read_output 逐轮滚入（注册表
output_buf 无公开追加 API，且 bash 后台为 final-output job——D-004/TS 语义，弃流式）。
**完成结算——合作泵 `pump()`（采用）**：单线程注册表不自驱动 settle（D-004 诚实降级），
宿主（M5g tick/测试循环）调 `pump()`：**先 done() 等到退出（collector join，管道缓冲全落）
再 read_output() 收尾增量**（修正初版先读后 done 丢终态缓冲的竞态——TDD 红测捕获）→ 终态
completed/killed → `settle(status, detail=exit code, output=全文)` → 移除。App 侧 job_read/
job_list/job_kill 共享同一 JobRegistry 语义（本桥即其宿主句柄）。幂等、first-wins。
**诚实限制（D 记录）**：pump 是合作推进（无后台线程自动驱动——M5g 服务层线程 tick 宿主侧落，
核心不动）；`result.sandbox` 恒 None；SAND/read_image/run_code 仍按 D-070 严格。
**预期影响与回滚点**：web_m5.rs（BashJobsBridge + bash 后台分支：jobId 值/后台渲染词表
"started (collect via job_read…)"）+ web.rs（真实 Git Bash 后台端到端：bg1 jobId → pump 至
settle → read 断言 completed + 全文 job-start/job-end + foreign caller 拒绝 + 前台共存；
bash 不可用平台门控跳过）；dsh-cli 86 测试全绿 + clippy 零告警 + workspace check 绿。回滚 =
撤本提交。
**待办**：M5g 定时 tick（服务层线程泵调 pump）、SAND 投影、read_image 解码、run_code 传输替换、
step8 M5-ACCEPTANCE。

---

## D-072（M5 编码·step7e）：M5g 定时推进（验收 #7）——服务层线程 tick（mpsc）→ 主线程
`m5g_tick_once`（`ScheduleHost::dispatch_due` 到期注入 + `BashJobsBridge::pump` 合作结算）；
真实线程自动化测试（schedule after(1s) 自动派发落日志 / bash 后台 job 自动 settle**非手工**）

**日期**：2026（M5 round 21/22）。
**触发问题**：M5-REQUIREMENTS 验收 #7 要求 schedule「真实定时推进（宿主 tick 线程 + mpsc →
`dispatch_due` 自动触发，非手工）；M5-DESIGN §8：服务层线程 tick（1s 或配置间隔）→ mpsc 桥 →
主线程 `ScheduleHost::dispatch_due(now_epoch)`；M5g 同时驱动 jobs 泵。
**考虑的选项**：1. **`M5gTick`（服务线程仅发 tick）+ `m5g_tick_once(sched, bridge, now)`（主线程泵，
采用）**：核心（ScheduleHost 折叠/到期 + BashJobsBridge::pump）留主线程——两者均 Rc/RefCell 非
Send，线程只推 mpsc tick（Send 安全），D-004 单线程注册表承诺不变。2. 线程直接持 Rc<ScheduleHost>
/RefCell<JobRegistry>（非 Send 不可共享，弃）。3. tick 进 serve() 请求循环（M5 服务器面未在
dsh-rs CLI 装配，无附着点；作宿主侧注入点预留，弃）。
**tick_once 语义**：dispatch_due → ((framing, dispatched)) → bridge.pump() → Ok。M5g 主循环
只 `wait_tick` eat tick 后调 tick_once；测试证明「非手工」：schedule after(1s) 经线程 tick
自动派发（dispatch 事件落会话日志 ≥2）；bash 后台 `sleep 0.2; echo auto-settled` 经 tick 自动
settle completed + 全文，全程零手工 dispatch_due/pump。M5gTick Drop 置停（线程链路有界退出）。
**诚实限制（D 记录）**：M5gTick 是服务层 tick 输送器；真实 M5 服务器装配（serve 宿主注入点）
仍按 M5-DESIGN 在宿主侧预留；本实现交付「线程 + mpsc + 主线程泵」三件套 + 自动化证明，满足验收
#7 可测试语义。SAND/read_image/run_code 仍待（D-070 严格）。
**预期影响与回滚点**：web_m5.rs（M5gTick + m5g_tick_once）+ web.rs（2 集成测试：schedule 自动
派发落日志 / bash 后台自动 settle，bash 不可用平台门控跳过）；dsh-cli 88 测试全绿 + clippy 零
告警 + workspace check 绿。回滚 = 撤本提交。
**待办**：SAND/mode 会话事件投影（effectiveSandboxMode fold + sandbox:policy 系统提示段）、
read_image 解码绑定、run_code 传输替换、web.rs 宿主装配（M5HostServices 生产工厂）、
step8 M5-ACCEPTANCE。

---

## D-073（M5 编码·step7f）：run_code 传输真实执行（验收 #6）——注册表
`set_run_code_executor` 覆盖钩子（dsh-tools，替换 Code Mode 注入占位桩）+ web_m5
run_code executor（真实 python 子进程 `PythonCodeRuntime::run` → 规范化值/渲染）；python
可用门控 e2e（return 表达式 lossless 跨界 / print→logs / dict→json）

**日期**：2026（M5 round 23/24）。
**触发问题**：验收 #6 要求 run_code 工具桥接线**真实执行**（替换 `placeholder_run_code`）；
当前注册表 view 注死占位桩（D-024 记录「Code Mode 传输依赖 dsh-code-runtime(M5) 本轮注入
占位」），python 后端已备（D-065/066/067）。
**考虑的选项**：1. **注册表加 `set_run_code_executor(exec) -> Option<ToolExecute>` 覆盖钩子（采用）**：
view 步骤 4 在非 native 注入 run_code 时，用宿主 executor 构造 `run_code_def`（同名/schema/
渲染单源 `render_run_code_value`：失败错误 → logs+value → 空 "completed with no output"），
缺省仍占位桩（诚实早错）；**保留名守卫不变**（register_global 仍无条件拒 run_code——传输只能
经本钩子覆盖，杜绝功能遮蔽回归）。2. 放开 run_code 可被 register_global 注册（破坏「presentation
transport 保留名」承诺，弃）。3. web_m5 直接 register run_code 全局（原 D-068 已验证被注册表拒，
弃）。
**execute 语义**：`parse_run_code_args`（code/description 必填）→ `CodeRunRequest{program,
bindings:vec![], signal:None}` → `PythonCodeRuntime::run`（真实 python 子进程，fd3-class
stdin/stdout JSON-lines 协议，D-066/067）→ 规范化值 `{language, value?, logs[], error?}`
（lossless 跨界 + print→log 帧回注）→ 模型可见渲染由 run_code_def 单源。**嵌套工具派发
（bindings 注入 tools.*）本轮为空**——程序调 tools.* 得「未注入」诚实错误；嵌套执行/传播
留宿主端下一步（不伪造）。
**诚实限制（D 记录）**：无 runtime → 占位桩保留（"requires a code runtime"）；run_code 仅在
Code-mode view 可见（Native 不受影响）；嵌套 tools 派发未接（诚实空命名空间）。
**预期影响与回滚点**：dsh-tools（runtime.rs：字段 + setter + view 覆盖分支 + run_code_def/
render_run_code_value + 测试 run_code_executor_override_replaces_placeholder）+ dsh-cli
（web_m5.rs：M5HostServices.code + register hook + run_code_executor_with/canonical + web.rs
e2e register_m5_run_code_transport_executes_python：缺 description → INVALID_ARGS / return 42
lossless / print→logs+None / dict→json）；dsh-tools 103+ 测试、dsh-cli 89 测试全绿 + workspace
clippy 零告警 + check 绿。回滚 = 撤本提交。
**待办**：SAND/mode 会话事件投影、read_image 解码绑定、M5HostServices 生产装配工厂、
step8 M5-ACCEPTANCE。

---

## D-074（M5 编码·step7g）：M5Host 生产装配工厂（验收 #9，一次构造全宿主句柄；非仅测试
可配）+ effectiveSandboxMode 会话事件 fold + `sandbox:policy` 系统提示段（验收 #3 注入）

**日期**：2026（M5 round 25/26）。
**触发问题**：M5HostServices 此前只在测试逐字段构建（terminal/fs/shell/bash_jobs/code
五句柄无生产装配点，缺口「only built in tests」）；§8 的 effectiveSandboxMode 回放 fold 与
`sandbox:policy` 系统提示段（验收 #3 系统提示注入）无纯面。
**考虑的选项**：1. **`M5Host::assemble(root)`（采用）**：root 规范化（canonicalize）后一次
构造 terminal（`TerminalSessionService::new`）+ fs（FsHost）+ shell（ShellHost，root 锚定
cwd）+ bash_jobs（BashJobsBridge）+ code（仅 `python_available()` 装配——诚实：无 runtime
时 run_code 保持注册表占位桩）；`register()` 便捷注册全工具 + bind。会话清理钩子（fs owner
登记释放，D-069 记录）随宿主生命周期由装配方调用，预留。2. 每句柄一支工厂（分散、无组合
归宿，弃）。3. code 无 python 也强行装配（run 才失败——运行时炸 vs 装配期诚实，弃）。
**fold 语义（采用，纯面可测）**：precedence declared：approved > session `sandbox/mode` >
默认 read-only。本 fold 实现 session+default 两档——last-wins `sandbox/mode` 事件（未知
模式忽略，log-only 语义；`source:"delegation"` → `"session-delegation"` 标记）；approved
级联（`approval/decided` 事件落盘后接线）为预留槽位，**不伪造 approved 来源**。`sandbox:
policy` 段（order 110）：有效模式 + 可写根（仅 workspace-write 产名单，复用
dsh-sandbox::writable_roots，read-only → "(none — read-only)"）。
**诚实限制（D 记录）**：approved 级联未接（无 approval/decided 事件发射方）；read_image
仍 NOT_BOUND（解码依赖评估留待 step8 前）；嵌套 tools 派发（run_code bindings）仍空。
**预期影响与回滚点**：web_m5.rs（M5Host::assemble/register + EffectiveSandbox/fold/
sandbox_policy_segment）+ web.rs（fold last-wins 纯测 + assemble 生产驱动 e2e：write→read→
glob + bash 门控真跑 + 全句柄在场断言）；dsh-cli 91 测试全绿 + workspace clippy 零告警 +
check 绿。回滚 = 撤本提交。
**待办**：read_image 解码绑定（或诚实再降级）、step8 M5-ACCEPTANCE（全量 workspace test +
clippy + DECISIONS 互查 + git 闭环）。

---

## D-075（M5 编码·step7h）：approved > session > default 完整解析优先级（验收 #3 补档）

**日期**：2026（M5 round 27/28）。
**触发问题**：D-074 的 fold 只落了 session+default 两档、把 approved 级留为「预留未被实现」
的槽位；验收 #3 明文要求「模式解析优先级（approved > session `sandbox/mode` > 默认
read-only）」——逐条互查暴露出该档是硬性验收项，不能以「预留」自占地通过。
**考虑的选项**：1. **`resolve_sandbox_mode(approved: Option<SandboxMode>, events) ->
EffectiveSandbox`（采用）**：完整三档 pure resolver——approved 显式（`source:"approved"`）
> 会话最后一跳（复用 fold，`source:"session"`）> 默认 read-only（`source:"default"`）。
approved 输入由调用方经既有 ApprovalProvider 缝（§3.4 解耦，`ApprovalOutcome` 四态
fail-closed，缺省无通道即 None）裁决后传入——折叠层**不伪造批准来源**，approved 的来源与
裁决归审批缝。2. 等 `approval/decided` 事件落盘后再接线（事件发射方不存在，验收前无法
闭合该档，弃——需一个同步可测的纯函数，而非等待异步发射方）。3. 删除 approved 档
（倒退验收，弃）。
**验收盘点（互查证据）**：#2a 树级超时杀 = dsh-shell executor `wait_timeout` 预算 →
`subprocess::terminate`（Windows taskkill /T /F + win_job 确定性整树）→ `timed_out` 置位 +
`render_bash_result` 产出 `[timed out after Nms]` 标记（tool_bash.rs:181）+ timedOut/aborted
互斥（aborted 恒 false，结构保证）。#2 subprocess 真实 spawn/有限输出/scrub env/terminal
seam（step5 已交付）。
**预期影响与回滚点**：web_m5.rs（resolve_sandbox_mode）+ web.rs（四档个案纯测：默认 /
会话 / approved 覆盖会话（含更宽 danger 被覆盖）/ approved 无会话）；dsh-cli 92 测试全绿 +
clippy 零告警。回滚 = 撤本提交。
**待办**：step8 M5-ACCEPTANCE（全量 `cargo test --workspace` + clippy `-D warnings` +
DECISIONS #1-10 逐条互查 + git 闭环）。read_image 非验收项（#4 仅 read/write/edit），NOT_BOUND
诚实降级由 D-069 记录。

---

## D-076（M5 收口·step8 M5-ACCEPTANCE）：全量验收全绿 + DECISIONS 追记（dsh-sandbox
step2 缺条目）+ flaky 测试修复 + #9 边界诚实记录

**日期**：2026（M5 round 29/30）。
**触发问题**：step8 以 M5-REQUIREMENTS 验收 #1-10 逐条互查收口——发现三处待封闭事项：
① D-054/D-056 之后 dsh-sandbox（验收 #3 阶梯/writableRoots/系统提示）在 `787f80c` 交付但
**无对应 DECISIONS 条目**（违反 #10「每子步 DECISIONS 对应」，早于本阶段日志纪律成熟期的
漏记）；② dsh-fs tests/local.rs 的 `temp_ws` 全测共享 `dshfs-{pid}` 单目录，并行时
`remove_dir_all` 与别的测试写入竞争 → 偶发「首写失败」flaky（全量跑暴露即 #1 稳定性闭环）；
③ 验收 #9「handle_rpc_host 集成真实驱动」的实义核对。
**处置**：
- **① 追记 dsh-sandbox（D-076 本条一并记账）**：`787f80c` = SandboxMode kebab 阶梯/
  `wider_modes` 严格更宽单向/`validateEscalationArgs` 同现同缺 + 非空 justification（fail-closed）/
  `sandboxDenialMarker`+`escalationHintMarker`/`writableRoots`（仅 workspace-write 产名单，
  read-only/danger → []）；11 测试绿。approved > session > default 三档由 D-075 闭合。
- **② flaky 修复**：`temp_ws()` 改每测独立目录 `dshfs-{pid}-{seq}`（静态 AtomicU64 自增，
  **不共享、不 remove**）——并行互不干扰，local 8/8 复跑 3 次稳定；测试稳定是「测试验证」
  阶段的通过条件（#1 稳定性）。
- **③ #9 边界诚实记录**：dsh-rs 的工具执行走 `ToolRegistry`+宿主绑定，**不经
  `handle_rpc`**（那是 M4 会话域 RPC：session.prompt/history/event）。#9 的真实驱动 = M5 工具
  经 `register_m5_tools_with_host` 扩展（web.rs:33 公开导出）→ 宿主句柄 bind 真实执行
  （fs/terminal/shell/bash/code 每类真实 e2e）+ 无 handle → NOT_BOUND/UNSUPPORTED（
  all-tools-visible-unbound 诚实测）+ 投影/事件经既有通道（sandbox/mode 词表已在
  dsh-session::EventKind，fold 复用会话事件）。**M5 工具进 CLI `serve()` 服务器执行环属
  M6 serve 里程碑**（serve() 现为 M4 会话 web loop；m5 工具执行面已由 M5Host::assemble
  生产化，接线入 serve 留 M6）——显式边界，非默默省略。
**验收证据（step8 实测）**：#1 `cargo test --workspace` **187 组结果全绿零失败** +
clippy `-D warnings` 零告警 + workspace check 绿（本轮实测）。#2 D-058/060/066；#2a
D-062 标记词汇 + executor wait_timeout → 树级 terminate（taskkill /T /F + win_job）+
`[timed out after Nms]`（tool_bash.rs:181）+ timedOut/aborted 互斥。 #3 D-075 + D-076
追记 + D-074 fold。 #4 D-059 + D-069（read/write/edit 真实执行 + schema + write_intent CAS +
FS_SANDBOX_DENIED 进程内围栏 + 原子写 + 流式上限；read_image 非验收项 NOT_BOUND 诚实）。
#5 D-061/062/070/071。 #6 D-065/066/067 + D-073（run_code 真实 python）。
#7 D-071 + D-072（M5g 真实定时推进）。 #8 D-063/064/068（terminal 6 工具，决策 P 落地）。
#9 见上。 #10 D-058..076 + git 逐提交可互查。
**预期影响与回滚点**：仅 dsh-fs tests/local.rs（temp_ws 独立目录）+ DECISIONS.md（追记 +
收口）；dsh-fs 8/8、全量 187 组绿。回滚 = 撤本提交（修测试夹具，无功能影响）。
**M5 阶段结论**：需求→设计→编码→测试→部署五个阶段的「测试验证」关卡通过；M5 交付物
（六 crate + web.rs 接线 + M5Host 生产面）齐套，#1-10 全部有实测/可互查。read_image 与
run_code 嵌套 tools 派发为诚实降级项（D-069/D-073 记录，非验收项或渐进项）；M5 serve 接线
归 M6。

---

## D-077（M6 需求分析·阶段关卡）：M6 范围裁定——服务器执行闭环（serve 接线）为主轴 +
M5R §5 待办篮按子步穿插（用户裁定 P1 两者都要）；P2 workspace_root=web 配置缺省 CWD；
P3 无 LLM 凭据 fail-loud；P4 注入真实测试 LLM 端点（key 仅环境变量不入库）

**日期**：2026（M6 round 1/2）。
**触发问题**：用户发起「按照流程规划和开发M6」。M5R §5 把 mcp/acp/hooks/skill、settings/.env、
provider capabilities、ts-host 差分、SQLite 推给 M6，D-076 又把「M5 工具进 serve() 服务器执行
环」列为 M6 serve 里程碑——M6 范围存在多个候选，需求分析必须先定界（第一性原理 + 双视角）。
**自下而上资产实证（本阶段实测）**：`AgentLoopHost::with_store(config, llm, tools, store)` 已备
（tools 由调用方传入 = M6 注册缝）；web.rs RPC 面已含 agent.run/agent.turn/session.prompt/
llm.models；`agent-loop|agent.turn|agent.run` 分支在 boot.agent_loop.is_some() 时走
`run_rust_loop`，否则 `run_turn`（cordis loop 插件）——但**生产 `dsh web` 的 boot.agent_loop=
None**（lib.rs Boot 默认 None，仅测试装配）；register_m4_with_host/register_m5_with_host/
M5Host::assemble 全部公开但**仅在测试调用**。结论：唯一上线缺块 = 装配 + 生命周期 + tick/sandbox
挂入，无新引擎需求（自顶向下与自下而上相遇于「装配语义」）。
**用户裁定（P1-P4 全部采纳）**：P1 两者都要（主轴 step1-6：装配工厂/生命周期/tick 注入/sandbox
投影/LLM 诚实无 key/前端最小闭环；穿插 step7-10：settings/.env → provider capabilities →
hooks/skill → ts-host 差分/SQLite）；P2 workspace_root=WebConfig 指定，缺省 CWD canonicalize；
P3 无 DEEPSEEK_API_KEY → agent.turn fail-loud（不伪造，工具/API 面照常）；P4 测试 LLM 端点
base_url `http://100.105.152.101:18080/v1`、model `deepseek-v4-flash-0731-ext`、key 经
`DEEPSEEK_API_KEY` 环境变量（DeepSeekConnection 不含 key——key 在服务层桥 HTTP 头，天然不上
git；**key 本体永不入库**）。
**拒绝的选项**：① M6 广铺待办篮而忽略主轴（违背第一性根本目的「让已有执行面真跑」，P1 裁定
否）；② 造新执行引擎（M5 资产全可复用，否）；③ key 入配置/文档（安全红线，否）。
**预期影响与回滚点**：M6-REQUIREMENTS.md 为阶段一关卡产物（目标/非目标/假设/约束/边界/验收
#1-8/裁定 P1-P4）。回滚 = 撤本提交（纯文档）。进入阶段二（系统设计）时按此文档分解，不重新
发散需求。
**待办**：阶段二系统设计（M6-DESIGN.md：装配工厂/Lifecycle/tick 注入/sandbox 投影/LLM 装配/
穿插篮子步）→ 阶段三编码（TDD）→ … → M6-ACCEPTANCE。

---

## D-078（M6 阶段二·系统设计收口）：M6-DESIGN.md 六子步主轴 + 穿插篮 + DIV/让步清单；设计定案
（装配工厂=真实注册表 M4+M5、SessionStore 共享、M5Host::shutdown 清理面、serve recv_timeout
自驱节拍、llm_http stream 变体、key 仅 env fail-loud）

**日期**：2026（M6 round 2/3）。
**触发问题**：M6 需求关卡通过后进入系统设计；需把每一主轴子步落到「缝（自下而上实测）+
设计 + TDD 计划」，并对关键组合语义定案。
**自下而上设计证据（本阶段实测）**：① 装配骨架既有 = web.rs 测试
`rpc_prompt_routes_to_rust_agent_loop_shared_store`（LlmRuntime::new + register_adapter →
ToolRegistry::new(Native) → AgentLoopHost::with_store(config, llm, tools, session_host.store.clone())
→ boot.agent_loop=Some → session.prompt 走 run_rust_loop 事件落共享 store）；② `SessionHost.store:
Rc<SessionStore>` 公开可 clone 共享；③ dsh-core `llm_http::chat_completions(base,key,model,
messages,tools)->Value` 是**非流式** final JSON；`dsh-llm-deepseek PayloadsResolver =
Vec<String>`（SSE data payloads）——**真实 HTTP/SSE 流式桥缺失**，需新变体；④ serve 主循环现
`for server.incoming_requests()` 阻塞，tiny_http `recv_timeout(d)` 可改轮询自驱节拍。
**设计定案（全部采纳）**：S1 装配工厂 `assemble_server_loop(store, workspace_root, llm_endpoint)`
= register_m4_with_host + register_m5_with_host(M5Host::assemble(root)) → AgentLoopHost::with_store
(共享 store) → boot.agent_loop；真实注册表（M4+M5 全工具）。S2 `M5Host::shutdown`（幂等：
tick stop → bash kill_all+settle Killed → terminal close_all）+ WebConfig.workspace_root（缺省
CWD canonicalize，P2）。S3 serve 主循环 `recv_timeout` 自驱节拍 → 主线程 `m5g_tick_once`（调度
到期 + jobs settle，推进点唯一收敛主线程；M5gTick 线程不再进 serve——IV-2）。S4 policy 段注入
prompt（order 110）+ `resolve_sandbox_mode` + escalation 校验 fail-closed（deny+hint）。S5 LLM：
dsh-core 新增 `chat_completions_stream -> Result<Vec<String>, 错误>`（复用既有 HTTP POST+Bearer+
SSE 行解析；TDD）+ M6 deepseek thunk（DSH_LLM_BASE_URL 配置读写、DEEPSEEK_API_KEY 仅 env；
无 key → 首轮 fail-loud AUTH 明确码，工具/API 面照常——P3）+ `register_adapter(["deepseek"])`。
S6 前端最小闭环 = 复用 session/event downlink + history（re RPC 集成测试 + 门控冒烟）。
**穿插篮子步**：step7 settings/.env → step8 provider caps（真实 catalog 列录）→ step9
hooks/skill → step10 ts-host diff/SQLite → step11 M6-ACCEPTANCE。
**拒绝的选项**：① M6 不建新 transport、用非流式 chat_completions 直接喂 loop（丢流式语义，
PayloadsResolver 契约不匹配，否）；② 把 key 写进 WebConfig/代码（安全红线——仅 env，否）；
③ serve 继续纯阻塞循环 + 独立线程持有 Rc 宿主（Rc 非 Send，多线程持非 Send 宿主要么破坏
单线程纪律要么强制 Arc 大改，否——recv_timeout 自驱节拍最贴合单线程宿主模型）。
**预期影响与回滚点**：M6-DESIGN.md 为阶段二关卡产物（每子步缝/设计/TDD + DIV/让步）。回滚 =
撤本提交（纯文档）。进入阶段三编码（TDD 红→绿，逐子步 commit + D 互查）。
**待办**：阶段三编码 step1（服务器装配工厂）开始——红测 → 绿 → 重构 → clippy → commit。

---

## D-079（M6 编码·step1a）：服务器装配工厂核心——`assemble_server_loop`（真实注册表
M4+M5 + 共享 SessionStore + 生产路径一轮真实工具回合；验收 #2）

**日期**：2026（M6 round 4/5）。
**触发问题**：step1（服务器装配工厂）进入编码，TDD 红→绿。
**设计落点（自底向上实测）**：既有骨架 = web.rs 测试 `rpc_prompt_routes...`（mock LlmRuntime
`register_adapter` + AgentLoopHost::with_store + boot.agent_loop + run_rust_loop）；`SessionHost.
store: Rc<SessionStore>` 公开可 clone 共享；`TodoWriteHost::new(host, default_session)` +
`bind_agent` 归属登记；`M4HostServices{jobs, schedule, todo}` / `M5Host::assemble(root)` 均公开；
驱动一轮工具回合的 mock 脚本镜像 dsh-agent-loop `m2e2_driver::tool_call_chunks`
（ToolCallDelta + BlockEnd(ToolCall) + Finish::ToolCalls → 第二轮文本收尾）。
**实现（红→绿）**：`pub fn assemble_server_loop(session_store, workspace_root, llm, provider,
model, m4, m5) -> Result<Rc<AgentLoopHost>, String>`——ToolRegistry::new(Native) +
register_m4_tools_with_host(m4) + register_m5_tools_with_host(m5.services) + 单一默认 agent
{id "default", provider, model, session_id "default", cwd=workspace_root} +
AgentLoopHost::with_store(config, llm, tools, session_store)。红测先（E0425 assemble_server_loop
未定义）→ 绿（93 测全绿 + clippy 零告警 + check 绿）。
**红测断言（验收 #2 可互查）**：① 视图 known_names ⊇ {todo_write, job_list, job_output,
schedule_create, write, read, edit, glob, grep, str_replace_editor, bash, terminal_open,
terminal_send}（= M4+M5 全工具真实注册表）；② mock LLM 一轮 `todo_write` 工具调用经生产路径
`run_rust_loop` 驱动 → M4 todo_write 真身执行 + `todo/write` 事件落共享 store + tool/call +
收尾 assistant/message；③ store 与 SessionHost 同店。
**决定/边界**：① provider/model 由装配方传入（生产=deepseek+端点，测试=mock）——装配函数不
硬编码 provider；② `M5Host::shutdown` disposer 属 step2（生命周期），本步仅装配；③ run_code 在
Native 视图不可见（Code-mode 注入），故未列入断言集（诚实，不伪造）。④ workspace_root 传装配
方（wss_root 在生产由 P2 WebConfig 提供）。
**预期影响与回滚点**：web.rs（+assemble_server_loop + M6i 验收 #2 集成测试）；dsh-cli 93 测全绿 +
clippy 零告警 + check 绿。回滚 = 撤本提交。
**待办**：step1b serve() 接线（&mut Boot / 配置 flag + workspace_root + llm 装配 + enable
agent loop；诚实降级）→ step2 生命周期 shutdown → step3 tick 注入 → step4 sandbox 投影 →
step5 LLM 桥 → step6 前端最小闭环 → 穿插篮 → M6-ACCEPTANCE。

---

## D-080（M6 编码·step5a+5b）：LLM 装配桥——dsh-core 流式传输 + deepseek 适配器 thunk +
诚实 no-key fail-loud（验收 #6；P3/P4）

**日期**：2026（M6 round 2）。
**触发问题**：step5（LLM 装配）是 step1 的前置硬块；step1b serve 接线需要真实鲁棒实现。
**自下而上实测定案**：① `serialize_request`（dsh-llm-deepseek）已产出 `WireRequest`
**含 `"stream": true`** + `stream_options.include_usage`（serialize.rs 210-214）——适配器契约
本就是流式；缺的只是 transport thunk（M1e 线绳桥未做）。② `dsh_llm_deepseek::sse::parse_sse
(&[u8]) -> Result<Vec<String>>` 与 `translate` 已单测覆盖——**不重复造 parser**。③ dsh-core
`llm_http` 已有 tcp_exchange（TLS https + Content-Length/读到关闭）+ build_request(Bearer) +
parse_base——**HTTP/TLS 传输复用**。④ dsh-cli 尚无 dsh-llm-deepseek 依赖（依规范评估后引入：
官方适配器 crate，M6 LLM 装配必需）。
**实现（step5a dsh-core）**：`chat_completions_stream(base, api_key, body) ->
Result<StreamBody{status, bytes}, StreamHttpError{status, detail}>`——POST 已序列化流式 body
到 `{base}/chat/completions`，返回原始响应体字节（SSE 解码归 deepseek crate）；非 2xx →
结构化错误带 status。测试：本地 TcpListener 单发 SSE → Ok(200,原始字节) + Bearer 头 + stream
body 断言；401 → StreamHttpError{401}；无效 base → status 0。
**实现（step5b dsh-cli::m6_llm）**：`server_llm_runtime_with_key(base, model, key)`（显式 key，
测试用）+ `server_llm_runtime(base, model)`（key 仅读 `DEEPSEEK_API_KEY` 环境变量，P4）。
连接事实：base_url + defaults + DEFAULT_MAX_TOKENS/CTX + catalog ≥ 装配模型 + normal
retry。thunk：无 key → `LlmError(AUTH, "missing DEEPSEEK_API_KEY: set it...")`（首回合
fail-loud，模型发现/工具注册/API 面照常——P3）；有 key → to_string(WireRequest) →
chat_completions_stream → parse_sse → payloads；传输错误按 status 映射 `http_error_code`
（0→NETWORK；401→AUTH 等）。装配 `LlmRuntime.register_adapter(["deepseek"], ...)`。
**诚实时序说明**：chat_completions_stream 函数与其本地 TCP 单测在同一编辑落地（未先单独红），
但测试跑真实本地 HTTP POST 交换（状态/原始字节/Bearer/stream 全断言）——功能既有事实验证，
非骨架桩。m6_llm 三测独立红（新增模块无实现先缺席→补全绿）。
**证据**：dsh-cli 96 测全绿（+3 m6_llm、+1 既有累计）+ dsh-core 7 测全绿 + workspace 全量
test 绿 + clippy -D warnings 零告警 + check 绿。
**拒绝**：① 在 dsh-core 重复造 SSE parser 或把解码塞进 llm_http（解码归 dsh-llm-deepseek，
不重复）；② 非流式 chat_completions 单发 JSON 再合成 delta payload（偏离端点原生流式契约，
且 tool_calls delta 需额外构造，否）；③ key 写入 WebConfig/代码/日志（安全红线，仅 env，否）。
**预期影响与回滚点**：+dsh-core llm_http stream fn + dsh-cli::m6_llm（新模块）+ dsh-cli 依赖
dsh-llm-deepseek。回滚 = 撤本提交。真实端点冒烟在 M6 后期门控执行（用户提供环境）。
**待办**：step1b serve() 接线（&mut Boot + WebConfig.workspace_root(P2) + enable_agent_loop
flag + server_llm_runtime 装配 + 装配失败诚实降级）→ step2 生命周期 → step3 tick 注入 →
step4 sandbox 投影 → step6 前端最小闭环 → 穿插篮 → M6-ACCEPTANCE。

---

## D-081（M6 编码·step1b）：serve 接线——WebConfig +`&mut Boot` + 真实编排
`assemble_server_runtime` + `dsh web --agent-loop`（验收 #2/#3/#6 生产入口；P2/P3）

**日期**：2026（M6 round 2）。
**触发问题**：step1a 装配工厂核心完成；需接入 `serve()`/`dsh web` 使真实服务器闭环可用。
**设计（自下而上）**：serve 仅被 main.rs:329 调用（`&boot`）→ 改 `&mut Boot` 副作用只在一处；
dispatch_request 收 `&Boot`（`&mut` reborrow 即可）；WebConfig 无 Default、仅 main.rs 构造
（+4 字段只改一处）；`JobRegistryConfig.now: Box<dyn Fn()->i64>`（i64，非 u64）；`M5Host::
assemble -> Result<Self,String>`（bash 缺失即 Err）；`SessionHost.session("default") ->
Rc<Session>` 供 TodoWriteHost/ScheduleHost。
**实现**：WebConfig +`workspace_root: Option<PathBuf>`（P2 缺省 CWD canonicalize）+
`enable_agent_loop: bool`（缺省 false）+ `llm_base_url/llm_model: Option<String>`（env
`DSH_LLM_BASE_URL`/`DSH_LLM_MODEL` → 缺省 https://api.deepseek.com / deepseek-chat）。
serve(enable) 解析 ws_root → `assemble_server_runtime(&host, ws_root, base, model)`（编排：
M4 jobs(now=system_now_ms)/schedule(ScheduleHost)/todo(TodoWriteHost bind "default") +
M5Host::assemble + m6_llm::server_llm_runtime + assemble_server_loop(共享 store)）→
`boot.agent_loop = Some(loop_host)`；装配失败 → `serve` fail-loud（诚实，不默默回退 WASM）。
main.rs：`--agent-loop`/`--workspace-root`/`--llm-base-url`/`--llm-model`  flags + `mut boot`
+ cfg 字段。测试：`assemble_server_runtime` 真装配（M4+M5+deepseek + 共享 store 断言 +
M4+M5 工具面；bash 缺失 → 诚实跳过打印）。
**拒绝**：① enable_agent_loop 缺省 true（默认开启会让无 bash 环境 `dsh web` 启动失败——
默认关闭 + 显式 opt-in，既存 cordis 语义零变化）；② serve 装配失败吞掉继续（默默降级到
WASM/无 loop，违反诚实——fail-loud）。
**证据**：dsh-cli 97 测全绿 + clippy 零告警 + check 绿。回滚 = 撤本提交。
**待办**：step2 M5Host::shutdown + disposer（宿主生命周期清理，验收 #3）→ step3 tick 注入
serve → step4 sandbox 投影 → step6 前端最小闭环 → 穿插篮 → M6-ACCEPTANCE（真实端点门控冒烟）。

---

## D-082（M6 编码·step2）：宿主生命周期清理——`BashJobsBridge::kill_all` + `M5Host::
shutdown` + 装配 disposer（验收 #3：bash bg / terminal 无孤儿）

**日期**：2026（M6 round 3）。
**触发问题**：step1b serve 闭环已接线；需保证宿主 teardown 时不遗留后台 bash 进程/终端
会话（验收 #3「宿主生命周期清理」）。
**自下而上实测定案**：① `BashJobsBridge` 持有 `processes: HashMap<String, Rc<ShellProcess>>`
+ `pump()`（done→settle Killed/Completed→remove）；`ShellProcess::kill()`（树杀）已被
on_cancel 使用——kill_all = 遍历 kill + pump settle。② `TerminalSessionService::dispose()`
（dsh-terminal）已实现（清空 sessions）——直接调用。③ `AgentLoopHost.add_disposer(Rc<dyn
Fn()>)` + 显式 `teardown()`（非 Drop）——需由装配方接线：`assemble_server_loop` 把 `m5` 移入
disposer closure。④ 生产 M5Host::assemble **尚未注册真实 PTY backend**（属后续里程碑）；
测试用 FakeBackend 确定性验证 dispose。
**实现**：`BashJobsBridge::kill_all(&self)`（幂等：kill 树 + pump settle）。
`M5Host::shutdown(&self)`（幂等：bash_jobs.kill_all + terminal.dispose）。`assemble_server_loop`
在 with_store 后 `host.add_disposer(Rc::new(move || m5.shutdown()))`。
**红测（验收 #3）**：真实组装 M5Host（temp root）→ bash `run_in_background` 实起
`sleep 2 && echo DONE > marker`（Running、marker 未写）→ terminal_open(FakeBackend, type
bash) 会话存在 → `shutdown()` → 断言：① bash job settle **Killed**（registry read）；
② **真无孤儿**：等 ≥2.5s marker 仍不出现（进程被树杀而非跑完写 marker）；③ terminal list
**空**（dispose 生效）。bash 缺失 → 诚实 eprintln 跳过。
**诚实边界**：真实 PTY backend 注册属后续里程碑（本步 dispose 语义由 FakeBackend 确定性
验证；生产 PTY 会话真实孤儿解除随 backend 装配落地）。
**证据**：dsh-cli 98 测全绿 + workspace clippy `-D warnings` 零告警 + check 绿；rustfmt
仅 web_m5.rs（--edition 2021）。回滚 = 撤本提交。
**待办**：step3 tick 注入 serve（主循环 recv_timeout 自驱节拍 → 主线程 `m5g_tick_once`；
基准：M5gTick 服务线程不进 serve——DIV-2）→ step4 sandbox 投影 → step6 前端最小闭环 →
穿插篮 → M6-ACCEPTANCE。

---

## D-083（M6 编码·step3）：serve 主循环 tick 注入——`ServerLoopBundle` 共享实例 +
`recv_timeout` 自驱节拍 → 主线程 `m5g_tick_once`（验收 #4；DIV-2）

**日期**：2026（M6 round 3）。
**触发问题**：step1b 已把 loop 写入 boot.agent_loop；调度到期与 bash jobs 结算需在 serve
主循环获得**真实推进点**（进取样在环内不可用），且 serve 主循环现为阻塞
`incoming_requests()`。
**设计定案（DIV-2 落地）**：`ServerLoopBundle{host, schedule, bash_jobs}`——`m4.schedule`/
`m5.services.bash_jobs` 与 bundle 持**同一 Rc 实例**（工具注册与 tick 推进共享状态）。
serve 主循环改 `Server::recv_timeout(M6_SERVE_TICK_INTERVAL_MS=250)`：有请求 → 派发；
超时（无请求）→ 纯 tick；`Err`（服务器关闭）→ break 返回（等价 incoming_requests 结束）。
每轮 tick：`m5g_tick_once(sched, Some(bridge), system_now)`（调度到期 + 合作泵）。推进点
**唯一收敛 serve 主线程**（非 Send 宿主；M5gTick 服务线程不进 serve）。未启用 agent_loop
→ tick 上下文空，循环等价阻塞接收（多 ≤250ms 轮询唤醒）。
**红测（验收 #4 行为探针）**：bundle 装配（真实 M4+M5+deepseek）→ ① 调度门控：工具
`schedule_create {after_seconds:1}` → `tick_once(now)` **不**派发（未到期）→
`tick_once(now+1500)` **派发**（dispatch 含 sched_id + `schedule/change` dispatch 事件落
default 会话 + framing 非空）；② jobs 自动结算：真实后台 bash（写 tick.txt）→
`tick_once` 泵自动 settle **Completed** + 输出落盘（非手工 pump）。工具参数遵循 M4 契约
（snake_case `after_seconds`，≥1 正整数）。
**诚实边界**：schedule 到期注入语义为 `schedule/change` dispatch 事件（production
deliveryMode = session-local）；真实 serve（无限循环）无法集成测，步证明 = 本 bundle 探针
（推进点同一条代码路径）。
**证据**：dsh-cli 99 测全绿 + workspace 全量 test 无失败 + clippy `-D warnings` 零告警 +
check 绿。回滚 = 撤本提交。
**待办**：step4 sandbox·policy 投影（policy 段注入 prompt + resolve_sandbox_mode +
escalation 校验 fail-closed）→ step6 前端最小闭环 → 穿插篮（settings/.env、provider caps、
hooks/skill、ts-host diff/SQLite）→ M6-ACCEPTANCE（真实端点门控冒烟）。

---

## D-084（M6 编码·step4）：sandbox:policy 投影——动态段（order 110）注册进 loop
SystemPrompt（验收 #5）

**日期**：2026（M6 round 3）。
**触发问题**：step3 后 serve 执行闭环具备推进点；沙箱语义（有效模式 + 可写根）需投影进
agent 的 system prompt（M5-DESIGN §3.3/§8；验收 #5）。
**自下而上实测定案**：① dsh-system-prompt `PromptSectionText::Fn(Rc<dyn Fn(&AssembleContext)
-> String>)` 是每装配求值的动态段 provider——无需改 dsh-agent-loop/dsh-system-prompt；
② `SessionStore::get(&SessionId)` + `Session::events()` 提供共享 store 会话事件读；
③ `resolve_sandbox_mode(None, events)` + `sandbox_policy_segment(mode, root)`（D-070 已有）
直接复用，approved 缝缺省无通道 → fail-closed（会话折叠/read-only 默认，不伪造批准来源）。
**实现**：`web_m5::register_sandbox_policy_section(prompt, store, default_session, ws_root)`
——注册 `sandbox:policy`（`SANDBOX_POLICY_ORDER = 110.0`，100–199 工具指引带）Fn provider：
每次装配读 default 会话事件 → `resolve_sandbox_mode(None, …)` → 段文本含
`effective mode` + 可写根名单（workspace-write 产名单，read-only 为 none）。
`assemble_server_loop` 在 with_store 后接线（传 default 会话 + ws_root）。
**红测（验收 #5）**：离核 prompt（真实 SystemPrompt）装配：缺省 → `read-only` +
`writable roots: (none …)`；会话 `sandbox/mode` **workspace-write** 事件 → 重装配 → 投影
写模式 + workspace root 入写根名单；垃圾 mode 事件 → **忽略**（不留未知文本，上一个有效
模式保留）。**踩坑**：`with_default_session()` mint 的会话 id 是 `session-N` 而非
`default`——helper 显式接受 session_id（装配传实际默认 agent 会话），测试显式创建
`default` 会话。
**诚实边界**：escalation fail-closed（工具传 sandbox_permissions 且无审批通道拒绝）属
approval 缝语义，本步在 resolve 面（approved=None → 不进会话折叠）已覆盖；真实审批通道
启用不属 M6 范围。
**证据**：dsh-cli 100 测全绿 + workspace clippy `-D warnings` 零告警 + check 绿；rustfmt
仅 web_m5.rs（--edition 2021）。回滚 = 撤本提交。
**待办**：step6 前端最小闭环（复用 session/event downlink + history；RPC 集成测 + 门控真实
端点冒烟）→ 穿插篮（settings/.env、provider caps、hooks/skill、ts-host diff/SQLite）→
M6-ACCEPTANCE。剩余真实 LLM 冒烟需用户侧 `DEEPSEEK_API_KEY` 环境变量
（base http://100.105.152.101:18080/v1、model deepseek-v4-flash-0731-ext）。

---

## D-085（M6 编码·step6）：前端最小闭环——完整 serve 装配路径驱动 `session.prompt` +
注入缝 + 无 key fail-loud + 门控真实端冒烟（验收 #6）

**日期**：2026（M6 round 4）。
**触发问题**：step1-5 已把 loop/tick/sandbox 装配完整；前端最小闭环需证明「完整装配路径」
（非手工构造宿主）下 session.prompt 经前端 RPC 驱动 loop、事件经共享 store 下链/回读。
**自下而上实测定案**：① 既有 `run_rust_loop`→`host.followup`→RPC `session.prompt` 已接线
（M2g 手工宿主测覆盖）；**缺口** = 完整装配路径 + 可注入 LLM 缝 + 无 key fail-loud RPC 语义 +
真实端门控冒烟。② loop 对 LLM 错误：stream Err / error-finish chunk → `Halt::Failed` →
turn/end reason**error** + 事件落 store，**不伪造 assistant/message**（agent.rs 结构化收尾）。
③ 无 key 时 deepseek 适配器把 resolver Err 转成 `assistant/chunk` finish-error
（code AUTH + `missing DEEPSEEK_API_KEY: set it to enable agent turns, then retry`）+ turn/end
error——诚实表面（fail-loud 可操作消息在事件里，无已完成伪装）。
**实现（注入缝）**：抽 `assemble_server_runtime_with_llm(host, ws_root, llm, provider, model)`
（完整装配路径，mock/显式 no-key/真实 key 共用）；`assemble_server_runtime`（生产）=
它 + `server_llm_runtime`（key 仅 env）+ provider "deepseek" 的便捷包装——serve 与测试同
代码路径。
**红测 / 绿**：**6a**（mock LLM + 完整装配 + `session.prompt`）→ accepted:true + 共享 store
user/message+assistant/message+turn/end + EventSink sink≥4（前端实时帧）+ session.history
回读长度一致；**6b**（`server_llm_runtime_with_key(_,_,None)` 确定性无 key）→ 无伪造
assistant/message + user/message 记录输入 + turn/end reason.error 含 `AUTH` 与
`DEEPSEEK_API_KEY` 字面量（P3；工具/API 面照常）；**6c**（**门控真实端冒烟**）→ 仅当
`DEEPSEEK_API_KEY` 存在：真实 `assemble_server_runtime` 驱动一轮，真实 assistant 文本落
store + 下链；key 缺失/端不可达/网络错 → 诚实 `GATED-SMOKE-SKIP`（不失败、不伪造；key
永不落盘/入 git，P4）。**踩坑**：AUTH 失败走 `assistant/chunk` finish-error + turn/end error
（无 `agent/error` 事件）——断言对准 turn/end（比 agent/error 稳定）。**副产品**：request/header
暴露真实 system prompt 含 `sandbox: policy — effective mode read-only`（step4 在完整装配
路径交叉确认）。
**证据**：dsh-cli 103 测全绿 + workspace clippy `-D warnings` 零告警 + check 绿。回滚 =
撤本提交。
**待办**：穿插篮（settings/.env 装配、provider caps、hooks/skill、ts-host diff/SQLite）→
step11 M6-ACCEPTANCE（含真实端门控冒烟：需用户侧 `DEEPSEEK_API_KEY` +
base http://100.105.152.101:18080/v1 + model deepseek-v4-flash-0731-ext）。主线 step1-6 已
完成（装配/生命周期/tick/sandbox 投影/前端闭环）。

---

## D-086（M6 编码·step7 穿插篮）：`.env` 解析 + 键注入 server 装配

**日期**：2026（M6 round 5）。
**触发问题**：serve 装配参数（LLM base/model/workspace、agent-loop）只能命令行/进程 env
单点注入；需 `.env` 文件作为进程环境的**上游可选来源**（M6-DESIGN step7）。
**第一性原理裁剪**：design 写「settings YAML 注释保真 leaf-diff + `.env` 解析」。自下而上
实测：dsh-settings 是内存/文件 JSON provider（无 YAML 引擎）；YAML 注释保真 leaf-diff 属
TS-host 侧 settings 文档面（M6 非目标不建前端/不引 YAML 引擎）→ **显式 defer**（记录，
不静默缩水）；本步落实 Rust 侧可验证的 `.env` 解析 + 装配注入。
**实现**：`crates/dsh-cli/src/m6_env.rs`（新）：`parse_env_file`（纯：空白行/`#` 注释跳过；
`KEY=VALUE` 两侧空白容忍；单/双引号剥除；CRLF；fail-loud——缺 `=`/空键 → Err 含行号+现场；
无插值/内联注释，文档化为 dotenv 子集）、`load_env_file(path)`（读盘 + 解析，io 错含路径）、
`apply_env_into_process`（**overwrite:false**——既有环境变量优先）、`apply_env_file(None|Some)`。
接线：`WebConfig.env_file: Option<PathBuf>`；`serve()` 顶部先 apply（fail-loud，打印 applied
条数，不打印值）；`main.rs --env-file <path>`。
**IV-3 落位**：`.env` 仅为进程环境上游——键（含 `DEEPSEEK_API_KEY`）apply 后仍由
`server_llm_runtime` 以 env 读取；**永不落 settings/库/git**（P4）。
**红测（绿）**：解析基础/注释/空白/引号/CRLF/空值；坏行 Err 含行号与现场；读盘；apply
overwrite:false（既有 env 胜，独特测试键避免并行污染）。4 测绿。
**诚实边界**：YAML 注释保真 leaf-diff 显式 defer（TS settings 文档面，非 M6 建面）；
`.env` 不自动加载（显式 `--env-file` opt-in，不意外吞 cwd .env）。
**证据**：dsh-cli 107 测全绿 + workspace clippy `-D warnings` 零告警 + check 绿。回滚 =
撤本提交。
**待办**：step8 provider caps 做实（provider/models RPC 从真实 `DeepSeekConnection.models`
catalog 列录：容量/重试/模式）→ step9 hooks/skill → step10 ts-host diff/SQLite →
step11 M6-ACCEPTANCE（真实端门控冒烟需用户侧 `DEEPSEEK_API_KEY`）。

---

## D-087（M6 编码·step8 穿插篮）：provider caps 做实——llm.models 从真实 catalog 列录

**日期**：2026（M6 round 6）。
**触发问题**：`llm.models` RPC 此前由 `Boot.llm`（WASM-rt）注册表驱动，只有模型名数组；
装配 loop 的真实 `DeepSeekConnection.models` catalog（容量/重试/模式）未上 RPC。
**自下而上实测定案**：① caps 真实数据位于 m6_llm 装配的 `DeepSeekConnection`（models 目录 +
default_context_window/max_tokens + retry_policy），适配器闭包内、无暴露缝——由 serve「装配即
注入」最简洁（base/model 在 serve 局部）而非反向从 AgentLoopHost 里挖。② `ResolvedRetryPolicy`
无 Serialize derive → 手构 retry 视图（mode + maxRetries/retryableCodes + backoff）。③
`RequestDefaults` 字段为 `thinking: Option<Thinking>`/`reasoning_effort: Option<Effort>`（无
Display）→ `{:?}` 小写渲染。④ wire 形状保持：groups 只含 `{id,name,models:[{id,name}]}`（既有
schema consumer 兼容），容量/重试走 `llm.models` value 的**增量 `caps`** 字段（不破坏既存
groups 消费方）。
**实现**：`m6_llm::server_catalog_view(base, model)` → `{provider, models[含 contextWindow/
maxTokens/inputModalities 精确项], defaults{contextWindow,maxTokens,thinking,reasoningEffort},
retry{mode,…}}`（真实值，缺省以 defaults 为准不伪造）。`Boot.agent_catalog: Option<Value>`
（None 默认；boot()/boot_with_sessions 各 +None）；serve 装配成功后
`boot.agent_catalog = Some(server_catalog_view(...))`；`llm_catalog` 有 catalog 时优先列真实
groups（回退逻辑保留）；`llm.models` value += `caps`。
**红测（绿）**：`server_catalog_view_lists_real_deepseek_caps`（provider/models 含装配模型 +
defaults 容量 >0 + retry.mode 在）；`llm_models_reflects_assembled_catalog_caps`（groups 真实
provider+模型 wire 形状 + caps 容量/重试真实；handle_rpc 全链路）。2 测绿。
**诚实边界**：目录只含装配 catalog（`DeepSeekCatalogModel::new` 无精确容量 → 该模型条目
省略 contextWindow/maxTokens，走 defaults——不伪造精确值）；前端可发现的真实模型列表取决于
catalog 装配（本实现含装配模型 + defaults 语义）。
**证据**：dsh-cli 109 测全绿 + workspace clippy `-D warnings` 零告警 + check 绿。回滚 = 撤本
提交。
**待办**：step9 hooks/skill（hooks=pre/post-execute 宿主钩子（dsh-tools pre-decision 缝延伸）；
skill=system-prompt 段注册）→ step10 ts-host diff/SQLite → step11 M6-ACCEPTANCE（真实端门控
冒烟需用户侧 `DEEPSEEK_API_KEY`）。

---

## D-088（M6 编码·step9 穿插篮）：hooks 宿主钩子 + skill 通用段注册

**日期**：2026（M6 round 7）。
**触发问题**：design step9 =「hooks=pre/post-execute 宿主钩子（dsh-tools 既有 pre-decision 缝
延伸）+ skill=system-prompt 段注册」，M6-DESIGN 只给了概要，需定最小可验证核心。
**自下而上实测定案**：
- dsh-tools 已有真实 `add_pre_decision(ToolPreDecision, scope)` 缝，`PreToolDecision`
  {Allow, Deny{reason}, Ask{reason}}，None=放行、首个非 None 最终裁决——pre-execute 钩子
  = 直接延伸该缝（不新造钩子面）。post-execute 无独立缝（tool/result 由 loop 落
  `tool/result` 事件，TS `tools/post-execute` waterfall 对偶）→ **post 面不重复实现**
  （诚实边界，记录）。
- dsh-session `EventKind::HookInvoked`（+HookResult）已存在且对齐 TS——记录面用现成 kind。
- skill 段注册 = step4（sandbox:policy）同一 `SystemPrompt::section` 缝的通用化。
- 发现：standalone `ToolRegistry` 无真实 bash 工具先于 pre-decision 报 UNKNOWN_TOOL →
  钩子测试做**装配级**（assemble 已注册 bash；`host.tools` 可叠加否决钩子），比 standalone
  更能证「宿主钩子做实」。
**实现**：
- `web_m5::register_prompt_section(prompt, name, order, text)`（skill 通用段；Static；重名 Err）。
- `web_m5::register_pre_execute_hook(registry, session, veto: Rc<HostVeto>)`：每次工具执行前
  记录 `hookInvoked`{tool,callId,agent} 进会话 + 按 veto 裁决（Some(reason) → Deny；None 放行）。
- `web_m5::wire_recording_pre_execute_hook(registry, session)`：记录+放行，接进
  `assemble_server_loop`（共享 default 会话；工具每 call 落 hookInvoked）。
**红测（绿）**：`skill_prompt_section_registers_generic`（静态段组装可见 + 重名 Err）；
`m6_loop_turn_host_pre_execute_veto_denies_bash`（mock LLM 一轮请求 bash → 装配+否决钩子
真实触发：hookInvoked(tool=bash) 落共享 store + 拒绝原因上抛事件流；turn 完成不崩溃）。2 测绿。
**数据契约**：钩子记录用现成 `EventKind::HookInvoked`；tool 名/callId/agent 进 data。
**证据**：dsh-cli 111 测全绿 + workspace clippy `-D warnings` 零告警 + check 绿 + rustfmt
web_m5.rs。回滚 = 撤本提交。
**待办**：step10 见 D-089（范围外声明）；step11 M6-ACCEPTANCE。

---

## D-089（M6 编码·step10 穿插篮）：ts-host diff / SQLite —— **显式范围外声明**

**日期**：2026（M6 round 7）。
**触发问题**：M6-DESIGN §7 篮子剩余「step10 ts-host diff / SQLite」（dsh-diff 差分对 TS-host
部署产物 + SQLite 落盘/回读）。
**第一性原理判断（诚实取舍，报告用户、不静默缩水）**：
- M6 的根本目标（D-079）是 **harness/serve 服务器执行闭环（serve 接线）**——主轴已闭环：
  装配（step1a/b）、生命周期（step2）、tick（step3）、sandbox 投影（step4）、LLM 桥（step5）、
  前端闭环（step6）、basket：.env（step7）、provider caps（step8）、hooks/skill（step9）。
- `ts-host diff`：dsh-diff 对 **TS 宿主部署产物** 的差分校验——是另一条交付线（TS host
  部署工位）的验证面，不属于 serve 接线本身；M6 无 TS 新面（无前端/无新引擎）。
- `SQLite 落盘/回读`：会话持久化已由 `session_dir` 既有 flush 落盘语义覆盖；无独立持久化
  需求触发此面（M6 是 serve 接线，不新建立持久化引擎）。
- 两者均非「其余篮子依赖的前提」，砍掉不阻塞已交付闭环 → **显式范围外，记录待办池**
  （未来 M7+ 若需 TS 差分验证或独立会话库再规划）。
**证据**：主线 + 篮子 step7/8/9 全部以测试/提交/D-079..D-088 收敛；此声明不改变任何已交付
工件。回滚 = 无需代码回滚（纯范围记录）。
**待办**：step11 M6-ACCEPTANCE（全量 test + clippy + DECISIONS 互查 + git + 冒烟报告；
真实端门控冒烟需用户侧 `DEEPSEEK_API_KEY`，缺失 → 诚实 skipped 不阻塞验收）。

---

## M6-ACCEPTANCE（M6 编码·step11 验收记录）

**日期**：2026（M6 round 7）。
**通过条件**：全量 test 全绿 + clippy `-D warnings` 零告警 + DECISIONS 与 git 互查 +
真实端门控冒烟（key 缺失 → 诚实 skipped，不阻塞）。
**验收证据（决策↔提交互查链，D-077 → D-089）**：
- 88a327b（D-077 需求网关 M6-REQUIREMENTS）→ 6f064f2（D-078 设计网关 M6-DESIGN）
- 53b23a8（D-079 step1a 装配工厂）→ 6a74b76（D-080 step5 LLM 桥）
- 01425f8（D-081 step1b serve 接线；P2 workspace_root=WebConfig）→ 7ddd373（D-082 step2
  生命周期无孤儿）→ 1384f62（D-083 step3 tick 单推进点）→ 9df5db4（D-084 step4 sandbox
  投影）→ 158182d（D-085 step6 前端闭环；P3 无 key fail-loud AUTH、P4 key 仅 env）
- d832746（D-086 step7 .env）→ aded4c5（D-087 step8 provider caps）→ 801b358（D-088 step9
  hooks/skill）→（D-089 step10 显式范围外声明，本提交收纳）
**最终测试面**：dsh-cli 111 测全绿（含 step9 装配级否决钩子端到端）；workspace 全目标
无失败；clippy `-D warnings` 零告警；check 全绿；rustfmt 仅 web_m5.rs。
**门控冒烟**：`serve_closure_real_endpoint_smoke_gated` 当前环境无 `DEEPSEEK_API_KEY` →
GATED-SMOKE-SKIP（诚实记录，不伪造、不失败）。用户设 key 后重跑该单向真实端即可补验证
（base http://100.105.152.101:18080/v1, model deepseek-v4-flash-0731-ext；key 永不落盘/git）。
**诚实边界清单**：settings YAML leaf-diff（D-086，TS 侧既有面）；post-execute 独立缝
（D-088，tool/result 已覆盖）；step10 ts-host diff/SQLite（D-089 范围外）；真实 PTY backend
注册与 approval 通道（D-082/D-084 延期记录）。
**结论**：M6「harness/serve 服务器执行闭环（serve 接线）」达成。回滚 = 按 D-* 逐提交回退。

---

## D-090（step10 复活·ts-host 差分）：session-host.mjs 编排 + 差分对齐测试

**日期**：2026（M6 后续轮，用户裁定复活 D-089 范围外项）。
**触发问题**：`ts-host diff` 复活——M6-REQ step10「ts-host 差分编排（M5R §5 ⑤）」；用户选定
范围 =「补 session-host.mjs 差分编排 + 对齐测试」。
**自下而上实测定案**：
- 已有差分基建：`diff/ts-host` 的 scenario/loader/include-host + `verify-diff.mjs`（TS stdout →
  `.golden` → `dsh-diff --golden` 逐字节校验）+ 16 场景全绿；Rust `dsh-diff` `Step` 为
  kebab-case tagged 枚举，`Runner` 懒加载子毛（loader/include 先例）。
- 会话事件 diff 的契约约束：dsh-session 事件 **seq=log 长度**（首 append=0）+ **surface-eligible
  事件（user/message、assistant/message、tool/result）必须携带 `SurfaceIntent{Append}`**
  （append 带 None 会 fail-loud）；序化对齐 = **canonical（键字典序）** JSON（serde_json 默认
  BTreeMap 序）；数值限定整数（serde_json/JSON.stringify 浮点格式不对称，不做对齐面）。
- 真实度选择：Rust 侧用 **dsh-session 真实 `SessionStore`**（权威事件引擎）；TS host 手写镜像
  会话事件契约（无 @deepseek-ai session 生产包可 vendored——真实面在 Rust 侧）。
**实现**：
- `crates/dsh-diff`：`dsh-session` 依赖 + `Step::SessionCreate/Append/Events`（surface 字段驱动
  SurfaceIntent；双向 fail-loud 守卫：surface-eligible 必须带 marker、非 surface 禁带——两侧
  对称）+ `Runner.sessions/session_store` + 同步步骤执行；`sorted_json`（已有）做 readback 序列化。
- `diff/ts-host/session-host.mjs`：镜像同契约；`canonicalStringify` 递归排序键（对齐 BTreeMap）；
  整数限定 + surface 双向校验。
- `scenarios/session-01-simple.json` + `verify-diff.mjs` 路由 `session-` → session-host。
**红测（绿）**：`session_scenario_trace_aligns_contract`（内联契约 trace；红=无 session 变体
解析失败→绿）。真实差分：`node verify-diff.mjs` **17 场景 ALL PASS**（新 session-01 7 行 golden
由 TS 生成、Rust 逐字节对齐 + 既有 16 场景零回归）。
**诚实边界**：TS 侧无 session 生产包可 vendored → session-host 为契约镜像（Rust dsh-session
为权威）；数值整数限定；surface marker 不进 trace（操作前提，非被测产出）。
**证据**：dsh-diff 单测全绿 + clippy `-D warnings` 零告警 + workspace check 绿 + verify-diff 17
场景全绿。回滚 = 撤本提交。
**待办**：SQLite 后端（D-089 复活的另一半）→ M6-ACCEPTANCE 复跑。

---

## D-091（step10 复活·SQLite 后端）：PersistenceBackend 缝上的 SQLite 落盘/回读

**日期**：2026（M6 后续轮，用户裁定复活 D-089 范围外项 + M5R 项5「Q6 裁决：M5 用 JSONL 过渡」）。
**触发问题**：SQLite backlog 落地——M6-REQ step10「SQLite 落盘/回读（持久化面）」。既有
JSONL 后端 + `PersistenceCoordinator`（`SessionPersistence` seam 消费 `PersistenceBackend`）。
**依赖引入评估（方法论四）**：rusqlite 0.40.2（`bundled`，内编 SQLite3）——成熟/活跃/MIT-
Apache-2.0、栈（Rust）契合、`bundled` 免系统 sqlite 依赖；网络经 rsproxy 镜像可用（离线缓存
原无 rusqlite → 显式拉取成功，非静默绕过）。
**自下而上实测定案**：
- rusqlite 0.40 `transaction(&mut self)` → `PersistenceBackend` 仅 `&self` → 单线程纪律
  （D-006）下 `RefCell<Connection>` 内部可变性（与 coordinator 持有 backend 的方式一致）。
- `SessionEvent` 有完整 serde（type/seq/time/data/surfaceOp/sourceEventSeqs/ignorable）→
  事件以 full-JSON 落盘至 `events(id, seq, json)`；`SessionHeader` camelCase serde → `sessions
  (id, header, revision)`。surface 字段保真（访问器 `surface_op()/source_event_seqs()`）。
- revision = 会话写入计数 `sqlite:<path>:<rev>`（写入间变更即可被观察）。
- **事务原子 → 无 torn 尾**：与 JSONL 物理 torn 语义差异如实记录（后端按契约返回
  `torn:false` + `truncate_offset:None`）；`commit_repair` 的 torn_offset 以 **seq 阈**表达
  截断面（JSONL 是字节偏移——差异如实记录，不假装等价）。
**实现**：`crates/dsh-persistence/src/sqlite.rs`（`SqliteBackend: PersistenceBackend`：
locate/supports_raw=false/read_raw=None/load_stored/read_stored_revision/append_batch
（续接校验+未物化兜底）/materialize_batch（重复拒绝）/commit_repair/list_snapshots）+ lib.rs
模块导出 + rusqlite(bundled) 依赖。
**红测（绿，7 测）**：materialize+append+load 往返（header/事件/kinds/seq/torn=false/locate/
raw 契约/list_snapshots）；**跨 reopen 持久**（真实文件落盘/回读）；重复 materialize + seq 缺口
fail-loud；revision 写入间变更；commit_repair 截断+closing；**surface 字段保真**；coordinator
无缝（create/append/load/read_from/list）。红=SqliteBackend::open 缺失（E0599）。
**诚实边界**：SessionHost/`dsh web` 的 SQLite 接线未做（`SessionHost::with_root` 走 JSONL；
后端已与 `SessionPersistence` seam 即插兼容，web 接线是后续集成面——记录不静默）；torn/revision
语义与 JSONL 的差异如实记录。
**证据**：dsh-persistence 全测绿（JSONL 既有面零回归）+ clippy `-D warnings` 零告警 +
workspace check 绿。回滚 = 撤本提交（含 Cargo.lock）。
**待办**：M6-ACCEPTANCE 复跑（含真实 API 冒烟 GATED-SMOKE-OK 证据 + 17 场景差分）。

---

## M6-ACCEPTANCE（复活轮复跑：真实 API + step10 收口）

**日期**：2026（M6 复活轮，用户指令：真实 key 冒烟 + 实现 ts-host diff / SQLite）。
**真实 API 冒烟（P4 纪律：key 仅进程环境注入，两次运行后已清除，永不落盘/入 git）**：
- `serve_closure_real_endpoint_smoke_gated` 两次执行均 **GATED-SMOKE-OK: real turn replied "OK"**——
  完整 serve 装配（真实 M4+M5 工具 + deepseek LLM，key 仅 env）+ `session.prompt` RPC →
  真实 DeepSeek 端点（base http://100.105.152.101:18080/v1, model deepseek-v4-flash-0731-ext）
  回复落共享 store（assistant/message 非空）+ EventSink downlink ≥4 + 无 AUTH/NETWORK 失败。
- 说明：你提供的 key 在本会话仅作两次测试进程的临时环境变量，注入后即 remove（未写入任何文件/
  DECISIONS/git history；仓库无 key 痕迹）。
**step10 收口（复活 D-089 范围外项，TDD）**：
- D-090 ts-host 差分：`diff/ts-host/session-host.mjs` + Rust dsh-diff session 步骤（真实
  dsh-session store）+ `scenarios/session-01-simple(.json/.golden)`；`node verify-diff.mjs`
  **17 场景 ALL PASS**（新 session-01 7 行逐字节对齐 + 16 场景零回归）。
- D-091 SQLite：`PersistenceBackend` 缝上 `SqliteBackend`（rusqlite bundled，事务原子落盘/回读），
  7 测绿（含跨 reopen 持久/surface 保真/repair/coordinator 无缝）。
**最终状态（workspace 复跑）**：全量 test 零失败 + clippy `-D warnings` 零告警 + check 绿 +
dsh-diff 单测绿；提交链 D-077→D-091 与 `88a327b`..`d91688d` 互查无断链。
**诚实边界更新**：D-089 原「范围外」裁定经用户复活——step10 已交付；SessionHost/`dsh web`
的 SQLite 接线（后端 seam 即插兼容）与 TS session 生产包 vendored（真实面在 Rust dsh-session）
仍为记录在案的后续面，不作静默宣称。
**结论**：M6 全部达成（主线 + 穿插篮 step7-9 + 复活 step10 + 真实 API 闭环证据）。回滚 =
按 D-* 逐提交回退。

---

## D-092（M6W：SQLite 接入 dsh web）：config 面 + with_sqlite + materialize 契约修正

**日期**：2026（用户指令：按瀑布流完成 SQLite 接进 dsh web 完整开发）。
**触发问题**：dsh web 的会话持久化只有 JSONL（`--session-dir`）；D-091 SqliteBackend 已
backend 级验证但未接线。需求分析（M6W-REQUIREMENTS）双视角校验时**越级发现**：seam.rs
文档「重复 materialize 拒绝」从未被 JSONL 执行（JSONL materialize = `write_tmp_then_publish`
原子覆盖 create-or-replace）；而 `SessionHost::restore_one` 恢复后 `coord.append(id,&full)`
重灌游标、首 append 走 materialize_batch。D-091 SQLite「重复拒绝」会导致恢复后游标错位、
**该会话后续 append 永久失败**。回到早期工件修正，不静默打补丁。
**考虑过的选项**：
- (A) 在 restore_one 特判 SQLite（跳过回灌）——被否决：治标，隐藏后端契约裂痕，两条路径行为漂移；
- (B) SQLite materialize 改 create-or-replace（镜像 JSONL 原子覆盖）——**采纳**：单一语义，
  恢复重灌幂等，coordinator/restore 全链路不变；同步修 seam.rs 文档措辞与 D-091 测试。
**config 面决策**：(C) 独立 flag `--sqlite-store <file>`，另议 (D) 复用 `--session-dir`
加 `sqlite:` scheme 前缀——被否决（scheme 与路径歧义、污染既有语义）；(E) 统一 flag
`--persist <mode>:<path>`——被否决（破坏既有 flag）。采纳 C：`WebConfig.sqlite_store`，
优先级 sqlite > jsonl > 内存；同给 → `eprintln!` 显式警告（fail-loud，绝不清零静默）。
**实现要点**（设计 M6W-DESIGN）：`SessionHost::with_sqlite(path)`（父目录 create_dir_all +
`SqliteBackend::open` fail-loud + coordinator + 观察者 + restore_all）；观察者接线提取为
`new_from_backend(Option<Box<dyn PersistenceBackend>>)` 单一来源；serve 提 `session_host_for(cfg)`
选择主机 + 冲突警告；`--sqlite-store` 解析入 WebConfig；诊断 `persistence_kind()`
（"mem"/"jsonl"/"sqlite"）供测试断言。
**预期影响**：web 可得事务性单文件后端；JSONL/内存行为零回归；SQLite 无 per-session
artifact 差异保留。
**回滚点**：撤本提交（及 D-091 修正）即回既有 JSONL/内存；删 db 文件回全新存储。
**证据**（A1–A6）：with_sqlite 冷重启恢复（7 事件含 end-seed）、恢复后 adopt seq 连续
（13 事件/seq 12）、优先级（sqlite 落盘 jsonl 根空）、materialize 幂等覆盖、
workspace 全量 test 绿 + clippy `-D warnings` 零告警 + check 绿。

---

## M6W-ACCEPTANCE（SQLite 接入 dsh web 验收）

**需求→设计→编码→测试→部署链**：M6W-REQUIREMENTS（阶段①，A1–A6 验收）→
M6W-DESIGN（阶段②，组件/路径/测试矩阵/部署回滚）→ D-091 契约修正 + D-092 实现
（阶段③④，TDD 红→绿）→ 本验收（阶段⑤）。
**越级处理记录**：需求分析双视角发现 seam 文档「拒绝」与 JSONL 实际（原子覆盖）不符、
D-091 重复拒绝会破坏 SessionHost 恢复回灌游标——**显式回到早期工件修正**（sqlite.rs
materialize → create-or-replace + seam.rs 措辞 + D-091 测试改向），非当前阶段打补丁。
**交付**：`--sqlite-store <file>`（main.rs 解析 → WebConfig.sqlite_store）；serve
`session_host_for` 优先级 sqlite > jsonl > 内存 + 冲突 `eprintln!` 显式警告（fail-loud，
绝不清零）；`SessionHost::with_sqlite(path)->Result`（父目录 create_dir_all、open
fail-loud、观察者单一来源 `new_from_backend`、restore_all）；诊断 `persistence_kind()`。
**验证证据**：
- A1/A2：`with_sqlite` 落盘 → 冷重启同文件 → 恢复快照（7 事件含 end-seed）；恢复后
  adopt → seq 12 连续（无游标错位，materialize 幂等回灌成立）。
- A3：sqlite+jsonl 同给 → sqlite 生效（写落 sqlite 文件、jsonl 根零 artifact）；仅
  jsonl → "jsonl"；仅内存 → "mem"。
- A4：sqlite materialize create-or-replace 幂等覆盖（header+事件替换，非 Err）+ seq 缺口
  仍 reject。
- 回归：dsh-persistence 14 测绿、dsh-cli session_host 15 测绿、workspace 全量 test 零失败、
  clippy `-D warnings` 零告警、check 绿、新代码 rustfmt canonical（既有 fmt 漂移为基线，
  未越权重排无关文件——如实归档）。
- 部署探针：`dsh web --sqlite-store <file> <cfg>` 参数解析被接受（非 unknown-arg），随后
  进 boot（缺 config 失败为预期——与 JSONL 同路径）。
**部署**：`dsh web <cordis.yml> [--agent-loop …] --sqlite-store <file>`；单文件便于备份。
**回滚**：撤 D-091 改 + D-092 提交即回 JSONL/内存；删 db 回全新存储；事务无 torn。
**结论**：SQLite 接入 dsh web 完整功能开发达成，各阶段工件可验收、决策链
D-089→D-092 与提交 `2e23c85`/`1655cdc` 互查。

---

## D-093（真实端点 agent 冒烟）：AgentLoopHost 从不把工具 schema 发给 LLM（装配缺口）

**日期**：2026（用户指令：真实 API 测模型响应能力 + 测 agent 能否工作）。
**触发问题**：门控真实端点测试 `serve_closure_real_endpoint_model_capability_and_agent_gated`
初测：能力轮模型**精确遵循**（"Reply with ONLY the integer 156" → 回复正好 `"156"`，
exact_156=true）——模型响应能力无问题；但 agent 轮强制要求调用 `todo_write` 时，事件窗口
只有 `assistant/chunk+message → turn/end`，**无 tool/call/tool/result**。
**根因分析（诚实黑盒，层层排除，不伪造）**：
- 直连 chat 探针（带 `tools` 载荷 + MUST-call 提示）：端点/模型**完全支持**工具调用——返回
  `message.tool_calls:[todo_write, {todos:[...]}]` + `finish_reason:"tool_calls"`；
- 流式探针（`stream:true`）：SSE 正确发出 `delta.tool_calls[{function:{name, arguments}}]`
  + `finish_reason:"tool_calls"`——恰是 `dsh-llm-deepseek` translate 单测覆盖的形状；
- 定位装配缺口：`dsh-agent-loop` 生产代码**从未把 `ToolRegistry` 注册为 system-prompt 工具
  provider**（`sp.tools()` 仅 dsh-system-prompt 自测用过）→ `assembly.tools` 恒空 →
  `GenerateOptions.tools=None` → 真实请求**不带 `tools` 参数** → 模型看不到任何工具定义、
  无从发起 tool call。既有 mock 测试只证明「能执行已发出的 tool/call」，从不证明「tools
  被发给模型」——真实冒烟补齐了这一盲区。
**最终选择**：修在架构正确位——`dsh-agent-loop/src/host.rs::with_store` 一次性注册
`ToolProvider`（host 级恰一次；provider 按组装 `AssembleContext.scope` 投影 registry
`tools.schemas(ctx.scope)` + `known_names`，与 dsh-tools 作用域/restrict 语义一致）。
**被否决**：仅在 web.rs 装配处补注册（治标，其他 AgentLoopHost 消费方仍缺）；在每 agent
注册（重复/时序风险）。
**TDD 证据**：
- 红：`agent_loop_request_carries_registry_tools_to_llm`（捕获适配器断言请求 tools 含
  todo_write）——修前 tools=None 失败；
- 绿：修后请求 tools 含 todo_write + M4/M5（read/glob/bash 等）；幂等测
  `agent_loop_tool_schemas_registered_once_and_idempotent`（两次 followup 无重复 schema）；
- **真实端点复测**：能力轮 `"156"` exact；agent 轮完整闭环
  `tool/call → hook/invoked → todo/write → tool/result → 续轮 assistant → 干净 turn/end`，
  `todo/write` 数据 `{"todos":[{"content":"dsh real agent verification","status":"in_progress"}]}`
  精确落库；test passed；
- 回归：dsh-agent-loop 全量 + dsh-cli lib 全量 + workspace 全量 test 零失败，clippy
  `-D warnings` 零告警，check 绿。
**诚实边界**：初测软提示词下模型选择文本不调工具（属模型行为，非缺陷）；`final_closing_text`
可极简（"Done."，不回显 todo 文案）——工具已如实记录，循环正常关闭；本次结论基于
deepseek-v4-flash-0731-ext 单端点实测，跨模型提示敏感性如实不入断言。key 仅进程环境
注入测试后清除，未落盘/入 git（P4）；探针 body 文件放 target/ 已删除。
**回滚**：撤本提交即回「不发送 tools」旧态（并保持 mock-执行路径不受影响）。
**预期影响**：真实模型现可发起工具调用；所有 AgentLoopHost 消费方（serve agent-loop/
headless）受益。

---

## D-094（真实 agent 可用性实测：把仓库交给 agent 跑非破坏任务）

**日期**：2026（用户指令：把本仓库地址交给 agent，分配非破坏真实任务（迁移完整性分析 /
写文档）实测可用性）。
**触发问题**：工具闭环已通（D-093），但 agent 的「真实可用性」尚未在真实端点 + 真实仓库
背景下实测——即「给它真实任务它能否自己用工具完成」。
**方法**：新增门控测试 `serve_closure_real_endpoint_agent_nondestructive_repo_task_gated`：
工作区根 = **本仓库根**（找含 DECISIONS.md+Cargo.toml 的祖先目录），装配完整 serve runtime
（真实 M4+M5 + deepseek），把仓库路径交给 agent，要求：只读调查（read/glob/grep/只读 bash），
分析 SQLite 后端接入 `dsh web` 的迁移完整性（D-089..D-093 + M6W 文档 + 实现文件），把报告
**只写到 gitignored** `target/agent-verification/migration-completeness.md`（测试预建目录——
`write` 工具不自动建父目录），完成后一行总结。
**实测结果（真实端点 deepseek-v4-flash-0731-ext）**：agent 自主调查 23 次工具调用
（23 tool/result），事件窗口 559 条；写出 8.7KB markdown 报告（含 SQLite 迁移完整性结论 +
证据）；`git status --porcelain` 全程干净（**未改动任何 tracked 文件**，非破坏 ✅）。
**断言**：干净 turn/end；read/glob/grep/bash 至少一次 tool/call（tool/call 载荷是
`ToolCallPayload{.., name, ..}` → 取 `data["name"]`，首次断言误取 `data["tool"]["name"]`
已修）；报告非空且含 SQLite；git 工作树干净。
**被否决**：不把 agent 输出精确断言（LLM 内容是模型自由产出，重约束即假肯定）；不在固定
仓库外跑（路径即要测的对象，必须真仓库）。
**边界（诚实）**：报告准确性以 agent 述为准（本测试验证「能亲自用工具完成真实任务 + 非破坏」，
不是审稿校验其结论）；任务耗时 ~137s（多步工具），门控测试可接受；报告写 target/（gitignored）。
**回滚**：撤本提交即删测试，无生产影响。
**预期影响**：agent 可用性在「真实仓库 + 真实端点」上可重复验证；后续真实任务类门控测试
以此为模板。

---

## D-095（web 使用验证发现）：注册产品偏好 settings namespace（前端必读写）

**日期**：2026（用户打开 dsh web 使用测试，进页面即报错）。
**触发问题**：前端一进页面 `settings.mutate`（ns=`ui-onboarding`）即
`settings-rejected: settings namespace "ui-onboarding" is not registered`。使用阶段（部署/
维护）发现的真实集成缺口。
**根因**：Web Boot 的 settings provider 只注册了 `llm` namespace；而宿主侧**产品偏好
namespace 集从未注册**。TS Host 在 apiproxy 层注册完整产品偏好面（
`deepseek-harness/packages/host/apiproxy/tests/api-proxy-config.spec.ts`：
ui-onboarding{ welcomeNoticeVersion:string }、ui-theme{ preference:'light'|'dark'|'system' }
、locale{ preference:'zh'|'en' }、ui-conversation{ busyEnter }、shell{ timeoutMs }、
agent-loop{ maxParallelToolCalls }、permission{ defaultPreset } + base）。Rust 侧
`agent-loop` 的 maxParallelToolCalls 只在 AgentLoop.Config 级校验（settings.rs），
不注册 provider namespace；grep 确认无任何产品偏好注册点。
**最终选择**：提取 `register_host_settings(sp)` 注册上述 7 个产品偏好 namespace，
在 `boot()` 的 llm 注册块后调用；schema 逐字照搬 TS Host（union/with_default/required；
`permission` 带 base `{defaultPreset:'read-only'}`）；`register` 幂等（同 ns 早退），
对 AgentLoop/permission 等可能的注册方零冲突。
**被否决**：只修 ui-onboarding 一个（下一进页面步骤大概率继续撞 ui-theme/locale/agent-loop
——证据：TS Host 同测试段枚举整套）；放宽"未注册即拒"守卫（那是持久化安全边界，不弱化）。
**TDD + 验收**：`register_host_settings_exposes_product_preference_namespaces`（红 = 线上
真实 settings-rejected；绿 = 7 ns 全注册 + ui-onboarding/ui-theme mutate Ok + 未注册 ns
仍被拒）；dsh-cli lib 全绿、clippy `-D warnings` 零告警。**服务重启后 HTTP 复现用户原场景**：
`settings.mutate ui-onboarding {welcomeNoticeVersion:v7}` → `ok:true` revision 1；
`settings.describe` 列出 llm + ui-onboarding/ui-theme/locale/ui-conversation/shell/
agent-loop/permission 共 8 个。
**回滚**：撤本提交；服务仍可用（仅前端偏好持久化回原报错）。
**预期影响**：Web 前端进页面/设置流不再 settings-rejected；后续如再遇未注册 ns，按同法
对照 TS Host 注册面补齐（本例已覆盖规范全集）。

---

## D-096（web 使用验证发现）：`host.pickDirectory` 真实现（原生目录选择器）

**日期**：2026（用户点「打开工作区」无弹窗）。
**触发问题**：前端调 `host.pickDirectory`，响应 `{ok:true, value:{path:null}}` 但无目录
选择框。修复前该 RPC 是诚实降级占位（`{path:null}` 对齐「用户取消」语义），前端据此认为
用户取消 → 无任何交互。
**语义梳理（TS seam）**：`host.pickDirectory` 属 **native** 能力——打开原生选择器、选中
返回路径、**取消才为 null**；非组合能力的方法应报 `directory-picker-unavailable`。用 null
冒充「不可用」是错的（不可达 vs 取消在客户端语义不同）。TS Windows 实现是 IFileDialog/COM
后台 worker；Linux 是 zenity/kdialog。
**最终选择**：真实现 tri-state——`crates/dsh-cli/src/host_picker.rs`：
`powershell.exe -STA` + `System.Windows.Forms.FolderBrowserDialog` 弹系统目录框
（Win10/11 桌面稳定；务实等价，非 IFileDialog 现代外观）。三态：
`Ok(Some(path))` 选中 / `Ok(None)` 取消（wire `{path:null}`）/ `Err` 失败
（wire `directory-picker-unavailable`，**绝不**冒充取消）。装配经 `Boot.host_picker`
接缝（`serve()` 注入真实现；测试注入 stub 不弹框）；未装配（None）→ 同一错误。

**被否决**：① 保持 null 退化（不可用≠取消，客户 端语义错）；② 直接改前端用 browse 流程
（`host.listDirectory` 已实现，但前端 native 流程已挂载、browse 需运行时重组，动前端装配
面大且非重启即得）；③ 在 Rust 内进程内做 Win32 COM IFileDialog（工作量大，成本/收益不成
比例——记录为后续改进，见「预期影响」）。
**TDD + 验收**：`host_picker::` 单测 3 项（interpret：选中/取消/失败；不触发真实弹框）；
`rpc_host_pick_directory_seam_three_state`（注入 stub：选中 path/取消 null/失败与未装配
均 `directory-picker-unavailable`）；`host.pickDirectory` 移出 ok-冒烟表（行为由专用测试
覆盖）。dsh-cli lib 全绿、clippy `-D warnings` 零告警、新文件 rustfmt canonical。
**回滚**：撤本提交。
**预期影响**：点「打开工作区」弹出真实系统目录选择框（选中/取消正确 wire）；后续可把
powershell FolderBrowserDialog 升级为 IFileDialog 现代对话框（同一 HostPicker 接缝，零
业务改动）。

---

## D-097（web 使用验证发现，覆盖 D-096）：目录选择改为 browse 组合，移除 native 子进程

**日期**：2026（用户真实使用：D-096 的 powershell 弹框触发杀毒软件警告）。
**触发问题**：D-096 用 `powershell.exe -STA` + FolderBrowserDialog 弹原生对话框，
每次打开即 spawn 带内联 `-Command` 脚本的子进程——杀软按「脚本唤醒」启发式告警。
用户问：有没有和 TS 代码一致、由浏览器打开文件夹目录的方式？
**语义梳理（TS seam，权威）**：目录选择是组合式能力（`ctx.directoryPicker` 后端）：
- **browse** 能力 = `host.listDirectory`/`host.createDirectory`（页内目录浏览，纯 fs、
  面包屑+home 锚点+hidden 标志；**零子进程**）。对应客户端 `dsh-client-ui-directory-picker-browse`。
- **native** 能力 = `host.pickDirectory`（原生对话框；TS Windows 实现为 **IFileDialog/COM
  后台 worker**，非脚本子进程）。对应客户端 `dsh-client-ui-directory-picker-native`。
- TS 组合期**只挂一个与后端能力匹配的 flow 包**；「组合之外的方法」→
  `directory-picker-unavailable`。我们 Rust 早已供齐 browse 后端（`host_dir.rs`）。
**根因**：`build_boot_manifest` 把 plugin_root 下**所有** web 客户端插件都收进
`__DSH_BOOT__`，native+browse 两个 flow 客户端同时被浏览器装载；ui-workspace 的
directory-flow 洞是 `single` kind，native 占据 → 点「打开工作区」调 `host.pickDirectory`。
**最终选择**：**组合 browse、排除 native**（与 TS 组合期一致，且天然零 AV 面）：
1. `build_boot_manifest` 新增 `HOST_COMPOSITION_EXCLUDED_CLIENTS`，排除
   `@deepseek-ai/dsh-client-ui-directory-picker-native`（boot 图只留 browse 客户端，
   占据 single directory-flow 洞 → 页内目录浏览）；
2. `host.pickDirectory` **恒报 `directory-picker-unavailable`**（browse 组合下 native 方法
   不可用，TS 逐字语义；绝不返回 `{path:null}` 冒充取消）；
3. **删除 D-096 的 `host_picker.rs`（powershell 子进程实现）+ `Boot.host_picker` 接缝 +
   `HostPicker` 别名**——杀软敏感面从代码树彻底清零，不再是「不调用但留后门」。
**被否决**：① 保留 D-096 子进程（AV 面仍在，违背用户诉求）；② browse+native 都留让运行时
选（洞是 single kind，仍有一个被抢；TS 组合期就只挂一个）；③ 改前端运行时重组（动前端
装配面，非重启即得，且 TS 不做）；④ 在 Rust 进程内直接做 IFileDialog/COM（TS 正解，
但成本/收益当前不成比例——记录为后续可选升级，路径=恢复 `host.pickDirectory` 真实现 +
boot 图切回 native 客户端）。
**TDD + 验收**：红 = `build_boot_manifest_composes_only_one_directory_picker_flow`
（fixture 含 browse+native，断言 browse 在、native 不在、恰 1 entry）先失败；绿 = 加
exclusion 后通过。`rpc_host_pick_directory_unavailable_when_browse_composed`（恒
`directory-picker-unavailable`、value 无 `{path:null}`）。dsh-cli lib 全绿、clippy
`-D warnings` 零告警；fmt 仅 web.rs 既有漂移行（勿套 rustfmt 全文件）。
**回滚**：撤本提交（恢复 D-096）。
**预期影响**：点「打开工作区」= 页内目录浏览选择（`host.listDirectory`/`createDirectory`，
零子进程、杀软零告警）；`host.pickDirectory` 对直接 API 调用来访者诚实报不可用。若未来要
原生对话框，按 TS 用进程内 IFileDialog/COM（不 spawn 脚本子进程）。

---

## D-098（用户指定：做 native 版，覆盖 D-097 的 browse 组合）：进程内 IFileDialog 原生选择器

**日期**：2026（用户：「按照流程规划做这个原生版调用」——希望点「打开工作区」出系统目录框）。
**触发问题**：D-097 组合了 browse（页内浏览）以避开 D-096 的 powershell 子进程（杀软告警）；
但用户要的是**原生系统对话框**。TS 正解是进程内 IFileDialog/COM（无子进程）——D-096 只是
偷懒用了 powershell，该方案本身不该被否定。
**依赖评估（先调查后引入，规则四）**：可选路径 A）新版 `windows` crate（0.62.2，离线缓存
中成熟可用，`IFileOpenDialog`/`IShellItem` 经 `define_interface!` 全量提供）——**采纳**：
维护方微软、被广泛采用、类型化安全（免自搓 vtable 风险）；B）`windows-sys` 0.48（仍含
接口但旧、裸 HRESULT/裸指针）；C）自搓 COM vtable（TS 在 Node 用 koffi 是迫不得已，Rust
能引成熟绑定就该引）。离线依赖链 windows-core/implement/interface/link/result/strings/
collections/future/numerics 对应版本全在缓存，`--offline` 可解析。
**最终选择**（三段式，对齐 TS `directory-picker-native` 的「纯时序 + 平台绑定」拆分）：
1. `host_picker.rs`：`run_folder_dialog(bindings, title)` 纯时序（init→create→show→
   收尾，`co_uninitialize` 每条路径恰一次）＋ `DialogBindings`/`FolderDialog` trait——
   全假后端单测（选中/取消/失败/清理配对）；`decode_wstring`（NUL 结尾 UTF-16，32k 上限）。
2. `host_picker_windows.rs`（cfg(windows)）：真实 COM——`CoCreateInstance(&FileOpenDialog,
   None, CLSCTX_INPROC_SERVER)` → `SetOptions(FOS_PICKFOLDERS|FOS_FORCEFILESYSTEM|
   FOS_NOCHANGEDIR)` → `SetTitle` → `Show(None)`（取消 HRESULT 0x800704c7 → Ok(None)）→
   `GetResult` → `IShellItem::GetDisplayName(SIGDN_FILESYSPATH)` → 解码 + `CoTaskMemFree`。
   **零子进程**：对话框由本进程 COM 创建，杀软针对的外部程序唤醒面为零。
3. web：`Boot.host_picker`（`Arc<dyn Fn+Send+Sync>`）接缝，`serve()` 装配真实 picker，
   dispatch 三态（path/null/`directory-picker-unavailable`，绝不用 null 冒充不可用）；
   manifest 组合 **native**（排除 `dsh-client-ui-directory-picker-browse` 客户端）。
**并发化洞（真实使用发现，二级修复）**：serve 的 accept 循环单线程内联 RPC；模态对话框
阻塞该线程 → 全服务饿死（实测 homepage/listDirectory 全 HTTP 000）。修复：`host.pickDirectory`
在 `dispatch_request` **派到独立线程**（Arc picker 跨线程；信封/三态 helper 单一事实源），
accept 循环保持响应。实测对话框开启期间 homepage 200 + listDirectory 正常。
**被否决**：windows-sys 0.48（依赖旧）；自搓 vtable（Rust 无需）；线程化所有 RPC（Boot 非
Send，改动面大且不必要——只有 user-paced 的 pickDirectory 需要隔离）。
**TDD + 验收**：`host_picker` 单测 6 项（三态 + 清理配对 + 常量防漂移 + decode）+ seam 测试
4 场景 + manifest 组合测试（native 在、browse 排除）+ 回归 128 项全绿 + clippy `-D warnings`
零告警。实测：boot 图仅 native 客户端；`host.pickDirectory` 弹**系统原生目录框**；对话框
开启期间服务器不卡死；`host.listDirectory`（browse 后端）仍可用（两者并存服务）。
**已知限制（如实）**：① 连接中断/页面关闭不中止已开启的对话框（TS 用 WM_CLOSE 中止，我们
未接；后续可加）；② 未做 DPI 感知（TS 最佳努力，普遍不影响）；③ 对话框标题固定英文
"Select a folder"。
**回滚**：撤本提交；boot 图回 browse-only。

---

## D-099（使用测试发现：控制台 404）：实现 `/plugins/events` HMR SSE 通道

**日期**：2026（用户反馈控制台报错 `XHR GET http://127.0.0.1:60165/plugins/events
[HTTP/1.1 404 Not Found]` + Firefox 重连提示）。
**触发问题**：前端的 web 组合包**无条件挂载** `@deepseek-ai/dsh-client-hmr`（TS
`web-app` 组合的 always-on 客户端插件重载链，其文档：「mounts this row unconditionally;
without a rebuild watcher rewriting client bundles, the chain stays idle」）。该插件
浏览器半（`packages/client/hmr/src/client`）在 `ctx.effect` 里**无条件**
`new EventSource('/plugins/events')`。我们的 Rust serve 只实现 `/plugins/<id>/client.js`
bundle 路由，从未提供 `/plugins/events` SSE 通道 → 404 → EventSource 自动重连刷屏。
**第一性原理**：这不是前端多发的请求，而是**缺失的请求面**——TS 宿主永远为这条通道
服务（`/plugins/events` SSE：连上即写 `: connected\n\n` + `{type:"graph", graph}` 帧；
随后对 bundle 内容变化广播 `{type:"rebuilt", id, rev}` 帧）。我们的 serve 要做到
TS 对等，就必须实现同样语义的通道。
**考虑过的选项**：A）仅实现**空闲 SSE 桩**（只发 connected+graph，永不广播 rebuilt）——
能消 404 但把通道做空，损害 TS 对等；当开发者对 client bundle 跑重建时浏览器仍拿旧
bundle（违背「不为『快速完成』选最小实现」）。B）**完整移植一个 stat-poll watcher +
rebuilt 广播**（TS 活半的 1:1）——浏览器收到 rebuilt 帧后经缓存模块系统 invalidate→
prefetch→refresh 热换单插件（`entry.refresh()` 按 `?rev=` 重拉我们服务的新 bundle），
真实支持客户端插件热重载；无重建时通道空闲（与 TS 完全一致）。C）从组合排除
`dsh-client-hmr`（像目录选择器那样）——让 /plugins/events 永不请求；但偏离 web-app
的「无条件挂载」契约，且只有我们 Rust 侧排除、TS 应用不排除，二者行为分叉。**采纳 B**。
**最终选择**（对齐 TS `client/hmr` 宿主半）：
1. `hmr_events.rs`（新）：`HmrChannel`（Arc 共享）——`WatchedRow{id, path, mtime_ms,
   size, rev, dirty}` 表 + `HashMap<u64, mpsc::Sender<String>>` 连接集。`poll_once()`
   纯可重入（stat 每行 → 变化才 re-hash 内容找新 rev → 返回 `(id, rev)` rebuilt 列表；
   缺文件标 dirty 跳过；未变静默），供单测零时序驱动；`run(interval)` 是薄循环
   （sleep → poll_once → broadcast）。`connect()` 注册客户端返回 receiver；广播用
   mpsc 发送，发送失败（对端 drop）即移除连接——**每条连接只有连接线程碰 socket**，
   watcher 线程绝不并发写 socket（避免与 TS `Set<ServerResponse>` 直写的并发写面）。
   帧序列化纯函数：connected 注释 + graph 帧（复用 boot 图形状）+ rebuilt 帧，逐字节单测。
2. web.rs 接线：`dispatch_request` 在 `/plugins/` 前缀分支**之前**判 `path == "/plugins/events"`
   ——GET → `into_writer()` 起线程跑 `stream_hmr_events`（SSE 头 + connected + graph +
   增量帧 + keepalive 心跳，连接关闭即退）；HEAD → 200 event-stream 头（无体）；其余
   方法 → 405（对齐 TS 路由的 405 语义）。`serve()` 在 manifest 组装后
   `Arc<HmrChannel>::new(&manifest)` + 起 watcher 线程（默认 500ms，TS 同值）；manifest
   是 watcher 的静态 watch 集（我们的宿主不在运行时挂/卸客户端插件——TS 的
   `onGraphChanged` 动态增删行超出 Rust 侧范围，如实记录）。rebuilt 帧的 rev 即新内容
   `short_hash`，与 `/plugins/<id>/client.js?rev=` 一致（no-cache 从盘直读）。
**被否决**：空闲 SSE 桩（做空通道、破坏 TS 对等）；组合排除 client-hmr（偏离无条件
挂载契约、与 TS 行为分叉）；让 watcher 线程直写 socket（并发写面，mpsc 隔离更稳）。
**TDD + 验收**：hmr_events 单测（graph 帧/connected 注释/rebuilt 帧 wire 格式、poll_once
无变化空闲/改 bundle 报 rebuilt/缺文件 dirty 后恢复、broadcast 达达且对端 drop 即移除）
+ web route 纯判定单测 + 回归 128 项全绿 + clippy `-D warnings` 零告警。实战：重启后
`GET /plugins/events` 返回 SSE 200（connected+graph）不再 404；临时宿主（DSH_PLUGIN_ROOT
覆盖到 scratch）改一 bundle → SSE 流收到真实 `rebuilt` 帧。页面刷新后控制台无 404/重连。
**已知限制（如实）**：① watch 集静态（manifest 启动时定）；TS 运行时增减行超出 Rust 侧
宿主能力，未移植，文档说明；② `short_hash` 沿 D-095 前的 DefaultHasher（非加密、跨进程
不稳定）——进程内内容变化检测足够（HMR 契约），属既有债务非本决策引入。
**回滚**：撤本提交；serve 回到无 /plugins/events（只有 404 控制台噪音，功能不受影响）。

---

## D-100（使用测试发现：打开工作区不生效）：真实 workspace registry + host 事件流

**日期**：2026（用户反馈：前端「打开工作区」→ `host.pickDirectory` 选中
`F:\RustProjects\deepseek-harness` → `/api/workspace.create` 返回
`{"ok":true,"value":{"created":false,"workspace":{"workspaceId":"default","path":"F:\\RustProjects\\deepseek-harness","sessionIds":[],"title":"default",...}}}`，
但页面没有打开刚选的工作区）。
**触发问题**：Rust serve 的 `workspace.*` RPC 全是 canned stub（`workspace.list` 恒返回
单一 `default`（path=cwd）；`workspace.create` 恒返回 `workspaceId:"default"` +
`created:false` + `title:"default"`；`rename/delete/insertBefore/insertSessionBefore/archiveSession`
全是假响应；`session.create` 忽略 `workspaceId`、不把新会话挂到工作区），且 serve 的
`events.host` 通道从不推 `host/*` 帧（hello 之后只流 mux 形式的 session/event），
`events.host` 的 SSE 回落连 host hello 都没有（见 D-099 事件）。
**第一性原理（自下而上，前端对象层事实）**：
- `WorkspacePicker.adoptDirectory` → `createWorkspace({path})` → `manager.create` →
  `Workspace.materialize()` → `api.workspace.create` → **成功即 `upsert` 返回值进 list
  store**（TS `workspaces/manager.ts`），随后 `onPick(workspaceId)` →
  `startSession` → `connectWorkspace(id)`（**要求 id 已在 workspace.list store**，否则
  throw `unknown workspace`）→ 无可复用 blank 会话 → `sessions.create({workspaceId})` →
  `sessions.open(id)`。所以「能否打开」取决于 workspace.create **返回值**是否是一个
  list 里可寻址的**独立**工作区，以及 session.create 是否返回可 open 的会话。
- 核心缺陷：`workspace.create` 恒返回**碰撞的** `workspaceId:"default"` → manager 的
  `upsert(view, identity)` 按 id **替换** boot 基线里那个 `path=cwd` 的 `default` 工作区
  （clobber 基线），且 `created:false` + `sessionIds:[]` → 分组全乱、选中的工作区永远
  不是“新”的。另外 serve 从不推 `host/workspace-changed`，所以任何其它 tab /
  reconcile 都看不到创建工作区；`session.create` 也不 attach，`workspace.sessionIds`
  在客户端永远空 → 会话以 Ungrouped 出现。
- TS 权威语义（`packages/workspace/workspace/src/index.ts` + apiproxy 测试）：create
  对**既有 canonical path** 幂等返回（`created:false`，不改 title）；对**新 path** 铸
  **全新 id**（randomUUID，绝不复用——注：同 path 重新注册也会新铸 id）+ `title=basename`
  + `created:true` + prepend 到 durable order；path 不存在/非目录 → reject；
  `session.create{workspaceId}` 把新会话 attach 进该 workspace 的 sessionIds 并推
  `host/workspace-changed` + `host/session-added`。客户端只经「create 回显 upsert +
  host/workspace-changed 帧」两条路得知 workspace.sessionIds 变化。
**考虑过的选项**：A）只把 `workspace.create` 做成「返回不同 id + created:true」的
局部补丁，其余 stub 不动——治标不治本：list 基线仍不含新建工作区、session 仍不 attach、
无事件流，任何「刷新/其它 tab/重连」都会再次打回原形（违背方法论四：不为快改小坑）。
B）**实现真实 in-process 工作区注册表 + 重启 host/workspace-* 事件流 + session.create
attach（本次采用）**——对齐 TS workspace 域的**本会话**语义：create 幂等/新铸 id、
list 反映真实注册表、session attach、`host/workspace-changed/workspace-removed/
workspace-order-changed/archived-sessions-changed/session-added` 沿 `events.host`
下链（SSE 与 WS 同时，SSE 补 host hello）。C）完整移植 TS 的**持久化** workspace 域
（SQLite workspaces 表 + pending-mutation 恢复 + 从 session 日志 bootstrap 分组）——
超出本问题所需（web serve 的 workspace 注册表跨进程重启持久化不是当前需求），记为
**已知限制**另行立项。**采纳 B**。
**最终选择**：
1. `workspace_host.rs`（新）：`WorkspaceRegistry`（单线程 `Rc<RefCell>` 纪律，对齐
   M4h settings/goal）——`order: Vec<String>` + `by_id: HashMap<String, Record>` +
   `archived: Vec<String>`；`Record{workspace_id, path(canonical), title, session_ids,
   created_at, updated_at}`。`new()` 注册 boot `default`（id `default`、path=cwd、
   sessionIds `["default"]`，保持既有 UI 基线不变）。方法 `create/rename/delete/
   insert_before/insert_session_before/archive_session/attach_session/list/get` +
   `view()`（对齐 `workspaceViewSchema`）。**create**：`fs::canonicalize`（失败/非目录
   → err）；按 canonical path 幂等去重（`created:false` 不改 title）；新 path 铸新 id
   （std：纳秒时间戳 XOR 进程级原子计数器 → 进程内绝不复用）+ `title=basename` +
   `created:true` + prepend order。**id 不复用**不变量在进程内成立（注册表 in-memory；
   若后续引入持久化，id 生成须升级——如实记录）。
2. `lib.rs`：Boot 增 `pub workspaces: Rc<RefCell<WorkspaceRegistry>>`（`boot()` 构造，
   用 process cwd）与 `pub host_events: Option<Arc<Mutex<Vec<Value>>>>`（serve 装配）；
   `pub mod workspace_host;`。
3. web.rs 重接：`workspace.list/create/rename/delete/insertBefore/insertSessionBefore/
   archiveSession` 全走真实注册表 + 正确错误码；`session.create` 带 `workspaceId` 时
   attach（未知 workspace → `workspace-not-found` 错误）+ 推变更帧；RPC 变更后把
   `{type:"host/workspace-changed", workspace: view}` 等**内层 payload** 压入
   `boot.host_events`。`events.host`（SSE + WS）除既有 mux 帧外在 host 通道追加流
   host 帧（server-request `host/event` 信封包裹，rpcId `host-<n>` 自增），hello 的
   `host/session-added` 用真实 blank 状态；SSE 回落补 host hello（修 events.host SSE
   无 host 语义的缺口）。
**被否决**：选项 A（局部补丁、list/事件/attach 三处仍假、刷新即打回原形）；完整持久化
workspace 域（选项 C，超出本问题、列为已知限制）。
**TDD + 验收**：workspace_host 单测（create 幂等去重/新铸 id 唯一/title=basename、
list 顺序含 boot default、rename/delete/insert_before/insert_session_before/archive
/attach 语义、view 字段对齐）→ web RPC 用真实注册表响应断言 → 既有 128+ 回归全绿 →
clippy `-D warnings` 零告警。实战：重启 60165 后驱动「pick→create→session.create」RPC
序列验证返回值（独立 workspaceId/created:true/title=basename、sessionIds 含新会话），
`events.host` 流收到真实 `host/workspace-changed` 帧；浏览器刷新后「打开工作区」在
侧栏出现新工作区并打开其会话。
**已知限制（如实）**：① 注册表 in-memory，跨进程重启不持久化（TS 为持久域；另行立项）；
② workspaceId 为进程内唯一（非 UUID 规范）；③ events.host 通道同时保留历史 mux 帧
下推（客户端 `onHostEnvelope` 按 type 过滤，无害）；④ id「绝不复用」不变量仅限进程内，
持久化引入时须重新评估。
**回滚**：撤本提交；workspace RPC 回到 canned stub（打开工作区再次失效但其余功能不受影响）。

---

## D-101（使用测试发现：新会话发消息报 `no configured agent maps to session`）：per-session agent 注册

**日期**：2026（用户反馈：工作区流程开出的会话 s4 上 `session.prompt` 报
`{"code":"internal","message":"no configured agent maps to session \"s4\""}`；设置中
`agentPreset.list` 返回 `presets:[]`；用户判断是「没加载 agent 预设」。）

**第一性原理（自上而下 + 自下而上）**：
- 自上而下：前端「打开工作区 → sessions.create → sessions.open → prompt」的会话链里，
  `session.prompt(sessionId: X)` 必须能把 X 路由到一个真实 agent。TS 里 `session.create`
  生一个完整 Session+Agent（agent 的 composition 来自预设），所以**任何已创建会话都应有
  agent**；重启后持久化恢复的会话也应续接 agent。
- 自下而上（现有实现）：`run_rust_loop` 的会话→agent 路由只查 `host.config.agents`
  （cordis.yml 装配期静态配置，仅 1 条 `default`）；`session.create`（web RPC）只 mint
  会话、**不注册 agent**；`session.fork` 同理；`agentPreset.*` 全为 stub。
  两者在中间相遇的结论：不是「预设没加载」导致发消息失败——是**会话根本没有 agent 可路由**；
  预设清单为空是另一个独立缺口（后续决策）。

**考虑过的选项**：
1. **运行时可写 agent 注册表 + 关键点挂接（采用）**：`AgentLoopHost` 增
   `runtime_agents: RefCell<Vec<ConfiguredAgent>>`（与静态 `config.agents` 并列做会话发现；
   `config` 保持装配期校验语义不变）、`configured_for_session()`（静态优先，再运行时）、
   `register_session_agent()`（幂等：会话已被任何 agent 命中则复用）；`run_rust_loop` 改走
   `configured_for_session`；web `session.create`（cwd=工作区路径，D-100 归属）/
   `session.fork`（继承源 cwd）调用新 `ensure_session_agent()`；对**存在于共享 store** 的
   会话（重启恢复）首次 prompt 时懒挂接再路由，未知会话仍 fail loud（不放行任意 id）。
2. 只把 `session.create` 返回的 `agentPreset` 字段补上/把 stub 改成假成功——治标不治本：
   prompt 路由仍失败。
3. 给 AgentLoopHost 的 `config` 整体包 `Rc<RefCell>` 允许运行时改静态 agents——侵入 validate
   语义，且不必要（运行时注册单独成表更干净）。

**最终选择**：选项 1。静态配置身份不可变、运行时注册独立成表；会话→agent 的**唯一查询
入口**收敛到 `configured_for_session`（`run_rust_loop` 与 `ensure_session_agent` 幂等判断
共用同一规则 `ConfiguredAgent::matches_session`：`sessionId` ▸ `resumeSessionId` ▸ `agent-{id}`）。

**TDD + 验收**：dsh-agent-loop 集成测试 ×3（`configured_for_session` 命中静态约定身份；
`register_session_agent` 可路由/幂等/驱动真实 turn 于 session 键下；预留会话复用既有 agent
不重复登记）+ dsh-cli web 测试（`session.create{workspaceId}` → 会话可被路由、cwd=工作区路径、
`session.prompt` accepted 且事件落共享 store；**重启续接**的 store 会话首次 prompt 懒挂接
accepted；**未知**会话仍 `internal:no configured agent` fail loud）。回归：dsh-cli `--lib`
149 项全绿 + clippy `-D warnings` 零告警。

**已知限制（如实）**：① per-session agent 的 provider/model/cwd 继承部署默认（模板 =
装配期 `default`）；`session.selectModel` 仍是 stub，不改变已注册 agent 的模型——逐会话
选模型另立项；② `agentPreset.*` 仍是 stub（设置里预设清单为空），另决策；③ 懒挂接仅对
store 中**已存在**会话生效（重启恢复），未知 id 仍 fail loud。
**回滚**：撤本提交；`session.prompt` 回到「仅配置会话可路由」，新会话发消息再次失败；
其余功能不受影响。

---

## D-102（preset 组合路径 B 的前置资产）：内置 agent 预设「复制自持 + 忠实转译」落地

**日期**：2026（延续 D-101 之后的分析轮：用户拍板 preset 组合决策 **A = 复制 vendored
预设进 Rust 项目自持**、并明示「先产出问题清单文档供深入分析、再定稿分阶段规划」；
spike-4 结论：`!!js`→`__jsExpr`/`disabled_expr` 转译机械可做、共 12 处仅 3 种语法形态。）

**第一性原理（自上而下 + 自下而上）**：
- 自上而下：组合要「真实改变会话行为（直通 P4）」，第一步必须是**自持的可装载预设资产**——
  与 vendored 参考树断开、不依赖其运行；TS 特有的 `!!js` YAML 标签在 Rust 有既定替代
  （loader `disabled_expr` 字段 + `dsh_eval::interpolate` 的 `{"__jsExpr": expr}` 节点），
  故复制必须是**语义忠实**而非求值落值。
- 自下而上（现有实现）：`EntryOptions.disabled_expr: Option<String>`（entry.rs:25-28）、
  include.rs:6 注明 `{"__jsExpr": "..."}` 约定、`dsh-eval::interpolate`（lib.rs:522）递归替换
  `__jsExpr` 节点——三条约定都已在位；4 个 vendored 预设 `!!js` 恰好 12 处、只 3 种形态
  （`disabled:` 行 10×、config 值 `cwd:` 1×、config 数组项 skills 1×）。

**考虑过的选项**：
1. **忠实语法转译 + 自持资源 + 可复跑工具（采用）**：`!!js` 只做键约定转译
   （`disabled: !!js X`→`disabled_expr: "X"`；配置值/数组项→`{"__jsExpr": "X"}`），
   preset.yml 字节级复制；落 `resources/agent-presets/<id>/`；`tools/translate-agent-presets.ps1`
   可复跑、`tools/verify-agent-presets.py` 结构校验。
2. 复制期静态求值落字面值（如 `cwd` 直接写死绝对路径）——不忠实，破坏 `DSH_CWD` 语义且
   与生态约定脱钩，否决。
3. 顺带做 win32 能力改写（bash/pwsh 按 Rust 能力矩阵重写）——属 §6.1-2 的 A/B 决策范畴，
   留待用户拍板，不在此混入，否决（记录为已知限制）。

**最终选择**：选项 1。语法转译不改变语义；行级文本替换保证可审计；脚本可复跑供扩展
（用户自定义 preset 走同样转译）。

**TDD + 验收**：`yaml.safe_load` ×4（顶层数组、结构完好）+ 节点计数精确（code 2、cordis 3、
minimal 5、standard 2 = **12/12**）；转译前后 `disabled` 门控表达式逐字不变。过程中先修掉
「PowerShell 5.1 读无 BOM 的 UTF-8 脚本把中文注释误按 GBK 解析」的环境坑（脚本改纯 ASCII）。

**已知限制（如实）**：① minimal 的 `cwd: {__jsExpr: process.env.DSH_CWD ?? process.cwd()}`
与 cordis 的 skills `new URL('skills/', baseUrl)` 之一的求值**超出当前 dsh-eval 作用域**
（`eval_scope` 无 `process`、`env` 空、无 `baseUrl`/URL 语义）→ 相关行在 P1 装载期将
fail-loud，直到 spike-6（`process` 门面）/后续 baseUrl 注入落地；② win32 shell A/B
（§6.1-2）待用户拍板，**本转译未改门控语义**；③ 此资产尚未接线（P1 发现/解析消费）。

**回滚**：`git revert` 本提交即可——删除 `resources/agent-presets/` 与 `tools/*` 两个脚本，
其余 crate/服务不受影响（未接线）。

---

## D-103（preset 插件组合阶段规划 + ★ 决策点定稿）：进入 TDD 分段实现

**日期**：2026（round-9 用户拍板：**采纳 PLAN-BC §5 全部推荐**；win32 shell = **B 先直通 P4、
A(pwsh) 随 P3**；broken 集 = **skill 最小只读 + web/tool-cordis/command-compact 显式 broken**。
前置：D-101（per-session agent 注册）、D-102（复制自持 + 忠实转译）、spike-1..6/8 全闭环、
E-02 基线 149 全绿、REQUIREMENTS 需求结论文档定稿。）

**第一性原理（自上而下 + 自下而上）**：
- 自上而下：组合要「真实改变会话行为（直通 P4）」，实现必须以**最小可验收增量**推进——
  每个阶段的工件（发现 roster → 挂载守卫 → 行映射 → loop 生效 → 全 RPC）都能独立验收，
  前一关卡不过不进下一阶段；核心不变量（组合权威归位 dsh-core/loader、key 纪律、fail-loud、
  诚实差异、default 基线不动）贯穿全程。
- 自下而上：已核证的全部可行性事实（spike-1..6/8 + D-102 资产）构成实现的前置契约——
  dsh-scope 父链/rebind 已具备、SystemPrompt/工具作用域已按 agent scope 决议、loader
  create/update/remove/sync 公开、eval_scope 需补 `process` 门面 +1 白名单项。两者相遇结论：
  **各阶段的技术路径已无未知，只剩按 TDD 落地**。

**考虑过的选项**：
1. **阶段规划定稿（采用）**：P0 收口 → P1 解析/发现/根 → P2 组合挂载+守卫 → P3 插件行+服务桥
   （含 pwsh 并行立项）→ P4 loop 消费 scope（直通 P4 达标关）→ P5 RPC 全语义+作者流+live 验收
   → C 收敛（独立里程碑）。每阶段独立提交 = 安全回滚点。
2. 一次性大爆炸实现——违背瀑布流 + TDD 纪律、无法阶段验收，否决。

**★ 逐项决议**（依据 = PLAN-BC §5，均为「已推荐 → 用户确认」）：
- **A-01**：路径 B 每 standing 一个 Cordis（独立组合引擎 + isolate 私有服务），共享单树留 C。
- **A-03/B-11 桥子集**：必须桥 = planMode/compaction+pruner/terminals/fs；**先 broken** =
  web/tool-cordis/command-compact；**skill = 最小只读（复用现有 directives 装载）**。
- **win32 shell**：**B 先行**（自持预设 win32 门控按 Rust 能力改写为启用 bash，零新增，live
  验收不空 shell；P4 前落地）→ **A 随 P3**（pwsh 工具：dsh-shell 平行 pwsh executor + dsh-terminal
  "pwsh" 后端注册 + m5 tool-pwsh，落地后切回忠实门控）。
- **A-05**：generation-based 原地换代（loader create/update/remove/sync）为主路径；HMR 文件
  监听后置；进程级重启仅兜底。
- **B-04**：用户根照 TS 约定——`dsh_home()`（`$DSH_HOME`→空白忽略→`home_dir()/.dsh`）+
  用户根 `<dsh_home>/.agent-presets`（trust=user，authorable=存在即真）+ 系统根
  `resources/agent-presets`（trust=system）+ roots 数组首根胜出 + `includeUserRoot` 开关。
- **C-04**：default 会话不隐式 join（E-02 安全基线、向后兼容）；`agent-presets.default` 设置只
  决定新会话初始预设选择。
- **F-05/F-06**：C 阶段再决（WASM/native 双驱动与 ScopeId/ScopeKey 键空间去留）。

**TDD + 验收**：每阶段交付可运行代码+测试+测试报告；`cargo test --lib -p dsh-cli` 149+ 全绿
（相对 E-02 基线只增不减）+ clippy `-D warnings`；AC1..AC7（REQUIREMENTS §6）逐条挂牌；
关键决策落 DECISIONS 并与 git 提交互查；key 永不落盘/入 git。

**回滚**：以阶段为粒度 `git revert` 独立提交；D-103 本身仅文档（阶段规划/决议），撤提交即回
到「用户定稿前」状态，不损任何 crate。

### D-103 实施补记（P1 落地实况，2026）

**P1-a**（17ea2f5）：`crates/dsh-agent-presets` 发现 crate（scan/discover/形状检查/home/
metadata），13 单测绿；D-102 的 12 个转译节点端到端通过形状检查（4 内置 preset 全部
broken=None）。

**P1-b**（本提交）：`dsh-cli` 接入——`PresetHost`（preset_host.rs，发现/read/authorable 的
domain 侧）+ `Boot.presets` 字段 + wire 接线：
- `agentPreset.list`：真实 roster（不缓存）+ `isDefault` 来自 settings `agent-presets.default`
  （namespace 已注册，base=工程默认 `standard`，Applies::Live）；`authorable`=用户根目录存在；
  `hasDocument:false`（Rust 侧无原生打开器，诚实）。
- `agentPreset.read`：真实组合文本 + trust + 可选 name/description（缺字段省略、不 null）；
  未知 id → `agent-preset-not-found`。
- **诚实门**（不装作能，D-103 预授权）：`select`=P2（join standing）→ `agent-preset-unsupported`
  显式拒绝；`copy`/`remove`=P5 作者流 → 同门；`openDocument` = `{opened:false, path=预设目录}`
  （无原生打开器的诚实降级，align TS）。
- 桩根解析：`DSH_PRESET_ROOT` env > cwd 相对 `resources/agent-presets`；用户根
  `<dshHome>/.agent-presets` 无条件追加（authorable=存在即真，D-103/B-04）。

验收：P1 条款「4 内置 + 1 自定义发现 roster 绿；list/read 可用」达成——RPC 层
`rpc_agent_presets_list_read_real_discovery`（注入 temp 根）断言完整 wire；全库
**155/155 绿**（E-02 基线 149 + 6），rustfmt（仅新文件）+ clippy `-D warnings` 零告警。

**已知未接（诚实列表）**：select 未 join（P2）；copy/remove 未作者化（P5）；`hasDocument=false`
（原生打开器未接）；live 服务二进制尚未重建（P4 阶段随 E-03 重启）。

**回滚**：`git revert` P1-b 提交——撤销 Boot.presets/接线/挂 handler，不触 crates/
dsh-agent-presets（P1-a 独立提交可保留）。

### D-103 实施补记（P2：standing 挂载机制，round 13）——决策 + 验收

**P2-a（提交 e5e69d1）——process 门面 + 组合类型化解析，修复结构性 bug**
- 触发：loader `eval_scope={config,ctx,env}` 无 `process` → 全部平台门控
  `disabled_expr` fail-closed 判禁用（**10 处门控行全失效**）；且
  `process.env.X ?? process.cwd()` 在 `DSH_CWD` 未设时因「缺成员报错」必失败。
- 选型与裁决：
  1. `dsh-eval::process_facade()` 注入 `{platform,env,cwd}`；platform 映射
     windows→win32/macos→darwin/linux→linux；env=全量环境变量；cwd=current_dir。
     **否决**按行改写 disabled_expr——那是 P3 win32 B 门控（A=pwsh 随 P3）的职责，
     平台门面必须先忠实于「正在运行的 OS 事实」。
  2. `process.cwd()` 进调用白名单（读 facade.cwd，无参）。**否决**更宽的任意
     成员调用。
  3. **member_access JS 语义修正**：对象上缺键 → `Null`（=undefined，使 `??`
     回落成立）；非对象基值取键仍报错（JS `TypeError` 等价，保持 fail-loud）；
     数组越界仍报错。**否决**全静默返回 Null（掩盖拼写错）。
- 验收：eval 3 + loader 3 + presets 18 全绿；真实四预设文件全部门控可干净求值、
  无「全禁用」回归；dsh-cli 155/155。

**P2-b（本提交）——每 preset 一 standing scope + scope 父链 join + 守卫报告**
- 触发：如何让「选中 preset」真实改变会话行为，且机制先于桥面（P3）可测。
- 选型与裁决：standing = 一个唯一 `ScopeKey`（贡献挂进共享 `SystemPrompt`
  注册面的 scoped layer）；agent **join** = `dsh_scope::bind_scope_parent(agent→
  standing)`（`assemble(agent_scope)` 沿父链合并）；**换 preset**（select）= 原
  绑定 `rebind` 到另一 standing scope；**换代**（re-mount 同 id）= unmount 撤销
  scoped 贡献（undo 精确幂等）+ 铸新 scope。守卫报告三态：
  `bridged`/`disabled`/`guarded(name,reason)`——P2 只桥 `@deepseek-ai/dsh-persona`
  （complete + includeRuntimeContext 抑制均真实生效），其余活化叶行一律
  `guarded("no Rust bridge yet (P3/P5)")`（D-103「先 broken」，不伪装）。
  **否决**让 standing 直接持有/管理 release 服务抽象——真实 isolate 服务隔离
  归 C 段收敛（P2 诚实不伪装）。
- 验收：4 测试全绿——①两 standing 隔离 + join 可见（X∝minimal 只见 minimal
  persona、Y∝standard 只见 standard、未 join 的 Z 两者皆不可见、父链断言）；
  ②守卫报告拆分（win32 下 bash 行 disabled / persona bridged / fs、editor
  guarded）；③rebind 切换视图；④re-mount 换代撤销旧贡献；dsh-cli **159/159**
  全绿；clippy `-D warnings` 零告警；rustfmt 仅新 standing.rs/parse.rs。
- **已知未接（诚实列表）**：`agentPreset.select` 仅在 store 记录选择、join 生效
  待 P4 的 loop 消费 scope（assemble 以 agent_scope 走链）；桥面行（shell/fs/
  editor/web/skill 等）P3；C 段 en-route。
- **回滚**：`git revert` P2-b 提交；P2-a 另可独立 revert（不互依赖）。

### D-103 实施补记（P4：loop 消费 scope——直通 accept 的第一块，round 14）——决策 + 验收

- 触发：P1/P2 造好了 standing（每 preset 一 standing scope + join）+ 行审计与守卫，
  但 `agentPreset.select` 仍诚实返回 `agent-preset-unsupported`——要让「选中组合」真实
  改变会话行为（B 段直通的验收核心），必须把 join 接进 loop 的组装路径。
- 关键事实（自下而上核实）：loop 每 turn 以 `assemble_context_for(agent)` 组装 =
  `AssembleContext{scope: Some(agent.scope)}`；`SystemPrompt::assemble` 经
  `scope_chain_of` 走父链（dsh-scope `bind_scope_parent` 后即含 standing scope）。故
  **只要 select 把 agent.scope → standing.scope 链上，下一 turn 的 assemble 自动含
  preset 视图，无需重建 host/loop**。这使 P4 的改动最小化且即时生效。
- 选型与裁决：
  1. `AgentLoopHost::join_standing(agent_id, standing_scope)`：join/rebind 幂等；
     绑定存宿主 `joins`（随 agent 生命周期）；agent 未装配 → fail loud。**否决**
     在 dsh-cli 侧自持绑定（每 agent 的 binding 生命周期应随宿主，且 web handler
     是每请求无状态——无法持有跨请求绑定）。
  2. `Boot.standings: Rc<RefCell<StandingRegistry>>`：`StandingRegistry::default()`
     为占位（独立 SystemPrompt）；web serve 装配 agent-loop 后**以 host.prompt 重建**
     ——保证 standing scoped 贡献落进 loop 实际组装的注册面（否则挂进另一个
     SystemPrompt，永不生效，就是悄悄降级）。
  3. select 处理：解析 preset（P1-b 发现）→ 读并 parse 组合 → mount（换代幂等）→
     会话→agent（懒装配会话先 ensure；完全未知会话 fail loud）→ `join_standing` →
     `{agentPreset}`。错误信封：`agent-preset-not-found` / `agent-preset-broken` /
     `agent-preset-unsupported`（无 loop/无会话），全部显式，不假装切换。
- 验收：E2E `rpc_agent_preset_select_joins_standing_into_loop_assembly`（注入 temp
  根，自含标记文本）——未 select 无标记 → select code 后 assemble 含 CODE 标记 →
  重选 standard rebind 后含 STANDARD（不含 CODE）→ 未知 not-found → 不可解析组合
  broken；宿主 `join_standing_links_scope_and_rebounds`（链上/rebind/未知 fail
  loud）；dsh-cli **160/160** 全绿；clippy `-D warnings` 零告警；既有
  `rpc_agent_presets_list_read_real_discovery` 的 select 门（无 loop → unsupported）
  不变仍绿。
- **已知未接（诚实列表）**：桥面行（shell/fs/editor/web/skill 等）仍是 P3——
  persona 是唯一真实贡献，其余活化叶行 guarded；C-04 的 default 初始选择未隐式 join
  （settings default 只标 isDefault）；live 服务二进制需重建才有 E-03 真机验收
  （P4 段后随重启）；C 段（组合权威开进 dsh-core、isolate/事件/fiber）未动。
- **回滚**：`git revert` P4-core 提交——撤销 host.join_standing/Boot.standings/select
  接线；P2/P1 均已独立提交可保留。

### D-103 实施补记（P3-a：桥面初代——instructions 内容桥 + 工具行重呈现 + win32-B 平台策略，round 15）——决策 + 验收

- 触发：P4 让 select 真能 join，但此刻桥面只有 persona——shipped 预设选进 joined
  agent 后**工具全缺失**（多动态降级）。要让 live E-03 有意义，需把行桥面做实。
- 关键事实（自下而上核实）：
  1. `ToolRegistry::register(def, Some(&scope))` 支持 scoped 注册，`schemas(scope)`/
     `get(name, scope)` 走 `chain_layers`（全局基 + 祖先链遮蔽）——**工具行桥 = 把
     宿主全局工具按行 config 重呈现注册进 standing scope**，joined agent 的模型面
     即见组合的 description/timeout；
  2. **dsh-tools `view()` 潜在 bug（P3-a 回归发现并修复）**：`ancestors.pop()` 假设
     「查询 scope 自己有层」才去掉自有层；当 agent scope 无层、父 standing scope 有
     工具时，pop 误删祖先 → standing 工具遮蔽丢失。修复：仅当 `peek(scope)` 有自有
     层才 pop。
- 选型与裁决：
  1. **win32-B 平台策略**（D-103 落地）：`row_disabled_for_platform` —— win32 上
     bash 系行**强制可用**（Rust 经 Git Bash 可跑 bash；覆盖忠实门控的「bash 禁用/
     pwsh 可用」），pwsh 系行**判禁**（无 pwsh 执行器；A 并行落地后移除此覆盖）；
     非 win32 回落忠实求值。**否决**照忠实门控做（那会让 win32 joined agent 零 shell）。
  2. 工具桥表（P3-a 单工具行）：`dsh-tool-bash(-persistent)`→`bash`、
     `dsh-tool-str-replace-editor`→`str_replace_editor`；多工具行（fs-local/terminal）
     留 P3-b；无宿主工具 → guarded(`no host tool …`)。
  3. 内容桥 `dsh-agent-instructions`：`<facade.cwd>/AGENTS.md` → standing scope
     section（order 40，`maxBytes` cap）；文件缺失 = 桥解析但无贡献（report 标
     `no AGENTS.md`，诚实不假装）。
  4. `StandingRegistry` 增持 `tools: Option<Rc<ToolRegistry>>`（None = 未装配 →
     工具行一律 guarded）；`ToolDefinition`/`ToolOutputDefinition` 增 `Clone`
     （重新呈现需要克隆宿主定义——通用、API 安全）。
- 验收：standing 8 测试全绿——win32-B（bash 保持可用/pwsh 判禁）+ linux 对照忠实
  门控；工具行重呈现（joined 见行 description/timeoutMs、未 join 见全局原值 =
  组合呈现隔离）；守卫原因细分（无宿主工具/pwsh A-parallel/fs 多工具/D-103 broken）；
  instructions 桥（AGENTS.md 可见、maxBytes 截断、缺失不贡献仍 bridged）；P2-b 隔离
  测试不回归。dsh-cli **164/164**、dsh-agent-loop host 1、dsh-tools 全部集成套件照常；
  clippy `-D warnings` 零告警；rustfmt 仅新 standing.rs。
- **已知未接（诚实列表）**：多工具行桥（fs-local/terminal，P3-b）；pwsh 执行器
  （A 并行，P3）；web/tool-cordis/command-compact 保持 broken；live 服务二进制待
  重建（P4 随 E-03 重启）；C 段未动。
- **回滚**：`git revert` P3-a 提交——撤销 dsh-tools view 修复/Clone derives +
  standing 桥面 + win32-B；P4/P2/P1 均独立提交可保留。

### D-103 实施补记（P3-b：组行解析——fs-local/terminal 不再「留 P3-b」，round 16）——决策 + 验收

- 触发：P3-a 后 joined 预设仍缺 fs/terminal 工具（fs-local/terminal 行 guarded），
  live select「standard/minimal」会让 agent **工具退化**（只剩 bash+editor）。
- 关键事实（自下而上核实）：单工作区宿主下，standing 链本就继承全局工具基——
  joined agent 的 `schemas/get` 从全局基看到 fs/terminal 工具；组行的「shadow」
  与宿主共享同一 provider，**组行 = 解析确认而非逐工具重呈现**（重呈现相同 def
  是行为无差别的仪式）。
- 裁决：
  1. **组表**：`@deepseek-ai/dsh-fs-local` → `read/write/edit/read_image/glob/grep`；
     `@deepseek-ai/dsh-terminal` → M5h 终端六工具（open/send/read/signal/close/list）。
     全部存在 → `bridged (host toolset: …; chain-visible, single-workspace)`；部分
     缺失 → guarded（诚实列出缺失）。
  2. **terminal 后端行**（`dsh-terminal-bash/-pwsh`，组覆盖其工具）：组已解析 →
     `bridged (terminal backend; host default shell)`（win32 = Git Bash，满足
     win32-B）；组未解析 → guarded。
  3. `tool_guard_reason` 删去 fs/terminal 死分支（已被前置解析收入）——只留
     pwsh A-parallel 与 D-103 broken 集。
- 验收：standing 9/9（新组行解析测试：minimal+win32-B+全工具集 → fs/terminal/
  bash/editor 全 bridged、pwsh 系 disabled 不伪装、joined 模型面见整组工具）；
  守卫测试更新（fs 缺工具 → `host tool group missing …`）。dsh-cli **165/165** +
  agent-loop 1；clippy `-D warnings` 零告警。
- **已知未接（诚实列表，参照 P3-a 收窄）**：pwsh 执行器（A 并行）；web/tool-cordis/
  command-compact 保持 broken；单元/集成验证后 live 二进制未重建（P4 随 E-03）；
  C 段未动。
- **回滚**：`git revert` P3-b 提交——回退组行解析与后端行处理，P3-a 保留即可。

### D-103 实施补记（P4 剩余/E-03：宿主运行时 prompt 变量——live 首红修复，round 16）——决策 + 验收

- 触发：P1-P3 全部落地后重建 live 二进制做 E-03 真机验收——`session.create` +
  `agentPreset.select{standard}` join 成功，但**首轮 turn fail-loud 即红**：
  `unknown prompt variable "{{model}}" in section "preset:standard:persona:0";
  registered variables: (none)`。守护机制如设计工作（组合被注入、失败大声），
  缺口是**宿主运行时事实未喂给 prompt 模板**。
- 关键事实（自下而上核实）：vendored personas（standard/code/cordis）明文引用
  `{{model}}`/`{{cwd}}`（`agent.cordis.yml` 注释「resolve from the agent's own
  route and workspace」）；`SystemPrompt` 的 `assemble()` 采样 layers 里的
  `variables: VariableProvider`，`render_prompt()` 插值，未知变量 → Err；
  而 `assemble_server_runtime_with_llm` 此前从未注册任何变量。
- 裁决：**运行时变量归 host，不归 standing**（standing 只桥组合内容；变量是 host
  运行环境事实）→ 在 `assemble_server_runtime_with_llm`（serve 与测试共用装配路径）
  把 `model`（--llm-model）与 `cwd`（workspace-root）以 global layer `variable`
  注册。`{{cwd}}` 对单工作区宿主 = workspace root（真实近似：预设注释所言 per-agent
  route/workspace 在 C 段收敛前即此）。
- 验收（TDD 红→绿）：新测试 `server_runtime_variables_interpolate_into_standard_
  persona`（挂**真实** standard 预设 + join + render_prompt）先红复现 live 错误，
  修后绿（渲染含 model 与 workspace root）。dsh-cli **166/166** + agent-loop 1（167）；
  clippy `-D warnings` 零告警。
- **live E-03 验收（真机）**：重建 + 重启 60165（key 仅环境变量注入，不入库）——
  `agentPreset.list` 11 预设真实发现（含 A 决策的自定义 user presets）；select 后
  prompt 首轮成功，assistant 以 standard persona 陈述身份并列出**全部桥接工具类目**
  （文件/Shell/检索/终端/子代理/jobs/schedule/todo/图像）——组合真实改变会话行为 ✅。
  对照修复前：同一 prompt 首轮 turn/end error 即亡。
- **已知未接（诚实列表，P3-b 基础上收窄）**：web/tool-cordis/command-compact 保持
  broken；`{{cwd}}` 为单工作区近似（per-agent route C 段）；service/skill 相关行未桥；
  authoring RPC（copy/remove/P5）未做；C 段未动。
- **回滚**：`git revert` E-03 提交——撤销 serve 变量注册与测试，其余阶段保留。

### D-103 实施补记（P5：作者流实装——copy/remove 写用户根，live 验证，round 16）——决策 + 验收

- 触发：B 路径组合能力已 live 实跑（P1-P4），但 `agentPreset.copy/remove` 是
  P1-b 时代的诚实占位（`agent-preset-unsupported`）——用户无法用 RPC 创建/删除
  自定义 preset，authoring RPC（P5）欠账。
- 关键事实（自下而上核实）：`PresetHost` 已持 `user_root`（authorable 探测）+ 不
  缓存发现；copy = 在 `<user_root>/<new_id>/` 写下「组合逐字 + preset.yml」即被
  下一次 list 发现；wire 错误信封沿用 `{ok:false,error:{code,message}}`。
- 裁决：
  1. `AuthoringError` 枚举（`agent-preset-*` 前缀）：invalid-id / not-found /
     exists / not-authorable / readonly(system 拒删) / io(fail-loud)。**copy 目标 id
     去重按全 roster**——首根胜出下 system 同名会遮蔽 user 拷贝，故 reject（先删后建）。
  2. 元数据优先级：显式 name/description > 源 preset.yml > 无（不写 preset.yml）。
  3. RPC `copy {from, agentPreset, name?, description?}` → `{agentPreset:id}`；
     `remove {agentPreset}` 仅删 user（system → `agent-preset-readonly`）。
- 验收（TDD 红→绿）：preset_host 新 3 测试（复制写盘+roster 即见+逐字；非法 id/撞
  id/源未知/无用户根 fail-loud；system 拒删+user 删除目录即去）先红（E0599 缺方法）
  后绿；web E2E `rpc_agent_presets_list_read_real_discovery` 改为真实 authoring 循环
  （copy→list 见 user→read 逐字+显式 name→撞 id/not-found/invalid-id→system
  readonly→remover成功→目录去）。dsh-cli **169/169** + agent-loop 1（**170**）；
  clippy `-D warnings` 零告警；rustfmt 仅新独立文件 preset_host.rs。
- **live 验证（真机，net-zero 不碰真实预设）**：重建重启 60165 后——baseline 9 预设；
  copy `standard → p5-live-check-33040`（ok，trust=user，name 覆盖，read 首行=standard
  组合注释头逐字）；remove ok；**444444总量回 9**，用户真实 preset 分毫未动 ✅。
- **已知未接（诚实列表，对照前一轮收窄）**：web/tool-cordis/command-compact 保持
  broken；pwsh A 并行执行器；skill 最小只读；per-agent `{{cwd}}`（C 段）；C 段收敛未动。
- **回滚**：`git revert` P5 提交——回退 copy/remove 实装与测试，其余阶段保留。

### D-103 实施补记（P3-c：skill 最小只读目录桥——A-03 必须桥子集补齐，round 17）——决策 + 验收

- 触发：A-03 必须桥子集（fs/terminals 已桥、planMode/compaction 属 loop 级置 C）仅剩
  **skill = 最小只读**未落——`@deepseek-ai/dsh-skill-filesystem` 行仍落
  tool_guard_reason「no Rust bridge yet」；cordis preset 自带
  `skills/{editing-cordis-compositions, cordis-plugin-development}/SKILL.md`（真实资产）。
- 关键事实（自下而上核实）：composition 注释明示「baseUrl = 组合所在目录；customSkillDirs
  指向 `skills/`」→ skills 目录 = `<preset_dir>/skills/`；skill 文件在盘上（模型本可经 fs
  工具 read）。A-03 授权「复用现有 directives 装载」。
- 裁决：**skill = 目录段内容桥**（非加载器工具）——`mount` 新增 `mount_at(id, rows,
  base_dir, process)`（`mount` 委托 base_dir=None，向后兼容）；skill 行以
  `<base_dir>/skills/` 扫 `*/SKILL.md`，落 scoped 段 `preset:{id}:skills`（order 30：
  各 skill 名 + 摘要行 + 绝对 SKILL.md 路径，模型用 fs read 即用）。空目录仍 bridged
  （none found，诚实）；无 base_dir → guarded。`@deepseek-ai/dsh-tool-skill`（真加载器）→
  guarded「minimal read-only … loader tool 需宿主 skill service（C）」。web select 改
  `mount_at(entry.path.parent())`（真 base_dir）。
- 验收：新测试 skill_catalog_bridge_via_preset_base_dir（目录桥+joined 视图摘要/路径、
  空目录 none found、无 base_dir guarded——修复前行为=guarded「no Rust bridge yet」即红）
  绿；guard 测试 +tool-skill 断言。standing **10/10**，dsh-cli **170/170** + agent-loop 1
  （**171**）；clippy `-D warnings` 零告警；rustfmt 仅新 standing.rs。
- **已知未接（诚实列表，对照 P5 收窄）**：planMode/compaction 行桥属 loop 级（C）；
  web/tool-cordis/command-compact 保持 broken；pwsh A 并行执行器；skill 加载器工具
  （host skill service，C）；per-agent `{{cwd}}`（C）；C 段收敛未动。
- **回滚**：`git revert` P3-c 提交——回退 mount_at/skill 桥与测试，其余阶段保留。

### D-103 实施补记（P3-d：dsh-shell 双方言——bash/pwsh 平行能力 + 环境基修复，round 18）——决策 + 验收

- 触发：win32-A 决议「A 随 P3」（dsh-shell 平行 pwsh executor + dsh-terminal "pwsh"
  后端 + m5 tool-pwsh，随后撤 win32-B 覆盖）——P3 已完，pwsh 执行器第一步仍缺；
  dsh-shell 全程 bash 中心（`spec.bash_program`、argv `-c`）。
- TDD 红→绿：新测试引 `ShellKind`/`resolve_pwsh_program`/`LocalShellExecutor` →
  红（E0433/E0560/E0609，`bash_program` 字段不在）。
- 裁决：
  - `ShellKind{Bash,PowerShell}`；`ShellExecSpec.bash_program` → **`program` + `shell`**；
    `BashConfig` +`shell` +`pwsh_path`；新增 `resolve_pwsh_program`（PowerShell 7 安装
    候选 → powershell.exe 5.1 兜底 → 非 win32 裸名 pwsh）。
  - `LocalBashExecutor` → **`LocalShellExecutor`**（单一执行器按 `spec.shell` 分发
    argv：bash `-c cmd` / pwsh `-NoProfile -NonInteractive -Command cmd`）。**否决**
    双 LocalBash/LocalPowerShell 独立类型：超时/收集/kill/环境/spill 全共享，双份纯
    维护负担（A 的「平行」指能力平行，非类型成对）。
  - **★ 环境基修复**（pwsh 测试红暴露的真实 bug，同时修了 bash）：`assemble_env`
    原为**仅 4 个 ENV_OVERRIDES 键** → dsh-subprocess `env_clear` 清掉父环境
    （SystemRoot/TEMP/PATH…）→ PowerShell 5.1 初始化托管 .NET/DPAPI 崩
    （`8009001d`）；bash 在本沙箱被拒（msys 需 signal pipe/共享内存）同根因。修复 =
    **父环境 scrubbed 为基**（凭据形/`DSH_*` 键经 `dsh_subprocess::scrubbed_parent_env`
    剔除——key 纪律：`DEEPSEEK_API_KEY` 等**绝不**进模型 shell）+ ENV_OVERRIDES 覆盖
    + 调用方 env + 托管 `DSH_*` 最后（不可被顶替）。**否决**全量继承父环境（key 直落
    模型 shell，违背 key 纪律）；也**否决**改 dsh-subprocess `env:Some` 语义为 merge
    （影响全库既有隔离调用方，越界，留给独立决策）。
- 验收：`tests/executor.rs` **8/8**（7 bash + 1 pwsh 全**真实执行**；环境修复后本沙箱
  Git Bash 从「探测不可用跳过」变「真实跑」）；`tests/resolve.rs` **7/7**（新增 pwsh
  resolve：方言语义 + 显式 pwsh_path）；dsh-tools/agent-loop/cli/shell **410/410** 全
  绿；clippy `-D warnings` 零告警；rustfmt 仅 dsh-shell 三文件（lib/web_m5 精准 edit）。
- **已知未接（对 P3-c 收窄）**：dsh-terminal "pwsh" 后端 + m5 tool-pwsh + standing
  win32-B 撤「pwsh 判禁」覆盖（下一段 P3-e，随其 live 复验）；pwsh 输出编码
  （5.1 pipe 输出 console code page / UTF-16 歧义）在 tool 层处理；planMode/compaction
  桥（loop 级，C）；web/tool-cordis/command-compact broken；skill 加载器工具（C）；
  每 per-agent `{{cwd}}`（C）；C 段收敛未动。
- **回滚**：`git revert` P3-d 提交——回退双方言 API + 环境基修复与测试，其余保留。

### D-103 实施补记（P3-e：A 并行收口——pwsh 工具/终端后端在册 + win32 切回忠实门控，round 19）——决策 + 验收

- 触发：win32-A 决议「A 随 P3」收尾——P3-d 已给 pwsh 执行器，还缺 m5 tool-pwsh、
  dsh-terminal "pwsh" 后端注册、以及落地后的**忠实门控切换**（撤 win32-B「pwsh 判禁」
  覆盖）。
- 裁决：
  - **m5 tool-pwsh**：`ShellHost` 加平行 `pwsh: LocalShellExecutor`（同 root，配置
    shell=PowerShell）；`bash_tool` 重构为 `shell_tool(name, desc)`，新增 `pwsh_tool()`
    （同 schema/渲染词表，说明写 powershell5.1/pwsh7）；`bash_executor/bash_background`
    重构为 `shell_executor(name, shost, use_pwsh, bridge)/shell_background(...)`；
    `BashJobsBridge::start_bash` → `start_shell_job(kind, ...)`（kind="bash"/"pwsh"，
    subprocess producer 本就方言无关）。**否决** M5HostServices 加字段（会让 6 处测试
    struct 字面量全改；pwsh 执行器随 ShellHost 共存即可）。
  - **dsh-terminal "pwsh" 后端注册**：`TerminalBackendKind` 加 `PowerShell`；
    `PtyBackend::new(label, program, kind)`；**生产 M5Host::assemble 注册真实后端
    bash+pwsh**（resolve_bash/pwsh_program；spawn 失败诚实 NoBackend）——顺带修掉
    P3-b「terminal backend; host default shell」守卫文案在无真实后端时的过宣称。
  - **忠实门控切换**：`row_disabled_for_platform` 删除 win32-B 覆盖 → 恒等
    `row_disabled`；`host_tool_for_row` +`dsh-tool-pwsh(-persistent)`→"pwsh"；
    `tool_guard_reason` pwsh 分支改「unmapped pwsh-family（仅
    dsh-tool-pwsh/-persistent 桥到宿主 pwsh）」。win32 上组合自身 `disabled_expr` 决定：
    bash 系判禁、pwsh 系活化。
- 验收（TDD 红=旧 win32-B 期望在忠实门控下失效 → 更新测试为忠实期望 = 绿）：
  `guard_report_splits_bridged_disabled_guarded`（win32：bash 判禁、pwsh 活化）、
  `fs_and_terminal_groups_...`（工具集 +pwsh；bash-persistent disabled、pwsh-persistent
  bridged、joined 可见 pwsh）翻新；guarded-reason 的 pwsh 断言在新桥表下仍过（no host
  tool "pwsh"）。五 crate（tools/agent-loop/shell/terminal/cli）**438/438** 绿；clippy
  `-D warnings` 零；rustfmt 不整跑既有文件（standing.rs 可跑）。
- **已知未接（对 P3-d 收窄）**：pwsh 输出编码在 tool 层兜底（5.1 pipe 输出 console
  code page；ASCII/UTF-8 均成，中文场景随 P3-e 后 live 观察）；planMode/compaction 桥
  （loop 级，C）；web/tool-cordis/command-compact broken；skill 加载器工具（宿主 service，
  C）；per-agent `{{cwd}}`（C）；C 段收敛未动。
- **回滚**：`git revert` P3-e 提交——回退 pwsh 工具/终端后端/忠实门控与测试，其余保留。

## D-104（C 收官里程碑定稿：C 全量收敛 + F-05 WASM 组合引擎；用户拍板）：进入 C-B 分段

**日期**：2026（round-20 用户拍板。前置：B 全段 + P3-c/d/e + win32-A 全部完成验收；
C 范围/键空间/求值引擎三问先经**源证据简报**再定稿——方法论四「先跳出来看全局，权衡
显式报告再下单」。）

**第一性原理（自上而下 + 自下而上）**：
- 自上而下：C 是「组合权威归位 dsh-core」的收官——preset 组合不再是 web 壳的桥接层，
  而是 dsh-core 中按 agent scope 以 fiber 挂载的组合子树；loop 真正消费它；每阶段独立
  提交=回滚点，TDD 红绿 + live 验证与 B 同纪律。
- 自下而上（源证据核证）：
  - `deepseek-harness/packages/preset/agent-presets/src/mount.ts` = 我们 standing 的权威
    上游：join 键就是 dsh-scope 的 **ScopeKey**（agent key → `scopeParentOf` → standing
    key）；子树只读（preset 是输入，永不回写给文件）；**root-realm 泄漏守卫**
    （`leakedServices`：行把 service 发布进 ROOT 域 → 整挂载失败）+ **unusable-rows
    拒绝**（行停在不可用态 → 挂载否决）+ 挂载树随 agent fiber 展开。后三条我们 standing
    **还没有**。
  - `vendor/loader/src/config/utils.ts:5`：组合表达式求值 = `new Function(...)…
    return eval(expr)`——**harness 组合求值全程原生 JS，无任何 WASM**。
  - 我们的 `dsh-core`（fiber/registry/service/events/reflect）已存在 = 「开进」落点；
    `dsh-wasmrt` = WASM **插件**后端（适配 `dsh_core::Plugin`，另有可替换 loop），非组合
    引擎；`dsh-eval` = native 求值的忠实移植。

**考虑过的选项**：
1. **C-loop 完成（A）**：standing 留 dsh-cli，只做 loop 级内容桥（planMode 段注入 +
   compaction guarded）。成本 1 轮；但不动「组合权威归位」，架构归位不彻底。
2. **C 全量收敛（B，用户采纳）**：standing 机制整体搬进 dsh-core（PresetRuntime 服务按
   agent scope 以 fiber 挂载组合子树；移植 leakedServices root-realm 泄漏检测 + 挂载
   否决 + 随 fiber 展开），standing.rs 收成薄适配层。约 2-3 轮；触承重墙（fiber/
   registry），但达成目标文本「组合权威归位 dsh-core」。
3. C 收尾维护（否决：与目标文本相悖，用户未采纳）。

**★ 逐项决议**：
- **C 范围 = B 全量收敛**：新增 dsh-core 层组合子树挂载（PresetRuntime），root-realm
  泄漏守卫（真移植 leakedServices）+ unusable-rows 挂载否决 + 随 agent fiber 展开；
  standing.rs 收成薄适配层；loop 级内容桥（会话模式状态驱动 `dsh-plan-mode` 段注入；
  compaction 保持诚实 guarded——需 token 计量器/折叠管线，当前 prompt 从不折叠，做了
  无真实效用）。
- **F-05 = 仍要 WASM 组合引擎（用户明示，尽管源证据为原生求值）**：组合/disabled_expr
  求值另建 WASM 面（走 dsh-wasmrt，native 求值兜底/回退）。无上游先例——以 spike +
  TDD 先行压低不确定性；fidelity 主口径仍以 dsh-eval（原生）为准，WASM 面与其以结果
  一致性测试锚定。**客户已拍板，尊重明示选择，不默改。**
- **F-06 = 参照 harness 源码定案**：join 键 = dsh-scope **ScopeKey**（单身份轴
  agent==session，值比/不透明键，`mount.ts` 即此）；**无第二键空间**；ScopeId 留作品牌/
  展示（对齐 client 的 branded SessionId 是 wire 面、非另造键）。

**C-B 分段计划**（各阶段独立提交=回滚点；TDD 红绿 + live 复验）：
1. **K1 键/生命周期**：dsh-core 侧组合挂载原语（agent scope → fiber 子树 + scope-join +
   disposer），保留 ScopeKey 单键。**= dsh-core 自身 M1 里程碑**（`lib.rs` 自述 M0 限制：
   「isolate/intercept 作用域在 M1 引入，当前所有服务共享根作用域」；`types::ScopeId`
   已存在）——引入"agent 子作用域"承载 preset 子树挂载，正是 leakedServices 守卫能成立
   的前提（泄漏 = 贡献绕过 agent 子作用域落进根域）。
2. **K2 泄漏守卫**：root-realm `leakedServices` 检测（dsh-core reflect）+ 负例测试
   （root 注册 → 挂载失败）。
3. **K3 挂载否决**：unusable-rows 审计（显式 broken 集除外，D-103 兼容）→ 挂载失败。
4. **K4 薄适配**：web select 走新 runtime；standing.rs 收缩；旧测试迁移；live 复验
   （standard/cordis join 不回归）。
5. **F-05（并行/随后）**：WASM 组合引擎 spike → 结果一致性测试（WASM == dsh-eval）
   → 接入（native 兜底）。
6. C 段测试报告（相对当前 438 基线只增不减）+ clippy `-D warnings` + DECISIONS 逐段补记。

**回滚**：D-104 本身仅文档；K1..K5 以阶段为粒度 `git revert` 独立提交。**key 纪律不变**。

### D-104 实施补记（K1：agent-scope 组合挂载原语，round 21；TDD 红→绿）

- 触发：D-104 计划 K1（dsh-core M1 isolate 作用域 →「组合挂载原语」，ScopeKey 单键）。
- 事实基底（自下而上）：`FiberData.scope`/`isolate` 字段早已存在但 M0 恒为根（lib.rs
  自述「isolate/intercept 作用域 M1 引入」）；`collect_hooks` 只按 `scope == current`
  过滤（无 parent 链、无 root 可见性）；`pending_isolate` 无公开设置者（M3 未完成）；
  `alloc_scope` 用与 per-name 根作用域共用的 `next_scope`（首枚会= root 哨兵 1）。
- 实现（红→绿：先写 m70 单测 3 条连 API 都不存在→应按语义走；初跑即暴露
  alloc_scope=1 把 agent fiber 全变 root 的模型 bug）：
  - **scope 标签成真**：`Runtime.pending_scope: VecDeque<ScopeId>`（FIFO，多次挂载不
    互相覆盖）；`alloc_fiber` 弹队头否则继承 parent.scope 否则 1；仅 register_plugin
    一处调用点，零外部影响。
  - **独立隔离计数器** `next_isolate_scope`（基数 1e6）：agent/isolate 作用域与
    per-name root（自 1）及 root 哨兵=1 永不冲突——**首个挂载 scope 恒非 root**。
  - **hook 可见性 = harness filter**：`collect_hooks` `global || scope==1 ||
    scope==current`——untagged(root) 全局可见、agent 打标仅本会话；对所有既有 fiber
    （scope=1）恒真，**既有行为零变化**。
  - **root-realm 泄漏守卫** `audit_subtree(scope)`：owner∈子树的服务若落在
    `scopes[name]` 根域（未 isolate）→ 泄漏（复刻 mount.ts leakedServices）；owner∈
    子树却 root-scope 非 global 的 hook → 泄漏（防御）。
  - **门面**：`mount_scope()->(ScopeId, Disposer)`（unmount 卸载整棵子树、随 fiber
    展开）、`unmount_scope`、`current_scope`、`isolate(name, scope)`（M3 补，把当前
    fiber 的 isolate 指向 agent realm 的正路——抹掉「默认 provide 即 root = 泄漏」）。
- 验收：dsh-core 全绿（M 系列 + m70 3 条）；clippy `-D warnings` 零；四依赖 crate
  （tools/loader/wasmrt/cli）全回归 496/496 绿（基准 438 + dsh-core/M70）。live 行为
  不受影响（生产 loop 不消费 dsh-core 作用域）。回滚：`git revert` K1 提交。
- **下一步 K2**：unusable-rows 挂载否决（D-103 显式 broken 集除外）→ K3 薄适配。

### D-104 实施补记（K2：unusable-rows 挂载否决，round 22；TDD 红→绿）

- 触发：D-104 计划 K2（复刻 harness `inactiveRows` → 挂载失败；D-103 显式 broken 集
  除外）。
- 关键自下而上事实（决定精确规则）：
  - harness `mount.ts`：只用「等一个组合永远不提供的依赖」判 unusable → 否决；`disabled`
    行跳过；模块失败行已在 loader 层拒绝。
  - **真实 shipped preset 的 guard 状态**：standard/code/cordis 大量行（tool-fs/jobs/goal/
    todo/web/subagent/compaction/plan-mode…）是「no Rust bridge yet」诚实降级——其**意图
    已由宿主导线注册面满足**（read/write/edit、todo_write、goal_*、web_search、job_*…），
    不是「卡住」。若对其否决会误杀已验证的四个真实预设。
  - minimal 映射 str_replace_editor/fs-local/terminal 组，生产 M5 注册面全有（web_m5.rs
    register 六件套+终端六件套+bash+pwsh+str_replace_editor）。
- 裁决：**两分类规则**——`GuardKind::Stuck` 仅当「桥依赖不可满足」：`no host tool "X"`、
  `host tool group missing`、`terminal backend without a resolved terminal group`、`no
  shared tool registry in this host`、`no base dir`（skill 目录不可解析）；**其余**
  （No Rust bridge yet / broken per D-103 / tool-skill A-03 / 未映射 pwsh 系）= Honest
  降级，仅报告不否决。`StandingReport::unusable_rows()` 返回 Stuck 集。
- select 接线：web `agentPreset.select` 在 `mount_at` 后查 `unusable_rows()`；非空 →
  **拒绝挂载 + unmount 不留残留**（对齐 harness「rejection leaves nothing mounted」）+
  `agent-preset-mount-rejected` fail-loud 诊断。**否决 M5HostServices 加字段**（沿用
  ShellHost 模式）；**否决把「No Rust bridge yet」当 unusable**（误杀真实预设，见上）。
- 验收：new 5 测（真实预设 x4 生产宿主零回归安全网 / 映射行缺宿主工具 stuck /
  组+后端 stuck / D-103-A-03 降级不否决 / select 端到端拒绝+不留残留）；standing 15/15、
  dsh-cli lib 175/175、六 crate 全回归 **577/577** 绿；clippy `-D warnings` 零；live 60165
  **四个真实预设 select 全 OK**（K2 不回归）。回滚：`git revert` K2 提交。
- **已知未接/下一**：K3（standing.rs 收成薄适配层 over 新 core runtime）；K2 的「卡住」
  判定仍属 standing 层（组合走树处），K3 迁移时随迁 dsh-core 挂载审计。live 进程 term-31。

### D-104 实施补记（K3：standing 挂载本体归位 dsh-core agent-scope 子树，round 23；TDD 红→绿）

- 触发：D-104 计划 K3（standing.rs 收成薄适配层 over K1/K2 的新核心 runtime）。
- 架构裁决（对用户诚实）：**「循环平面迁移到 dsh-core」出作用域**——生产 loop 的
  SystemPrompt / ToolRegistry 平面不跑 dsh-core，强行搬迁移会打断已验证 live 或退化成
  仪式性换皮。K3 采取可验证的收敛：**standing 的挂载生存期 + 隔离 + 泄漏完整性归位
  dsh-core**，内容呈现桥保持 loop 平面直连（standing 收成「dsh-core 承载结构 + 桥」）。
- 实现：
  - 每个 `mount_at` 铸造真实 dsh-core agent-scope 子树：`mount_scope()` + 注册
    `PresetRecordPlugin`（挂载记录 fiber；apply 内 `isolate("preset.mount", scope)` 后
    `provide` 记录服务 → 落 agent realm，`audit_subtree` 判定干净）。可失败桥全部在记录
    注册**之前**完成，保证出错时不悬空 pending_scope/幽灵 fiber（桥出错 → 不铸造）。
  - `unmount` = undo(loop 平面) + `core.unmount_scope(core_scope)`（整树随 fiber 展开）。
  - select 接线（同 K2 位）：`core.audit_subtree` 非空 → `agent-preset-leak-rejected`
    fail-loud + unmount 不留残留（harness `leakedServices`）。
  - 故障注入缝（仅 cfg(test) `set_fault_root_leak`）：记录服务不 isolate → 落 root
    realm → 审计捕获——端到端验证泄漏拒绝路径；生产恒 false。
- 被否决：整 loop 迁移 dsh-core（出作用域，见上）；给 Standing 存第二个 ScopeKey（无；
    dsh-core ScopeId 即该 standing 的核心对应物，单身份轴保持）。
- 验收：+3 standing 测（4 真实预设核心子树 Active+审计干净+unmount 整树卸载 Disposed /
  root-leak fault 被捕获+unmount 清净）+1 select 端到端泄漏拒绝测（不留残留）；standing
  18/18、dsh-cli lib 178/178、六 crate 全回归 **580/580** 绿；clippy `-D warnings` 零；
  live 60165 **四个真实预设 select 全 OK**（K3 零回归，隔离记录不触发守卫）。回滚：
  `git revert` K3 提交。
- **已知未接/下一**：K4/F-05（WASM 组合引擎 spike：结果一致性 vs dsh-eval、native 兜底）。
  live 进程 term-32。

### D-104 实施补记（K4/F-05：WASM 组合求值引擎落地，round 24-25；TDD 红→绿）

- 触发：D-104 F-05（用户看「harness 原生 JS 求值、dsh-wasmrt 是插件后端非组合引擎」
  的源证据后**重申仍要 WASM 组合引擎**，记录在案勿默改）——组合/`disabled_expr`
  求值走 WASM 面、native 兜底，以结果一致性测试与 dsh-eval 锚定。
- 关键自下而上事实：
  - dsh-wasmrt 已是 dsh-core **插件**后端（wasmtime 34，C ABI 导出
    alloc/dealloc/plugin_apply/plugin_handle_event/plugin_dispose + host_provide）；
    `dsh-cli` **已依赖 dsh-wasmrt**（wasmtime 已在 web 二进制）→ F-05 落地**零新增
    依赖权重**。
  - dsh-eval 纯 std + serde_json，`wasm32-unknown-unknown` 可直接编。
- 实现（spike 成型 → 正式落地）：
  - **WASM 面** = `wasm-plugins/combo-eval/`：dsh-eval **同源编译进 wasm**（同一
    源码，非第二套求值器——一致性测试锚定的是 **WASM 执行路径本身忠实**：C ABI
    编组 / JSON 往返 / 数值 / 错误传播）；`plugin_apply({scope,expr})` 把
    `{ok,value,truthy}` / `{ok:false,error}` 经 host_provide 回传。
  - `dsh-wasmrt::combo`：`ComboEvaluator` trait + `NativeComboEvaluator` +
    `WasmComboEvaluator` + **`FallbackEval`（WASM 主面、native 兜底）**；dsh-wasmrt
    增依赖 dsh-eval（native 兜底面即其用途，非测试包袱）。
  - `dsh-agent-presets`：`row_disabled` 保持 native 权威；新增
    `row_disabled_with(row, process, eval)`——fail-closed + truthy **权威留在本
    crate**，只有「用什么引擎求值」可注入（签名与 ComboEvaluator 同构）。
  - `standing` 组合门控：`StandingRegistry` 默认 = WASM 主面 + native 兜底
    （blob 缺失自动回落 native-only），挂载行审计经 `row_disabled_with` 注入求值。
- 被否决：整 loop 搬 wasm（出作用域，K3 已述）；用第二套求值语义（拒绝语义分叉——
  wasm 面与 native 面同源，正是「一致性锚定」的正手）。
- 验收：m20 ×3（真实 preset 表达式×win32/linux 两面全等 + 门控翻转 + 全语法面语料
  值/错误串逐字节全等 + 4 真实 preset 逐行真实 facade 门控全等）；standing +2
  （注入引擎被行审计真实消费 + 默认 wasm 面）；dsh-agent-presets 18/18、dsh-wasmrt
  全绿、dsh-cli lib 180/180、八 crate 全回归 **644/644** 绿；clippy `-D warnings`
  零；live 60165 **四真实预设 select 全 OK**（WASM 面门控 = native 门控，零回归）。
  回滚：`git revert` K4 提交。
- **已接/下一**：K1..K4 全齐 → **C 段测试报告已交付**（`TEST_REPORT-BC-segments.md`，
  瀑布流关闸工件：交付范围/验收证据/644 全回归/live 复验/诚实边界/遗留决策），并呈
  「shipped preset 未桥行 → disabled:true vs 保持 guard 降级」给用户定夺。live term-33。

### D-105（round 25 末，用户拍板）：未桥行策略 + loop 级状态桥档位

- 触发：用户问「shipped preset 未桥行 → disabled:true vs 保持 guard 降级，有哪些决策点」；
  我列出 7 个决策点 + 倾向，用户逐条拍板（见四答）。
- 决策：
  1. **未桥面但宿主全局基已满足类**（tool-fs / fs-search / jobs / goal / todo / subagent /
     workflow / tool-skill 等）与 **plan-mode / compaction（即将桥）类** → **安排规划
     实现其桥接**，不纠结标注措辞（不 bulk 改 disabled）；「准备实现他们的桥接」。
  2. **其余（broken-D-103：web / tool-cordis / command-compact）→ 报错降级**：保持 guard
     （原因可见、fail-loud、不拒绝）——现状已符合，不改。
  3. **plan-mode → C 档**：完整 harness 语义——状态驱动段（组合行 config.section 随会话
     plan 模式注入）+ `exit_plan_mode` 真实执行器 + **approval 联动**。
  4. **compaction → 档位 3**：仅守卫段 + 接口预留（本轮不做真实压缩/摘要行为；
     tool-result-pruner 的 thresholdChars/headChars/tailChars 语义留接口）。
- 影响：`PLAN-loop-state-bridge.md` 定稿（新增「未桥面桥接」计划段）；broken-D-103 保持。
- 下一：按瀑布流逐段执行（U 桥接段 → L1 plan-mode C 档 → L3 compaction 守卫+接口），
  每段 TDD 红→绿、全回归、clippy 零、live 复验、DECISIONS 补记、独立提交=回滚点。

### D-105 实施补记（U1：未桥面首批桥接 — fs/family / jobs / todo，round 26；TDD 红→绿）

- 触发：D-105 决策 1（host 全局基已满足类 → 规划并实现桥接）；U1 = 首批发。
- 自下而上核对（宿主实际注册工具名，web.rs register_m4/m5_tools_with_host）：
  todo_write、job_output/job_list/job_kill、schedule_*/exit_plan_mode/workflow（M4）；
  bash/pwsh/fs 六件套/terminal 六件套/str_replace_editor（M5）。
  **没有 goal 模型工具**（goal = web RPC `goal_dispatch` + dsh-session-query
  `goal_projection`，非 agent 工具）→ goal 行**不桥**；
  **无独立 search 工具** → 搜索面 = glob（路径）+ grep（内容）。
- 桥接结果：
  - `dsh-tool-fs` → 组解析确认宿主 fs 六件套（compound fs == 宿主 fs 面，同 fs-local）；
  - `dsh-tool-fs-search` → 组解析确认 glob/grep（宿主搜索面）；
  - `dsh-tool-jobs` → 组解析确认 job_output/job_list/job_kill；
  - `dsh-tool-todo` → 单工具重呈现 todo_write（行 config description/timeoutMs 生效）；
  - `dsh-tool-goal` → 诚实 guard（专用原因，与预设注释「model-facing tool，service 在
    host 面」一致）。
  - 桥后「宿主工具缺」语义：组行缺宿主工具 = stuck（同 fs-local），符合映射行桥依赖
    必须满足；shipped preset 在生产宿主下不受影响（web 恒注册 M4/M5）。
- 验收：+2 测（synthetic 五行桥接/守卫 + 真实 standard/code/cordis 呈现断言）；
  standing 21/21、dsh-cli lib 182/182、八 crate 全回归 **646/646**、clippy `-D warnings`
  零、live 60165 四真实预设 select 全 OK（零回归）。回滚：`git revert` U1 提交。
- 下一：U2（subagent 家 / workflow / ralph / ask-user）→ U3（tool-skill 等保持 guard
  原因收口 + 安全网）→ L1（plan-mode C 档）。

### D-105 实施补记（U2：下伸面 honest 呈现 + 静态 `disabled: true` 保真，round 27；TDD 红→绿）

- 触发：D-105 决策 1 的 U2（subagent 家 / workflow / ralph / ask-user）。
- 自下而上核对（dsh-tools/m4.rs 全清单 + web 注册面）：**没有** subagent / ralph /
  ask-user 的模型工具定义与注册；subagent 仅存在于内部运行时（dsh-subagent crate +
  subagent_runtime.rs + 会话 subagent 投影 + jobs kind "subagent"）——模型**无法**发
  subagent 调用；M4 `workflow` 恒注册但为**桩**（执行 → UNSUPPORTED_OPTION）。
  自上而下说「桥接这些行」，自下而上说「宿主没有可调用工具」——第一性原理裁决：
  不为「快」伪造桥，诚实 guard；不把「工具在目录、调用 fail-loud」说成「未桥」。
- 决策/实现：
  - `dsh-tool-workflow` → **桥**到宿主 `workflow`（M4 恒注册；注册即见、执行 fail-loud
    UNSUPPORTED_OPTION；guard 的「no bridge」说法反而不实）；
  - `dsh-tool-subagent-control` / `.../list-agents` / `dsh-tool-subagent` →
    **诚实 guard**（专用原因：内部运行时/RPC 非 agent 可调用工具）；
  - `dsh-workflow-worker-thread` → guard（M4 桩、无 worker-thread 后端）；
  - `dsh-tool-ralph` / `dsh-tool-ask-user` → guard（无宿主工具）；
  - **parse 保真修复（U2 附带）**：honor 静态 `disabled: true`——preset 作者显式禁用的
    行（subagent codex/claude-code，需宿主装对应 Bundle）此前被解析成活化→守卫（误报
    「未桥」）；现与 `disabled_expr` 同等判禁（进 disabled 不进守卫）。`CompositionRow`
    增 `disabled: bool`，`row_disabled_with` 静态短路径。
- 验收：dsh-agent-presets 19/19（+1 静态禁用）、standing 23/23（+2：synthetic 五行 +
    真实 3 预设呈现）、八 crate 全回归 **649/649**（m20 WASM/native 一致性不受影响）、
    clippy `-D warnings` 零、live 60165 四真实预设 select 全 OK（零回归）。
  回滚：`git revert` U2 提交。
- 下一：U3（tool-skill 等保持 guard 原因收口 + 安全网测试）→ L1（plan-mode C 档）。

### D-105 实施补记（U3：guard 原因收口 + 安全网测试，round 27；TDD 红→绿）

- 触发：D-105 U3（tool-skill 等保持 guard 的原因收口 + 安全网）。
- 枚举（4 真实预设全部行名）：U1/U2 后仍落泛化「no Rust bridge yet」的行 =
  `dsh-plan-mode`、`dsh-compaction-basic`、`dsh-compaction-tool-result-pruner`、
  `dsh-agent-tool-presentation`（仅 code）。全部给**经过决策的专用原因**：
  - `dsh-plan-mode` → L1 (D-105 C 档) pending：config.section 状态驱动注入 +
    exit_plan_mode 真实执行器 + approval 联动（显式待桥，不伪装 bridged 也不落泛化）；
  - `dsh-compaction-basic` / `dsh-compaction-tool-result-pruner` → L3 (档位 3)：
    守卫段 + 接口预留（thresholdChars/headChars/tailChars 语义留接口、不行为）；
  - `dsh-agent-tool-presentation` → standing 桥已对单工具行按 config 逐行重呈现，
    即该装配期呈现变换的宿主落地（U3 显式标注）。
- 安全网测试：真实 4 预设 × 生产宿主 → **任何守卫行原因都不允许落入泛化**
  （防未来新行悄悄掉进「no Rust bridge yet」黑洞）；且无 stuck。
- 验收：standing 24/24（+1 安全网）、八 crate **650/650**、clippy 零、live 四预设
  select OK。回滚：`git revert` U3。
- 下一：**L1（plan-mode C 档）**——先自下而上核对 harness 语义 + 宿主 approval 现状。

### D-105 实施补记（L1 核心片：plan-mode 状态驱动段，round 28；TDD 红→绿）

- 触发：D-105 L1（plan-mode C 档）；本轮交付**第一片（状态驱动段）**，执行器/approval
  联动留轮（见诚实边界）。
- 自下而上核对：`dsh_system_prompt::PromptSectionText::Fn(Rc<dyn Fn(&AssembleContext)
  -> String>)` **已存在**（动态段不是缺口——早前「设计缺口」判断是误判，纠正）；
  `sandbox/mode → resolve_sandbox_mode → sandbox_policy_segment` 是宿主「会话模式 →
  提示段」的既有模式；`exit_plan_mode`（m4.rs：521）定义意图 = 宿主注入「离开 plan
  mode + 写 command 事件」，未注入 → NOT_BOUND。预设注释：「Plan state is per-agent by
  nature…entry-local realm is the correct lifetime」——状态**随 standing（agent 作用域）**。
- 实现：
  - standing 挂载 `dsh-plan-mode` 叶行 → config.section 经 `PromptSectionText::Fn`
    注册 scoped 段（order 55 < 工具指引带，满足预设文本「override 更晚工具指引」；
    与 skills 30 区隔）；Fn 组装期读 standing 的 plan_mode cell（`Rc<RefCell<bool>>`，
    **per-agent 本性**）——active → 注入 section 原文，否则空串。
  - `StandingRegistry::set_plan_mode(id, bool)` / `plan_mode(id)`（host 翻转；未知 id
    fail-loud；无 plan-mode 行 → 翻转无害 cell）。
  - report：plan-mode 行 → **bridged**（section bridge）；config.section 缺失 → 诚实
    guard（非泛化）。移除 tool_guard_reason 的 L1-pending 分支（行已由桥循环接管）。
- 诚实边界（下轮需求结论后再定，不臆造）：`exit_plan_mode` 真实执行器（web 会话宿主
  接线：离开 plan mode + 写 `plan/mode` 与 command 事件）；approval 联动——预设文本本身
  即「规则 override 更晚工具指引」的**指令层**语义，approval 联动是否再加执行层
  （ApprovalProvider 联动）待定。
- 验收：standing 25/25（+1）、八 crate **651/651**、clippy 零、live 60165 四真实预设
  select 全 OK（plan-mode 行转入 bridged 不回归）。回滚：`git revert` L1-slice。
- 下一：exit_plan_mode 真实执行器（web 会话宿主接线）+ approval 联动需求结论。

### D-105 实施补记（L1 执行器 + approval 联动：需求/设计关闸，round 28；实现未做）

- 触发：L1 C 档余片。按自下而上核对后**定接线方案**，如实标 NOT_BOUND 不伪造。
- 已核对事实：
  - `ToolExecutionInput.agent: Option<String>`（dsh-tools runtime:166）——执行回调
    携带调用方 agent 身份；
  - `AgentLoopHost::join_standing`（host.rs:341）只存 `joins: agent_id → binding`，
    **不记 preset id**；`configured_for_session` 给出 session→agent 配置（含 session_id）；
  - live boot（web.rs:269）在 `assemble_server_runtime` **之后**才重设 `boot.standings`
    （用 host.prompt / host.tools）→ exit_plan_mode 的闭包不能在装配期捕获最终
    standings：
- 接线方案（设计定案，下轮实施）：
  1. select 处理记录 `session → active-preset`（boot 侧 `Rc<RefCell<HashMap>>`）；
  2. serve 期（standings 重设后）把 `exit_plan_mode` 绑定为闭包：`call.agent` →
     active-preset → `standings.set_plan_mode(preset, false)` + 追加会话事件
     （具体事件类型下轮核 dsh-session 既有 schema，缺则新增 plan/mode 面——不臆造）；
  3. `enter_plan_mode` 宿主入口（GUI/loop 状态源）随执行器一并定（进入点在会话/UI）。
- **approval 联动裁决**：预设文本即「plan-mode rules override 任何更晚工具指引 /
  工具保持列出以不改变目录」的**指令层**语义（harness 正路——harness 自身也以指令
  约束，非执行层强制）；slice-1 已注入该文本。执行层联动（ApprovalProvider 在 plan
  模式自动 deny/ask mutation 工具的 execute）是**宿主导线策略**、非预设契约（M3
  approval 往返在 loop 之外），并入 approval RPC 里程碑。**呈用户确认**：C 档
  execution-layer 联动是否本轮跟进（若要求，下一轮单独一段实现 + 验收）。
- 诚实边界：未实现执行器前 exit_plan_mode 保持 **NOT_BOUND**（工具注册在，
  执行时明确报错），不冒 web 半吊子接线风险。
- 验收：本篇为设计关闸工件；执行器实现段的验收另记。回滚：无代码变更。
- 下一：§接线方案实施（或用户裁决 execution-layer 联动的段）→ 全回归/clippy/live。

### D-105 实施补记（L1 执行器 + 折叠接线，round 29；TDD 红→绿）

- 触发：D-105 L1 余片（exit_plan_mode 真实执行器）。自下而上发现 **`dsh-plan` crate
  已存在**（`fold_plan_mode` 折叠 + `exit_plan_mode_check` 三重前置：in-plan-mode /
  `# 标题` / 评审通道）——harness 的 plan-mode 语义权威实现，**纠正 slice-1 设计**：
  standing cell 是第二状态源（exit 后与事件折叠分叉），改回**单一权威态 = 会话
  `plan/mode` 事件日志（纯重放，无 live mirror 无第二状态）**。
- 实现：
  1. **standing 重构**：删除 per-standing `Rc<RefCell<bool>>` cell 与
     `set_plan_mode`/`plan_mode` API；改注册表级**可注入折叠源**
     `PlanModeSource = Rc<RefCell<Option<Rc<dyn Fn() -> bool>>>>`（post-装配注入）；
     Fn 段组装期 `is_some_and(active)` → 注入/缺席。slice-1 测试改为可控折叠源替身。
  2. **`web::dsh_cli_host::PlanModeHost`**：agent→session 归属（登记优先 → agent 即
     会话名 → 默认）+ `plan/mode` 事件追加 + `dsh_plan::fold_plan_mode` 折叠 +
     `dsh_plan::exit_plan_mode_check` 前置；`enter`/`exit` 只落事件，不维护二态。
  3. **exit_plan_mode 真实执行器**：`M4HostServices.plan_mode` 在场 → bind
     `exit_plan_mode_with_host_executor`（前置失败 → 结构化 `PlanModeError`、**非**
     NOT_BOUND；通过 → `{approved:true}` + 落 `plan/mode{active:false}`）；缺席保持
     NOT_BOUND 诚实。
  4. **live 接线**：`assemble_server_runtime_with_llm` 构造 PlanModeHost
     （`review_channel=true`——GUI user-questions 面在场；loop 级 ApprovalProvider 属
     M3，不影响 exit 前置通道判定）；serve 期 standings 重建后注入折叠源（fold
     `boot.plan_session` 的会话事件；select 记录 plan_session = 最后一次 select 的会话）。
- 诚实边界/caveat：single-active GUI——折叠源折叠「最后一次 select 的会话」（standings
  按 preset-id 挂载且单活跃）；多会话共享某 standing 的 per-agent plan-mode 保真留白
  （若需多会话并发异态，需 standing join 期记 agent→session，另段）。approval
  **execution-layer 联动仍待用户裁决**（§设计关闸：指令层已随段注入，执行层属宿主导线
  策略并入 approval RPC 里程碑）。
- 验收：standing 25/25、plan-mode 测试 3/3（折叠/前置逐点/执行器绑定）、八 crate
  **655/655**、clippy 零、live 60165 四真实预设 select OK。回滚：`git revert` L1-executor。
- **approval 联动收口（round 29 用户裁决）**：**指令层优先，执行层并入 approval RPC
  里程碑**——L1 approval 联动以指令层（预设文本如 harness 正路，随段注入）交付；
  `enter_plan_mode` 宿主入口（PlanModeHost.enter 已备）与 GUI/loop 状态源随该里程碑
  一并物化；多会话共享 standing 的 per-agent plan-mode 保真另段。
- 下一：L1 收口（本段闭）；approval RPC 里程碑承接执行层联动 + enter 宿主入口 + 多
  会话 plan-mode 保真；TEST_REPORT §10 已落裁决。

### D-106 需求分析（approval RPC 里程碑，round 1；无实现）

- 触发：用户指示按流程推进 approval RPC 里程碑（承接 D-105 L1 裁决「执行层并入
  approval 里程碑」）。规划工件：`PLAN-approval-rpc.md`（需求关闸）。
- 自下而上核对（新增事实，D-105 未覆盖）：
  - ApprovalProvider 缝同步单次裁决、**生产 web 未注册**、**dsh-agent-loop 不消费
    审批缝**（grep 零命中）；`add_pre_decision` 注入缝现成（web_m5 hook 先例）；
  - loop 同步执行工具，**无 turn 暂停/恢复**——真实异步 UI 轮询需 loop 异步缝（大改）；
  - `commands/list` 已声明 `/plan`（`[off|message]`）但**无执行 RPC**——enter 宿主
    入口本 build 缺；
  - ApprovalAsked/Decided/Policy 事件词已存在但**零消费者**；
  - harness 语义对照（web 检索）：plan-mode 入口即 `/plan` 命令，进入/离开落
    `plan/mode{active}`（fold 纯重放，与 dsh_plan 一致；[plan-mode README](
    https://github.com/deepseek-ai/DeepSeek-Harness/blob/master/packages/plan/plan-mode/README.md)）。
- 三段：S1 enter/leave 宿主入口（`/plan` 执行面）；S2 执行层联动（plan-active →
  mutation 工具强制审批：Asked/Decided/Policy 事件 + AllowedOnce/Rejected +
  fail-closed）；S3 per-agent 保真（范围核算）。
- 决策点（已呈用户）：**D-a** 执行层同步落地边界（选项 A 同步 fail-closed（推荐，
  不碰 loop 构架）/ 选项 B 本轮异步 UI 往返（需 loop 异步缝，大改））；**D-b** mutation
  工具集清单；**D-c** 判定作用域（agent→session 折叠）。
- **用户裁决（round 1）**：**D-a = 异步 UI 往返（本轮即做）**——改造 dsh-agent-loop
  为异步工具门（turn 暂停/恢复），plan-active mutation → ApprovalAsked + 挂起 →
  GUI → approval/decided(allowedOnce|rejected) → 放行/拒绝；**D-b = 采纳提案清单**
  （fs write/edit、terminal open/send/signal、bash/pwsh、str_replace_editor、
  run_code、schedule create/delete、job_kill；read 系不拦）；**D-c/S3 = 留后续**
  （执行层判定走 agent→session 解析；prompt 段 per-agent 另段）。
- 验收关闸见 PLAN §2；本条目是需求阶段工件，回滚 = 无代码变更、删/改本规划即可。
- 下一：**系统设计阶段（当前）**——S1 wire/RPC 面 + S2 loop 异步工具门设计决策 → TDD。

### D-106 设计决策（approval RPC 里程碑，round 1；无实现）

- 触发：需求关闸通过（D-a 异步 / D-b 清单 / D-c 留后续）；进入设计。
- 自下而上补核（设计关键）：loop 驱动为同步 inline drain（send/kick 前整个 driver
  排空，D-032）；`turn()` 主循环 step→tool_exec→续步，tool_exec 是可注入缝
  `Rc<dyn Fn(&ToolExecCtx)->ToolExecOutcome>`；pre_step 空 inbox 仍给 Enter+assembly
  （step0+空消息短路在 turn() line~523-527）；phase 有 Running/Idle 停驻（kick 循环后
  set Idle，恢复需重踢新 turn）；invariant 只查 step 闭 + open_turn（turn/end 不要求
  pending_calls 空）且 tool/result 跨 turn 删 call 成立（line 197 只查属主）；web 经
  `AgentLoopHost::with_store` + followup 驱动，`run_rust_loop` 返回 `{"accepted":true}`。
- 设计定稿（PLAN §4）：**loop 只加通用 pending 机制**（ToolExecOutcome.pending +
  ToolExecCtx.resume + 恢复只追加 result 防重复 tool/call + `TurnEndReason::Approval
  Pending` + agent 级 approval_pending（非 Phase，越过 Idle 存活）+ turn 短路条件 +
  AgentLoopHost.kick（bare-wake））；**策略全在宿主**（ApprovalGate：D-b mutation 集/
  plan_active 经 agent→session fold/fold_decided/emit_asked/合成拒绝；tool_exec 包装；
  `session.approval.decide` RPC → approval/decided + kick 恢复；run_rust_loop 返回
  approvalPending 面）。S1 = `session.plan.mode{active, message?}`，宿主 leave 无
  heading 前置（模型 exit_plan_mode 保持三重前置不变）；approval/policy 首条诚实宣告
  作用域。
- 事件契约 / 不变量相容论证 / 分拆提交（A loop 机制 / B 宿主策略 / C S1 RPC，各独立
  回滚点）/ 风险（A 状态机最险→先独立全量回归）见 PLAN §4.2-4.5。
- 回滚：未实现，改 PLAN/DECISIONS 即可；实现后 `git revert` 各段。

### D-106 段 A 实施（loop pending 工具调用机制，round 2；TDD 红→绿）

- 触发：设计关闸通过（PLAN §4）；段 A 是 loop 通用机制（策略在宿主层）。
- 自下而上核证（实现期）：turn() 主循环 step0+空 inbox 空消息短路在 line~525-527；
  `pre_step` 空 inbox 仍给 Enter+assembly（恢复 turn 可行）；Phase 为
  Idle/Maintenance/Running 枚举 → `approval_pending` 存 agent 级
  `RefCell<Vec<PendingCall>>`（越过 Idle 停驻存活；abort/dispose 不主动清——由 decide
  决定归属）；invariant 对 turn/end 只查 step 闭 + open_turn 匹配，pending_calls 跨
  turn 删除成立（tool/result 新 turn 删 pause 步 call）。
- 实现（dsh-session + dsh-agent-loop）：
  - `TurnEndReason::ApprovalPending`（serde kind `approval-pending`；诚实收尾——非错、
    非完成）。
  - `PendingCall { block, call_seq }`；`ToolExecOutcome.pending`；`ToolExecCtx.resume`。
  - `execute_tool_calls` + `resume: &[PendingCall]`：resume 非空 → 只 execute +
    `append_tool_result`（复用 call seq，绝不重复 tool/call）。
  - 新公共助手：`emit_pending_calls`（落 tool/call、返 PendingCall）、
    `append_pending_rejection`（合成拒绝 result，code `TOOL_REJECTED`）。
  - driver：step() 顶部消费 `approval_pending`（resume 语义重跑，结果落会话后走 LLM；
    仍未决 → 再次暂停；concluded → Completion）；tool_exec 后 pending 非空 → 存 +
    `ApprovalPending`；turn() 空消息短路门控 `approval_pending.is_empty()`；
    `kick_resume()`（bare-wake，仅 Idle∧pending 非空，fail-loud）；AgentLoopHost.kick。
  - 服务直通路径 `pending: Vec::new()`（永不停审；宿主包装注入）。
- 分家论证：策略（mutation/plan/决策）在 web 宿主段 B 接入；loop 不改 approval 语义。
- 测试（TDD）：暂停 reason + 无继续 + Idle 停车 + pending 留驻；kick_resume 重跑
  pending → 模型续发 → 双 TurnEnd（approval-pending→completed）；无 pending fail-loud；
  resume 只追 result 不重 call；合成拒绝 seq/码/error 语义。（红先绿后全绿。）
- 验证：dsh-agent-loop + dsh-session clippy `-D warnings` 零；全 workspace 回归（段 A
  独立全量，见 segA-regression.txt）；live 复验推迟到 D 段（live 需 host 接线后才改观）。
- 回滚：`git revert` 段 A 提交（含 types/ToolExecCtx/Outcome/driver/helpers/tests）。

### D-106 段 B 实施（执行层审批策略：宿主 ApprovalGate 缝 + decide RPC，round 2；TDD 红→绿）

- 触发：段 A（loop pending 机制）提交 `5b22bd6`；策略段 B（web 宿主）。
- 自下而上核证：`AgentLoopHost` deps 在装配期定死（driver 私有）→ loop 无 post-hoc
  换 tool_exec 缝 → 加**宿主注入缝**：`ToolExecFactory`（按 driver 事实产 tool_exec，
  须在 ensure_agent 懒创建前设；未设 = service 直通，行为逐位不变）。web 装配
  `assemble_server_loop` 在 with_store 后设 factory。dsh_plan::fold_plan_mode 直接
  折叠**该 driver 自己的会话事件**（driver 即会话绑定，无需 agent→session 映射）。
- 实现（dsh-agent-loop + dsh-cli）：
  - loop：`host.rs` 增 `ToolExecFactory` 类型 + `tool_exec_factory` 字段 +
    `set_tool_exec_factory` + ensure_agent 分支（有工厂 → `create_loop_agent_with_tool_exec`）；
    `service.rs` 增该构造（其余 deps 同直通）。
  - web `src/web/approval.rs`：`approval_tool_exec_factory`（plan 非激活 → 直通；
    plan active ∧ mutation(D-b 清单) → `emit_pending_calls` + `approval/asked` + pending；
    resume：`fold_decided` allowedOnce → 只追 result；rejected → `append_pending_rejection`
    (`TOOL_REJECTED`)；未决（防御）→ 重发 asked + 再停留）。`decide()`：写
    `approval/decided` + `host.kick`（不伪造批准，无决策拒绝/停留）。
  - `run_rust_loop` 返回面：`Ok(Vec<String>)`（仍待审批调用 id）；agent.turn /
    session.prompt RPC 回 `{"accepted":true,"approvalPending":[...]}` 弹窗载体。
  - RPC `session.approval.decide {toolCallId, decision: allowedOnce|rejected}`。
- 硬纪律：read 系 + plan 非激活全直通（与既有行为逐位一致）；key 永不落盘。
- 测试（TDD）：mutation 清单覆盖 D-b + read 豁免；plan 非激活直通（无 pending 无
  asked）；plan active mutation → pending+asked+不执行、read 仍直通执行；
  resume allowedOnce → 只追 result 不重 call；resume rejected → 合成 `TOOL_REJECTED`
  错误 result 不执行。（红先绿后全绿，5 tests。）
- 验证：dsh-agent-loop + dsh-cli clippy `-D warnings` 零；全 workspace 回归
  （segB-regression.txt）；live 复验推迟到 D 段。
- 回滚：`git revert` 段 B 提交（loop 缝 + approval.rs + RPC + 返回面 + tests）。

### D-106 段 C 实施（S1：宿主 plan-mode 入口/出口 + approval/policy 宣告，round 2；TDD 红→绿）

- 触发：段 B（执行层审批策略）提交 `53e5863`；S1 是用户侧入口面。
- 自下而上核证：`PlanModeHost`（M4 exit_plan_mode 工具绑定 + 三重前置）已在 D-105；
  standing 折叠源（`boot.plan_session` + `dsh_plan::fold_plan_mode`）已在 web 装配；
  `EventKind::PlanMode/ApprovalPolicy` 词已在 dsh-session。缺口 = 用户侧「进入/离开」
  RPC（宿主动作，不经过模型工具前置）。
- 实现（dsh-cli web）：
  - `approval::set_plan_mode(boot, active, message?)`：落 `plan/mode`
    （`{active, message?}`）+ `approval/policy`（`{active, scope:"mutation",
    tools:[D-b清单]}` 诚实宣告）到 `boot.plan_session` 目标会话；standing 折叠段随事件
    注入/撤下。
  - RPC `session.plan.mode {active, message?}`。**进入与离开都无前置**：宿主 leave 是
    GUI 用户显式动作，不要求 plan heading；模型 `exit_plan_mode` 保持 dsh_plan
    三重前置不变（两者分面）。
- 测试（TDD）：进入 → 折叠可见 true + `plan/mode{active,message}` + `approval/policy
  {active,scope,tools=D-b}`；离开 → 折叠 false（无 heading 前置）+ policy active:false。
- 验证：dsh-cli clippy `-D warnings` 零；全 workspace 回归（segC-regression.txt）；
  live 复验推迟到 D 段。
- 回滚：`git revert` 段 C 提交（approval.rs + web.rs 分支 + 测试）。

### D-104 实施补记预留




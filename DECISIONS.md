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



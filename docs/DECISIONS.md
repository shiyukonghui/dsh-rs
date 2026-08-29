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

### D-106 段 D 收口（approval RPC 里程碑完成，round 2）

- 触发：段 C 提交 `2bbaa68`；里程碑门闸（回归/clippy/live/TEST_REPORT）。
- 全 workspace 回归：段 A/B/C 各独立全量 **191/191 套件、0 真失败**
  （segA/segB/segC-regression.txt；逐段含新增测试；segB/segC 已并回库）。
- clippy `-D warnings` 零（三个受影响 crate + workspace 全量）。
- live :60165 复验（新二进制）：serve 起服 + `host.describe`；`session.plan.mode`
  进入 → `plan/mode` + `approval/policy` 落会话 + fold 投影 active:true 即时可见；
  `session.approval.decide` 未决 → fail-loud 结构化错误。
- **环境阻塞呈报（非代码缺陷）**：live 真机模型回合被 `api.deepseek.com` HTTP_418
  拦截（`deepseek-v4-flash-0731-ext` 与 `deepseek-chat` 均 418），各换模型名自修无效 →
  判定网关/凭据环境问题；真实模型驱动「mutation→弹窗→decide→续跑」GUI 目视待用户端
  修复后复验（确定性路径已由 approve 5/5 + S1 1/1 + 段 A driver 恢复系列覆盖，均走真实
  装配）。处理过程已记入 TEST_REPORT §12；未因环境问题妥协架构或功能。
- TEST_REPORT-BC-segments.md 追加 D-106 章（§11-13）。
- milestone 目标：①② 完成；③（S3 per-agent 保真）与 D-c 一致留后续。

### D-106 段 D live 复验补（round 2 续，真机全闭环达成）

- 触发：`api.deepseek.com` 418 后用户澄清——key 属**自部署网关**，base =
  `http://100.105.152.101:18080/v1`、model = `deepseek-v4-flash-0731-ext`、key 同前。
- 自修/排障：按自部署 base 重启 live → 首轮模型回合报 `NETWORK malformed HTTP
  response`。诊断（curl/裸 socket 探针）一度得 401——**查明是跨 `term` 进程取
  `$env:DEEPSEEK_API_KEY` 为空**（探针自身伪影，非网关拒绝）。同进程带真 key 复测：
  认证通过（401 消失）。恢复**已验证可用 live 配方**（term-2..38：
  `target\web\cordis.yml` + `--workspace-root` + `--sqlite-store` + 自部署 base/model +
  key 仅 env）重起。
- **真机全闭环（live :60165，自部署网关真实模型）**：
  - plan 激活（`session.plan.mode`）→ `plan/mode` + `approval/policy{scope:mutation,
    tools:D-b}` + fold 投影 `active:true`；
  - 模型调 `bash` → `approval/asked{tool:"bash"}` + `tool/call` + turn `approval-pending`
    挂起 + **`tool/result` 0（未执行）**，`session.prompt` 返回 `approvalPending:[callId]`；
  - `decide{allowedOnce}` → 恢复 → `tool/result{isError:false, "hi-live-approval\n"}`（真
    执行）→ 模型续跑 completed；
  - `decide{rejected}` → 恢复 → `tool/result{isError:true, "the user rejected tool
    \"bash\""}`（不执行、合成拒绝）→ 模型续跑 completed。
- 结论：执行层审批（D-b mutation 集 + 一次性 allowedOnce/Rejected）在**真实网关 + 真实
  模型**下逐帧验证；此前「环境阻塞」判定撤销（根因是我跨进程取 key 的探针伪影 + 旧
  配置变体瞬时抖动）。key 纪律不变（仅 live 进程 env 注入）。

### D-107（S3 需求结论 + 设计，round 3；无实现）

- 触发：S3 = D-106 里程碑目标③「多会话共享 standing 的 per-agent plan-mode 保真」——
  审批执行层已是 per-driver 折叠（段 B），但**standing 提示层的 plan-mode 段折叠源是
  全局的**（`boot.plan_session` 最后一次 select 的会话），多会话共享一预设时 A 的提示
  会带上 B 的 plan 状态。
- 第一性原理（目标剥到底）：「plan 激活 → 该 agent 系统提示含 dsh-plan-mode 段」的
  per-agent 真义 = **折叠该 agent 自身会话的 plan/mode 事件重放**（`fold_plan_mode`），
  与「GUI 上次选中哪个会话」无关。做对它就同时满足单/多会话。
- 自上而下：按组装 agent → 拿会话身份 → 折叠该会话。自下而上：`AssembleContext` 现
  只带 `scope`；`dsh_agent::assemble_context_for(agent)` 是唯一从 Agent 构造点（loop
  agent.rs:713 组装走它）；`Agent.id = 会话 id`；standing plan-mode 段注册为
  `PromptSectionText::Fn(&AssembleContext)` 但当前忽略 ctx。两向可遇：**给 AssembleContext
  补会话身份，折叠源按 ctx 身份解析**。
- 验收：两会话共享 standing，A 进 plan → A 组装含段、B 不含；翻转亦然；无身份组装
  （None）回退 boot.plan_session（原单活跃行为保留，兼容既有测试）。
- 设计决策：
  - D-S3.1 `AssembleContext` 增 `session_id: Option<String>`（additive、Default 保持）；
    既有字面量补 `..Default::default()`。
  - D-S3.2 `assemble_context_for` 填 `session_id = Some(agent.id)`。
  - D-S3.3 standing 折叠源 `PlanModeSource` Fn 签名改 `Fn(Option<&str>) -> bool`（组装
    会话 id；None→全局回退）；plan-mode 段 Fn 传 `ctx.session_id.as_deref()`。
  - D-S3.4 web.rs 折叠解析器抽 `plan_mode_resolver(plan_session, store)` 便于单测：
    Some(sid)→fold 该会话；None→fold plan_session。语义：身份在场 per-agent 权威，
    boot.plan_session 仅 None 回退（GUI `session.plan.mode` 本就写目标会话，per-agent
    折叠自然反映）。
  - 被否决：scope→session 注册表解析（隐式 + 费一张表 + 难测）；线程局部/全局
    override（脆弱不诚实）；保持全局源继续（正是被修缺陷）。
- 回滚点：D-S3.1/3.2 一个提交、D-S3.3/3.4 一个提交，可独立 revert。

### D-108（GUI 里程碑需求结论 + 设计，round 3；Rust 实现待做，fork 分支已建/已装依赖）

- 触发：D-106 收口后用户许可「GUI 任务」——让 **DeepSeek Harness 前端（项目内源码
  fork `deepseek-harness`，新分支 `feature/approval-gui`）** 对我们的 Rust serve 的
  approval 弹窗闭环可用；安装包（npx）零接触；依赖安装与测试由我完成。
- 需求核证（自下而上，读 fork 源码权威契约）：
  - **wire 契约**（`packages/host/apiproxy/src/api/{approvals.ts,approvals.schema.ts,
    events.ts,events.schema.ts,rpc.schema.ts}` + `tests/api-proxy-approval.spec.ts`）：
    - `approval/requested` MuxFrame `{type, sessionId, approvalId, toolName, callId?,
      reason?}`，装进 **server-request** `{type:"server-request", rpcId(stable),
      method, payload}`（stable rpcId，**pending 期间在 mux 重开时逐字重放**）。
    - 前端答复 = **client-response** `{type:"client-response", rpcId(echo requested 的
      rpcId), result:{ok:true,value:{sessionId, approvalId, outcome:
      'allowed-once'|'rejected'}}}`，走 **`POST /api/respond`**（非 unary RPC，回应体
      RpcReceipt）；首次 → `{accepted:true}`；迟到 → `{accepted:false,
      reason:'not-pending'}`；畸形/审计不符 → `{accepted:false, reason:'bad-response'}`。
    - 结算帧 `approval/resolved` MuxFrame `{type, sessionId, approvalId, outcome}`
      （outcome 含 host 侧 'cancelled'/'unavailable'）。
    - **approvalId = 审计 id**（配对 `approval/asked` 与 `approval/decided`；按 callId
      配平并行 ask）——不是 callId。
  - 服务域（`packages/interaction/user-approval/src/index.ts`）：语言恰是我们 D-106 的
    `approval/asked`/`approval/decided`/`approval/policy`；`ApprovalService.request`
    要求**开 turn 内**（审计对 turn 封闭）；answerer waterfall `approval/request`，
    无 answerer → fail-closed 'unavailable'；policy 'ask'|'never'，'never' 确定性拒绝。
- 我们的 gap：approval 执行层已 per-driver（段 B），但**不发射前端 wire 帧、不接受
  `/api/respond`、approval/asked 无配对审计 id**。
- 设计：
  - G-a `approval_wire`（dsh-cli/web，新文件）：pending 注册表 `ApprovalWire`——
    每次 mutation 挂起 → `push_requested({rpcId:"approval-<n>", session, approvalId
    (=审计 id), callId, toolName, reason})`；decide → `resolve(rpcId, outcome)`；
    提供 pending 重放 + append-only 帧游标（requested+resolved 按 seq）。
  - G-b `approval/asked` 事件增 **审计 `id`**（配对 decided），approvalId 用它；
    decide 写 `approval/decided{id,outcome}` 保持配对（对齐服务域）。
  - G-c mux 下链：SSE/WS `events.mux` 线程**重放 pending requested**（开链）+ 增量推
    requested/resolved 帧（与 session/event 同信封 server-request）。
  - G-d `POST /api/respond` 处理 client-response（echo rpcId 路由 pending）、
    allowed-once/rejected → 映射 decide + kick；返回 `{accepted}` 语义（not-pending/
    bad-response）。保留 `session.approval.decide`（早期验证路径向后兼容）。
  - 前端侧：fork 的 connection/runtime 已内建 PendingApproval composer + respond——
    预期**零前端改动**，以 fork 构建产物 + 真机闭环验证为准。
- 验收（对齐 fork 规格，Rust 侧单测 + live）：requested 帧/重放/rpcId 稳定性、
  respond 首答/迟到/畸形、resolved 广播、allowed-once 真执行、rejected 合成拒绝、
  GUI 弹窗现形响应。
- 被否决：动 npx 安装包（会破坏下载）；改 fork 的 loop 核心（一切皆插件、最小
  blast radius）；approvalId 直接用 callId（不忠实审计配对）。
- 纪律记档：fork 侧非平凡改动需 Agent Note + 快照 + 文档 + pnpm 门禁；凭据永不提交。
- 实施记录（round 4，`web/approval_wire.rs` 新建 + `web.rs`/`approval.rs`/`lib.rs` 接线）：
  - G-a `ApprovalWire`（`Arc<Mutex>`；append-only 帧日志 + pending 表；stable rpcId
    `approval-<n>`；approvalId = 审计 id `ap-<call_id>` 派生，配对 asked/decided；
    `pending_requests()` 重放 + `frames_since()` 增量；`resolve_by_rpc`/`resolve_by_call_id`）。
  - G-c mux 下链：SSE `stream_sse_events` + WS `stream_ws_events` 各持 wire 游标——
    开链重放 still-pending requested（逐字同 rpcId），随后增量推 requested/resolved
    （信封 `{type:"server-request", rpcId, method:<帧类型>, payload}`，对齐
    `fullFrame()` method = frame.type）。
  - G-d `POST /api/respond`：`/api` 分派加专用 arm → `approval_respond`（client-response
    echo rpcId 路由 pending → 校验 sessionId+approvalId+outcome → 映射到执行层
    decide + kick → resolve + 返回 RpcReceipt）；语义对齐 harness（accepted/not-pending/
    bad-response）。
  - G-b 审计配对：asked（3 处）与 decided 事件增 `id` 字段；approval_tool_exec 挂起时
    push_requested；decide() 落 decided 后按 call id 结算 wire（`session.approval.decide`
    与 respond 两路径都推进 wire，前端 pending 不悬挂）。
  - 验收：8 个 approval_wire 单测（requested 帧形/rpcId 稳定性/重放逐字/resolved 终态/
    增量游标/respond accepted+not-pending+bad-response×审计 mismatch+decide 失败不
    resolve）；dsh-cli lib 204 全绿；clippy `-D warnings` 零。提交见下一 commit。
  - true-machine 验证（真自部署网关 deepseek-v4-flash-0731-ext + fork 前端 dist 为
    web-root + `DSH_PLUGIN_ROOT` 指 junction 聚合的 42 个 web 客户端包）：
    - **闭环 ALLOW**：plan enter → 真模型 bash 调用 → `approval/requested`（stable
      rpcId、approvalId `ap-<callId>`、sessionId/callId/reason 齐全）→
      `POST /api/respond` allowed-once → `{accepted:true}` → `approval/resolved`
      (allowed-once) → bash 真执行（marker 落盘）→ 迟到 respond `{accepted:false,
      reason:"not-pending"}`。
    - **闭环 REJECT**：第二 turn → 新 requested（新 rpcId）→ respond rejected →
      resolved (rejected) → 工具**未执行**（marker 不存在）。
    - 自下而上发现：`build_boot_manifest` 用 `file_type().is_dir()` 会把 junction
      （Windows reparse point）当 symlink 跳过 → 42 包全漏扫。改为 `path.is_dir()`
      （跟随符号链接/联接点，对齐 pnpm/node_modules 链接分布）；stage1 插件测试仍
      绿。证据：`target/wire-driver.mjs`（Node；fetch SSE + POST respond）。
  - 浏览器级真机：交给用户开 `http://127.0.0.1:60165` 点「允许/拒绝」（前端 composer
    由 fork 内在 `apps/web/tests/approval-composer.e2e.ts` 担保；本里程碑向后端契约
    已在本 DECISIONS 验证）。

### D-104 实施补记预留

---

## D-111：fork 前端端到端（Chrome DevTools MCP 真浏览器）暴露的 serve 两缺口

**日期**：2025（本机时间）

**触发的问题**：G（D-108）wire/后端闭环已验证，但「浏览器级 GUI 打点」是本里程碑的
收口验收。按用户要求用 Chrome DevTools MCP 做真浏览器测试时，fork 前端（`apps/web`）
在自部署 serve 上连失败两处，均是从未在 wire 层暴露的 **serve 功能缺口**：
1. 页面仅渲染「Failed to load plugins / web boot: window.__ModuleLoader__
   bootstrap facade is missing」——我们 `render_index_with_boot` 只注入了
   `__DSH_BOOT__`，缺 Host 侧的 `__ModuleLoader__` 门面 + parser preload。
2. 前端 composer 的 `/plan` 命令走 `commands/execute` RPC，我们的 `/api` 派发
   返回 `not-implemented: method "commands/execute" not implemented by dsh web`。

**自下而上验证**：fork 权威契约 `packages/client/modules/src/index.ts` 的
`bootInjections`：注入顺序 = queue 门面 IIFE → `@deepseek-ai/dsh-client-modules`
与 `-runtime` 的**阻塞经典** preload `<script>`（先于 Vite shell 执行，module 默认
deferred）→ `__DSH_BOOT__` global。`commands/execute` 的语义在
`packages/plan/plan-mode/src/index.ts` 的 `/plan` handler（`set(agent, active)` +
可选 steer message）与 `ui-commands` admission 契约（`ok:true + value:undefined`
→ “unknown or malformed command”；`value.result {kind,text}` 为成功结果）。

**最终选择**：
1. `render_index_with_boot` 扩展为三件套注入（门面 + preload + boot），逐字对齐
   `bootInjections` 的 queue facade 文本；缺 modules/runtime entry 时跳过对应
   preload，`__DSH_BOOT__` 照旧注入。
2. `/api` 派发新增 `commands/execute` arm → `commands_execute(boot, line, images)`：
   只实现 `/plan[ off|message]`；`/plan off` + images → error 结果（前端保留草稿）；
   message 进入 plan 事件 `message` 字段（复用 D-106 的 `set_plan_mode` steering）；
   `off` 时按折叠前置判定「Plan mode off.」/「Plan mode is already inactive.」；
   其余命令/非命令 → `value:undefined`。

**被否决**：在 npx 安装包上打补丁（禁止区）；改 fork 前端让 /plan 走别的 RPC
（fork 零接触、命令路径是 Host 责任）。

**预期影响与回滚点**：fork 前端自此可完整 boot 并原生进入 plan 模式；`commands/execute`
只认 plan（compact/goal/subagents 诚实返回 unknown，与本 serve 的 support 面一致）。
回滚：撤销 `web.rs` 该 arm + `render_index_with_boot` 三件套即可，不影响 D-108 wire
契约。

**验证**：单测 `render_index_injects_module_loader_facade_then_preloads_then_boot` +
`commands_execute_plan_flips_mode_like_frontend_command`；dsh-cli lib 206/206 绿；
clippy `-D warnings` 零。真浏览器：页面完整 boot（42 plugin entries + 内测声明 +
composer 全要素），`/plan` 命令路径待 D-112 浏览器闭环确认。

---

## D-112：手动抓包 + 用户手动测试暴露的四个 serve 缺口（approval/host 帧、args 包裹、steer、plan 投影实时帧）

**日期**：2025（本机时间）

**触发的问题**：用户按 D-111 清单手动测试 fork GUI 时，控制台与抓包连续暴露四个此前
wire 层从未暴露的真实传送/语义缺口：
1. 控制台 ZodError `dropping malformed WebSocket frame on /api/events.host`——we
   把 **approval wire 帧（`approval/requested`/`approval/resolved`）无条件下推给
   `events.host`**，而 host 帧联合（zod `HostFrame`）不包含 `approval/*` → 前端判
   malformed 丢弃；approval 帧只该走 `events.mux` 母线（与 session/event 同槽）。
2. 点击发送报 `command.execute failed ... commands/execute rejected "result"`，
   抓包显示**真实线路把参数包在 `payload.args`**（`{type:"client-request",...,
   payload:{args:{agentId,line,images}}}`），我们的 arm 直接读 `payload.line` 取不到
   → 把 `/plan` 当未知命令返回 `value:null` → 前端 zod 拒绝。
3. `/plan <message>` RPC 返回 `{ok,value:{commandId,result}}`（成功了）但「没有跳到
   下一步页面」——fork 的 `/plan <message>` 会 `agent.steer(createUserMessage(...))`
   （把消息投入会话并驱动下一轮）；我们只落 `plan/mode`（message 仅记录，assembly
   不消费）不投消息不起轮 → 模型根本没开始。
4. 同一现象的另一面：前端 plan 徽章/占位符读 `session.projections` 的 `plan` 键，
   该键**只由 `session/history` 基线（冷启动）+ 实时 `session/projection` 帧喂**；
   我们的 mux 流**零投影帧** → UI 永远不知道 plan 已开（D-107 的 per-agent 折叠只
   服务了审批门，没服务 UI 投影）。

**自下而上验证**：
- `packages/client/connection/src/client/fixture.ts` 的 `projectionFramesOf`：
  `plan/mode` 与 `command/run[name=plan,args:string]` 推进 → 下发
  `{type:"session/projection", sessionId, key:"plan", value:{active,pending}, seq}`；
  host 帧只在 `events.host`；approval 帧只在 mux。
- fork `plan-mode/src/index.ts` `/plan` handler 329-331：非空 message/attachments →
  `agent.steer(createUserMessage(...))`。
- 抓包实证（用户提供）：`payload:{args:{...}}`；fixture `call()` 也解 `payload.args`。

**最终选择**：
1. approval wire 重放/增量下推全部收窄到 `!is_host`（SSE + WS 两处）；`events.host`
   只下推 `host/*`（握手 `host/session-added` + 宿主事件日志），帧联合自洽。
2. `commands/execute` arm 读 `payload.get("args").unwrap_or(payload)`（保留平铺回退），
   `agentId`/`line`/`images` 从解包后的对象取。
3. `commands_execute`：非空 message → 在 `set_plan_mode_on(active=true, message)` 后调用
   `crate::run_rust_loop(boot, 目标会话, message)`（目标会话 = agentId，回退
   plan_session）——镜像 fork 的 steer：投入 user 消息并阻塞驱动一轮（与
   `session.prompt` 同路径同同步语义；命中审批门则暂停待批）。
4. mux 流新增 plan 投影实时发布器 `plan_projection_frame`（per-session 惰性
   `PlanUnitState` 增量折叠；触发=plan/mode、command/run[plan]；仅在 `!is_host` 下推），
   `value` 用 `dsh_plan::projection::{plan_unit_apply, plan_projection_view}`（与
   `session.history` 投影块同一折叠、同 `{active,pending}` 视图）。

**被否决**：
- 给 events.host 的帧联合打补丁或改前端（fork/安装包零接触）。
- 在 commands/execute 里让 action 返回后由前端另行提交 prompt（命令路径已消费掉该
  输入，会重复提交；fork 语义是 handler 内 steer）。
- 只修 plan 投影不改 approve/args/steer（四者是同一批真机暴露的传送缺口，同批收口）。

**预期影响与回滚点**：events.host 不再被 approval 帧污染（ZodError 消失）；`/plan
<message>` 从「成功但无动作」变为「进入计划模式 + 消息投入并起一轮」；plan 徽章/占位符
随投影帧实时更新。回滚：分别撤销 ①②③④ 处的改动即可，互不耦合，wire 契约
（`api-proxy-approval.spec.ts`）不受影响。

**验证**：新增/更新单测 `commands_execute_unwraps_args_wrapper`（args 包裹真实形状 +
steer 落 s2）、`commands_execute_routes_by_agent_id`（per-agent 路由 + steer 落同会话）、
`plan_projection_frame_publishes_on_plan_events`（触发/形状/独立会话）；dsh-cli lib
209/209 绿；clippy 零。浏览器闭环（plan 徽章 + approve 条 + allow + marker）待 D-113
用户手动复测确认。

---

## D-113：审批决策归位真实归属（消灭「硬编码 default」缺陷）+ events.host 的 session/event 泄漏

**日期**：2025（本机时间）

**触发的问题**：用户手动测试 60880 时报三类现象：
1. 控制台仍 `events.host` ZodError `dropping malformed WebSocket frame ... No matching
   discriminator`——D-112 只把 approval 帧收窄到 `!is_host`，但流循环里
   `mux_session_event_frame`（`session/event` 帧）仍在 host 流**无条件**下推，而
   HostFrame 联合不含它 → 前端 zod 丢弃。
2. `/api/respond` 对形状正确的应答（`sessionId:"s2"`、`approvalId:"ap-..."`、
   `outcome:"allowed-once"` 逐字回显）返回 `{"accepted":false,"reason":"bad-response"}`。
3. 用户质疑「为什么有硬编码 default 目录，是否导致不该有的逻辑」——盘点全仓
   `"default"` 硬编码后确认三类角色：①启动种子/握手（GUI 初始空白会话，纯展示，
   无逻辑）；②设计性回退链 `plan_session → "default"`（全局命令无会话身份时的
   兜底）；③**真正的缺陷**：`approval::decide` 硬编码 `AGENT="default"` + store
   `"default"`（D-106 单会话时代遗留，per-session agent 普及后未跟上）。

**自下而上验证**：
- wire 的事实：approval requested 由 `approval_tool_exec` 以 `session.id().raw()`
  （="s2"）mint，pending 挂在 driver agent `session-s2`；`decide` 却
  `pending_calls("default")` → 该 driver 不存在 → Err → `bad-response`；即便存在，
  `approval/decided` 也会写进 "default" 会话，resume 在 "s2" 会话 `fold_decided`
  不到 → 永远「仍未决定」（坏得更隐蔽）。
- fork `api-proxy-approval.spec.ts`：respond 校验 `sessionId`（=requested 帧的
  driver 会话）+ `approvalId` 相关性；审计错配 → `bad-response`；success =
  `accepted:true`。真实 flow 里 sessionId 逐字一致，故本批挂点只在 decide 路由。
- `AgentLoopHost`（host.rs）已有 `configured_for_session` 精确映射；
  `ReactLoopAgent.agent.session` 公字段可拿 driver 会话 → 跨 agent 按 call id 定位
  无信息缺口。

**最终选择**（用户确认范围 A：最小必要）：
1. `AgentLoopHost::pending_by_call_id(call_id) -> Option<(agent_id, Rc<Session>)>`：
   跨全装配 agent 按 call id 定位持有该 pending 的 driver（per-session
   `session-<sid>` 与默认 agent 同表可查），不依赖硬编码 "default"。
2. `approval::decide` 改用 `pending_by_call_id`：`approval/decided` 写到**真实归属
   会话** + 按真实 agent 裸踢恢复；公开签名不变 `(boot, call_id, decision)`，
   `/api/respond` 与 `session.approval.decide` 两个调用点同时受益。
3. `events.host` 流（SSE + WS）不再下推 `mux_session_event_frame`（与 plan 投影
   一并收进 `!is_host`）——host 通道只推 `host/*`（握手 + 宿主事件日志），帧联合
   自洽。
4. `agent.run`/`agent-loop` 旧路径：带 `sessionId` 则按会话路由（与
   `session.prompt` 一致），无则回退 "default"（顺手接会话，消除无条件写死）。

**被否决**：
- 收拢全部 "default" 种子/回退链为单一常量 + Boot 单一事实源（范围 B）——改动面
  大、会动握手帧/GUI 基线/大量测试 fixture，留单独一轮设计评审，不混入本批。
- `approval_respond` 放松 `value.sessionId` 校验——fork 契约硬性校验审计相关性，
  且真实 flow 里 sessionId 本就一致，不放松。

**预期影响与回滚点**：per-session 审批 `accepted:true` 且恢复后**真执行**（不再
bad-response/永不恢复）；`events.host` 不再收 `session/event`（ZodError 消失）。
回滚：①-④ 各自独立可回；`pending_by_call_id` 是纯增量方法。

**验证**：新增单测 `plan_approval_respond_routes_to_per_session_agent`（mock LLM
真 loop：s2 会话 plan 激活 → bash 挂起 pending（不执行）→ respond allowed-once →
`accepted:true` → kick 后 bash 真执行、tool/result 无错、tool/call 不重复；修复前
此测试红 = `bad-response`，精确复现用户抓包）。dsh-cli lib 210/210 绿；dsh-agent-loop
lib 绿；clippy（--all-targets）零。live 60880 脚本化 wire 全链 PASS（session.create →
/plan → plan 投影实时帧 → prompt 门控挂起 → respond `accepted:true` → resolved 广播 →
bash 真执行写 marker → events.host 零泄漏，9 项全绿）；浏览器手动复测待 D-114 收口。

---

## D-114：发送按钮↔停止按钮——host/session-status 运行位帧 + 真取消接线（真实中断待架构决策）

**日期**：2025（本机时间）

**触发的问题**：用户报告「消息发送后，发送按钮应变为停止按钮，现在没有这个过程，
导致无法停止模型工作」。剥到第一性原理后拆成两层：
1. **按钮不切换**：fork 前端 `InputBar.tsx` 的
   `primaryStops = running && subagent===null` —— 发送/停止由会话的 `running` 位
   驱动，而客户端 `Session.handleRunning` 的**唯一**写入源是 `host/session-status`
   帧（`SessionManager` 只消费该帧 + `session.list` 摘要，不看 `session/event`）。
   我们后端从未推过该帧 → `running` 恒 false → 按钮永不变。
2. **即使变了也停不下来**：serve 主循环单线程（D-004/D-006 Rc/RefCell 纪律），
   `session.prompt` 经 `run_rust_loop` 同步排空整轮 turn（`kick()` 到 idle 才返回），
   turn 期间 accept 循环被占用 → 点停止发出的 `session.cancel` RPC 根本送不进循环，
   等 turn 自然结束后送达时 driver 已 idle → no-op。

**自下而上验证**：`session.ts` `derivePhase`/`handleRunning` 与 `manager.ts`
`case 'host/session-status'`（`handleRunning(frame.running)`）逐行确认按钮驱动源；
`SessionStore::enter` 已给每个会话装 append 转发钩子（`store.on_event` 在
append 提交同步触发，`session_host.rs:105` 已有同模式消费者）→ turn/start、
turn/end 落盘瞬间可推帧，无需等 tick（单线程 turn 期间无 tick 可达）。

**最终选择**（范围：先解决「有这个过程」+ 诚实接线）：
1. `install_session_running_frames(store, host_events)`：store 级 `on_event`
   投影——`turn/start`→`{type:"host/session-status", sessionId, running:true}`、
   `turn/end`→`false`（含 approval-pending 的 turn/end，回退后按钮恢复「发送」，
   审批态由 approval 条承载）。serve 装配在 `boot.host_events` 就绪后接线，
   agent-loop 门控。单点覆盖所有 turn 驱动（prompt/steer/kick/子代理）。
2. `session.cancel` 从纯 stub 改为**真取消接线**：按 `configured_for_session(sid)`
   定位 driver → `driver.cancel(AgentCancelCause::User, keep_inbox:false)` →
   driver 在 step 边界检查 `abort_reason` → `turn/end reason=aborted`；幂等
   （idle/未知会话 no-op，一律 `accepted:true`）。

**被否决**：只做按钮切换不做取消接线——会出现一个「点了没用的停止按钮」
（比没有更糟）；同步驱动的取消注入口已就位，属必要的下一步资产。

**已知边界（显式报告，未在本批越级改架构）**：单线程 serve 下 `session.cancel`
需在 turn 间隙送达才有意义；turn 内并发送达目前送不进。真实「生成中即停」需要
架构决策——两个候选：
- **方案 I（协作式泵）**：driver 在 step 边界让出给请求泵（单线程内重入处理
  `session.cancel` 一类只读/取消请求，其余暂存 turn 后派发）。保持 Rc 单线程纪律，
  改动集中在 dsh-agent-loop 热循环 + serve 派发。
- **方案 II（Send 化 + worker 线程）**：dsh-session/dsh-agent/dsh-agent-loop 全栈
  Rc→Arc+Mutex 送长 RPC 上 worker 线程。根治但也推翻 D-004/D-006 单线程纪律，
  跨 crate、影响面大。
待用户拍板（横向纪律：不默默选方案、不因「快」降级）。

**预期影响与回滚点**：按钮在模型生成时变为「停止」（running:true 先于 false 到达），
events.host 新增 `host/session-status`（zod 联合本就含该型，无新拒绝）；取消 RPC
语义从谎报变为真中止（可送达时）。回滚：①/②各自独立可回。

**验证**：新增单测 `session_running_frames_follow_turn_boundaries_per_turn`
（两轮文本 turn → host_events 收到 `[true,false,true,false]`，修复前编译失败=红）
与 `session_cancel_accepted_idempotent_and_keeps_turns_driving`（已知会话/幂等/
未知会话 no-op/取消不破坏后续 turn）。dsh-cli lib 212/212 绿；clippy（--lib --tests）
零。live 60880 脚本化 wire PASS（5/5）：逐 turn `[true,false]`、`running:true`
在 turn 仍在生成时已到达（按钮可停）、events.host 全帧为已知 host/* 类型
（零 zod 拒绝）。浏览器手动复测 + 真实中断架构决策待用户。

---

## D-115：请求面并发化——Rc/RefCell 单线程纪律 → Arc/Mutex + worker 线程（用户拍板方案 II）

**日期**：2025（本机时间）

**触发的问题**：D-114 修复「发送→停止按钮」后暴露第二层：单线程 serve 主循环里
`session.prompt` 同步排空整轮 turn（LLM 流为阻塞 Iterator），turn 期间 accept
循环被占死 → `session.cancel` 无法并发送达，「生成中一键即停」不可达。用户提出用
性能/复杂度/拓展性/架构优雅度/远见五维判据求长远方案，明确「不为短期省事让后续
付出巨大代价」，最终拍板**方案 II（Send 化 + worker 线程）分阶段**，方案 I
（协作式泵）因脆弱的隐性重入、无法跑多秒级请求、单线程序列死「同时仅一 turn」、
不具拓展性/远见而被否决。

**自下而上盘点（Phase 0 库存）**：
- `dsh-session`：`Session.data: RefCell<SessionData>`；`SessionStore` 的
  `store/counter/on_*` 全 `RefCell`；所有句柄 `Rc<Session>`/`Rc<SessionStore>`。
- `dsh-agent`：`Agent`、`AgentRegistry`（store/order/factory/initiator 全
  `RefCell`）、`Inbox.inner: Rc<RefCell<InboxInner>>`、`AgentBus.items` +
  监听器族（`InboxNotify/NextFn/ChainListener/AgentListener = Rc<dyn Fn>`）、
  `model_selection.sel`、`invariant.last`。
- `dsh-agent-loop`：`ReactLoopAgent.phase/approval_pending`（RefCell）、
  `LoopDeps` 全 `Rc<dyn Fn>`（assemble/prepare_call/stream/project_context/
  tool_exec）；`AgentLoopHost` 的 `agents/runtime_agents/disposers/joins/
  tool_exec_factory` 全 `RefCell`。
- `dsh-llm`：`LlmRuntime.adapters: RefCell<HashMap>` + `Rc<dyn LlmAdapter>`。
- `dsh-tools`：`ToolRegistry` slots、`approval/run_code_executor` 槽
  （`Rc<RefCell<Option<..>>>`）、`ToolExecution.aborted: Rc<Cell<bool>>`、
  `ToolExecute = Rc<dyn Fn>`。
- 已并发面（不改）：EventSink `Arc<Mutex<Vec>>`（mux/SSE 流线程）、host_events、
  tick 的 schedule/bash_jobs（`Arc`）。→ 9 成并发形态已定型，请求面是最后补齐。

**设计（Phase 0 结论）**：
1. **锁粒度**（粗边界，热路径单锁）：
   - `Session.data` → `Mutex<SessionData>`（append 校验-提交-通知在锁内；见死锁审计）；
   - `SessionStore` 单 `Mutex` 护 map+counter；回调表 `Mutex<Vec<..>>`；
   - `ReactLoopAgent.phase`/`approval_pending` 各一 `Mutex`；`AgentRegistry`/`Inbox`
     /`AgentBus` 各一 `Mutex`；
   - `dsh-tools` 槽 → `Mutex<Option<..>>`、`aborted: Rc<Cell<bool>>` → `AtomicBool`；
   - `Rc<dyn Fn(..)>` 监听器/闭包族 → `Arc<dyn Fn(..) + Send + Sync>`（最宽的连锁：
     所有闭包提供方须 Send+Sync，机械但大量）。
2. **死锁审计（前置门槛）**：`append` 在持有 `Session.data` 锁时同步调用
   store 观察者；观察者（session_host 持久化→sink、running-frames→host_events）
   只取**叶子锁**（Arc<Mutex<Vec>>），且绝不反向取 session/store 锁 →
   排序 `session.data → 叶子锁` 一致无环。迁移后逐观察者复核「回调不得再取本会话/本
   store 锁」（如 `session.id()` 须读非锁字段或锁外取）。
3. **worker 化（Phase 4）**：长 RPC（`session.prompt`/`agent.run|loop|turn`/
   `commands/execute` steer + `/plan <msg>`/审批 decide kick 的恢复回合）上
   worker 线程（Result 经 channel 回填 accept 线程，HTTP 同步契约不变）；accept
   线程空闲接 `session.cancel` → 真一键即停；每 driver 仍单 turn（相位机连续），
   跨会话 turn 可并行（扩展点）。
4. **顺序**：Phase 1 `dsh-session` → Phase 2 `dsh-agent` → Phase 3
   `dsh-agent-loop`+`dsh-llm`+`dsh-tools` → Phase 4 serve worker 化 →
   Phase 5 回归 + live wire 验证 + 文档。每阶段 `cargo test` 全量关闸。

**最终选择**：方案 II（Send 化 + worker 线程）分阶段；方案 I（协作式泵）否决；
full async（tokio）维持 D-004/D-006 既判否决（全套栈 async 分歧最大）。

**被否决**：方案 I（见上）；「只把 turn 送 worker 而 store 留在主线程」——store 是
主线程（RPC 读 history/plan/respond）与 worker（写事件）共享的唯一 Rc 纠缠点，
不 Send 化即死锁/借用冲突（自下而上验证为不可行）。

**预期影响与回滚点**：serve 获真并发（生成中 cancel/steer/二会话/随时交互），
对齐 fork 请求面性质；三 crate 公开类型 Rc→Arc 连锁更新全部消费者。回滚：分阶段
各自可回；Phase 1-3 独立，Phase 4 在 1-3 之上。

**验证（Phase 0 现状）**：本条为设计阶段工件，无代码。Phase 1-5 逐阶段以
dsh-session 61 / dsh-cli 212 / dsh-agent-loop 全量测试为关闸；终态补
live 60880 wire 全链（含生成中 cancel 即停）。

---

## D-115（实施·Phase 1）：dsh-session/dsh-persistence 整体 Send 化——粗锁迁移 +
Rc→Arc 连锁更新全部消费者（dsh-agent/dsh-agent-loop/dsh-diff/dsh-cli）

**日期**：2025（本机时间）

**触发问题**：D-115 方案 II 的 Phase 1「dsh-session」落地——把 `Session/SessionStore`
单线程 Rc/RefCell 纪律迁到 Arc/Mutex，并连锁更新全部消费者（设计 §4 顺序 Phase 1
dsh-session）。dsh-persistence（coordinator/sqlite/write_behind）因 SessionHost 闭包
要 Send+Sync 而随迁。

**自下而上库存修正（与 Phase 0 盘点核对）**：phase 0 预期 `dsh-session` 61 测试；实际
本轮 gate 以 dsh-session 69 / dsh-persistence 77 / dsh-agent 44 / dsh-agent-loop 全量
/ dsh-diff 22 / dsh-cli 212+18 计。dsh-llm/dtah-tools（`Rc<dyn LlmAdapter>`/
`Rc<dyn Fn>` 族）**未动**（Phase 3 预算），故 agent/add 面仍 Rc 单线程语义——Session
升级后两端类型由 `Arc<Session>` 接缝兼容，已由编译期验证。

**决策事项**：
1. `dsh-session/runtime.rs`：`Session.data: RefCell<SessionData>` → `Mutex<SessionData>`；
   观察者类型 `Rc<dyn Fn(&SessionEvent)>` → `Arc<dyn Fn(&SessionEvent)+Send+Sync>`；
   所有 `.borrow()/.borrow_mut()` → `.lock().unwrap()`（粗边界单锁，热路径唯一）。
2. `dsh-session/store.rs`：`SessionStore` 拆为 `Arc<Self>` 形态——`StoreEntry{ session:
   Arc<Session>, announced }`；`SessionStore{ state: Mutex<StoreState>, on_created/
   on_disposed/on_event/on_flush: Mutex<Vec<..>> }`；`create/enter/fork` 收 `&Arc<Self>`
   且返回 `Arc<Session>`；`Arc::downgrade` 避免回调克隆引用环。观察者回调
   `Arc<dyn Fn(&Session,&SessionEvent)+Send+Sync>`。
3. **死锁审计执行（设计门槛）**：`append` 持 `Session.data` 锁 → 调 store 观察者；
   观察者（session_host 持久化→sink `Arc<Mutex<Vec>>`、running-frames→host_events
   `Arc<Mutex<Vec>>`）只取叶子锁，绝不反向取 session/store 锁——锁序 `session.data →
   叶子锁` 无环；`session_host.on_event` 闭包捕获 `coord`（Arc）+ `sink`（Arc），
   `session.id()` 在锁内经头字段读取（append 锁内取值后同步回调，语义不变）。
4. `dsh-persistence`：`coordinator` 的 `states: RefCell<HashMap>` → `Mutex`、
   `prepared: RefCell<VecDeque>` → `Mutex`；backend `Box<dyn PersistenceBackend>` →
   `Box<dyn PersistenceBackend + Send + Sync>`；`SqliteBackend.conn: RefCell<Connection>`
   → `Mutex<Connection>`（rusqlite Connection 本身 Send，Mutex 补 Sync）；
   `write_behind.FailureReporter = Rc<dyn Fn>` → `Arc<dyn Fn+Send+Sync>`。
   `SessionPersistence` trait 未动（`&self` 方法，不加 Send 界——dsh-session-query
   的 `&dyn SessionPersistence` 传参不受影响）。
5. **消费者连锁（机械但大量）**：`Rc<Session>`/`Rc<SessionStore>`/`Rc<SessionHost>`/
   `Rc<PersistenceCoordinator>` 全部 → `Arc<…>`；`on_event/on_flush/on_created`
   `Box::new` → `Arc::new`。dsh-agent（inbox/registry 的 `session: Arc<Session>`，
   `AgentRegistry`/`Inbox` 自身 Rc/RefCell 保留至 Phase 2）；dsh-agent-loop
   （host/service/build_request/runtime_context/invariant/tool_calls/agent 的
   `&Arc<Session>`；`AgentLoopHost.store: Arc<SessionStore>`；`AgentLoopHost` 自身仍
   Rc<dyn> 闭包笔至 Phase 3）；dsh-diff（session_store/sessions → Arc）；dsh-cli
   （`SessionHost{store: Arc, coord: Option<Arc<PersistenceCoordinator>>}`，
   `new_from_backend` 收 `Box<dyn PersistenceBackend+Send+Sync>`，m5/approval/
   subagent_runtime/web 连锁）。`AgentLoopHost`（Rc）与 `SessionHost`（Arc）共存——
   web.rs `assemble_server_loop` 仍返回 `Rc<AgentLoopHost>`（Phase 3 再 Send 化），
   与 `Arc<SessionHost>` 无类型冲突（共享 `Arc<SessionStore>`）。
6. **环境问题（按 D 纪律记档）**：Phase 1 gate「cargo test -p dsh-cli」被运行中的
   dsh.exe web（60880 演示服务）占用 `target\debug\dsh.exe` 锁住 → 无法重链二进制。
   已与用户确认后 Stop-Process 该进程 → 测试/clippy 跑完 → 以原命令行重启（PID
   变化，端口 60880 HTTP 200 验证通过）。决策不受影响，仅临时阻碍。

**最终选择**：上述 1-5。粗锁（单一整表锁）+ `Arc<dyn Fn+Send+Sync>` 回调 + 全消费者
Rc→Arc 连锁；`SessionPersistence`/`LlmRuntime`/`ToolRegistry` 保持 Phase 3/未动。
**被否决**：细粒度锁/读优先锁（阶段目的不达——worker 化只需 Send+Sync，不为并发热度
优化增加锁复杂度与死锁面）；让 callback 仍是 `Rc<dyn Fn>` 而只把数据 Arc 化（回调在
闭包捕获 `Arc<SessionStore>` 时须 Send，否则 worker 无法握整个状态——自下而上不可行）。

**预期影响与回滚点**：dsh-session/dsh-persistence/dsh-agent/dsh-agent-loop/dsh-diff/
dsh-cli 全部公开「会话句柄」类型由 `Rc` 改为 `Arc`（破坏性换型，编译期联动）；所有
会话操作从借用到加锁（`Session` 方法 body 内 `.lock().unwrap()`）；内存/性能成本为
Mutex 粗锁（本机单会话为主，可忽略）。回滚 = 逆向连锁换型（git revert 本提交）。
**验证（Phase 1 关闸）**：`cargo test -p dsh-session -p dsh-persistence -p dsh-agent
-p dsh-agent-loop -p dsh-diff -p dsh-cli --tests` 全绿（dsh-session 69 / dsh-persistence
77 / dsh-agent 44 / dsh-agent-loop 各套 / dsh-diff 22 / dsh-cli 212+18，EXIT=0）；
`cargo clippy` 上述五 crate `--lib --tests` 零告警；`cargo check --workspace` 绿。
60880 演示服务重启验证 HTTP 200。

---

## D-115（实施·Phase 2）：dsh-agent 整体 Send 化——Agent/Registry/Bus/Inbox/监听器族
Arc 化 + 连锁更新 dsh-scope（ScopeKey/BaseFilter）与 dsh-agent-loop 消费面

**日期**：2025（本机时间）

**触发问题**：D-115 方案 II 的 Phase 2「dsh-agent」落地——把 `Agent`、`AgentRegistry`、
`Inbox`、`AgentBus`、监听器族（`InboxNotify/NextFn/ChainListener/AgentListener`）、
`model_selection.sel`、`invariant.last` 全部迁到 Arc/Mutex/Atomic，使 dsh-agent 公开
句柄成为 Send+Sync（serve worker 化前置；设计 §4 顺序 Phase 2 dsh-agent）。

**自下而上库存修正（与 Phase 0/Phase 1 盘点核对）**：**dsh-scope 隐性入局**——`Agent`/
`AgentCtx`/`AgentBus.items` 嵌入 `ScopeKey(Rc<()>)`、`AgentEntry.carrier` 存
`ScopeCarrier{ base_filter: BaseFilter = Rc<dyn Fn> }`；`Rc<()>` 与 `Rc<dyn Fn>` 均为
!Send+!Sync，直接堵死 Agent/AgentEntry 的 Send+Sync。故 dsh-scope 作为公开类型传递依赖
随迁（设计「三 crate 公开类型 Rc→Arc 连锁更新全部消费者」的自然外延）。dsh-system-prompt
**不在库存**：`register_assemble_listener`/`ToolProvider`/`VariableProvider`/
`AssembleNext`/`AssembleListener` 保持 `Rc<dyn Fn>`（Phase 3 预算外）——model_selection
的组装侧监听器保持 Rc，仅闭包内捕获 Arc（sel）即可。

**决策事项**：
1. `dsh-scope`：`ScopeKey(Rc<()>)` → `ScopeKey(Arc<()>)`（指针身份语义不变：
   `new()` 每次铸造全新 Arc，`ptr()`/`Eq`/`Hash` 按指针）；`BaseFilter = Rc<dyn Fn()>
   → `Arc<dyn Fn() -> bool + Send + Sync>`。`Scope`/`ScopedContext`/`ScopeDisposer`
   内部仍 Rc/RefCell 保留（不被 dsh-agent Send+Sync 面捕获；Phase 3/4 如有 worker 跨
   线程作用域父链再议——模块级 SCOPE_STATE 仍是 thread_local）。
2. `dsh-agent/agent_bus.rs`：监听器族 → `Arc<dyn Fn(..) + Send + Sync>`；
   `items: Rc<RefCell<Vec<BusItem>>>` → `Mutex<Vec<Arc<BusItem>>>`；`select` 克隆
   Arc 在外层收集后逐个执行（收集-再执行语义保留）。
3. `dsh-agent/inbox.rs`：`Inbox{ inner: Arc<Mutex<InboxInner>> }`；
   `InboxNotify = Arc<dyn Fn(&InboxNotification)+Send+Sync>`；所有 borrow → lock。
4. `dsh-agent/registry.rs`：`Agent.status: Cell<AgentStatus>` → `AtomicU8`
   （0=Idle,1=Running，`status()`/`set_status()` 原子读写）；`AgentEntry` 的
   `announced/announcing/detach_requested: Cell<bool>` → `AtomicBool`；
   registry `store/order/factory/initiator: RefCell` → `Mutex`；句柄 `Rc<Agent>`/`
   Rc<AgentEntry>` → `Arc`；`factory: Rc<dyn AgentFactory>` → `Arc<dyn AgentFactory +
   Send + Sync>`；register/enter_agent/set_factory 的 disposer `Rc<dyn Fn()>` →
   `Arc<dyn Fn() + Send + Sync>`。`AgentFactory::create_agent/resume_agent` 返回
   `Arc<Agent>`。锁序审计：`list/roots` 先克隆 order 再查 store（Mutex 短持）；
   detach_entered「store 检查 → store 删除 → order retain」顺序取锁不嵌套，无环。
5. `dsh-agent/invariant.rs`：`fail: Rc<dyn Fn(String)>` →
   `Arc<dyn Fn(String)+Send+Sync>`；`last: Rc<RefCell<HashMap>>` → `Arc<Mutex<..>>`。
6. `dsh-agent/model_selection.rs`：`sel: Rc<RefCell<ModelSelectionRef>>` →
   `Arc<Mutex<..>>`；请求侧 `agent/request` 监听器 Arc；组装侧
   `register_assemble_listener` 保持 Rc（dsh-system-prompt 库存外）但捕获 Arc sel。
7. `dsh-agent-loop` 消费面连锁：`ReactLoopAgent.agent/registry: Rc<..>` →
   `Arc<Agent>/Arc<AgentRegistry>`；`new()/build_loop_deps/create_loop_agent(_with_
   tool_exec)` 签名 Rc→Arc；`AgentLoopHost.registry: Arc<AgentRegistry>`。
   **新增共享取消令牌（Phase 4 前置缝）**：`ReactLoopAgent.cancel_token:
   Arc<Mutex<Option<AgentCancelCause>>>` + `cancel_token()` 句柄 + `abort_reason()`
   先消费令牌再回退 phase——因为 bus 监听器现在须 Send+Sync，测试里无法在监听器闭包
   捕获 `Rc<ReactLoopAgent>` 直调 `cancel()`；令牌让 Send+Sync 上下文（bus 监听器 /
   未来 worker 的 accept 线程）注入取消，driver 在 turn/step 边界轮询消费。`cancel()`
   仅在**运行/维护相位**写令牌（idle 取消不得污染下一个 turn——对齐 dsh-cli web 回归
   `session_cancel_accepted_idempotent_and_keeps_turns_driving`，仅清 inbox 不设 abort）。
8. 测试连锁：m2d_agent/m2d_inbox/m2e2_driver/m2e3_service/m2f_interaction/m2_scope
   的 `Rc<Agent>`/`Rc<AgentRegistry>` → Arc；bus 监听器闭包全 Arc+Send+Sync；日志
   `Rc<RefCell<Vec/..>>` → `Arc<Mutex<..>>`/Atomic；`Agent.status.get()` → `status()`；
   cancel 类测试经 `cancel_token()` 注入而非捕获 driver；steer 类测试经 Send 的
   `agent.inbox.append_msg(NextStep)` 直注（对正在排空的 turn 与 steer 等价——
   主循环按 next_step 续跑）。

**最终选择**：上述 1-8。原子化 status/标志 + 整表 Mutex（粗锁，与 Phase 1 一致）；
D-115 库存外的 dsh-scope 最小随迁（ScopeKey/BaseFilter Arc 化，Scope 内部不变）；
dsh-agent-loop 仅消费面连锁 + 共享取消令牌（其自身 RefCell/LoopDeps/AgentLoopHost
内部 Send 化仍属 Phase 3）。
**被否决**：把 `ScopeKey` 内部换成 `Mutex<()>`/裸指针靠 unsafe 假性 Send（语义劣化、
伤害代码卫生）；把 dsh-agent 各 Communicating 容器拆细锁/读写锁（与 Phase 1 粗锁决定
一致——worker 化只要求 Send+Sync，不给并发热度加复杂度）；让 bus 监听器保持非 Send
而仅把数据 Arc 化（worker 无法持有含监听器的 bus——自下而上不可行）。

**预期影响与回滚点**：dsh-agent 全部公开句柄/监听器类型由 Rc→Arc（破坏性换型，
编译期联动 dsh-scope/dsh-agent-loop/dsh-cli 消费者）；bus/registry/inbox 操作从借用
变加锁（热路径粗锁，本机单会话可忽略）。回滚 = 逆向连锁（git revert 本提交）。
**验证（Phase 2 关闸）**：`cargo test --workspace` 全绿 EXIT=0（191 套 ok，含
dsh-agent 21+12+11 / dsh-agent-loop 各套 / dsh-scope 24 / dsh-cli 212+18；
dsh-cli web `session_cancel_accepted_idempotent_and_keeps_turns_driving` 经取消令牌
修正后回归通过）；`cargo clippy --workspace --all-targets` EXIT=0 零告警。
60880 演示服务以 Phase 1 原命令行（`--workspace-root target/web-workspace
--sqlite-store target/web/sessions.sqlite`）重启验证 HTTP 200（PID 变化）。

---

## D-115（实施·Phase 3）：dsh-scope store/dsh-system-prompt/dsh-llm(+deepseek)/dsh-tools/dsh-agent-loop 整体 Send 化——LoopDeps/监听器/适配器/工具族 Arc + Send+Sync（round 4）

**触发问题**：Phase 2 只覆盖 dsh-agent 的公开句柄/监听器。Phase 4 的 worker 线程
要把「整条请求面」（LoopDeps 五个闭包 + 其捕获的服务句柄）送过线程边界，故依赖图
上游的三个库（dsh-scope store / dsh-system-prompt / dsh-llm / dsh-tools）与
dsh-agent-loop 自身内部仍是 `Rc`/`RefCell`/`Cell` 的面必须一起 Send 化（自下而上
约束：LoopDeps 闭包若要 `+ Send + Sync`，捕获的 `Rc<SystemPrompt>`/`Rc<LlmRuntime>`/
`Rc<ToolRegistry>` 必须先变 Arc——D-115 库存清单里「dsh-system-prompt Phase 3 预算外」
的注记经自下而上发现**不可行**，实际必须入局）。

**考虑的选项**：
- (a) 只 Send 化 LoopDeps/agent-loop 表面，dsh-system-prompt/llm/tools 保持 Rc 作为
   worker 内创建的「本线程服务」（worker 自建服务、Rc 不出线程）。——被否决：
   服务装配（host 建 loop）发生在主线程/accept 线程，worker 需要拿到同一实例或重建
   整个服务树；重建则失去全局注册（tools/prompt）与共享 store 语义，等于两套状态。
- (b) 三个上游库整面 Rc→Arc（闭包族 `Rc<dyn Fn>` → `Arc<dyn Fn + Send + Sync>`、
   `Rc<RefCell<X>>` → `Arc<Mutex<X>>`/Atomic、句柄 `Rc<T>` → `Arc<T>`），agent-loop
   内部同理 + LoopDeps 五个闭包 Arc。——**选定**。破坏性换型一次性在此关闸消化。

**决策事项**（TDD：先按 `cargo check` 红面逐 crate 收敛，再测试绿，最后 clippy 0）：
1. `dsh-scope/src/store.rs`：`ScopedLayers` 的 `create_layer`/`on_change` →
   `Arc<dyn Fn + Send + Sync>`；`Shared<K,V> = Arc<Mutex<Table>>`；`Undo =
   Arc<dyn Fn() + Send + Sync>`；`NamedEntries`/`AnonymousEntries`/`ScopedLayers` 泛型
   bound `V/L: Send + Sync`（闭包捕获共享表要求）；`make_undo` 幂等 `AtomicBool`；
   `anon_uid` 仍 thread_local（匿名条目 uid 不需要跨线程；worker 里每个线程自己计数，
   匿名条目本身不跨层共享）；Iter 族基于 lock 快照。**实验教训**：effect 的 action/
   工厂闭包是「用户可 panic」代码——Rc/RefCell 时代借出随 unwind 释放不污染后续；
   `Mutex` 若在持锁期 panic 会毒化锁使恢复路径（`cleans_up_failed_factories` 测试）
   全 panic。故 `effect` 先**锁外**建层（查缺短锁 → 工厂锁外执行 → 成功后再短锁插入），
   保 panic 恢复语义（测试 `scoped_layers_cleans_up_failed_factories...` 存活）。
2. `dsh-system-prompt`：`ToolProvider`/`VariableProvider`/`AssembleNext`/
   `AssembleListener`/`PromptSectionText::Fn`/`PromptContextText::Fn` 全 Arc+Send+Sync；
   `SystemPrompt{change_notify: Arc<dyn Fn()+Send+Sync>}`、`listeners: Arc<Mutex<Vec>>`；
   `new(config, change_notify)` 签名换 Arc（连锁 host/m2d/m2c/standing 调用点）；
   `install`（invariant.rs）监听器 Arc。
3. `dsh-llm`：`adapters: RefCell<HashMap>` → `Mutex<HashMap>`；`register_adapter(..,
   adapter: Arc<dyn LlmAdapter + Send + Sync>)`；`get_registration` 返回
   `Arc<AdapterRegistration>`（克隆出表释放锁）；`clone_rc` → `clone_registration`；
   `PreparedCallStream = Box<dyn FnMut(..)>` 保持非 Send（prepared 流只在 worker 内
   构造/消费，无需跨线程）。
4. `dsh-llm-deepseek`：`PayloadsResolver`/`resolve_connection` → Arc+Send+Sync。
5. `dsh-tools`：闭包族 `ToolRender/Execute/Finalize/IsConcurrencySafe/PresentCall/
   PresentResult/Disposer/Guard/PreDecision/ApprovalProvider` 全 Arc+Send+Sync；
   `ToolSignal{aborted: AtomicBool, reason: Arc<Mutex<Option<String>>>}`、
   `ToolRunContext.concludes_turn: Arc<AtomicBool>`；`ToolLayer` 的
   `mode/restrictions/guards/pre_decisions: Rc<RefCell<..>>` → `Arc<Mutex<..>>`、
   `tools: NamedEntries<Arc<ToolDefinition>>`（值 Rc→Arc，随容器 Send）；
   `ToolRegistry{on_change: Arc<dyn Fn+Send+Sync>, approval/run_code_executor:
   Arc<Mutex<Option<..>>>}`；`register(_global)` 取 `Arc<ToolDefinition>`；
   `M4Tool`/`M5Tool` slot `Arc<Mutex<Option<ToolExecute>>>` + `Arc<ToolDefinition>`；
   `schema.rs` `define_tool` 包裹闭包全 Arc。
6. `dsh-agent-loop`：`LoopDeps` 五个闭包 → `Arc<dyn Fn + Send + Sync>`（assemble/
   prepare_call/stream/project_context/tool_exec）；`RuntimeContextProjection` 捕获
   `Arc<Mutex>`；`ReactLoopAgent{phase: Mutex<Phase>, request_header_logged:
   AtomicBool, approval_pending: Mutex<Vec<PendingCall>>, propose: Arc<dyn Fn+Send+Sync>}`
   `new()` → `Arc<Self>`；`AgentLoopHost{agents/runtime_agents/disposers/joins/
   tool_exec_factory: Mutex, llm/tools/prompt: Arc, disposers: Arc<dyn Fn+Send+Sync>}`、
   `with_store/new` → `Arc<Self>`；`ToolExecFactory` → `dyn Fn(..) -> Arc<..> + Send
   + Sync`（host 要跨 worker 持有工厂）。**关键死锁修复**：
   `pending_by_call_id` 原来 `for id in self.agents.lock().unwrap().keys()..` ——for 头部
   临时 `MutexGuard` 活到循环体结束，循环内 `self.agent(&id)` 重复锁同一非重入 Mutex →
   任何非空 agents 表必死锁（dsh-cli 子代理在跑全量测试时抓到 `plan_approval_respond_
   routes_to_per_session_agent` 挂起；这也会挂生产 `session.approval.decide` RPC）。
   修复：先物化 `let ids: Vec<String> = lock().keys().cloned().collect()` 再遍历（短锁）。
7. `dsh-cli`（最大消费面）：llm/tools/prompt/host/ReactLoopAgent 句柄 Rc→Arc；
   dsh_cli_host 的 `RefCell`→`Mutex`；全部 M4/M5 工具执行器/系统提示 Fn/审批钩子 Arc；
   `GoalsRoundPort` 持 `Arc<ReactLoopAgent>`；boot 的 `plan_session` → `Arc<Mutex>`。
   **自下而上发现**：`dsh_jobs::JobRegistry`（含 `Box<dyn Fn>`）、`dsh_shell::Shell
   Process`（Box）、`dsh_terminal::TerminalSessionService`（Box<dyn TerminalBackend>）
   是 !Send，且其 crate 不在本关闸批次 —— `Arc<Mutex<T>>` 不可能。方案：小 `ThreadCell
   <T>` thread-local 桥（per-instance id + keepalive Arc 入 thread_local 状态池；
   Drop 清；复用仓库既有 CURRENT_CTX 纪律），句柄 Send+Sync 而底物仍单线程驻留
   serve/测试线程（Phase 4 worker 化顺延——这些底物的 worker 化是 Phase 4 范围）。
8. 测试连锁（全部子代理并行机械转换，行为/断言逐字保留）：dsh-tools 5 文件（m2b_
   tools/m2b_tools_runtime/m2f_approval/m3_guard/m4_tools）+ m2b_tools.rs（补），
   dsh-agent-loop 5 文件（m2e2_driver/m2e3_scheduler/m2e3_service/m2f_interaction/
   m2g_host），dsh-cli web 内 ~57 处 + m6_llm + standing；dsh-agent m2d_agent 的
   `sp()` → Arc + `install_model_selection(sp: &SystemPrompt)`（上游签名已换）。

**最终选择**：上述 1-8。粗锁 + Send+Sync 全链；`ThreadCell` 仅用于三个 !Send 底物
（最小、与既有 thread_local 纪律一致），不扩大为通用并发抽象。
**被否决**：(a) worker 自建服务树（两套状态、丢共享注册）；为 !Send 底物改其 crate
（超出本关闸破坏半径，Phase 4 再议）；把 `PreparedCallStream` 强 Send（无跨线程需要，
徒增约束）。
**预期影响与回滚点**：dsh-scope store 泛型 bound 收紧为 `V/L: Send+Sync`、dsh-system
-prompt/llm/tools/agent-loop/dsh-cli 全部公开句柄与闭包类型 Rc→Arc（破坏性换型，
编译期一次消化）；add_guard/present_as 等的 effect 锁外建层保 panic 恢复。回滚 =
git revert 本提交（型面回到 Rc 即失效，需协同回滚整批）。
**验证（Phase 3 关闸）**：`cargo test --workspace` 全绿 EXIT=0（191 套 ok；本关闸新增
/增强覆盖：dsh-scope 24 / dsh-system-prompt 42 / dsh-tools 28+27+9+11+12+22+16 /
dsh-llm 29 / dsh-llm-deepseek 34+4 / dsh-agent-loop 1+18+16+7+9+3+2+12 / dsh-cli
212+18；dsh-cli web 全量含 `plan_approval_respond_routes_to_per_session_agent` 经
`pending_by_call_id` 短锁修复通过）；`cargo clippy --workspace --all-targets` EXIT=0
零告警（收尾修 3 处：m2_scope unused `Cell`、m2c type_complexity allow、
ToolExecFactory `+Send+Sync`）。

---

## D-115（实施·Phase 4）：serve worker 化 + 传输中断化——长 RPC 上 worker 线程、M5 底物 Send 化、阻塞读可中断（round 4）

**触发问题**：serve 主循环（`recv_timeout` → `dispatch_request` 内联）把
`session.prompt`/`agent.run` 同步驱动整轮 turn：turn 排空期间 accept 循环被占死，
`session.cancel`（以及任何其它 RPC）无法并发送达 → 「生成中一键即停」不可达（HANDOFF
0.3 明示是 D-114 的设计使然，等 D-115 完成）。Phase 1-3 已把请求面全部 Send+Sync，
解锁 worker 线程执行长 RPC。需求/设计文档：
`.spec/phase4-serve-worker/requirements.md`、`design.md`。

**考虑的选项**：
- **取消语义（用户两轮提问拍板）**：(A) 仅 worker + step 边界合作式取消——长生成中
  cancel 要等整段 LLM 阻塞读读完才生效，不是真·生成中停止；(B) worker + **传输中断**
  ——请求级取消谓词直插阻塞读循环，abort 主动断开在途读（对齐 TS
  `packages/llm/llm-deepseek/src/adapter.ts` 的 `AbortSignal.any([options.signal, ...])`
  → `fetch(url,{signal})` + `adapterFailureChunk` 的 ABORTED）。**选 B**（用户第二次
  提问确认）。研究确认：Rust 现状 `dsh_core::llm_http::chat_completions_stream` 是
  **整段阻塞读**（Get-Content 式读到 Content-Length/close），`parse_sse` 一次性解析；
  `GenerateOptions` 无 signal 字段——不改传输则 worker 化只省「UI 不卡死」，停不下来。
- **worker 结果回填**：D-115 §3 原文「Result 经 channel 回填 accept」 vs pickDirectory
  先例（worker 持 `tiny_http::Request` 直接 respond）。**选后者**——同文件既有先例
  （web.rs `host.pickDirectory`），accept 完全不占、HTTP 同步契约不变，零额外路由。
- **M5 底物**：三 crate（dsh-jobs/dsh-shell/dsh-terminal）整体 Send 化 + 移除 Phase 3
  的 `ThreadCell` 桥改 `Arc<Mutex>`。`TerminalBackend` trait 不加 `Send` supertrait
  （只 Box 处 `+ Send`；`PtyBackend` 已 Send）。**用户二次确认共享 JobRegistry**：
  M4 job_* 工具与 M5 BashJobsBridge 共享同一 `Arc<Mutex<JobRegistry>>`（worker 线程
  里 run_in_background 起的 bash job 可被 job_kill/job_read 命中）；连锁把 M4 job_*
  执行器的 caller 从 `None` 改为 `ctx.agent`（授权围栏下 owner 才能读自己的 job——
  自下而上发现 `list(None)` 只返无主 job，共享注册表不传 caller 等于依旧不可见）。

**最终选择与理由**：
1. **传输中断化（B）**：`dsh_core::llm_http::chat_completions_stream_abortable` +
   `tcp_exchange_abortable`（200ms 短读超时轮询 cancel 谓词；置位 → 置 abort 标志、
   立即返回已读部分、不报错——abort 是正常终止语义）。`GenerateOptions.signal:
   Option<dsh_llm::AbortSignal>`（Send+Sync 共享谓词；serde skip 不入 wire/日志）。
   `dsh-llm-deepseek::DeepSeekAdapter::stream`：空 payload + signal aborted →
   `FinishReason::Aborted`（不落 EMPTY_RESPONSE/STREAM_CLOSED）。`dsh-cli::m6_llm`
   resolver 用可中断读并绑 `opts.signal`。`dsh-agent-loop` driver 装配
   `request.signal` = `cancel_token` 的**非消费**谓词（step 边界 `abort_reason()` 仍
   消费令牌；传输轮询只观察）。理由：真·生成中一键即停（D-115 目标）+ 对齐 TS 参考。
   预期影响：既有非 abort 路径零变化（signal=None 缺省语义不变）。
2. **serve worker 化**：`dispatch_request` 的通用 Post arm 对长方法白名单
   （`session.prompt`/`agent-loop`/`agent.turn`/`agent.run`/`commands/execute`/
   `session.approval.decide`）→ spawn 线程、move `Request` + `ServeWorkerFacts`
   （agent_loop/plan_session/approval_wire 三个 Arc——全部 Send+Sync）、worker 内读
   body → `dispatch_long_rpc`（host 参数化核心 `run_rust_loop_on_host`/
   `ensure_session_agent_on_host`/`decide_on_host`/`set_plan_mode_on_host`/
   `commands_execute_on_host`，摆脱 `&Boot`）→ `request.respond`。`session.cancel`、
   短 RPC、SSE/WS/静态、`respond`（审批 wire 收据）**刻意留 accept 同步**。每 driver
   单 turn 由相位机保证（`followup` 非 Idle 追加 inbox）；跨会话并行是扩展点。
   测试：worker 线程驱动完整 turn 与 inline 同语义（accept 不占）+ **一键即停验收**
   `accept_thread_sends_cancel_while_worker_turn_runs`：慢 mock 流在 worker 阻塞时
   `session.cancel` 从 accept 并发送达立即返回 + turn/end reason aborted。
3. **M5 底物 Send 化 + 移 ThreadCell + 共享注册表**：dsh-jobs `Box<dyn Fn>` →
   `+Send`；dsh-shell `ShellProcess` `Rc<RefCell<Inner>>` → `Arc<Mutex<Inner>>`（
   `SubprocessHandle` 已 Send——含 `win_job::Job` 既有 `unsafe impl Send`）；dsh-terminal
   `BackendProvider`/`OwnerLiveness`/`TerminalSession.backend` → `+Send`。dsh-cli 删
   `ThreadCell` 全桥（web_m5.rs + web.rs 25 处 `.with` → `lock`），`M5Host::assemble`
   取可选共享注册表；新增编译期 `send_asserts.rs`（每 crate 一文件）。理由：worker
   线程调 M5 工具是常态路径，`ThreadCell` 跨线程 panic 与目标直接冲突。

**自下而上发现（超出原库存，已显式处理）**：① 设计文档 §4.4 承诺的跨 M4/M5 注册表
共享**原代码不存在**（两实例各自独立）——本次实现共享 + 测试证明；② 共享后 M4
`job_list(None)` 仍看不到 owner 化 job（授权围栏设计如此）——执行器改传
`ctx.agent`，语义从「总览 all」转为「owner 见自己的 + 无主」。

**验证（Phase 4 关闸）**：`cargo test --workspace` 全绿 EXIT=0（194 套 ok；新增：
dsh-core abortable read 2 项、deepseek Aborted 映射 2 项、m6_llm 慢流中断 1 项、
driver signal 装配 1 项（m2e2 `request_signal_observes_shared_cancel_token`）、
三 crate send_asserts 各 1 项、dsh-cli worker 语义 + 一键即停 + 共享注册表 3 项；
dsh-cli lib 212→216）；`cargo clippy --workspace --all-targets` EXIT=0 零告警。
60880 演示服务以原命令行重启 → HTTP 200（kill 前 `cargo test`/build，装后重启，已按
既有批准模式执行）。

**环境事故与纪律强化**：Phase 4 中期误用 PS 5.1 批量文本替换打坏 `web.rs`（UTF-8 被
GBK 重编码、中文注释不可逆损坏）。修复 = `git checkout HEAD --` + 按已枚举清单重新
应用（子代理报告逐条 + 本会话已知改动），`cargo check`/`test`/`clippy` 全量验证等价。
纪律：对含中文文件一律用 `edit`/`write`/`cmd`/`bash`（原生 UTF-8），禁用 PS 5.1 文本
重写；多实例替换用 `edit replace_all`。

**被否决**：(a) 仅 worker + 合作式取消（非真即停——用户 B）；(b) worker 结果经 channel
回填 accept + pending Request 表（无收益复杂度）；(c) 每会话常驻 worker/线程池
（扩展点不强做）；(d) `TerminalBackend` trait 加 `Send` supertrait（破坏性扩面，Pty
已 Send 无需）；(e) 不共享注册表（bash job 对 job_kill 不可见——用户选共享）。

**预期影响与回滚点**：三 crate 公开类型破坏性换型（`+Send`/`Arc<Mutex>`，编译期连锁
dsh-cli 消费者）；`GenerateOptions.signal` 新字段（serde skip，wire 不变）；worker
化后长 RPC 的 HTTP 响应时点不变、cancel 可 turn 中并发送达（传输中断 B + step 边界
双保险）。回滚 = git revert 本提交（含三 crate 型面，需一起回）。

---

## D-115-Web（D1）：多 plugin_root —— 浏览器 roster 补 base 层，前端物化渲染

**触发问题**：Rust `dsh web` 下发前端后，浏览器报「37 entries did not activate」、
页面空白。重启无效。诊断链（需求分析见 `.spec/frontend-packaging/`）：
- Rust `build_boot_manifest` 从**单一 plugin_root**（web-app 层 node_modules）扫描
  `dsh.client.platform=="web"`；
- vendored 前端 dist 是**旧协议**（queue 门面，与 Rust 注入逐字匹配），安装版 dist 是
  新协议 N3（自装 loader 拒绝已存在）→ 必须用 vendored 编译产物随 Rust 打包；
- **base 层**（packages/bundle/base）的 `dsh-typert-registry`（提供 `typert`）与
  `dsh-api-gateway`（client 半提供 `remote`，inject=[typert,connection]）因 pnpm 隔离
  不在 web-app 层 → Rust 扫不到 → 这俩 client.js 404 → `typert`/`remote` 无人提供 →
  runtime（依赖 connection/typert/remote/remote.commands）无法激活 → slots/sessions/
  workspaces/conversationEvents/Views 全缺 → 37 连锁 pending。
- 权威：TS host 的 modules node 半 `compose()` 遍历 loader entries → `require.resolve`
  逐包取 `dsh.client`——**roster 来自组合，非目录盲扫**；Rust 多 root = 复现 base+web
  两层组合。

**考虑的选项**：
- (a) 只把 web-root 换成含 base 的单一 node_modules——不存在（pnpm 隔离，无单目录含两层）。
- (b) 多 plugin_root 合并扫描（base 前、web 后，同名后者覆盖，对齐 cordis patch 层叠）。——
  **选定**。
- (c) 复制/链接 base bundle 进 web-app node_modules——维护包袱、占盘。否决。
- (d) 弃 vendored dist 用安装版——协议 N3 错配，否决。

**决策事项**：
1. `WebConfig.plugin_root: PathBuf` → `plugin_roots: Vec<PathBuf>`（多 root，有序）。
2. `build_boot_manifest` 单 root 委托新 `build_boot_manifest_multi(roots)`；后者遍历每
   root 依 `dsh.client.platform=="web"` + `lib/client.js` 收录，**同名 id 后者覆盖**；
   `HOST_COMPOSITION_EXCLUDED_CLIENTS`（native picker 流程）过滤保持。
3. `main.rs default_plugin_root` → `default_plugin_roots`：env `DSH_PLUGIN_ROOT`（兼容）；
   否则从 web_root 向上找 web-app 层 `@deepseek-ai`，再从祖辈逐级找
   `…/bundle/base/node_modules/@deepseek-ai` 兄弟作 base 层（base 前、web 后）。
   修正记录：初版 base 候选路径层级算错（按 web-app 子目录算），后改为「向上扫描
   祖辈找 base 兄弟」——首次验证仍浏览器 37 pending 即此因（manifest 有 entry 但
   bundle 404）。

**验证（D1 关闸）**：`cargo test -p dsh-cli --lib` 217 全绿（新增
`build_boot_manifest_multi_merges_base_and_web_roots`：两层合并、后者覆盖、非 web 跳过）；
60884（vendored dist + 多 root）浏览器 playwright/msedge headless 验证：console/page
errors **空**、`[class*="frame"]` 锚点命中、bodyLen 4347→42269、完整应用壳渲染
（sidebar/新会话/工作区/设置/echo-loop），boot 页消失 → **物化达成**。
后端已验证 base 层 gateway/typert client.js 现 200（此前 404）。

**待办（后续 D2/D3）**：补齐 remote 端点真实实现（messageFeedback/fileReferences/
sessionReferenceResolver/pluginInventory/dynamicCordisRunner）+ wasm 承载
（host-remote world）。dynamicCordisRunner 依赖 TS sandbox 的 4 方法
（getClientCode/invoke/reportRenderFailure/reportClientGuardFailure）用户接受显式
not-implemented，留待调研。

**回滚点**：git revert 本提交（plugin_root→plugin_roots 破坏性，需协同回滚）。

---

## D-115-Web（D3 + D2 前两簇）：wasm 组件承载 remote 端点 + 真实端点实现

**触发问题**：D1 已让浏览器渲染，但 `dsh-client-runtime` 依赖的 `remote`/`remote.commands`/
`remote.goals` 等命名空间只有命令/目标实现了 dispatch，缺 messageFeedback(3)/
fileReferences(1)/sessionReferenceResolver(1)/pluginInventory(1)/dynamicCordisRunner(12)
——这些不到位，`ui-setting-plugin-inventory`、`ui-message-feedback`、`ui-reference`、
`ui-cordis` 等 UI 仍无法用。用户裁定：**D3 全部新增端点放 wasm 插件承载**（组件模型、
禁 C ABI 漂移）；D2 全真实实现（禁空表/占位/假数据）。

**考量与抉择**：
- 承载路径：组件模型（wasmtime::component bindgen + cargo component 0.21.1 工具链已装、
  echo-loop 先例完整；`remote.handle` 天然返回 `list<u8>` 结果）vs C ABI（core-module
  加载快、分发小，但 `plugin_handle_event` 无返回通道需先扩 ABI）。**选组件模型**——
  复杂功能插件在类型安全/资源生命周期/多接口组合/结构化错误全面占优；用户明确
  「确定走组件模型，禁止功能漂移到 C ABI」。
- host-remote world 的 wit 放独立目录 `wit-host-remote/` + 独立 package `dsh:host-remote`
  （避免与 wit-dsh/dsh-loop 的 `dsh:dsh` 模块冲突——bindgen 同 crate 多 world 同名模块
  问题，D3 早期踩坑）。组件依赖经 Cargo.toml `[package.metadata.component.target.
  dependencies]` 指向该目录。
- 端点 get（只读投影）不够 messageFeedback 写类端点用 → host-services 补 `set`（真实
  持久后端由宿主投影器实现）；真实时钟 `time`/真实 uuid `newVersion`/会话消息
  `sessionMessages`/会话 identity `sessionIdentity`/持久 KV `kv` 服务均由宿主投影。

**决策事项**：
1. `dsh-wasmrt/src/remote.rs`：`WasmRemoteEndpointPlugin`（组件模型）+ `RemoteService
   Projector` trait（get/set 宿主投影）+ `host_services::Host` for store + thread_local
   projector 注入（send 纪律同 component.rs）。导出经 `lib.rs`。
2. `wasm-plugins/host-remote/`：cdylib 组件，`remote.handle` 路由端点；业务逻辑在组件
   （pluginInventory：loader 投影跳 group 映射 wire；messageFeedback：note 校验/
   target-not-found 需要真实会话消息/version-conflict 乐观并发/持久 KV + 真实时间与
   uuid），未知端点 → not-implemented 错误（fail-loud）。
3. wit `host-services` 补 `set`；组件重建（cargo component build）。

**TDD 验证**：`tests/m31_host_remote.rs` 5 测试全绿——承载桥回路（host→组件→host）、
pluginInventory（group 跳过+wire 映射+真实 loader 调用）、messageFeedback 生命周期
（put→list→delete、版本并发、note-blank/too-large、session-not-found）、未知端点
not-implemented。dsh-wasmrt 全量回归绿。

**待办（D2 剩余）**：fileReferences / sessionReferenceResolver / dynamicCordisRunner
（真实子集 + TS-sandbox 依赖 4 方法 not-implemented）。dsh-cli EndpointHost 统一路由 +
真实宿主投影器装配（loader/sqlite/session 真实数据源）。dynamicCordisRunner 4 方法
待调研（getClientCode/invoke/reportRenderFailure/reportClientGuardFailure）。

**回滚点**：git revert 本提交（新增 crate 模块 + wit + 组件——均为新增，回滚干净）。

---

## D-115-Web（D2 收口）：真实宿主投影器 + dispatch wasm 回落 + wire 信封对齐 + 动态装配阶段 A

**触发问题**：D3 组件实现了端点业务，但 serve 未装配：无 `RemoteServiceProjector`
（wasm 端点反查宿主落空）→ dispatch 无 wasm 回落 → 前端 Cordis/inventory 等 UI
仍不可用。且组件返回 wire 与前端 RpcResult 信封不符（zod 拒）。

**调研发现（根本解法）**：Rust **dsh-loader 本就具备真实动态装配能力**（
`register_plugin(name, Arc<dyn Plugin>)` + `create(EntryOptions)`→`start_entry` 起
fiber + `write` 落盘 cordis.yml），不是「无动态 cordis 装配」。缺的是端点没接线 +
Boot 不暴露 loader。用户裁定**分阶段 A→B→C 清除根本**（不选 manifest 一次性代理）。

**决策事项**：
1. `Boot` 增 `loader: Option<dsh_loader::Loader>`（boot() clone），为投影器/动态装配
   提供真实句柄。
2. `dsh-cli/src/remote_host.rs`：`RemoteHost` 实现 `RemoteServiceProjector`——真实数据源：
   `loader`（dsh-loader.entries()）、`dynamicPlugins`（真实已组合插件，agentId="default"
   对齐 schema string）、`sessionMessages`/`sessionIdentity`/`sessionCandidates`
   （SessionEvent sink 平坦流按 session 过滤/去重）、`agentWorkspace`/`workspaceFiles`
   （default 工作区 + 真实 fs 扫描）、`time`（墙钟）、`newVersion`（uuid v4）、
   `kv`（进程内持久 map）。未知/只读服务 → 规范化错误；`set` 只允许 kv（真持久）。
3. `serve` 装配：host.sink 后构造 RemoteHost（注入 sink/loader/workspaces）+ 读
   host-remote 组件字节 → `WasmRemoteEndpointPlugin` → `boot.remote_plugin/remote_projector`。
4. dispatch 回落：`dispatch_wasm_remote`——`namespace/method` 拆分 → plugin.handle →
   组件已信封透传；未装配 → `internal` 错误（诚实，占位 era 的 `{ok:true,value:[]}`
   dynamicCordisRunner 占位**废除**）。

**wire 信封修正（真浏览器抓 zod 实证）**：前端 `rpc.call` 期望 server 返回
`{ok:true, value: <业务值>}` RpcResult 信封（value 才是 descriptor 解析对象），且
RpcError 联合 **无 not-implemented code**。组件全部端点改信封：pluginInventory
`{ok:true,value:{entries}}`、fileReferences/动态 inventory/sessionReference
`{ok:true,value:[...]}`、syncInspectManifest `{ok:true,value:null}`（Rust 无 cordis
inspect 宿主 → 诚实零态，非占位）、未知端点错误 code 用 `internal`（合法联合）；
`agentId` 必须 string（schema `intersection(string,unknown)`，null 被 zod 拒）。

**验证（D2 收口关闸）**：m31 全绿 8/8（envelope 断言 + syncInspectManifest 零态 +
internal code）；dsh-cli 217 全绿（`rpc_dynamic_cordis_runner_unassembled` 取代占位
era 测试）；clippy 0；60880 真浏览器 render-smoke **console/page errors 空**、bodyLen
47149→48279、DOM 深度证实 **Cordis Plugin 面板真实渲染**（0 running，来自 wasm 端点
读真实 loader）；`pluginInventory/list` 经 HTTP RPC 返回真实 entries（echo-loop /
dsh:services）。

**端到端实证（慢 mock LLM agent-loop，60885）**：会话主线 `session.prompt` 经
agent-loop 装配后仍工作（accepted:true，`--agent-loop` flag 必需——仅 `--llm-base-url`
不自动启用）；真实 user/message 事件产生 messageId（`prompt-default`，**事件 data.id
才是 messageId**——初版投影读 data.messageId/data.message.id 皆空 → 修读 data.id）；
对真实消息 `messageFeedback/put` → **ok + 真实 uuid v4 + 真实墙钟**；`list` 读回同物品
（持久链路组件 → host-services.get("kv") → RemoteHost.kv）。

**阶段 B/C（后续目标）**：dynamicCordisRunner 动态装配（runHostHalf→loader.create +
注册 wasm 组件插件→fiber 真跑；stopFromPanel→dispose；approval 状态机投影）+ 动态
wasm 包管理（按包路径/版本加载组件字节）。TS-sandbox 依赖 4 方法（getClientCode/
invoke/reportRenderFailure/reportClientGuardFailure）显式 internal 错误 + message
说明（用户已接受此诚实边界）。

**回滚点**：git revert 本提交（Boot 字段 + remote_host + dispatch 回落 + 组件信封）。

---

## D-115-Web（阶段 B/C）：dynamicCordisRunner 真实动态装配 + 动态包注册表 + wire 外壳修正

**触发问题**：阶段 A 后 dynamicCordisRunner 仍有 runHostHalf/stopFromPanel/undefineFromPanel/
settleUserRun/resolveRequestRun/resolveInspectQuery 未实现（面板 Play/Stop/Remove/Approve/
Decline 不可用）；且前端 gateway `rpc.call('/api', e, {args})` 把参数包在 `payload.args`
（此前 dispatch 直透 payload → 组件读平铺字段落空，真前端调用会失败——既往 curl 手写平铺
掩盖了此缺口）；错误信封缺 `details`（前端 serverResponseSchema 要求）。

**子代理调查确认**（deepseek-harness cordis-host-runner/packages）：启动热路径 = inventory +
syncInspectManifest（板块 driver）；面板热路径 = runHostHalf(直接)/settleUserRun/stopFromPanel/
undefineFromPanel/resolveRequestRun(拒绝)；后台 = getClientCode/invoke/report*/resolveInspectQuery。
inventory 的 packages = 该插件已定义的全部不可变版本（define order）；必填仅 pluginId/agentId/
packages，activeRun/latestRun 可选（诚实缺省合法）。

**决策事项**：
1. **阶段 B（真实动态装配）**：`RemoteHost` 增 `dynamic_packages` 注册表（`DynamicPackage`
   结构：pluginId/packageId/name/purpose/wasm 组件字节）——**dsh-plugin world 组件
   （WasmComponentPlugin，组件模型，禁 C ABI）为动态包载体**（hello-component 为真实验证物）。
   - `dynamic_activate`：查包 → WasmComponentPlugin → `loader.register_plugin` +
     `loader.create(entry id "dyn:<pluginId>")` → **真实 fiber 启动**；run_id=entry id。
   - `dynamic_stop`：`loader.remove`（dispose + 移除 entry，保留包定义）；未跑 → 诚实 not-running。
   - `dynamic_undefine`：stop + 注册表移除。
   - host-services set: `dynamicActivate/dynamicStop/dynamicUndefine`；get: `dynamicRegistry`。
2. **组件端点（wasm）**：runHostHalf（→dynamicActivate，收 `{ok:true,pluginId,packageId,
   pluginRunId,waitingFor:[],startedHere:true}`）、stopFromPanel（`{ok:true}`/`{ok:false,reason(
   plugin-missing|not-running),message}`）、undefineFromPanel（`{ok:true,wasRunning}`）、
   settleUserRun（**诚实 `{ok:false,reason:not-running}`**——Rust 无 client half 无 pending
   approval），resolveRequestRun/resolveInspectQuery（**`{accepted:false}` 诚实**——无 pending
   请求/查询可决；subagent 确认此即契约正确语义非伪造）。
3. **阶段 C（动态包注册表）**：inventory 数据源改 `dynamic_packages` 注册表（packages=已定义
   版本）+ 装配状态（activeRun/latestRun **running 时才附**，否则缺省——optional 键 undefined
   放行，null 会被 zod 拒）；agentId 恒 "default"（schema 拒 null）。
4. **wire 外壳修正**：dispatch_wasm_remote 解包 `payload.args`（前端真实形态）；错误信封统一
   补 `details:{}`（serverResponseSchema 要求）；未装配回落 code 改 `internal`（合法联合）。
   —— subagent 实证此二缺口否则真前端调用失败。

**TDD 验证**：dsh-cli 220 全绿（新增 `dynamic_assembly_activates_stops_undefines`、
`dynamic_wasm_runner_full_chain`（含阶段 C inventory running 状态 + resolve 诚实空态 +
args 壳）、`dispatch_wasm_remote_unwraps_args_entry`（解包 + details 补全）；m31 8/8
（更新：runHostHalf 真实实现后 stub 下 `{ok:false,message}` + resolve 空态）；clippy 0。

**待办**：真实 key 端到端（起 agent-loop 实例 + 真实 turn + 面板动态装配在浏览器实操）。
TS-sandbox 依赖 4 方法（getClientCode/invoke/reportRenderFailure/reportClientGuardFailure）
保持组件 internal 兜底（诚实，用户已接受）。`@pluginId` 正则词法差（loader id vs TS
`<prefix>-<n>`）影响 `@` 引用——功能缺口已记录，暂不影响 wire。

**回滚点**：git revert 本提交（remote_host 动态装配 + 组件新端点 + dispatch 解包）。

---

## D-115-Web（阶段 B/C 部署化 + 真实 key 端到端实证）

**触发问题**：serve 装配需动态包**来源**（面板才有真包可装配）；发现 UTF-8 BOM 破坏
package.json 扫描；用户提供真实 key 要求真实模型端到端验证。

**决策事项**：
1. **动态插件目录（WebConfig.dynamic_plugins_dir + `--dynamic-plugins-dir <dir>`）**：
   serve 扫描 `<dir>/<pluginId>/package.json`（name/version/purpose）+ `<dir>/<pluginId>/
   plugin.wasm`（dsh-plugin world 组件字节）→ 注册进 RemoteHost.dynamic_packages。
   缺失/无效目录 → 跳过（诚实，不 fail-loud 阻断 serve）。
2. **UTF-8 BOM 容错**：省 `strip_prefix('\u{feff}')`（PowerShell Set-Content -Encoding UTF8
   产 BOM，Serde from_str 拒）。实证扫描目录含 BOM 时 0 包。
3. **真实 key e2e（60886，agent-loop + `--env-file .env-e2e` + 动态插件目录 + vendored
   dist）**：
   - 真实模型 turn（`session.prompt`→accepted，模型 deepseek-v4-flash-0731-ext）产生真实
     user 消息 `prompt-default`；
   - `messageFeedback/put` 对真实消息 → **ok + uuid v4 + 墙钟**（真实 key 下完整闭环）；
   - `dynamicCordisRunner/inventory` → **真实列出 hello 包**（serve 扫描 → 注册 → wasm
     端点接线）；`runHostHalf` → **ok + pluginRunId:dyn:hello + startedHere**（hello 组件
     真装配进 loader）；inventory 反映 activeRun + latestRun(running)；pluginInventory
     显示 **dyn:hello fiberPhase active**（真实 loader 状态）；`stopFromPanel` → ok +
     dyn:hello entry 移除（真 dispose）；
   - **render-smoke：consoleErrors [] / pageErrors []、ok:true、bodyLen 41662、模型名
     deepseek-v4-flash-0731-ext 渲染**——真实前端在 args 壳 + details 补全后干净。
   关键：**上一轮 curl 平铺 payload 掩盖了前端真实 `{args}` 壳缺口**；本轮以真实浏览器
   （playwright msedge）+ 真实 {args} 形态验证，确认修正必要且充分。

**TDD 验证**：dsh-cli 221 全绿（+`scan_dynamic_plugins_dir_real_dir` 真实目录断言）、m31
8/8、clippy 0。

**待办**：浏览器「面板点按」级 puppeteer 交互（runHostHalf 按钮点击）留待真实 UI 演练；
`@pluginId` 词法缺口已记录。stub-capability：动态包容器 = dsh-plugin world 组件（hello 为
真实验证物；echo-loop 等非 dsh-plugin world 需转化后可作包）。

**回滚点**：git revert 本提交（WebConfig 字段 + scan + serve 注册）。

---

## D-115-Web（报告期修复）：设置页模型 CRUD 不可用 + 多轮对话「multiple start Match」

**触发问题**（用户报告，60886 真实 key 实例）：
1. 设置页「模型配置」增删改查不可用（页面无 provider 行/编辑禁用/保存失败）；
2. 前端 console 报 `conversation Context …:input-messageprompt-s3 received more than one
   start Match`——用户在 s3 会话多轮对话后复现。

**子代理调查（只读）**：
- **② 根因**（runtime conversation-assembler 深挖）：前端 `input-message` node 的 start
  Match **只由 `user/message` 事件触发**（message.ts:44-48），`inbox/spliced`/重放都不会；
  `acceptMatch`（conversation-assembler.ts:395-397）对同一 key 第二个 start 抛错，one-
  start-per-context 是**有单测的不变量**。触发它的不是事件重复——是**跨 turn 复用同一
  消息 id**：Rust `run_rust_loop_on_host`（lib.rs:554）User 消息 id 恒为 `prompt-{session_id}`
  （s3 两轮都是 prompt-s3），前端按 id 建 context → 第二轮 start 撞已有 → 抛错。
  修复点=后端：user 消息 id 会话内唯一。
- **① 根因**（api-proxy/ui-settings-models 深挖）：Rust 只注册了 `llm` settings namespace，
  但前端模型设置页数据源是 `llm.providers`（configurable-provider 目录）+ `settings.describe`
  + `settings.mutate` + `credentials.*` + `llm.discoverModels`。Rust `llm_providers` 读
  `boot.llm`（旧 LlmService 注册表）恒空 → 无 provider 行（设置页静默失效）；`llm.discoverModels`
  恒空 → 拉取模型永远 fetchEmpty。`settings.mutate('llm')` 本可写（已注册）——但前端目录行
  的 settingsNs 决定它 mutate 哪个 ns；Rust 声明 settingsNs='llm' 即前端写 'llm' ✓。

**决策事项**：
1. **②（消息 id 唯一化）**：lib.rs `run_rust_loop_on_host` user 消息 id 改
   `prompt-{session_id}-{host.events(session_id).len()}`——会话内单调（每次 prompt 递增，
   恢复会话续接也单调）。旧日志（修复前跨 turn 同 id）重放仍会触发前端 one-start 防护
   （正确拒绝 + UI 显示历史加载失败）——**向后兼容需前端 replaceWindow 重建 key
   （{id}~{seq}）**，属 vendored 前端契约修改，非本次主修复范围（subagent：前端容忍是错的，
   静默吞真实消息/掩盖数据损坏）。s3 为修复前测试数据：清理即可恢复打开。
2. **①（llm.providers 真实目录 + discoverModels）**：
   - `llm_providers`：返回**可配置 provider 目录**——deepseek 行 `{provider:'deepseek',
     displayName:'DeepSeek', settingsNs:'llm', settingsPath:[], active: agent_loop.is_some(),
     declared:true}` + `boot.llm` 注册路由追加（对齐 TS api-proxy providers 语义：目录∪注册表）。
   - `llm.discoverModels`：payload `{settingsNs, provider?, baseURL?...}`；provider 匹配
     `boot.agent_catalog` → 返回 catalog 真实模型（serve 探测获得）；非装配 provider →
     诚实空（Rust 无 TS 外部端点探测，不伪造）。settingsNs='llm'（Rust 已注册）使前端
     mutate/describe 直写真实 llm namespace（改 baseURL/model/apiKey/provider 真实生效）。
3. **既有能力确认**：settings.mutate/replace/update（真实写 dsh_settings + revision 冲突）
   + credentials.*（describe/set/unset 已实现）——「改/删/存密钥」路径 Rust 已支持，本次
   未改。

**TDD 验证**：dsh-cli 222 全绿（新增 `llm_providers_declare_directory_and_discover_models`、
`session_create_registers_agent_and_prompt_routes` 扩展两轮 prompt → 断言两个
`prompt-{sid}-N` 唯一 id；`rpc_session_prompt_runs_turn` 既有）；clippy 0。

**真实 key e2e（60886）**：
- 两轮真实 turn → history 两个不同 `prompt-default-11` / `prompt-default-20`（未修前都是
  `prompt-default`）；
- `llm.providers` → `[{provider:'deepseek', settingsNs:'llm', active:true, declared:true}]`
  （设置页显示 provider 行）；
- `settings.mutate('llm', set model)` → ok + revision:1，describe 读回 user.value.model 生效
  （编辑真实可写）；
- `llm.discoverModels` → `[{id:'deepseek-v4-flash-0731-ext'}]`（拉取模型真实）；
- render-smoke：consoleErrors [] / pageErrors []；s3 旧会话打开 → 前端防护显示
  「历史加载失败：…received more than one start Match」——旧坏数据边界（预期，非回归）。

**待办/边界**：
- 旧会话（修复前跨 turn 同 id 持久化）打开仍显示历史加载失败——处理选项：清测试会话
  （s3）或后续在 vendored runtime replaceWindow 做 {id}~{seq} 向后兼容（独立 PR，改前端
  one-start 不变量需连带更新其单测）。
- `session.selectModel` 仍仅 echo（不持久化默认模型到 settings）——默认模型切换重启回退，
  记录待办（可写 agent-default-model namespace 对齐 TS）。

**回滚点**：git revert 本提交（lib.rs id 唯一 + web.rs llm_providers/discoverModels）。

---

## D-115-Web（模型配置 CRUD 对齐 TS harness）——需求分析/系统设计

**触发问题**：用户要求按瀑布流让 Rust dsh web 的模型配置增删改查与 TS harness 完全一致；
且用户指示调研 llm crate 1.3.8 / genai 作为 pi-ai 多 provider 承载。

**调研结论（决策依据）**：
1. **llm crate 名字两次占用的澄清**（docs.rs/crate/llm/1.3.8 Note 权威）：0.1.x =
   rustformers 本地推理库（已归档）；1.0.0+ = graniet 远程多 provider HTTP 客户端。
   即便 1.3.8 可胜任，用户最终选定 **`genai = "0.6.5"`**（稳定版、多 provider 姿态更贴合）。
2. **genai 0.6.5 API 双源实证**（本地 spike `cargo build` 成功 + subagent 源码核对）：
   `Client::builder().with_adapter_kind(...)`（bound-adapter 免名嗅探）、
   `with_auth_resolver_fn`（apiKeyEnv→`AuthData::from_env`）、`with_service_target_resolver`
   （自定义 endpoint）、`exec_chat/exec_chat_stream`、`all_model_names(AdapterKind,
   ProviderConfig{endpoint,auth})`。`AdapterKind` 27 变体覆盖 pi-ai 协议
   （openai-completions→OpenAI、openai-responses→OpenAIResp、anthropic-messages→Anthropic）。
3. **异步约束**：genai 是 tokio async（edition 2024，需 Rust 1.85+；dsh 用 1.94 ✓）；
   dsh-core 已依赖 tokio rt+macros；现有 llm_http 是同步面 → genai 适配器内部持共享
   tokio runtime，`block_on` 桥接同步 `LlmAdapter`。

**设计决策（用户确认）**：
- 持久化：**Rust 自有文件**（settings.yaml + .credentials.yaml），不读 TS 的 C 盘 $DSH_HOME；
  serve 装配按 cfg 路径用 `SettingsProvider::file` + `CredentialProvider::file`，注册逻辑
  抽象为可复用函数（boot 与 serve 共用防 drift）。
- namespace 注册：`llm-deepseek`（扁平 {apiKeyEnv,baseURL,thinking,reasoningEffort,maxTokens,
  defaultContextWindow,models[]}, live）、`llm-pi-ai`（providers dict, live）、
  `agent-default-model`（{provider,model,reasoningEffort?}, live）。
- `llm.providers` 目录行对齐：deepseek-official → settingsNs='llm-deepseek' settingsPath=[]；
  pi-ai 行 → settingsNs='llm-pi-ai' settingsPath=['providers',route]；boot.llm 注册追加
  settingsNs=''。（修正当前返回 settingsNs='llm' 的错误。）
- `llm.discoverModels` 真实探测：llm-deepseek → 装配 catalog；llm-pi-ai → genai
  `all_model_names`；失败诚实 code。
- `session.selectModel`：校验（provider/model 可解析）+ `settings.replace('agent-default-model',
  {provider,model,reasoningEffort?})`；未注册 → model-unavailable。
- **genai 集成**：`dsh-cli/src/genai_llm.rs` 实现 LlmAdapter 注册 pi-ai 路由；deepseek 走
  现有 llm_http 不动；两路径统一 LlmRuntime 的 provider 路由。

**工件**：`.spec/models-config-crud/requirements.md`（定稿）、`design.md`（定稿 D-A~D-F）。

**验证**：genai 0.6.5 已在 scratch crate `target/genai-spike` 编译成功（71s），Cargo.toml
未被污染；真实 key e2e 在编码阶段后执行。

**回滚点**：设计阶段无代码改动，未 commit 业务变更；实现阶段提交后 git revert 该提交即可。

---

## D-115-Web（项目定位转向）：服务装配单元（Service as Assembly Unit）立项

**触发问题（用户裁定，项目根本方向）**：用户明确：「把 Rust 插件变成像 cordis 服务插件
一样的『服务装配单元』是项目创建的根本意义和基石；这个不完成，其他所有操作（模型配置
CRUD、wasm 端点承载、前端包装……）都是在偏离核心目标。」即 Rust 重写的根本意义是**复刻
Cordis 的配置驱动/依赖激活/可热更插件装配模型**，而不只是翻译 API。

**调研（只读权威提取）**：
- TS Cordis 装配契约已逐行提取（vendor/cordis + vendor/loader + vendor/include +
  vendor/hmr）：插件三形态、inject/Config/provide/intercept、按模块 specifier 解析、依赖
  隐式等待（提供者 Active→notify）、epoch 驱动、`[Service.init]` async generator、
  EntryTree 事务、HMR 四层、patch 层配置式装配。
- Rust 现状自读实证：dsh-core Plugin trait + dsh-loader（Cordis loader 移植）+ notify/
  epoch/refresh_fiber 核心 + DshServicesPlugin（服务提供者插件）+ 各 wasm 插件都 impl
  Plugin + dsh-diff（TS 原版 cordis trace 对比）——**装配引擎核心已存在**；缺的是「面」：
  服务插件被 lib.rs 特判注册、`config.wasm` 特判、平名仓库缺身份模型、`!!js` 空 ctx、
  无生成器 effect、无持久化写回。

**核心缺口清单（docs/SERVICE-ASSEMBLY-HANDOFF.md 全量）**：
- A1 插件身份键模型（回调 vs 平名仓库——最深差异）；A2 `!!js` 作用域缺 ctx 服务；
  A3 提供者 check/strict-active；A4 注入快照/unprovide 顺序/父链 walk；A5 intercept 合并；
  A6 `[Service.init]` 生成器 effect；A7 持久化写回。B1-B4 对齐项（extend/invoke、Group
  折叠、HMR 模块热更、config simplify）。

**阶段目标（用户确认的锚点）**：`--agent-loop` 时实际推理由 Rust 原生 loop 驱动；目标状态 =
  cordis.yml 声明一行插件（如 llm-pi-ai 或自定义服务）→ Rust 运行时按名解析、依赖激活、
  配置生效、可热更、持久化回写，语义与 TS cordis 等价。验收 = dsh-diff golden 行为等价 +
  m 系列测试。

**工件**：`docs/SERVICE-ASSEMBLY-HANDOFF.md`（交接文档，交付新 agent）。

**影响**：模型配置 CRUD（进行中的 genai/namespace 工作）是「面向用户的功能子集」；服务
装配单元是「根本底座」。两者并行但后者优先级更高；genai 适配器应设计为可被装配的服务插件
（可注册进 loader 仓库），避免后续改造。

**回滚点**：文档/决策无代码改动；后续架构演进各 commit 各自回滚。

---

## D-116（服务装配单元 Phase 1 需求分析定稿）

**日期**：2026-08-26

**触发问题**：用户按 `docs/SERVICE-ASSEMBLY-HANDOFF.md` 规划下一阶段开发——「本次只需要服务装配
单元的开发」。按瀑布流先做需求分析（方法论二：第一性原理 + 双视角 + 复盘追问），产出阶段关卡工件。

**关键调研（回答用户之问：deepseek harness 如何把前端组件作为 cordis 装配单元）**：
subagent 对 harness fork（`deepseek-harness/`）只读检索证实：前端插件与后端服务插件是**同一个
「插件=装配单元」模型**——同一份 vendored `@deepseek-ai/cordis` 的 Context/Fiber/Loader 运行时，
唯一区别是「代码到达层」（前端 `__DSH_BOOT__` 清单 + `ClientModuleSystem` 挂 `ctx.loader.internal`，
替代 Node ESM loader）。实证见 `.spec/service-assembly/harness-frontend-assembly-research.md`
（全断言带文件:行号）。
**Rust 侧对应现实**：`dsh web` 已做 roster 生成 + bundle 服务（前端行由浏览器内 TS cordis 激活，
`assertEntriesActive`）；Rust 侧「服务装配单元」的真正待办 = **后端服务插件 entry 化**。

**关键决策（用户逐条确认，D-S1..D-S5）**：
1. **D-S1 范围** = Phase 1 后端服务插件 entry 化（消除 boot 名称特判 + 「非 services 必 config.wasm」
   假设；新增自定义服务 entry 可声明装配）。**显式排除**「前端组件行的 Rust 引擎激活」（另一条大线）。
2. **D-S2 A1 身份键** = **与 deepseek harness 一致**：插件身份 = 解析后的插件实现本体（Rust 等价 =
   Arc 指针/新生代 uid）；name 仍为解析键，但「同名同实现=同身份、同名新实现=新身份」（cordis
   `registry.has(callback)` / re-import=新身份 的口径）。改动面深，设计阶段细化。
3. **D-S3 A2 `!!js` 条件装配** = 记录为边界，spike 另立。
4. **D-S4 A7 持久化写回** = **本轮做**：运行时 loader 更新（create/update/remove）除记录外真实写回
   cordis.yml（原子写），重启按落盘配置恢复；Config.simplify 反解随对齐面处理。
5. **D-S5 未提交 WIP** = commit 保留（模型配置 CRUD 线检查点 `c76d37d`，与装配单元开发解耦）。

**自下而上核实**：loader 按名解析已成立（loader.rs:385 注册 / loader.rs:724-728 查 `plugins.get(name)`
→ apply）；缺口 = boot 名称特判（lib.rs:174）+「非 services 必 config.wasm」（lib.rs:176-198）+
A1 平名仓库（loader.rs:40 / registry.rs:34）+ A7 writes 仅记录不落盘（loader.rs:41-42）。

**阶段结论**：需求分析关闸工件定稿 → 进入阶段 2（系统设计）。A1/A7 实现细节（身份键结构、落盘事务/
反解、与 include/HMR 接线）在设计阶段细化并按 TDD 落地。

**预期影响与回滚点**：本提交纯文档（`.spec/service-assembly/requirements.md` + 研究报告 +
DECISIONS 条目）。回滚 = 撤本提交即回授予前状态；后续编码各 commit 各自回滚（改动 → 提交 →
本条目互查）。

---

## D-117（服务装配单元 Phase 1 系统设计定稿）

**日期**：2026-08-26

**触发问题**：需求关闸通过后进入阶段 2（系统设计），需把 E1（entry 化）+ E2（A1 身份键）+
E3（A7 写回）+ E4（等价性）落成可验收设计（`.spec/service-assembly/design.md`）。

**自下而上核实的新事实（设计期）**：
- `EntryOptions` 全字段 `Serialize/Deserialize`（entry.rs:14-42）→ `serde_yaml::to_string(entries)`
  即无损 YAML 配置反解（`merge_path_for_include` lib.rs:564 已有先例）；`dsh-cli` 已依赖
  dsh-persistence（`fs_atomic::atomic_write` 可用）——A7 落盘无新依赖。
- `boot()` 依赖 `dsh:services` 经 `include.load()` 应用（lib.rs:211-215 `get_typed("sessions")`——
  证明 services entry 已走 loader 按名 apply）；E1 只改 loop 装配判定，`dsh:services` 行为零变化。
- HMR refresh 的 loop 定位（lib.rs:255）同有 `name != "dsh:services"` 特判，随 E1 一并消除。
- `Arc::ptr_eq` 对 `Arc<dyn Plugin>` 成立 → 可作「同实现=同身份」判定；`dyn Plugin` 指针身份用
  每注册铸新 `Arc<()>` token（复用 dsh-scope ScopeKey 的 Arc 身份纪律）。

**关键设计决策**：
1. **E1**：`boot()` 装配循环只认 `config.wasm` 为 loop 入口；新增 `register_host_service_plugins`
   登记面（现 dsh:services，未来 genai 适配器等追加）；其余入口全走 include.load() 按名解析。
2. **E2（A1 实现为本身份）**：`plugins: HashMap<String, PluginRecord{ identity: PluginIdentity(Arc<()>),
   plugin: Arc<dyn Plugin>, generation: u64 }>`；`register_plugin` 同一 Arc 幂等、不同 Arc 铸新身份
   +generation+=1（harness re-import=新身份 口径）；`load_plugin` 把身份记录到 Entry（为 HMR
   换代 / case-4 预备）。**范围控制**：本阶段只做注册语义+可观察身份+Entry 记录，B3 HMR 完整
   链路后续。
3. **E3（A7）**：loader 增通用 seam `set_persist(PersistSink)` + `entry_options()` 权威有序列表 +
   `persist()`；触发点在 create/update/remove 的 `write()` 之后；宿主在 boot 完成后挂 seam，
   原子写主 config_path（合并后权威列表）；Config.simplify 由 Value→YAML 直写承担（DIV-3）。
4. **E4**：新增/扩展 dsh-diff「服务依赖激活」剧本（06-dependency-gate 形态），TS golden 对齐。
5. **实现顺序**：S1(E2)→S2(E1)→S3(E3)→S4(E4)，各独立提交=回滚点；TDD 红→绿。

**阶段结论**：系统设计关闸工件定稿（`.spec/service-assembly/design.md`）→ 进入阶段 3（编码实现，
TDD 红→绿，S1..S4 逐提交）。

**预期影响与回滚点**：本提交纯文档。回滚 = 撤本提交；S1..S4 编码各自独立可回。

---

## D-118（服务装配单元 Phase 1 · S1 编码：A1 插件身份键落地）

**日期**：2026-08-26

**触发问题**：按设计 S1=E2（A1 实现为本身份）进入编码（TDD 红→绿）。

**自下而上核实**：`st.plugins` 仅 dsh-loader 内部 3 处使用（register_plugin / load_plugin /
load_plugin_async）；`Arc::ptr_eq` 对 `Arc<dyn Plugin>` 成立；`dsh-diff` 的 `.plugins` 是自己
结构体字段（无关）。

**实现（红→绿）**：
- 新增 `crates/dsh-loader/src/identity.rs`：`PluginIdentity(Arc<()>)`（指针身份，PartialEq/Eq/Hash
  按 `Arc::as_ptr`）+ `PluginRecord { identity, plugin, generation }`。
- 仓库 `plugins: HashMap<String, PluginRecord>`；`register_plugin` 语义：同名**同一 Arc** → 幂等
  （身份/generation 不变）；同名**新实现** → 铸新身份 + generation+=1（harness re-import=新身份）。
- `load_plugin`/`load_plugin_async` 解析 `record` → 把 `identity` 记录到 `Entry.identity`。
- 新增访问器 `plugin_identity/plugin_generation/entry_identity`；`Entry` 增 `identity: Option<..>`
  （4 处构造点补 `identity: None`；group 合成插件不记身份——不进注册表、B2 后续）。
- 红测 `tests/m16_identity.rs` ×4：同实现幂等 / 新实现新身份+generation 递增 / entry 记录解析
  身份且换代重挂载更新 / 未知 name None。

**范围控制（DIV-4）**：本步只做注册语义 + 可观察身份 + Entry 记录；B3（HMR 模块热更完整链路）
后续阶段。

**环境处理（D-115 同款已记）**：`target\debug\dsh.exe` 被先前阶段遗留的 60880 演示服务（PID
36560）占用 → 按 D-115 先例 Stop-Process 后跑测试/clippy；演示服务未自动重启（恢复命令见
D-118 提交信息保留；需要时由用户决定是否以原命令行重启）。

**验证（阶段门槛）**：dsh-loader m16_identity 4/4 绿；dsh-core/dsh-loader/dsh-diff/dsh-wasmrt/
dsh-cli 全量回归 EXIT=0；`cargo clippy -p dsh-loader --all-targets -- -D warnings` 零告警。

**预期影响与回滚点**：仓库键型变更收拢 dsh-loader 内；公开量只增（PluginIdentity/PluginRecord +
访问器），破坏性面 = `LoaderState.plugins` 字段型（无外部读）。回滚 = `git revert` 本提交
（独立回滚点）。

---

## D-119（服务装配单元 Phase 1 · S2 编码：E1 服务插件 entry 化）

**日期**：2026-08-26

**触发问题**：按设计 S2=E1（entry 化——boot 装配循环只认 `config.wasm` 为 loop；消除
`dsh:services` 名称特判与「非 services 必 config.wasm」假设）进入编码（TDD 红→绿）。

**实现（红→绿）**：
- 新增公开 `register_host_service_plugins(loader)`：宿主可用服务插件登记面（现 `dsh:services`；
  未来 genai/llm-pi-ai 适配器在此追加）——「名称 → 实现」登记收敛于此，消除 boot 内联特判。
- `boot_with_host_plugins(config, overlays, wasm_base, extra_host_plugins)`：boot 前把宿主/测试
  追加的服务插件按名注册进仓库 → include.load() 按名解析 apply；`boot()` = 便捷包装（`&[]`）。
- loop 装配：只认 `config.wasm` 入口构建 `WasmLoopPlugin`；其余入口（服务/普通插件）由
  include.load() 处理（服务插件 entry 化判据）。
- HMR refresh 的 loop 定位由 `name != "dsh:services"` 改为 `config.wasm` 存在性判定（否则
  「服务 entry 出现在 loop 前」会把 loop 误指到服务行）。
- 红测 T1/T2（m9_boot.rs）：cordis.yml 声明 `dsh:test-svc`（自定义服务，apply 时
  `provide("test-svc-marker", Arc::new(42i64))`，loop 置于其后）→ 修改前 `boot_with_host_plugins`
  不存在（E0425 红）/ loop 定位误判；修改后 boot 成功、按名 apply、marker 可见 + refresh 不误判
  loop、run_turn 仍由 echo-loop 驱动。

**偏差（DIV-1 落地）**：「新增服务插件 entry」的实现可用性 = 宿主登记（静态）+ 测试注入
（`boot_with_host_plugins` extra）；cordis.yml 声明而仓库缺失 → `unknown plugin` fail-loud。

**验证（阶段门槛）**：m9_boot 20/20 绿（含 2 新）；dsh-core/dsh-loader/dsh-diff/dsh-wasmrt/
dsh-cli 全量回归 EXIT=0；`cargo clippy -p dsh-cli --all-targets -- -D warnings` 零告警。

**预期影响与回滚点**：`boot()` 签名不变（wrapper 语义），主调用方（main.rs/m9_boot）零改动；
新增公开 `boot_with_host_plugins`/`register_host_service_plugins`。回滚 = `git revert` 本提交
（独立回滚点）；S1（身份键）与 S3+（写回）互不依赖。

---

## D-120（服务装配单元 Phase 1 · S3 编码：E3/A7 持久化写回）

**日期**：2026-08-26

**触发问题**：按设计 S3=E3（A7 持久化写回——运行时 loader create/update/remove 真实写回
cordis.yml）进入编码（TDD 红→绿）。

**自下而上核实**：`LoaderState.writes: Vec<String>`（loader.rs:41-42）只记录不落盘；`write()`
是每次成功变异的单一提交点（14 处调用，全部位于 create/update/remove 的 Result 返回函数内）；
`dsh_persistence::fs_atomic::atomic_write(&Path, &[u8])` 签名确定、dsh-cli 已依赖 dsh-persistence；
`Boot.loader: Option<Loader>` 在 boot() 装配（lib.rs:347）。

**实现（红→绿）**：
- dsh-loader：
  - `PersistSink = Rc<dyn Fn(&[EntryOptions]) -> Result<(), String>>`；`Loader.persist:
    RefCell<Option<PersistSink>>`（`Loader::new` 置 None；`#[derive(Clone)]` 兼容）。
  - `entry_options()`：root 组声明顺序的权威入口列表（`serde_yaml::to_string` 即 cordis.yml 拓扑）。
  - `write(record)` 改 `Result`：记录 + sink 存在则落盘，错误 `map_err(CordisError::Internal)?`
    fail-loud；14 处调用点改 `self.write(...)?`。
  - `set_persist(Option<PersistSink>)`。
- dsh-cli：`attach_config_persist(loader, config_path)`——宿主在 boot 完成后把 seam 挂到 loader，
  原子写主配置（避免启动期 include.load() 意外回写）。
- 红测：`m17_persist.rs` ×4（create 权威列表/update+顺序/remove/sink 错误 fail-loud）+ m9_boot
  `runtime_mutation_persists_to_config_and_reboots`（loader.create → 主配置落盘含 `dsh:test-svc` →
  重 boot 恢复 apply，marker 可见）。

**范围/DIV**：写回目标 = 主 config_path（合并后权威列表）；overlay 变更物化进主文件（DIV-2）；
Config.simplify 由 Value→YAML 直写承担（DIV-3）；group 嵌套以 root 组 entry_config 保真
（Round-trip 由 m9_boot 既有 group 场景零回归覆盖）。

**验证（阶段门槛）**：m17_persist 4/4 + m9_boot 21/21 绿；dsh-core/dsh-loader/dsh-diff/dsh-wasmrt/
dsh-cli 全量回归 EXIT=0；clippy `-D warnings` 零。

**预期影响与回滚点**：`write()` 返回型改变但全内联（14 处 `?`）；公开量只增
（PersistSink/set_persist/entry_options/attach_config_persist）。回滚 = `git revert` 本提交
（独立回滚点）。

---

## D-121（服务装配单元 Phase 1 · S4 编码：E4 dsh-diff 服务依赖激活等价剧本）

**日期**：2026-08-26

**触发问题**：按设计 S4=E4——「服务插件依赖激活」dsh-diff 等价剧本：cordis.yml 声明的服务 entry
经 **loader 按名路径**装配：提供者 provide → 依赖方 inject 等待自动激活（与 TS cordis 语义等价）。

**自下而上核实**：06-dependency-gate 已是 cordis **非 loader**（ctx.plugin 直挂）层面的依赖门等价；
dsh-diff `ScenarioPlugin`（Rust）已支持 `ApplyOp::Provide`（lib.rs:610，trace `provide:{svc}:{json}`
与 TS `JSON.stringify` 一致）；**loader-host.mjs（TS）的 apply DSL 只支持 log/log-config**——「loader
按名 entry + provide 服务」的等价面缺对称支持 → 本步补齐。

**实现（红→绿）**：
- `diff/ts-host/loader-host.mjs` buildPlugin 增 `case 'provide'`（trace + `ctx.provide`），镜像
  scenario-host；`verify-diff.mjs` 把 `loader-13-*` 加入 ASYNC_SCENARIOS。
- 新增 `scenarios/loader-13-service-entry-dependency-activation.json`：consumer（`inject:["svc"]`）+
  provider（`provide svc:"v1"`）经 `loader-create` 顺序挂载 + `loader-remove` + 重建。
- golden 由 **TS loader-host（vendored cordis-plugin-loader）生成**；Rust dsh-diff 逐行对比。

**验证（等价关闸）**：`node verify-diff.mjs` **18/18 PASS**——新 `loader-13` golden 27 行精确匹配
（`plugin:consumer` PENDING（无 apply）→ provider provide → consumer Loading→Active → 卸载双 Unload →
再挂载再激活），既有 17 场景零回归（golden 逐字节未变）。

**预期影响与回滚点**：纯 diff 基建（loader-host + verify-diff + scenario + golden），无 Rust 运行面
改动。回滚 = `git revert` 本提交；对 S1-S3 零影响。

---

## D-122（服务装配单元 Phase 1 · 部署接线：serve 挂持久化 seam + 验收报告）

**日期**：2026-08-26

**触发问题**：S3 的 `attach_config_persist` 只做了公开 helper + 集成测试，生产 `serve()` 未接线——
运行时动态装配（dynamicCordisRunner loader.create/remove）在 `dsh web` 下不会落盘；按设计 §5.2
「宿主在 boot 完成后挂 seam」补上部署接线。

**实现**：
- `WebConfig` 增 `config_path: PathBuf`（仅 main.rs 构造点 +1 字段）；
- `serve()` 在 loader 可用处（boot 完成后、start 前）`if let Some(loader) = boot.loader.clone() {
  crate::attach_config_persist(&loader, &cfg.config_path); }`——启动期 include.load() 无 seam，
  无意外回写；运行时任何 loader 变更真实原子写回主 cordis.yml。

**验证**：`cargo build -p dsh-cli` 绿 + m9_boot 21/21 + 全 workspace test EXIT=0 + clippy `-D warnings`
零（serve 接线后复跑）。

**预期影响与回滚点**：`dsh web` 运行时动态装配开始落盘（重启恢复）；`boot()` 语义零变化。回滚 =
`git revert` 本提交（WebConfig 字段 + serve 三行接线，独立回滚点）。

---

## D-123（服务装配单元 Phase 1 验收收口：阶段 4 关闸 + 阶段 5 部署冒烟 + acceptance 工件）

**日期**：2026-08-26

**触发问题**：S1-S4 + 部署接线全部落地，按瀑布流做阶段 4（测试验证）与阶段 5（部署）验收收口，
产出可审计验收工件。

**阶段 4 关闸证据**：`cargo test --workspace` EXIT=0 全绿（含 m16_identity 4/4、m17_persist 4/4、
m9_boot 21/21 新套件）；`cargo clippy --workspace --all-targets -- -D warnings` EXIT=0 零告警；
`node diff/ts-host/verify-diff.mjs` 18/18 PASS（新 loader-13 golden 27 行逐行一致，既有 17 场景
golden 逐字节未变）。

**阶段 5 部署冒烟**：真实 serve（生产 `target/web/cordis.yml`，port 60881）——`/` HTTP 200、
`/api/host.describe` RPC 返回真实宿主信息、stderr 干净；persist seam 已挂载（D-122）。agent-turn
冒烟按门控纪律诚实跳过（无 DEEPSEEK_API_KEY → fail-loud AUTH；key 仅进程环境不入库验后清除）。
冒烟进程已停（不占 dsh.exe）。先前 60880 遗留演示服务（S1 前 Stop）恢复由用户决定（D-118 保留原
命令行）。

**工件**：`.spec/service-assembly/acceptance.md`（验收报告：交付范围/测试证据/部署运行/回滚/诚实
边界/决策链互查）。

**预期影响与回滚点**：Phase 1 全部五个阶段过关。回滚 = 各步独立 `git revert`（见 acceptance §3.3）。
下一阶段（A2/A3/A4/A5/A6/B 类 + A1 HMR 完整链路）按 handoff 缺口清单另行立项。

---

## D-124（服务装配单元 Phase 2 需求分析定稿：A3+A4 依赖激活核对）

**日期**：2026-08-26

**触发问题**：用户「继续下一阶段开发」。Phase 1 完成后，handoff 缺口清单剩余核心项：A3（提供者
check/strict-active）、A4（注入快照/unprovide 顺序/父链 walk）、A6（生成器 effect）、A5（intercept
合并）、B 类。范围经第一性原理定界后用户确认 **A3+A4 依赖激活核对**（闭环「按依赖自动激活」验收面）。

**自下而上核实（Phase 2 预勘）**：
- A3 核心已存在：`CheckFn`/`Impl.check`/`check_ok()`（reflect.rs:11-34）、`check_impls` false→PENDING
  （runtime.rs:616）、`provide_with(name,value,check)`（context.rs:1275）、provide 仅 ACTIVE fiber
  （context.rs:1284 `InactiveEffect`）。
- A4：disposer = `remove_impl→notify`（context.rs:1308）vs TS「先 notify 再自清」（reflect.ts:297-303）
  ——**顺序待 golden 判定**；epoch/refresh 已有（runtime.rs:633-666）；`resolve_scope` 父链 walk 已有
  （runtime.rs:301-315）。
- 缺口 = **等价覆盖为零**（18 个 dsh-diff 剧本均无 check 用例）+ DSL 无 check 参数（两侧对称扩展）
  + unprovide 顺序待判定 + 跨 realm golden 缺失。

**关键决策（Phase 2）**：P2-SCOPE=A3+A4；DIV-2-1 顺序分歧以 TS 为权威修复 Rust；DIV-2-2 check
golden 用静态 bool（动态态变 spike 另立）；DIV-2-3 父链 walk 以「解析落 realm + 块级时序」为准
（loader 级 isolate 场景承载）。

**阶段结论**：需求关闸工件 `.spec/service-assembly-p2/requirements.md` 定稿 → 进入阶段 2（系统设计）。

**预期影响与回滚点**：本提交纯文档。回滚 = 撤本提交；后续设计/编码各自独立可回。

---

## D-125（服务装配单元 Phase 2 系统设计定稿：A3+A4 核对分解）

**日期**：2026-08-26

**触发问题**：Phase 2 需求关闸通过 → 阶段 2（系统设计）。把 A3/A4 核对分解为可验收设计。

**设计要点**：
- **S1**：dsh-diff DSL 对称扩展——`provide` op 增可选 `check`（bool；TS scenario-host 与 Rust
  `ApplyOp::Provide` 两侧对称）——A3 的 check 门可在剧本表达。
- **S2**（golden 集）：`scenario-10-provide-check-gate`（A3a）、`scenario-11-unprovide-order`
  （A4a，provide+立即 unprovide）、`loader-15-cross-realm-walk`（A4c，group isolate realm 父链 walk）；
  A3b strict-active 由既有 06/loader-13 覆盖。
- **S3**：m 系列锁定（m7 check await / m3_isolate 跨 realm）。
- 关键设计决策：`provide` 的 disposer 亦入 `disposers` 索引（可被 `dispose-effect` 定向——A4a
  unprovide 而不卸 fiber 的载体）；顺序分歧若 golden 暴露 → 以 TS 为权威修复（DIV-2-1）。

**验证（设计关闸）**：.spec/service-assembly-p2/design.md 定稿；实现按 S1→S2→S3 TDD。

**预期影响与回滚点**：本提交纯文档。回滚 = 撤本提交。

---

## D-126（服务装配单元 Phase 2 编码落定：A3+A4 等价核对全过 + 零核心修复）

**日期**：2026-08-26

**触发问题**：按设计 S1→S2→S3 实施 A3+A4 核对（TDD 红→绿）。

**实施（S1/S2/S3）**：
- **S1 DSL 对称扩展**：scenario-host.mjs 与 Rust `ApplyOp::Provide` 增可选 `check`（false → 依赖方
  PENDING）；`provide` 的 disposer 入 `disposers` 索引（`dispose-effect` 可定向 unprovide）。
- **S2 golden 集**（TS 生成、Rust 逐行对齐，各首次即 PASS）：
  - `scenario-10-provide-check-gate.golden`（6 行）：provider `provide svc` 带 `check:false` →
    provider Active、consumer 保持 PENDING（**A3a check 门**）。
  - `scenario-11-unprovide-order.golden`（6 行）：provide 后立即 unprovide → 后续依赖方 PENDING、
    provider 保持 Active（**A4a unprovide 序，trace 级无可观察差**——Rust `remove_impl→notify` 与
    TS「先 notify 再自清」等价）。
  - `loader-15-cross-realm-walk.golden`（14 行）：group isolate realm 内 provider provide svc →
    子 consumer 沿父链 walk 解析到组 realm → Active+apply/log（**A4c 父链 walk**）。
- **S3 m 系列锁定**：m7_await `await_gated_by_check_predicate`（check=false → 依赖方 Pending）+ m3_isolate
  `group_realm_walk_resolves_parent_provider`（跨组 realm 解析）——均绿。
- **关键结论**：A3a/A4a/A4c 与 TS 等价**首次即对齐，无需 dsh-core 核心修复**（疑似 check 缺失/
  unprovide 顺序分歧经实证不存在；A3 的 check 谓词在核心早已具备、18 剧本缺覆盖是唯一的真缺口）。

**验证（阶段 3/4 关闸）**：`node verify-diff.mjs` **21/21 PASS**（新增 3 场景 + 既有 18 零回归）；
dsh-core/dsh-loader/dsh-diff/dsh-wasmrt/dsh-cli 全量 EXIT=0；clippy `-D warnings` 零。

**预期影响与回滚点**：dsh-diff DSL（Provide 加 check 字段 + disposer 索引）为兼容增量（既有剧本
零变化）；m 系列只增测试。回滚 = `git revert` 本提交（dsh-diff/diff-ts-host/测试/剧本，独立回滚点）。

---

## D-127（服务装配单元 Phase 2 验收收口：阶段 4 关闸 + 阶段 5 部署冒烟 + acceptance 工件）

**日期**：2026-08-26

**触发问题**：Phase 2 编码完成 → 阶段 4（测试验证）与阶段 5（部署）验收收口。

**阶段 4 关闸**：`cargo test --workspace` EXIT=0（含 m7_await 5/5、m3_isolate 3/3 新用例）；
`cargo clippy --workspace --all-targets -- -D warnings` EXIT=0；`node verify-diff.mjs` 21/21 PASS
（三个新 golden 逐行对齐）。

**阶段 5 部署冒烟**：`dsh web target/web/cordis.yml`（port 60882）`/` HTTP 200、进程干净退出——
Phase 2 零改运行面（dsh-core/boot/serve 未动），serve 零回归（冒烟后进程已停）。

**工件**：`.spec/service-assembly-p2/acceptance.md`（验收报告：A3a/A4a/A4c 等价证据、阶段 4 证据、
部署/回滚、诚实边界、决策链互查）。

**预期影响与回滚点**：Phase 2 全部五阶段过关。回滚 = `git revert e97dc05`（编码，独立）/
`3cbd48b`（设计）等。剩余缺口 A5/A6/B 类 + A3 动态 check spike 后续按需立项。

---

## D-128（服务装配单元 Phase 3 需求分析定稿：B3 HMR 模块热更）

**日期**：2026-08-26

**触发问题**：用户「继续下一阶段开发」。验收五维度中「可热更」仍未闭合（`hmr.rs` 只做配置文件
watcher，缺插件实现级热更）。范围经第一性原理定界后用户确认 **B3 HMR 模块热更**（借 Phase 1 A1
的身份换代基础，直接闭环「可热更」验收维）。

**自下而上核实（Phase 3 预勘）**：
- A1 检测数据已就绪：`register_plugin` 同名新 Arc → 新身份+generation（loader.rs:392-408）；
  `Entry.identity` 记录（Phase 1）；访问器 `plugin_identity/generation/entry_identity`。
- 缺「同 name 换实现」路径：`loader.update` 的 replace 分支只认 name/group/inject 差
  （loader.rs:575）；`remove+create` 破坏性（dispose/group 抖动）；无 identity 换代驱动的
  replace/reload 层；dynamic_activate 无同 entry 换代。
- 依赖方重活：epoch=owner uid 拼接（runtime.rs:633-666）——reload 后 uid 是否换、依赖方是否
  自动重活，**设计期自下而上定**（DIV-3-1：uid 复用则显式刷新受影响依赖方）。

**关键决策**：P3-SCOPE=B3；H1 换代入口=`replace_plugin(name,new_impl)`（走 A1 语义 + 驱动 reload）；
H2 entry 保真 reload（不破坏 id/options/group）；DIV-3-2 本例无新 dsh-diff golden（DSL 无法表达
同 name 换实现）→ m-series 红→绿为等价主证据。

**阶段结论**：需求关闸工件 `.spec/service-assembly-p3/requirements.md` 定稿 → 进入阶段 2（系统设计）。

**预期影响与回滚点**：本提交纯文档。回滚 = 撤本提交；后续设计/编码各自独立可回。

---

## D-129（服务装配单元 Phase 3 系统设计定稿：B3 replace_plugin + reload）

**日期**：2026-08-26

**触发问题**：Phase 3 需求关闸通过 → 阶段 2（系统设计）。把「身份换代 → 受影响 entry reload」落成
可验收 API 设计。

**关键设计定案**：
- `Loader::replace_plugin(name, new_impl) -> Result<usize, CordisError>`：同 Arc 幂等（Ok(0)）；新
  Arc → A1 换代 + 收集 `entry.options.name==name && identity!=新` 的 entry → `reload_entry`（count）。
- `reload_entry` = `dispose_entry + start_entry`（保 entry 记录/options/group，重挂载新实现）。
- **DIV-3-1 定案**：externals→全重载 = 依赖方经 **fiber uid 换代/epoch 自动重活**（自下而上证实：
  dispose 置 uid None、重载重新分配 runtime.rs:208-209/755 → 提供者 reload 后 epoch 变 → 依赖方
  自动 Load）——无需显式刷新依赖方。
- 观测访问器 `stale_entry_ids(name)`；group 入口（合成 GroupPlugin）不参与（B2 后续）。

**验证（设计关闸）**：`design.md` 定稿；实现 S1（TDD T1-T4 红→绿）+ 回归（verify-diff 21 零回归、
clippy 0）+ S3 部署冒烟。

**预期影响与回滚点**：本提交纯文档。回滚 = 撤本提交。

## D-130（服务装配单元 Phase 3 编码落定：B3 replace_plugin + reload 红→绿 + 核心模块缓存替换根因修复）

**日期**：2026-08-26

**触发问题**：D-129 设计关闸通过 → 阶段 3（TDD 编码）。实现 `replace_plugin`/`stale_entry_ids`/
`reload_entry`（loader 层）+ m18 T1-T4 红测。T1/T4 首轮红：reload 后 entry 仍 apply **旧实现 v1**。

**根因（自下而上实证 + 修复，触及 dsh-core）**：
- **现象**：`replace_plugin` 换代后 `reload_entry`→`start_entry`→`load_plugin` 取到**新** Arc
  （loader registry 已更新），但 `ctx.plugin_arc` 走 runtime `register_plugin` 后，`begin_load`
  （runtime.rs:672-680）从 **`self.registry[runtime_key].plugin`**（按名模块缓存）取插件——该缓存此前
  仅在 `or_insert_with` **首次**注册时写入（runtime.rs:555-563），同名 re-import 不更新 → reload 取到
  陈旧实现。同理暴露 `remove+create 后重载旧实现` 与 `dynamic_activate 新 entry 潜在取旧的潜伏缺陷`。
- **修复**：runtime `register_plugin` 处**始终** `record.plugin = plugin.clone()`（按名覆盖），对齐
  cordis `registry.plugin(name, cb)` 的**按名替换**语义（模块 re-import = 该名新实现）。既有 21
  golden / m-series 无「同 name 换实现」用例，覆盖更新零回归；m16 A1-c 只断言 loader 级身份，不受影响。

**关键实现**（均过 T1-T4 红→绿 + 清理临时 dbg 探针）：
- `Loader::replace_plugin`：同 Arc 幂等 `Ok(0)`；新 Arc → A1 换代 + `stale_entry_ids`（identity 非当前）
  → 逐个 `reload_entry`，返回受影响数。
- `stale_entry_ids`：`entry.options.name==name && identity.is_some() && identity != 当前身份` 集合。
- `reload_entry`：disabled no-op；否则 `dispose_entry + start_entry`（entry 保真，identity 重记为
  新身份）；依赖方经 uid/epoch 自动重活（DIV-3-1 兑现，T2 锁）。
- m18：T1 换实现重载 / T2 依赖方重活 / T3 同实现幂等 / T4 受影响计数 + stale 观测，4/4 绿。

**阶段 4 验证（编码关闸）**：`cargo test --workspace` EXIT=0（198 目标 0 失败，含 m18 4/4）；
`cargo clippy --workspace --all-targets -- -D warnings` EXIT=0；`node diff/ts-host/verify-diff.mjs`
21/21 PASS（golden 逐字节不变）。受影响的 crate：dsh-core（运行时模块缓存替换）+ dsh-loader（热更层）+ m18。

**预期影响与回滚点**：本提交 = 可运行代码 + 测试。回滚 = `git revert` 本提交（loader 层 + core
缓存替换 + m18 随特征级整体回滚；core 修复独立回滚会使 B3 退回「reload 取旧实现」）。

## D-131（服务装配单元 Phase 3 验收收口：阶段 4 关闸 + 阶段 5 部署冒烟 + acceptance 工件）

**日期**：2026-08-26

**触发问题**：D-130 编码关闸通过 → 阶段 4（测试验证）与阶段 5（部署与维护）验收收口。

**阶段 4 关闸**：`cargo test --workspace` EXIT=0（198 目标 0 失败，含 m18 4/4 红→绿）；
`cargo clippy --workspace --all-targets -- -D warnings` EXIT=0；`node diff/ts-host/verify-diff.mjs`
21/21 PASS（golden 逐字节零回归）；等价主证据 = m-series + 既有 21 场景零回归（DIV-3-2）。

**阶段 5 部署冒烟**：`dsh web target/web/cordis.yml --port 60883`（本轮含 dsh-core 运行时改动）
→ `GET /` HTTP 200（len 13270），进程干净停止——真实启动链路零回归。部署 = `replace_plugin`
公开 API（serve/dynamic runner 可选接线）；回滚 = `git revert a8793e3`。

**编码期发现的如实收口**：设计假设「reload 以当前注册新实现重挂载」在 TDD 红测下暴露 dsh-core
模块缓存缺位（`registry[name].plugin` 仅首注册写入、re-import 取旧）——按「越级」纪律先定位再修复
（runtime `register_plugin` 按名覆盖，对齐 cordis `registry.plugin(name,cb)` 替换语义），全量重验零回归。

**工件**：`.spec/service-assembly-p3/acceptance.md`（交付范围核对/阶段 4 证据/编码期发现/部署回滚/
诚实边界/决策链互查）。

**预期影响与回滚点**：Phase 3 五阶段全闭环。回滚 = `git revert a8793e3`（编码）+ 撤 D-131 工件提交；
文档提交可独立撤。后续缺口：A6/A5/A2/B1/B2(Group 折叠)/B4 + A3 动态 check spike。

## D-132（服务装配单元 Phase 4 需求分析定稿：A6 异步生成器 effect，[Service.init] 完整形态）

**日期**：2026-08-27

**触发问题**：用户新目标「推进 A6（最深核心缺口）」。验收五维「依赖激活」的最后一坑：cordis 长效插件
标准形态是 `async* [Service.init] { yield 清理项; await 启动体; … }`，Rust `EffectOutcome::Await` 只是
M27 等价子集，无「逐项 yield 跨 await 立即收集 + epoch 中途取消 + 失败前 disposer 保留」。

**自下而上核对（源码实证）**：
- cordis `_execute` async-iterator 分支（npm cordis 4.0.0-rc.8 `lib/index.js:798-840` 与
  `@deepseek-ai/cordis src/fiber.ts:356-400` **逐字一致**）：`await Promise.resolve()` → 每轮
  `if (runner.epoch !== oldEpoch) return`（中途取消）→ `await iter.next()` → `safeCollect(值)`；
  卸载 `splice(0).reverse()` 逆序。
- Rust `EffectOutcome`：`Many` 已覆盖同步生成器（整批）、`Await` 已覆盖 thenable/单 future；
  **唯一真缺口 = 异步生成器逐项形态**。
- Rust `Group`（loader.rs:304-344）已用 `Await + ctx.effect("group-stop")` 近似「先注册清理再挂子项」——不改（保持兼容）。
- 等价基础设施：dsh-diff DSL/TS host（scenario-host.mjs 目前只生成函数 apply）需小扩展以支持
  「apply 返回生成器」插件 + yield/await 步进（A6-GOLDEN=A）。

**关键决策（用户确认）**：A6-SCOPE=A（核心 effect 能力补齐 + 等价证明，新增 async 生成器 effect
形态 + sync now_or_never/async drive_async_loads 双驱动 + m-series；不改 Group）；A6-GOLDEN=A
（新增 1 个 async-generator golden，TS 原版 ↔ Rust 逐行一致，m-series 作锁定）。

**阶段结论**：需求关闭工件 `.spec/service-assembly-p4/requirements.md` 定稿（目标/非目标/假设/
缺口核对/验收 T1-T4/决策收敛）→ 进入阶段 2（系统设计）。
**预期影响与回滚点**：本提交纯文档。回滚 = 撤本提交；后续设计/编码各自独立可回。

## D-133（服务装配单元 Phase 4 系统设计定稿：A6 Stream effect + 双驱动 + golden 等价）

**日期**：2026-08-27

**触发问题**：D-132 需求关闸通过 → 阶段 2（系统设计）。把「异步生成器 effect（[Service.init] 完整
形态）」落成可验收设计。

**关键设计定案**：
- **S1 dsh-core**：`EffectOutcome::Stream(LocalBoxStream<'static, GenItem>)`（`GenItem = Result<Disposer,
  CordisError>`）+ `FiberData::push_gen_disposer`（逐项直插 `disposers` 注册序）。async 驱动
  （`drive_async_loads` Apply 分支）与 sync 驱动（`now_or_never(s.next())`）共用循环：逐项立即收集、
  卸载逆序、**epoch 中途取消**（判定键 = `fiber.epoch` 变化，忠实 cordis `runner.epoch`）、
  **失败前 disposer 保留**（`Some(Err)` → `fail_fiber`，已收集保留）。
- **S2 m-series**：`m19_async_gen.rs` T1 逐项收集+逆序卸载 / T2 生成器体内同步翻转自身 epoch 的中途
  取消（确定性、无运行时）/ T3 失败前 disposer 保留。
- **S3 golden**：场景 DSL 扩 `gen`（yield/await/throw 步进）+ scenario-host 生成 async-generator 插件
  + Rust dsh-diff 同 DSL 建流 → `scenario-12-async-generator`（**T1+T3 融合**：yield A / await m1 /
  yield B / await m2 / throw boom + unload → dispose:B,A 逆序）逐行等价。
- **DIV-4-1** Rust 用 LocalBoxStream（pull）表达、语义对 `_execute` async-iterator 分支逐行等价；
  **DIV-4-2** 真 pending 仅 async 模式推进（sync 与 Await 同限）；**DIV-4-3** T2 中途取消仅 m-series
  （单 await 步内不可外部中断）；**DIV-4-4** 不改 Group/Await（A6-SCOPE=A）。

**验证（设计关闸）**：`design.md` 定稿；实现 S1（TDD T1-T3 红→绿）+ S2/S3（m19 + golden）+ 回归
（verify-diff 22 全过含新 golden） + 阶段 4/5 关闸。

**预期影响与回滚点**：本提交纯文档。回滚 = 撤本提交。

## D-135（服务装配单元 Phase 4 编码落定：A6 Stream effect 红→绿 + DSL/golden 等价）

**日期**：2026-08-27

**触发问题**：D-133 设计关闸通过 → 阶段 3（TDD 编码）实现 `EffectOutcome::Stream` 双驱动 +
`GenItem`/`push_gen_disposer`（fiber.rs）+ `StreamDrive`/`drive_stream_sync`/`drive_stream_async`
（context.rs）+ m19 T1-T3 + DSL（`GenOp` yield/await/throw）与 TS host 生成器插件 + golden。

**关键实现（TDD 红→绿 + 自下而上实证修正）**：
- **形态**：`EffectOutcome::Stream(LocalBoxStream<'static, GenItem>)`（逐项产出 disposer）；驱动循环
  逐项 `push_gen_disposer`（立即收集/注册序）；**epoch 中途取消**（判定键 = `fiber.epoch` 变化，
  忠实 cordis `runner.epoch`）；`Err` 项 `fail_fiber`（失败前已收集保留）。sync `now_or_never` /
  async `drive_async_loads` 双插桩；`run_load` 中途取消分支按 cordis `_reload` 语义 `run_unload`
  （已收集逆序运行）。
- **红测实证修正**：T2 首版断言「flip 步产出不收集」→ 对照 cordis `_execute` 循环语义修正为
  「flip 步产出的 B **先**收集、此后循环顶 pre-check 停止后续（C 不收集）」；T3 经 loader fail-loud
  （`fiber_error` → `create` Err + 回滚 → 失败前 A 保留并在回滚卸载运行）——与既有 loader-02
  fail-loud 约定一致。
- **TS host 实证修正**：cordis `isConstructor` 对 `function` 声明走 `new` 分支并丢弃返回值 →
  生成器插件必须用**箭头函数**（非构造器）；`throw` 步使 `await ctx.plugin()` **reject**（scenario-host
  无法取回 Failed fiber 引用）→ **golden 只承载 T1 排序场景**（yield A/await m1/yield B/await m2/yield C
  → Active → unload），失败路径（T3）由 m-series 锁定（DIV-4-5）。
- **等价证据**：`scenario-12-async-generator` golden 14 行（TS 原版 cordis async generator ↔ Rust
  LocalBoxStream 逐行一致：`plugin:g` / `status` / `apply:g` / `effect-reg:A` / `gen-await:m1` /
  `effect-reg:B` / `gen-await:m2` / `effect-reg:C` / Active / Unloading / `dispose:C,B,A` / Disposed）。
  m19 T1-T3 3/3 绿（T1 逐项收集+逆序卸载 / T2 中途取消+保留 / T3 失败前保留+fail-loud）。

**阶段 4 验证（编码关闸）**：`cargo test --workspace` EXIT=0（199 目标 0 失败，含 m19 3/3）；`cargo
clippy --workspace --all-targets -- -D warnings` EXIT=0；`node diff/ts-host/verify-diff.mjs`
**22/22 PASS**（21 既有逐字节不变 + scenario-12 新 golden）。受影响 crate：dsh-core（Stream effect +
双驱动）+ dsh-loader（m19）+ dsh-diff（DSL/GenOp + futures-util 依赖）+ scenario-host（TS 生成器插件）。

**预期影响与回滚点**：本提交 = 可运行代码 + 测试 + golden。回滚 = `git revert` 本提交（核心 +
m19 + DSL/golden 特征级整体回滚）；DSL/golden 部分可独立回滚（`scenario-12-*` 删除或保留均可）。

## D-136（服务装配单元 Phase 4 验收收口：阶段 4 关闸 + 阶段 5 部署冒烟 + acceptance 工件）

**日期**：2026-08-27

**触发问题**：D-135 编码关闸通过 → 阶段 4（测试验证）与阶段 5（部署与维护）验收收口。

**阶段 4 关闸**：`cargo test --workspace` EXIT=0（199 目标 0 失败，含 m19 3/3 红→绿）；
`cargo clippy --workspace --all-targets -- -D warnings` EXIT=0；`node diff/ts-host/verify-diff.mjs`
**22/22 PASS**（21 既有 golden 逐字节零回归 + scenario-12 新 golden 14 行 TS 原版↔Rust 逐行一致）。

**阶段 5 部署冒烟**：`dsh web target/web/cordis.yml --port 60884`（本轮含 dsh-core 运行时改动）
→ `GET /` HTTP 200（len 13270），进程干净停止——真实启动链路零回归。回滚 = `git revert dd9cd1a`。

**编码期发现如实收口**：T2 语义（flip 步产出先收集、pre-check 后停止）、T3 载体（scenario-host
对 reject 的 Failed fiber 无法取回 → golden 只承载 T1、T3 由 m-series 锁定 DIV-4-5）、TS host
箭头函数修正、Rust run_load 中途取消补 run_unload——全部按「红测定位 → 对照 TS 语义 → 修复 →
全量重验」收口。

**工件**：`.spec/service-assembly-p4/acceptance.md`（交付核对/阶段 4 证据/编码期发现/部署回滚/
诚实边界/决策链互查）。

**预期影响与回滚点**：Phase 4（A6）五阶段全闭环，「依赖激活」验收维度的最深核心缺口闭合。
回滚 = `git revert dd9cd1a`（编码）+ 撤 D-136 工件提交。后续缺口：A5（intercept resolveConfig）/
A2（!!js 边界）/ B1（extend）/ B2（Group 折叠）/ B4（config simplify）+ A3 动态 check spike（按
HANDOFF 优先级续做）。

## D-137（服务装配单元 Phase 5 需求分析定稿：A5 对象形态 inject 拦截配置合并）

**日期**：2026-08-27

**触发问题**：用户新目标「逐一修复剩余 HANDOFF 缺口按优先级：A5 → A2 → B 类 + A3」。A5 为第一优先
（pi-ai 前置）。cordis 插件可写 `inject: { 'svc': cfg }` 对象形态，配置写入本 fiber 自身 intercept
层最内层（fiber.ts:700-705），服务 `[Service.resolveConfig]`（service.ts:86-102）以最高优先级合并之。

**自下而上核对（源码实证）**：
- Rust `ctx.intercept()`（context.rs:1606）+ `resolve_config`（context.rs:1636，父链 walk + per-layer
  值 + base/head + 浅合并）已存在且由 `07-intercept-merge` golden 对齐——运行期层叠合并不用重做。
- **唯一真缺口** = `Plugin::inject()`（registry.rs:18-21）只有名字数组，**无配置通道** ⇒ 对象形态
  inject 无法表达（pi-ai 前置）。
- `resolve_config` 沿 `fd.parent` 收集（对子代可见的机制已有），只需把注入配置并入本 fiber 自身层。

**关键决策（用户确认）**：A5-SCOPE=A（对象形态 inject 全流程：`Plugin::inject_configs()` 新可选方法
默认空 + 装载并入本 fiber 最内层 + `resolve_config` 最高优先级；不破坏既有实现）；A5-GOLDEN=A
（新增对象形态 inject golden，TS 原版↔Rust 逐行一致；m-series 作锁定）。`Config.merge` 深合并
**不做**（缺 pi-ai 深合并证据，DIV-5-x 后续可按需立项）。

**阶段结论**：需求关闭工件 `.spec/service-assembly-p5/requirements.md` 定稿（目标/非目标/假设/
缺口核对/验收 T1-T4/决策收敛/复盘追问结论）→ 进入阶段 2（系统设计）。
**预期影响与回滚点**：本提交纯文档。回滚 = 撤本提交；后续设计/编码各自独立可回。

## D-138（服务装配单元 Phase 5 系统设计定稿：A5 对象形态 inject + m20 + golden）

**日期**：2026-08-27

**触发问题**：D-137 需求关闸通过 → 阶段 2（系统设计）。把「对象形态 inject（inject: {svc:cfg} →
最内层）」落成可验收设计。

**关键设计定案**：
- **S1**：`Plugin::inject_configs() -> Vec<(String,Value)>`（新可选方法默认空，不破坏既有实现）；
  依赖名集 = `inject()` 名字 ∪ 配置键（cordis `Object.keys(inject)`=deps）；`register_plugin` 装载时
  `f.intercept.extend(pending_ic) + extend(own_cfgs)`（最内层；同名胜后者赢）。
- **S2**：m20 T1（子注入配置最内层 > 父 intercept）/ T2（base→注入层→head）/ T3（父注入配置沿父链
  对子代可见）；需 provider 提供注入键（键即依赖）。
- **S3 golden**：`scenario-13-object-inject-config`（父 provide+srv + intercept{a:1,p:1} + 子
  injectConfig{srv:{a:9,b:2}} + resolve-config → {"a":9,"b":2}）TS 原版↔Rust 逐行一致。
- **DIV-5-1** 浅合并（`Config.merge` 深合并不做，缺证据）；**DIV-5-2** 配置键即依赖（cordis 语义）；
  **DIV-5-3** `inject_configs()` 返回 Vec（元数据量小）。

**验证（设计关闸）**：`design.md` 定稿；实现 S1（TDD T1-T3 红→绿）+ S2/S3（m20 + scenario-13）+ 回归
（verify-diff 23 全过含新 golden）+ 阶段 4/5 关闸。

**预期影响与回滚点**：本提交纯文档。回滚 = 撤本提交。

## D-139（服务装配单元 Phase 5 编码落定：A5 对象形态 inject 红→绿 + golden）

**日期**：2026-08-27

**触发问题**：D-138 设计关闸通过 → 阶段 3（TDD 编码）实现对象形态 inject。

**关键实现（红→绿 + 自下而上实证修正）**：
- **S1**：`Plugin::inject_configs() -> Vec<(String,Value)>`（registry.rs 新可选方法默认空）；
  `runtime.register_plugin` 依赖名集 = `inject()` 名字 ∪ 配置键（cordis `Object.keys(inject)`=deps），
  本 fiber 自身 intercept 层 `extend(pending_ic) + extend(own_cfgs)`（最内层/最高优先级）。
- **S2**：`FnPlugin` 增 `inject_configs` 字段/构造器；m20 T1（子注入配置 > 父 intercept）/ T2（base→注入层
  →head 浅合并序）/ T3（父注入配置沿父链对子代可见）3/3 绿。红测修正：T1/T2 首版断言把 base 当
  「最高」——对照 cordis `Object.assign({}, base, …)` 修正为 base **最低**（被注入层覆盖 b:2）；
  父 intercept 的 `p:1` 保留（非覆盖）。
- **S3 DSL/golden**：`PluginDesc.inject_config`（`#[serde(rename="injectConfig")]`——JSON camelCase
  与 serde 字段名失配是首轮红因）；`ScenarioPlugin.inject()` 含配置键 + `inject_configs()`；
  scenario-host `plugin.inject = { ...injectConfig }`（对象）。`scenario-13-object-inject-config`
  golden 14 行 TS 原版↔Rust 逐行一致（child 注入配置最内层 → resolve-config {"a":9,"b":2,"p":1}）。
- **追证（preserve_order 取舍）**：曾试全局 serde_json `preserve_order`（插入序）——**破坏 9 个既有
  golden**（loader/include/session 宿主键序为**排序**，非插入序）。回退；改为仅对 scenario-host
  `resolve-config` trace 用 `stableStringify`（对象键递归字典序）规范化——JSON 键序无语义，两侧
  键序确定性一致；既有 22 golden 逐字节不变。DIV-5-4。

**阶段 4 验证（编码关闸）**：`cargo test --workspace` EXIT=0（200 目标 0 失败，含 m20 3/3）；`cargo
clippy --workspace --all-targets -- -D warnings` EXIT=0；`node diff/ts-host/verify-diff.mjs`
**23/23 PASS**（22 既有逐字节不变 + scenario-13）。

**预期影响与回滚点**：本提交 = 代码 + 测试 + golden。回滚 = `git revert` 本提交（core/loader/diff/
host/golden 特征级整体）；scenario-13 可独立删除。

## D-140（服务装配单元 Phase 5 验收收口：A5 阶段 4 关闸 + 阶段 5 部署冒烟 + acceptance 工件）

**日期**：2026-08-27

**触发问题**：D-139 编码关闸通过 → 阶段 4（测试验证）与阶段 5（部署与维护）验收收口。

**阶段 4 关闸**：`cargo test --workspace` EXIT=0（200 目标 0 失败，含 m20 3/3 红→绿）；`cargo clippy
--workspace --all-targets -- -D warnings` EXIT=0；`node diff/ts-host/verify-diff.mjs` **23/23 PASS**
（22 既有 golden 逐字节零回归 + scenario-13 14 行 TS 原版↔Rust 逐行一致）。

**阶段 5 部署冒烟**：`dsh web target/web/cordis.yml --port 60885`（本轮含 dsh-core register_plugin
改动）→ `GET /` HTTP 200（len 13270 与基线一致），进程干净停止。回滚 = `git revert 260f031`。

**关键取舍如实收口（DIV-5-4）**：全局 serde_json `preserve_order` 破坏 9 个既有 golden（loader/include/
session 宿主键序为排序）→ 回退；仅 scenario-host `resolve-config` trace 用 stableStringify 键序规范化
（JSON 键序无语义）。轮内决策日志完整记录，避免未来重蹈。m20 红测两处断言修正（base 最低优先级 /
父 intercept `p:1` 保留）纳入验收工件。

**工件**：`.spec/service-assembly-p5/acceptance.md`（交付核对/阶段 4 证据/编码期发现/部署回滚/
诚实边界/决策链互查）。

**预期影响与回滚点**：Phase 5（A5，pi-ai 前置）五阶段全闭环。回滚 = `git revert 260f031` + 撤 D-140
工件提交。后续按目标优先级：A2（!!js 求值范围）→ B 类（B1 extend / B2 Group 折叠 / B4 config
simplify）+ A3 动态 check spike。

## D-141（服务装配单元 Phase 6 需求分析定稿：A2 !!js 求值作用域绑定注入服务）

**日期**：2026-08-27

**触发问题**：A5 闭环后按目标优先级进入 A2（!!js 求值范围——HANDOFF §3 A2）。

**自下而上核对（源码实证，含 fork 权威读取）**：
- fork eval：`new Function('ctx','expr','with(ctx){ return eval(expr) }')` + `interpolate(ctx,value)`
  递归替换 `{__jsExpr}`（fork config/utils.ts:5-22）；ctx = 入口扩展 Context（注入服务混入属性 →
  裸标识符 + `ctx.svc` 均可读，lib/index.js:338/370）。
- Rust `eval_scope`（loader.rs:124-136）= `{config, process, ctx:{}, env:{}}`——**ctx 空**（唯一缺口）。
- `internal/config` waterfall（context.rs:742-748）早于 `current.push(fid)`（753）——绑定须经
  `args[0]=fid` 取目标纤维，不可用 current_fiber 时序。
- 服务值暴露通道 = `get_value`（Value 型；`Arc<dyn Any>` 非 JSON）。TS host 现无 `!!js` 支持。

**关键决策（用户确认）**：A2-SCOPE=B（仅 Rust 侧——ctx 绑定 + 成员/裸标识符 + m21 m-series/单测；
无 golden，证据退一档）；A2-BARE=A（ctx 成员 + **裸标识符**：服务名注入顶层作用域，与显式键
config/process/env/ctx 冲突时显式键优先）。

**阶段结论**：需求关闭工件 `.spec/service-assembly-p6/requirements.md` 定稿 → 进入阶段 2（系统设计）。
**预期影响与回滚点**：本提交纯文档。回滚 = 撤本提交。

## D-142（服务装配单元 Phase 6 系统设计定稿：A2 eval_scope 绑注入服务 + m21）

**日期**：2026-08-27

**触发问题**：D-141 需求关闸通过 → 阶段 2（系统设计）。

**关键设计定案**：
- **S1**：`eval_scope_with_services(config, process, services)`——services 对象 → `ctx`（成员访问）+
  服务名顶层裸标识符（显式键 config/process/env/ctx 优先）；空 services = 现状（m3 零回归）。
- **S2**：internal/config 监听器从 `args[0]=fid` 取目标纤维（waterfall 早于 current.push 的约束）→
  `{name → get_value(name)}` 遍历 `fiber(fid).inject`；disabled 表达式绑定当前纤维（best-effort）。
- **S3 m21**：T1 裸标识符读注入服务 / T2 ctx 成员 + 显式键优先 / T3 未注入服务 fail-loud 保留。
- **DIV-6-1** 仅 Value 型服务暴露；**DIV-6-2** get_value 按监听时刻可见性解析。

**验证（设计关闸）**：`design.md` 定稿；实现 S1（TDD T1-T3 红→绿）+ S2/S3（m21）+ 回归（workspace +
clippy）+ 阶段 4/5 关闸。

**预期影响与回滚点**：本提交纯文档。回滚 = 撤本提交。

## D-143（服务装配单元 Phase 6 编码落定：A2 eval_scope 绑注入服务红→绿）

**日期**：2026-08-27

**触发问题**：D-142 设计关闸通过 → 阶段 3（TDD 编码）把 `!!js` 求值作用域绑定注入就绪上下文。

**关键实现（红→绿）**：
- **S1**：`eval_scope_with_services(config, process, services)`——services 对象 → `ctx`（成员访问）
  + 服务名顶层**裸标识符**（显式键 config/process/env/ctx 优先，不做 `with` 语句）；`eval_scope`
  改 `#[cfg(test)]`（生产路径统一走带 services 的构造）；`eval_scope_with_process` 委托空 services
  （m3/既有单测零回归）。
- **S2**：`fiber_service_ctx(ctx, fid)`（`fiber(fid).inject` 名单 × `get_value`，仅 Value 服务）——
  `internal/config` 监听器从 **`args[0]=fid`**（waterfall 早于 `current.push(fid)` 的时序约束）绑定
  目标纤维；`entry_disabled` 增 `&Cordis` 参数（disabled 表达式绑当前纤维，best-effort）。
- **S3**：m21 T1（裸标识符读注入服务 `svc.k`）/ T2（`ctx.svc` 成员 + 服务名=config 不覆盖显式键）/
  T3（未注入服务 fail-loud 保留原 config + eval-error 写回标记）3/3 绿。

**阶段 4 验证（编码关闸）**：`cargo test --workspace` EXIT=0（201 目标 0 失败，含 m21 3/3）；`cargo
clippy --workspace --all-targets -- -D warnings` EXIT=0；`node diff/ts-host/verify-diff.mjs`
**23/23 PASS**（golden 零回归——listener 增 internal/get 读取与 entry_disabled 参数化对既有
scenario/loader 场景无 trace 影响）。

**预期影响与回滚点**：本提交 = 代码 + 测试。回滚 = `git revert` 本提交（S1+S2+S3 特征级整体）；
m21 可独立删除。A2-SCOPE=B（无 golden）——等价证据由 m21 + 单测锁定。

## D-144（服务装配单元 Phase 6 验收收口：A2 阶段 4 关闸 + 阶段 5 部署冒烟 + acceptance 工件）

**日期**：2026-08-27

**触发问题**：D-143 编码关闸通过 → 阶段 4（测试验证）与阶段 5（部署与维护）验收收口。

**阶段 4 关闸**：`cargo test --workspace` EXIT=0（201 目标 0 失败，含 m21 3/3）；`cargo clippy
--workspace --all-targets -- -D warnings` EXIT=0；`node diff/ts-host/verify-diff.mjs` **23/23 PASS**
（listener 增 internal/get 读取与 entry_disabled 参数化对既有 golden 逐字节零回归）。

**阶段 5 部署冒烟**：`dsh web target/web/cordis.yml --port 60886`（本轮含 dsh-loader internal/config
改动）→ `GET /` HTTP 200（len 13270 与基线一致），进程干净停止。回滚 = `git revert 1dd6476`。

**关键取舍如实收口**：`internal/config` waterfall 早于 `current.push(fid)`（时序约束）→ 经
`args[0]=fid` 取目标纤维；`get_value` 嵌套 waterfall 可重入（独立 WfChain）已核实；`eval_scope`
生产路径统一走 `eval_scope_with_services`（空 services 行为不变）；A2-SCOPE=B 等价证据退档 m21 +
单测（用户确认）。

**工件**：`.spec/service-assembly-p6/acceptance.md`（交付核对/阶段 4 证据/编码期发现/部署回滚/
诚实边界/决策链互查）。

**预期影响与回滚点**：Phase 6（A2）五阶段全闭环。回滚 = `git revert 1dd6476` + 撤 D-144 工件提交。
后续按目标优先级：B 类（B1 extend / B2 Group 折叠 / B4 config simplify）+ A3 动态 check spike。

## D-145（服务装配单元 Phase 7 需求分析定稿：B1 Service 派生作用域实例 + 可调用服务）

**日期**：2026-08-27

**触发问题**：A2 闭环后按目标优先级进入 B1（HANDOFF §3 B1：[Service.extend] 派生作用域实例 +
可调用服务，service.ts:65-73）。

**自下而上核对（源码实证，含 fork 权威读取）**：fork `createCallable`（utils.ts:226/logger.ts:208，
`Service[invoke]` → 可调用如 `ctx.logger()`）+ `Service[extend]`（service.ts:65-73：callable→rebuild；
else `Object.create(this)`+assign）；Rust `Service = name+check`（service.rs）且 `provide_service` 注册
`Arc<dyn Any>`、`get` 平 Arc——**无通用 invoke/extend 原语**；缺 Service 类型直达通道（Any→dyn
Service 无法下转型）。

**关键决策（用户确认）**：B1-SCOPE=A（可调用+派生全流程——`extend(self: Arc<Self>)` 默认恒等 +
`invoke` 默认 Err + 独立 `srv_store` 通道 + `ctx.get_extended/call_service`；logger 演示不改生产，
DIV-7-1）；B1-PROOF=A（m-series m22 T1-T4 + 单测，无 golden——TS host 无 Service 子类支持）。

**阶段结论**：需求关闭工件 `.spec/service-assembly-p7/requirements.md` 定稿 → 进入阶段 2（系统设计）。
**预期影响与回滚点**：本提交纯文档。回滚 = 撤本提交。

## D-146（服务装配单元 Phase 7 系统设计定稿：B1 extend/invoke 原语 + srv 通道 + m22）

**日期**：2026-08-27

**触发问题**：D-145 需求关闸通过 → 阶段 2（系统设计）。

**关键设计定案**：
- **S1**：`Service` trait 增 `extend(self: Arc<Self>, ctx)`（默认恒等，对象安全）+ `invoke(&self,
  ctx, args)`（默认 Err "not callable"）+ `as_any()`（派生字段读出通道）。
- **S2**：`Runtime.srv: HashMap<(ScopeId,String), Arc<dyn Service>>`（Service 类型直达，绕开
  Any→dyn Service 不可下转型）；`provide_service` 签名不变追加 srv 注册（同 `resolve_scope` 键）+
  组合 disposer（Rc::new d1+d2）；`srv_lookup` 按当前纤维 scope 链镜像 impl 解析；
  `ctx.get_extended`/`ctx.call_service`。
- **S3 m22**：T1 自定义派生（访问方纤维名标记）/ T2 默认恒等 ptr_eq / T3 invoke 加和 / T4 不可调用
  Err + m1_service/logger 回归（DIV-7-1）。
- **DIV-7-1** 生产 logger 不改；**DIV-7-2** 仅 `provide_service` 进 srv 通道；**DIV-7-3** extend 默认
  恒等（Rust 泛型克隆不可行）。

**验证（设计关闸）**：`design.md` 定稿；实现 S1（TDD T1-T4 红→绿）+ S2/S3（m22）+ 回归（workspace +
clippy）+ 阶段 4/5 关闸。

**预期影响与回滚点**：本提交纯文档。回滚 = 撤本提交。

## D-147（服务装配单元 Phase 7 编码落定：B1 extend/invoke 原语 + srv 通道红→绿）

**日期**：2026-08-27

**触发问题**：D-146 设计关闸通过 → 阶段 3（TDD 编码）实现 Service 派生/可调用原语。

**关键实现（红→绿 + 自下而上实证修正）**：
- **S1**：`Service` trait 增 `extend(&self, ctx) -> Option<Arc<dyn Service>>`（默认 `None`=恒等）+ `invoke`
  （默认 Err "not callable"）。**红测实证修正**：`self: Arc<Self>` receiver 的默认体 `{ self }` 对
  unsized `Self`（dyn 使用）无法编译（E0277）→ 改为 `&self` + `Option`（None=恒等，`get_extended`
  内保持原 Arc）——更简单且对象安全；`as_any` 因同 unsized 问题弃用（T1 改用**观察日志**读取派生
  标记，沿用 m-series 模式）。
- **S2**：`Runtime.srv: HashMap<(ScopeId,String), Arc<dyn Service>>`（独立服务通道，绕开
  Any→dyn Service 不可下转型）；`provide_service` 签名不变、追加 srv 注册（**与 `insert_impl` 同
  源作用域解析**：`resolve_scope(...).unwrap_or_else(scope_for)`——拆分 effect 执行晚于 insert_impl，
  不能依赖执行顺序预填 scopes，必须显式同源保证键对齐）+ 组合 disposer（Rc::new d1+d2）；
  `srv_lookup`（当前纤维 scope 链）/ `get_extended`（None→恒等）/ `call_service`。
- **S3**：m22 T1（自定义派生绑定访问方纤维，观察日志 "derived:child"）/ T2（默认恒等 Arch::ptr_eq）/
  T3（invoke 加和 → 3）/ T4（不可调用 Err）4/4 绿。Service 需 `Send + Sync` → 观察日志用
  `Arc<Mutex<Vec<String>>>`。

**阶段 4 验证（编码关闸）**：`cargo test --workspace` EXIT=0（202 目标 0 失败，含 m22 4/4）；`cargo
clippy --workspace --all-targets -- -D warnings` EXIT=0；`node diff/ts-host/verify-diff.mjs`
**23/23 PASS**（golden 零回归）。`provide_service` 既有唯一使用（m1_service）零改动。

**预期影响与回滚点**：本提交 = 代码 + 测试。回滚 = `git revert` 本提交（S1+S2+S3 特征级整体）；
m22 可独立删除。B1-PROOF=A（无 golden）；生产 logger 不改（DIV-7-1）。

## D-148（服务装配单元 Phase 7 验收收口：B1 阶段 4 关闸 + 阶段 5 部署冒烟 + acceptance 工件）

**日期**：2026-08-27

**触发问题**：D-147 编码关闸通过 → 阶段 4（测试验证）与阶段 5（部署与维护）验收收口。

**阶段 4 关闸**：`cargo test --workspace` EXIT=0（202 目标 0 失败，含 m22 4/4 + m1_service 既有
provide_service 零改动）；`cargo clippy --workspace --all-targets -- -D warnings` EXIT=0；
`node diff/ts-host/verify-diff.mjs` **23/23 PASS**（golden 零回归）。

**阶段 5 部署冒烟**：`dsh web target/web/cordis.yml --port 60887`（本轮含 dsh-core Service 改动）→
`GET /` HTTP 200（len 13270 与基线一致），进程干净停止。回滚 = `git revert 962986d`。

**关键取舍如实收口**：E0277（`self: Arc<Self>` 默认体对 unsized Self 不编译）→ `&self + Option`
（None=恒等）；作用域键与 `insert_impl` 同源解析（不依赖执行顺序）；`as_any` 弃用改观察日志（Send+Sync
用 Mutex）。B1-PROOF=A 证据退档 m22 + 单测（用户确认）。

**工件**：`.spec/service-assembly-p7/acceptance.md`（交付核对/阶段 4 证据/编码期发现/部署回滚/
诚实边界/决策链互查）。

**预期影响与回滚点**：Phase 7（B1）五阶段全闭环。回滚 = `git revert 962986d` + 撤 D-148 工件提交。
后续按目标优先级：B2 Group 折叠 / B4 config simplify + A3 动态 check spike。

## D-149（服务装配单元 Phase 8 需求+设计定稿：B2 Group 子入口失败 fail-loud）

**日期**：2026-08-27

**触发问题**：B1 闭环后进入 B2（HANDOFF §3 B2「Group 折叠差异」）。

**自下而上核对（源码实证）**：
- HANDOFF「Rust 无独立 Group fiber」**已过时**——M22 起 `GroupPlugin`（loader.rs:341）为真实 fiber；
  三约定中**事件顺序/init await**（loader-10 golden，async 驱动）与**group 同 realm**（m3_isolate
  group_realm_walk + loader-15 golden）均已对齐。
- **真缺口（红证 m23 首版）**：fork `_start`（lib/index.js:522-533）`await fiber.await()` —— 子失败
  reject → group update 抛 → **loader 装载失败**；Rust `load_group_plugin` 只查 Group 自身 fiber_error，
  组内子失败被吞（`.ok()` / Await 恒 None）→ group 保持 Active（m23 红：`Some(Active)` vs `Failed`）。

**需求收敛（阶段 1）**：B2 = 三约定核对（已有证据收口）+ 子失败缺口按 **fail-loud + 回滚** 修复（cordis
装载事务语义）；子失败场景无 golden（TS loader-sync reject）→ **m-series 证据**（延续 A2/B1 的 B 类
非核心先例）。非目标：不动 dsh-core fiber 失败层、不动正常分组路径。

**设计收敛（阶段 2）**：`group_child_error(gid)`（`entries[g].subgroup → groups[sg].data` 逐子
`fiber_error`）→ `load_group_plugin`(sync) + `load_group_plugin_async` 在自身 fiber 无错后前置检查 →
Err → `create/update` 既有回滚（`dispose_entry(g)` 级联停止子入口）清理。检测点在子入口完全 settle 后
（H2）。DIV-8-1（无 golden）/ DIV-8-2（组中组传递覆盖）/ DIV-8-3（返回首个失败子错误）。

**阶段结论**：需求/设计工件 `.spec/service-assembly-p8/{requirements,design}.md` 定稿 → 编码（m23
红→绿）。
**预期影响与回滚点**：本提交纯文档。回滚 = 撤本提交；编码各自独立可回。

## D-150（服务装配单元 Phase 8 编码落定：B2 group_child_error + m23 红→绿）

**日期**：2026-08-27

**触发问题**：D-149 需求+设计关闸通过 → 阶段 3（TDD 编码）。

**关键实现（红→绿）**：
- **S1**：`group_child_error(gid)`（dsh-loader）——`entries[g].subgroup → groups[sg].data` 逐子
  `ctx.fiber_error`（首个失败子返回其错误）；`load_group_plugin`(sync) 与 `load_group_plugin_async`
  在 Group 自身 fiber_error 检查之后前置调用 → Err → `create`/`update` 既有回滚
  （`dispose_entry(g)` 级联停止子入口）完成清理。
- **S2**：m23 首版红（修复前 `create(g)` 返回 Ok、group Active——实锤吞错）→ 修订断言为
  **fail-loud + 回滚**契约（`unwrap_err` 含 "boom"；`applied>=1`（c1 先加载）；`fiber("c1"/"c2"/"g")`
  均 None）→ 绿。修订理由：cordis 是**装载事务失败**（loader-sync reject + 回滚），非「Group 纤维留存
  Failed」——按 m20 T3/loader-02 fail-loud 先例。

**阶段 4 验证（编码关闸）**：`cargo test -p dsh-loader` EXIT=0（含 m23）；`cargo test --workspace`
EXIT=0（203 目标 0 失败）；`cargo clippy --workspace --all-targets -- -D warnings` EXIT=0；
`node diff/ts-host/verify-diff.mjs` **23/23 PASS**（loader-10 等正常分组路径零回归——healthy 子入口
无 fiber_error，新检查不触发）。

**预期影响与回滚点**：本提交 = 代码 + 测试。回滚 = `git revert` 本提交（两组检查 + m23）;DIV-8-1
（无 golden，m-series 证据）。B4/A3 后续。

## D-151（服务装配单元 Phase 8 验收收口：B2 阶段 4 关闸 + 阶段 5 部署冒烟 + acceptance 工件）

**日期**：2026-08-27

**触发问题**：D-150 编码关闸通过 → 阶段 4（测试验证）与阶段 5（部署与维护）验收收口。

**阶段 4 关闸**：`cargo test -p dsh-loader` EXIT=0（含 m23）；`cargo test --workspace` EXIT=0
（203 目标 0 失败）；`cargo clippy --workspace --all-targets -- -D warnings` EXIT=0；
`node diff/ts-host/verify-diff.mjs` **23/23 PASS**（loader-10/15 正常分组路径零回归）。

**阶段 5 部署冒烟**：`dsh web target/web/cordis.yml --port 60888`（本轮含 dsh-loader 组装载检查）→
`GET /` HTTP 200（len 13270 与基线一致），进程干净停止。回滚 = `git revert 746e982`。

**关键取舍如实收口**：HANDOFF B2 条目已过时（真实 fiber 早已存在——M22）；三约定既有证据锁定；剩余
真缺口 = 子失败吞错（m23 红证）→ 按 cordis **装载事务**语义 fail-loud + 回滚（修订 m23 断言：非
「Group 留存 Failed」）；修复在 loader 事务层，dsh-core 零改动（DIV-8-1..3）。

**工件**：`.spec/service-assembly-p8/acceptance.md`（交付核对/阶段 4 证据/编码期发现/部署回滚/
诚实边界/决策链互查）。

**预期影响与回滚点**：Phase 8（B2）验收完成。回滚 = `git revert 746e982` + 撤 D-151 工件提交。
后续按目标优先级：B4 config simplify + A3 动态 check spike。

## D-152（服务装配单元 Phase 9 需求+设计定稿：B4 config simplify 回写 unparse）

**日期**：2026-08-27

**触发问题**：B2 闭环 → 进入 B4（HANDOFF §3 B4「config simplify 回写 unparse」）。

**自下而上核对（源码实证）**：fork `internal/update`（@deepseek-ai/cordis-plugin-loader index.ts:103-109）
`entry.options.config = Config['simplify'](config)` 后 tree.write；`Config.simplify` =
schemastery `Schema.prototype.simplify`（@deepseek-ai/schemastery src/index.ts:407-442）：默认相等→null、
object 删 null/undefined 键、dict 保、array/tuple 映射、intersect 合并、union try resolve、else 原值。
Rust `write_back`（loader.rs:263-285）存**原始配置**——缺 simplify。

**需求收敛（阶段 1）**：dsh-schema 移植 `simplify` + loader write_back 接入（插件声明 config_schema
时简化写回，无 schema 原样——in-memory=落盘一致，cordis 同字段）。非目标：merge 深合并（validate_config
下次装载补默认，DIV-9-3）、golden（TS loader-host 无 Config schema 场景；B 类 m-series 先例）。

**设计收敛（阶段 2）**：S1 `simplify` 逐分支措辞对齐 schemastery；S2 write_back 取
`entries[id]→options.name→plugins[name].plugin.config_schema()` 后简化（注意借用释放）；S3 m24 T1
（schema+默认删键，无简化则红）/T2（无 schema 原样）/T3（嵌套 object/dict/array）。DIV-9-1（deepEqual
dict 特判降级 serde_json 深等）/DIV-9-2（在写回而非序列化前简化——避免内存=落盘分离）/DIV-9-3。

**阶段结论**：需求/设计工件 `.spec/service-assembly-p9/{requirements,design}.md` 定稿 → 编码（S1→S2→S3）。
**预期影响与回滚点**：本提交纯文档。回滚 = 撤本提交；编码各自独立可回。

## D-153（服务装配单元 Phase 9 编码落定：B4 dsh-schema simplify + m24 红→绿）

**日期**：2026-08-27

**触发问题**：D-152 需求+设计关闸通过 → 阶段 3（TDD 编码）。

**关键实现（红→绿）**：
- **S1（dsh-schema）**：`pub fn simplify(schema, value)`——schemastery `Schema.prototype.simplify`
  逐字：默认深等→Null、null 透传、object 逐键（未声明键**删**、子简化 Null 删；全删→`{}`==默认`{}`
  →**Null**；schemastery deepEqual(result, default) 收尾）、dict 保 null、array/tuple 逐项、intersect
  合并、union try `resolve`、else 原值。（红期实证：`Schema::object()` 默认即 `{}`——全默认 object 正确
  塌缩为 Null，初版断言 {a:{}} 是错误预期，按 schemastery 修订。）
- **S2（loader write_back）**：入口插件 `config_schema` 存在 → `simplify` 后存回。红期实证 cordis
  语义：`internal/update` 仅运行时 `update_with(fid,cfg,false)` 触发（`_patchContext` 的
  `fiber.update(cfg,true)` 带 noSave=true→跳过；create 不简化）→ 修正 T1 为运行时更新断言。
- **S3（m24）**：T1 运行时更新写回简化（`{def:5,other:2}`→`{other:2}`）/ T2 无 schema 原样 /
  T3 十分支单测——3/3 绿。另：dsh-loader 增 `dsh-schema` 依赖（此前未直接引用）。

**阶段 4 验证（编码关闸）**：`cargo test -p dsh-loader`（含 m24）；`cargo test --workspace` EXIT=0
（**204 目标 0 失败**）；verify-diff **23/23**（无 schema 插件路径零回归）。

**预期影响与回滚点**：回滚 = `git revert` 本提交。待阶段 5（clippy + serve 冒烟 + acceptance + D-154）。

## D-154（服务装配单元 Phase 9 验收收口：B4 阶段 4 关闸 + 阶段 5 部署冒烟 + acceptance 工件）

**日期**：2026-08-27

**触发问题**：D-153 编码关闸通过 → 阶段 4/5 验收收口。

**阶段 4 关闸**：`cargo test --workspace` EXIT=0（204 目标 0 失败）；`cargo clippy --workspace
--all-targets -- -D warnings` EXIT=0（红期 m24 unused import 修复后并入 D-153）；verify-diff **23/23**
零回归（no-schema 插件路径 + goldens）。

**阶段 5 部署冒烟**：`dsh web target/web/cordis.yml --port 60889` → `GET /` HTTP 200（len 13270 基线
一致），进程干净停止。回滚 = `git revert e65546a`。

**关键取舍如实收口**：① 简化触点 = 运行时 `update_with(false)`（红期实证 cordis `_patchContext`
noSave=true 跳过、create 不简化）——修正 T1 为运行时更新断言；② object 全默认 → Null（`{}`==`{}`
默认，schemastery deepEqual(result,default) 收尾）——初版 `{a:{}}` 预期错误，按 schemastery 修订；
③ 未声明 object 键删（`schema?.simplify` undefined → 删）。DIV-9-1/9-2/9-4。

**工件**：`.spec/service-assembly-p9/acceptance.md`（交付核对/阶段 4 证据/编码期发现/部署回滚/
诚实边界/决策链互查）。

**预期影响与回滚点**：Phase 9（B4）验收完成。回滚 = `git revert e65546a` + 撤 D-154 工件提交。
后续按目标优先级：**A3 动态 check spike**（最后一个 HANDOFF 缺口）。

## D-155（服务装配单元 Phase 10 需求+设计定稿：A3 动态 check spike）

**日期**：2026-08-27

**触发问题**：B4 闭环 → 进入 A3（HANDOFF §A3「提供者可用性谓词 check + strict-active」）——目标原文
「动态 check spike」。

**自下而上核对（源码实证）**：
- **HANDOFF 直接问题已否**：Rust `provide` **有** check 谓词——`provide_with(name, value,
  Some(CheckFn))`（context.rs:1406）+ `check_ok()` + `check_impls`（runtime.rs:630）+ 静态门已被
  `m7_await::await_gated_by_check_predicate` 与 `scenario-10-provide-check-gate.golden` 锁定。
- **cordis 动态再求值触发点**：provide-while-Active / unprovide / 提供者 ACTIVE↔NON-ACTIVE 翻转 →
  `notify` → 依赖方 `_checkImpl`（重求值 check，不成立删 store→epoch INACTIVE）→ `_refresh`。
  Rust 由 produce-disposer 驱动的卸载路径（unload → remove_impl+notify → re-provide → finish_load
  notify）+ `check_impls`/`refresh_fiber` 覆盖同语义。纯谓词翻转（无 notify 触发点）在 cordis
  **非反应式**——Rust 须同位。

**需求收敛（阶段 1）**：spike 验证（非修复）——m25 断言 5 序列（静态门 / 纯翻转非反应式 /
重载+true→激活 / 重载+false→失效 / 往返），pass = 全绿；红则回需求重评估。

**设计收敛（阶段 2）**：m25 用 `Arc<AtomicBool>` 谓词 + `update_with` 驱动 provider 重载观察 consumer
状态（`fiber_state`）。DIV-10-1（动态不可 golden，m-series 锁定）/DIV-10-2（不加剧自动 notify，cordis
非反应式同位）/DIV-10-3（A3 闭环 = 存在性 + parity，**零生产改动**）。

**阶段结论**：需求/设计工件 `.spec/service-assembly-p10/{requirements,design}.md` 定稿 → spike
（m25 红→绿判定）。
**预期影响与回滚点**：本提交纯文档。回滚 = 撤本提交；spike 测试各自独立可回。

## D-156（服务装配单元 Phase 10 spike 落定：A3 动态 check parity 锁定 + m25 全绿）

**日期**：2026-08-27

**触发问题**：D-155 关闸通过 → 阶段 3（spike 验证测试）。

**spike 判定（m25，1 测试 / 5 断言，红→绿）**：
1. 静态门：check=false → provider Active、consumer Pending（既有 m7/golden 复锁）。
2. **纯谓词翻转（无 notify 触发点）→ consumer 保持 Pending**——cordis 非反应式同位（DIV-10-2）。
3. `update_with(provider,false)`（卸载→re-provide→finish_load notify）+ check=true → consumer 激活。
4. check=false + 重载 → consumer 回 Pending。
5. check=true + 重载 → 再激活（往返）。
**结论**：Rust 动态 check 再求值触发点与 cordis **全面 parity**——**零生产代码改动**（机制本就存在：
provide/unprovide disposer + finish_load notify + check_impls/refresh_fiber；纯翻转非反应系 cordis
语义而非缺口）。A3 = 直接问题（谓词存在，既有证据）+ 动态触发点 parity（m25）。

**阶段 4 回归**：`cargo test --workspace` EXIT=0（**205 目标 0 失败**，m24 204 + m25）；clippy -D
warnings 0；verify-diff **23/23**。

**预期影响与回滚点**：回滚 = `git revert` 本提交。待阶段 5（serve 冒烟 + acceptance + D-157）。

## D-157（服务装配单元 Phase 10 验收收口：A3 spike 阶段 4 关闸 + 阶段 5 部署冒烟 + acceptance）

**日期**：2026-08-27

**触发问题**：D-156 spike 落定 → 阶段 4/5 验收收口。

**阶段 4 关闸**：workspace EXIT=0（205 目标）；clippy 0（红期 doc 列表缩进 lint 修复）；verify-diff
23/23 零回归。

**阶段 5 部署冒烟**：`dsh web target/web/cordis.yml --port 60890` → `GET /` HTTP 200（len 13270 基线
一致），进程干净停止。零生产路径变化（纯测试锁定）。

**关键取舍如实收口**：A3 = 谓词存在性（m7/scenario-10，既有）+ 动态触发点 parity（m25 5 断言）——
**零生产代码改动**；纯谓词翻转非反应式系 cordis 语义（DIV-10-2），不引入越界广播；动态翻转不可
golden（DIV-10-1），m-series 锁定。

**工件**：`.spec/service-assembly-p10/acceptance.md`（交付核对/阶段 4 证据/spike 结论/部署回滚/
诚实边界/决策链互查）。

**预期影响与回滚点**：Phase 10（A3）验收完成——**目标全部缺口（A5/A2/B1/B2/B4/A3）闭环**。回滚 =
各阶段 git revert 特征级。目标可标 complete。

## D-158（beyond 目标 Phase A1 需求+设计定稿：插件身份键模型收口——remove_plugin + case-4 + 文档化偏差）

**日期**：2026-08-27

**触发问题**：用户指示按流程规划 beyond 目标 → 需求分析（方法论二）落 A1。

**自下而上核对（源码实证）**：
- **A1 已形核**：身份 = `Arc<()>` token（identity.rs:14-41，Arc::ptr_eq）+ `generation`；`register_plugin`
  同 Arc 幂等/新 Arc 换代（loader.rs:467-483）；`replace_plugin`（B3/HMR）换代 → stale entry reload
  （loader.rs:492-512）；m16（T1-T5）+ m18（T1-T4）已绿。
- **真实剩余 = 三件**：① 无 `remove_plugin`（cordis `registry.delete`，registry.ts:258-267）→ case-4
  「模块消失→self-dispose 合法」路径不可达（无 API、无测试；seven_case case-4 loader.rs:220-227
  存在但从未触发）；② case-4 无测试；③ A1 偏差未显式文档化。

**需求收敛（阶段 1，用户确认 3 问）**：Q1=A 文档化偏差收口（注册表键结构不动，同名多实现=宿主层责任——
HANDOFF「或显式声明为文档化偏差」分支）；Q2=case-4 用 m-series（DSL 无 delete-plugin）；Q3=remove_plugin
语义 = **先删 core registry + st.plugins 记录，再 dispose 该名所有存活 fiber**（顺序不变量：先删后
unload，否则 case-4 误落 case-7 disabled）。

**设计收敛（阶段 2）**：S1 `Loader::remove_plugin(name)->Result<usize>`（rt.registry.remove 取 fibers +
st.plugins.remove + 逐 fid `ctx.unload`）；S2 m26 T1（remove 后 entry 不自禁用、无 disable 写回）/
T2（对照：插件仍注册时 self-dispose → disabled，case-7 复锁）/T3（ghost → Ok(0)）；S3 identity.rs +
DECISIONS 补文档化偏差（DIV-A1-1 一名一实现顺序换代/同名多实现宿主层；DIV-A1-2 m-series；DIV-A1-3
不写持久化，cordis delete 不动 entry.options）。

**阶段结论**：需求/设计工件 `.spec/service-assembly-a1/{requirements,design}.md` 定稿 → 编码
（m26 红→remove_plugin 绿）。
**预期影响与回滚点**：本提交纯文档。回滚 = 撤本提交；编码各自独立可回。

## D-159（beyond 目标 Phase A1 编码落定：remove_plugin + m26 红→绿 + 文档化偏差）

**日期**：2026-08-27

**触发问题**：D-158 关闸通过 → 阶段 3（TDD 编码）。

**关键实现（红→绿）**：
- **S1**：`Loader::remove_plugin(name) -> Result<usize, CordisError>`——先 `rt.registry.remove(name)`
  取该名存活 fibers + `st.plugins.remove(name)`，**后**逐 fid `ctx.unload`（顺序不变量：先删记录后
  unload，fiber dispose 的 `internal/plugin(dispose)` 经 seven_case case-4 `registry.contains_key`
  =假 → 合法 Continue，entry 不自禁用、无 `disable:` 写回）。未注册名 → Ok(0) 幂等。
- **S2（m26，3 测 / 红→绿）**：T1 `remove_plugin("p")` → 返回 1（一存活 fiber）、entry 不 disabled、
  无 `disable:a` 写回、`plugin_identity("p")` None；T2（对照）插件仍注册时 `ctx.unload` →
  case-7 entry disabled + `disable:a` 写回（既有语义复锁）；T3 ghost → Ok(0) 无副作用。
  红 = `no method named remove_plugin`；绿 = 3/3。
- **S3**：identity.rs 模块 doc 补 A1 文档化偏差声明（DIV-A1-1：平名单记录 + 宿主层多实现消解；
  case-4 经 remove_plugin 触发，m26 锁定）。

**阶段 4 验证**：`cargo test --workspace` EXIT=0（**206 目标 0 失败**，205 + m26）；clippy -D warnings
0；verify-diff **23/23**（replace/HMR/m16/m18 零回归）。

**预期影响与回滚点**：回滚 = `git revert` 本提交。待阶段 5（serve 冒烟 + acceptance + D-160）。

## D-160（beyond 目标 Phase A1 验收收口：阶段 4 关闸 + 阶段 5 部署冒烟 + acceptance）

**日期**：2026-08-27

**触发问题**：D-159 编码关闸通过 → 阶段 4/5 验收收口。

**阶段 4 关闸**：workspace EXIT=0（206 目标）；clippy 0；verify-diff 23/23 零回归（replace/HMR
路径不动）。

**阶段 5 部署冒烟**：`dsh web target/web/cordis.yml --port 60891` → `GET /` HTTP 200（len 13270 基线
一致），进程干净停止。回滚 = `git revert e169ef3`。

**关键取舍如实收口**：A1 已形核（身份 token + generation + replace_plugin/B3，非从零）；收口三件 =
remove_plugin / case-4 可触发验证 / 文档化偏差。顺序不变量实证（先删记录后 unload，否则 case-4 误落
case-7）。对照 T2 复锁站部语义。DIV-A1-1..3（多实现宿主层 / m-series 证据 / 不写持久化）。

**工件**：`.spec/service-assembly-a1/acceptance.md`（交付核对/阶段 4 证据/编码期发现/部署回滚/
诚实边界/决策链互查）。

**预期影响与回滚点**：beyond 目标 A1 验收完成。回滚 = `git revert e169ef3` + 撤 D-158/160 工件提交。
下一步候选：A4（注入快照/unprovide 顺序/父链 walk）/ A6 已闭环 / B4 已闭环——由用户指派优先级。

## D-161（beyond 目标 Phase A4 需求+设计定稿：注入快照 / unprovide 唤醒顺序 / 父链 walk 收口）

**日期**：2026-08-27

**触发问题**：目标「分别完成A4和更完整 HMR」→ A4 需求分析（方法论二）。

**自下而上核对（源码实证）**：① unprovide 顺序——cordis provide disposer = 删 impl → notify
（await 依赖方）→ **最后**删自身 fiber.store（reflect.ts:297-303）；Rust provide_with disposer =
remove_impl + notify + transitions（context.rs:1436-1445）。「stale 自访问窗口」仅存于 provide disposer
异步体内、后续 disposer 串行运行在其后 → observable 契约 = notify 先于后续 disposer + 端态
ctx.get→None。② 父链 walk——Rust resolve_scope 沿父链查 isolate（runtime.rs:307-321），loader-15
已锁跨 realm。③ reload 快照——cordis fiber.ts:647 `store={..._store}`。

**用户确认 Q1=B：m27 + TS golden**。收敛：A4 = 扩 loader-host/dsh-diff DSL 新 op `dispose-check`
（op 位置注册 disposer、卸载 trace `dispose-check:svc:{JSON(get)}`，双侧同构，收集逆序 fiber.rs:132-164）
→ 3 新 golden（G1 unprovide 唤醒排序 / G2 walk 3 层边界 / G3 reload 快照）+ m27 三断言；仅实测偏差
才对齐（DIV-A4-1 sta 窗口不可达，不做 per-fiber store 回退）。

**阶段结论**：需求/设计工件 `.spec/service-assembly-a4/{requirements,design}.md` 定稿 → 编码
（DSL→goldens→m27）。
**预期影响与回滚点**：本提交纯文档。回滚 = 撤本提交。

## D-162（beyond 目标 Phase HMR 需求+设计定稿：宿主侧插件模块热更 + 删除后 entry 处置）

**日期**：2026-08-27

**触发问题**：目标「更完整 HMR」——replace/remove_plugin 仅测试调用（无生产调用者）。

**需求收敛（用户确认 Q2=A）**：host 插件模块 HMR 接入 + entry 处置策略 + 集成测试。删除后策略选
**保留但 inert**（cordis delete 同径：不动 entry.options、不自禁用；可再注册 revive 或显式 remove）。
非目标：插件文件 watcher / specifier→Arc 解析（harness 层 FIXME）。

**设计收敛（阶段 2）**：`Loader::sync_plugin(name, PluginEvent{Register|Replace|Delete}) ->
PluginSyncOutcome{reloaded, disposed, retained}`（薄封装委托 register_plugin / replace_plugin /
remove_plugin）；e2e 测试 m27_hmr_host 序列（Register/Replace/Delete/Revive）；文档化契约 +
DIV-HMR-1..3（retained 策略 / 无文件 watch / 薄封装）。

**阶段结论**：需求/设计工件 `.spec/plugin-hmr/{requirements,design}.md` 定稿 → 编码。
**预期影响与回滚点**：本提交纯文档。回滚 = 撤本提交。

## D-163（A4 编码实现：DSL dispose-check + golden 22/24 + 组 isolate 修复 + m27 三断言）

**日期**：2026-08-27

**触发问题**：A4 设计（D-161）实施；TDD 红→绿。

**关键实现与发现**：
- `dispose-check` op 双侧（loader-host.mjs / dsh-diff ApplyOp::DisposeCheck）：**非 strict** 读取
  （TS `ctx.reflect.get(svc,false)` ↔ Rust `ctx.get`）判别「disposer 运行在 provide-remove 前/后」；
  经 `ctx.effect`/`fiber.effect` 在 op 位置注册、卸载逆序运行。
- **loader-22 unprovide-self-access**（11 行）与 **loader-24 reload-store-snapshot**（17 行）golden
  字节一致（25/25 PASS）。loader-24 修正两处：update options 须带 id/name（dsh-diff 归一化为全量
  的既有约定，loader-12 亦然）；provider 单独 reload 以剔除依赖方完成与自清的交错。
- **发现并修复真实偏差**：Rust loader 从未应用 group 入口的 `isolate`（load_group_plugin 不设
  pending_isolate）——m3_isolate / loader-15 的兄弟节点无自身 isolate 致两路皆 Active、无法分辨；
  新 golden 探针（嵌套 gIso 边界 b1）首次暴露。修复：新增 `entry_isolate_map` helper，
  sync+async load_group_plugin 应用组 isolate（DIV-A4-4 记录）。m27 T2 锁定。
- **异步交错边界**（DIV-A4-5）：cordis `_unload` 走 `Promise.all`**并发** disposers（每 disposer 先
  `await Promise.resolve()`），Rust 顺序逆序（依赖方完整 settle 后才自清）。语义上 Rust ⊇ cordis
  （notify 先于自清、端态一致均成立），但 `consumer Unloading:Pending` 与 `dispose-check` 的相对
  行序不可字节对齐。**决策**：不改全局 disposer 调度（危及 20+ goldens）；golden 收窄剔除该窗口；
  唤醒顺序改由 m27 T1（确定性：consumer-unloading 先于 provider-later）承担。
- **m27_a4**（3 断言全绿）：T1 唤醒顺序+自访问 absent；T2 组 isolate 边界+3 层 walk；T3 就地
  reload 身份不变+新配置可见+依赖方重解析。

**验证**：cargo test --workspace 209 目标 0 失败；clippy 0；verify-diff 25/25。
**阶段结论**：A4 编码完成 → 验收（阶段 4→5）。
**预期影响与回滚点**：`entry_isolate_map` 使 group 入口 isolate 真正生效（行为修正；既有
goldens 不受影响——loader-15 路径两路等价已验证）；回滚 = revert 本提交（连同 loader.rs 修复）。

## D-164（A4 验收：serve 冒烟 + acceptance 工件；HMR 进入编码）

**日期**：2026-08-27

**验收证据**：`cargo test --workspace` 209 目标 0 失败；clippy 0；verify-diff 25/25
（含 loader-22/24 新 golden）；serve 冒烟 `GET /` **HTTP 200 len 13270**（与基线一致，
经 dsh-cli 重建；cwd 必须为仓库根，Start-Job 默认 cwd 会使 wasm 相对路径解析失败——环境说明）。
工件 `.spec/service-assembly-a4/acceptance.md`。

**阶段结论**：A4 闭环（D-158/D-161/D-163/D-164）。下一步：HMR 编码（D-162 设计已闸）。
**预期影响与回滚点**：本提交含 acceptance 工件 + 决策日志。回滚 = 撤本提交。

## D-165（HMR 编码实现：sync_plugin 事件入口 + 删除后保留但 inert + e2e）

**日期**：2026-08-27

**触发问题**：D-162 设计实施（TDD 红→绿）。

**关键实现**：
- `Loader::sync_plugin(name, PluginEvent) -> PluginSyncOutcome`（模块级 `PluginEvent` 三变体 +
  `PluginSyncOutcome{reloaded, disposed, retained}`，lib.rs 导出）：Register/Replace 委托
  `replace_plugin`（同 Arc 幂等→reloaded=[]；新 Arc→换代+reload，reloaded=受影响集 =
  该名、e.identity 已设的 entry——含 inert 待复活者）；Delete 委托 `remove_plugin`
  （retained=该名全部 entry、disposed=fiber 数）。
- **e2e m27_hmr_host**（TDD 红→绿，红线修正两处）：Delete 后 `fiber()` 仍指 Disposed fiber
  （不变量：Disposed 状态而非字段清空）；Delete 后 re-register = **全新 lineage**（记录被清、
  generation 重置 1、新身份 token）——诚实语义非 Bug。e2e 锁定：Register 幂等 / Replace
  reload+换代 / Delete 保留但 inert（不 disabled、无 disable 写）/ Revive（新 fiber+新实现）。
- identity.rs 文档补充：HMR 契约 + Delete+re-register 全新 lineage（DIV-HMR-2 关联）。

**验证**：cargo test --workspace 0 失败；clippy 0；verify-diff 25/25。
**阶段结论**：HMR 编码完成 → 验收（阶段 4→5）。
**预期影响与回滚点**：纯增量 API（A1/B3 零改动）；回滚 = revert 本提交。

## D-166（HMR 验收：serve 冒烟 + acceptance 工件；目标"分别完成A4和更完整HMR" 双闭环）

**日期**：2026-08-27

**验收证据**：`cargo test --workspace` 0 失败；clippy 0；verify-diff 25/25；
serve 冒烟 `GET /` **HTTP 200 len 13270**（基线一致）。工件 `.spec/plugin-hmr/acceptance.md`。

**阶段结论**：HMR 闭环（D-162/D-165/D-166）。当前目标「分别完成A4和更完整HMR」双项俱成：
A4（D-158/D-161/D-163/D-164：unprovide 唤醒顺序 / 3 层 walk+组 isolate 边界修复 / 快照 +
golden 22/24 + m27）；HMR（D-162/D-165/D-166：sync_plugin 事件入口 + 删除后保留但 inert +
e2e）。下一步候选：A2 收口复查 / group 嵌套异步时序（M27/M28 纵深）/ harness FIXME（插件文件
→ name 解析）。
**预期影响与回滚点**：本提交含 HMR acceptance 工件。回滚 = 撤本提交。

## D-167（新目标需求分析过闸：group 嵌套异步时序 M27/M28，聚焦 Finish 时序）

**日期**：2026-08-27

**触发问题**：用户新目标「继续任务：A2 收口复查 / group 嵌套异步时序（M27/M28）/ harness
FIXME（插件文件→name 解析），从更底层的任务做起」（goal-68b94531）。

**方法论二复盘（自下而上核对）**：三候选层级——M27/M28（核心运行时 finish/驱动序，依赖最底层）
> A2 收口复查（对已闭环 `!!js` eval_scope 绑定，D-141..144/`.spec/service-assembly-p6` 的复核）
> harness FIXME（DIV-HMR-2 推迟的「插件文件→注册名」装配胶水，顶层依赖 loader API）。

**用户确认（ask_user_question）**：order=A（从 M27/M28 做起）；target=A（**聚焦 Finish 时序**：
修复嵌套组「Group 提前/不聚末尾 Active」+「Group Active 先于子入口」两类；**不动 disposer 并发**
=DIV-A4-5 类保持文档化另行立项；全量字节级 B 与仅语义级 C 均被否）。

**阶段结论**：需求工件 `.spec/group-nested-async/requirements.md` 定稿 → 阶段 2（系统设计）。
**预期影响与回滚点**：本提交纯文档。回滚 = 撤本提交。

## D-168（M27/M28 系统设计过闸：Finish 时序批次规则 + m28 + 划界）

**日期**：2026-08-27

**触发问题**：D-167 需求确认后进入阶段 2（设计）。

**S1 实证（三探针 + fork 源码）**：cordis `Group = async* [Service.init]` 的 inertia 挂在其
`await update(children)`（`Promise.allSettled(children.create)`）上 → **偏序：父组 finish 晚于
全部后裔 settle**；Rust `await_children`（context.rs Finish 臂）只查 Loading 后裔、独立入队
Finish 不保证偏序 → probe-nested-finish（3 组）中 Pending-only 组中途出队。排除
`associate:'loader'`（仅为 proxy 转发，非激活门）。已实证 2 层嵌套/隔离探针的组位置双侧一致。

**S2 设计**：Finish 臂 `should_wait` 扩展——组延迟条件 = ①Loading 后裔（现行）OR ②批内存在
普通 fiber（await_children=false）的排队 Apply/Finish 任务。②使 Pending-only 组纳入批次末尾
（C1 G>L），@仅组延迟、普通任务必然排空 → **无死锁**；不动 notify/disposer/unload/sync 路径。

**划界**：DIV-nested-1（Pending-only 提前 finish → ②修复）；DIV-nested-2（mount 时序两阶段
= 装载调度非 Finish，B 口径预留）；DIV-A4-5（disposer 并发）不动。

**实现顺序（TDD）**：m28 红（3 层嵌套+隔离；末态 + G>L 不变量 via take_trace）→ Finish 臂② →
绿 + 全回归 + 尝试嵌套 golden。

**阶段结论**：工件 `.spec/group-nested-async/design.md` 定稿 → 阶段 3（TDD 编码）。
**预期影响与回滚点**：本提交纯文档。回滚 = 撤本提交。

## D-169（M27/M28 编码实现 B 口径：顺延法 deferred-await 解决 DIV-nested-2 + loader-25）

**日期**：2026-08-27

**触发问题**：编码期实测（loader-25，3 次稳定）M28 后组 finish 已对齐，**唯一**剩余偏差 =
`status:p:Loading:Active` 落点：Rust 在 `plugin:c`/`plugin:b`（孙辈注册）后、cordis 在前 →
DIV-nested-2（mount 时序）。当晚用户问询时提供 A（按设计回退）与 B（扩口径 mount 时序）二选一；
**用户裁决 = B**。

**机制（研读 vendored cordis src）**：cordis 组子入口 `create()` = `entry.update → init →
import → _start(registry.plugin → fiber 构造: internal/plugin → _refresh → _reload) →
fiber.await()`——import+构造+reload 的多 hop（≥3 微任务）使**扁平子 Active（~2 hop）抢在组兄弟
孙辈注册（≥3 hop）之前**；Rust `drive_async_loads` 在 `Apply(gInner)` 内联注册孙辈 → 提前 hop。

**实现（顺延法，最小 core 变更）**：
- `AsyncTask::Await(FiberId)` 变体 + `Runtime.pending_awaits: HashMap<FiberId,
  LocalBoxFuture<EffectOutcome>>` 暂存 `EffectOutcome::Await` future。
- drive `Apply` 臂遇 Await：**不内联 `fut.await`**；标记 `await_children`、存 future、入队
  `Await(fid)`。新 `Await(fid)` 臂：yield → 取出 `fut.await`（子/孙入口注册在此发生）→
  `pop_current` → collect → 入队 `Finish`。
- **current 栈修复**（顺延的必要配套）：延迟窗口内多个组 apply 都留在 `current`（push 序），
  Await 任务 FIFO 执行时栈顶未必本组 → 子入口 `parent` 误挂兄弟组（其 isolate 遮 svc → 消费方
  永久 Pending）；Await 臂执行前 `retain != fid + push(fid)` 抬顶、运行毕 `retain != fid`。
- `should_wait.queued_plain` match 增 `Await(_) => false`（不计数；组 finish 由①Loading 后裔 +
  ②普通任务排队保住批次）。
- 效果：孙辈注册晚一个队列 hop → loader-25 字节对齐；flat（loader-10）不变；m28 C1/C2 保持。

**验证**：红（回退 context.rs → m28 C1 FAIL）→ 绿；loader-25 字节逐行一致（21 行）；
`cargo test --workspace` EXIT=0；`clippy -D warnings` 0；`verify-diff.mjs` **26/26**
（25 旧零回归 + loader-25）；serve 冒烟 HTTP 200/13270（基线一致）。

**预期影响与回滚点**：改动集中在 dsh-core 异步装载驱动 Await 处理 + loader 组路径；golden
26/26 + m-series 全绿锁定。回滚 = 撤销 `AsyncTask::Await` 变体 + `pending_awaits` + drive Await
臂与 current 顶抬（复现 D-168 状态）；m28 C1/C2 不依赖 hop 结构，回滚后仍绿；loader-25 golden
与 verify-diff 登记随回滚移除。

## D-170（M27/M28 验收过闸：B 口径达成，目标段闭环）

**日期**：2026-08-27

**阶段结论**：M27/M28（group 嵌套异步时序，B 口径 mount 时序）**闭环**：
D-167（需求，用户确认起点= M27/M28）→ D-168（设计，Finish 批次+A 划界）→ 编码实测定位
DIV-nested-2 → **用户裁决 B** → D-169（编码：顺延法 + current 修复 + loader-25 字节 golden）→
D-170（本验收）。验收标准全部达成：loader-25（3 层嵌套 + isolate + Pending-only）字节级 PASS、
旧 25 golden 零回归（verify-diff 26/26）、cargo test 0 失败、clippy 0、serve 200/13270。
验收工件 `.spec/group-nested-async/acceptance.md`；requirements §6 记录 B 口径更新。

**诚实边界**：顺延法给「组子入口创建」统一加一个 hop；更复杂批内形态（多组兄弟互依、
>3 层少分支深链）与 cordis 精确微任务序的逐字节一致性仅由现有 26 golden + m28 语义锁定，
未测试形态不背书。卸载/更新/HMR 路径不受 load 顺延影响（loader-24/m15/m16/m18 全绿）。
DIV-A4-5（disposer 并发）保持文档化、未触碰。

**预期影响与回滚点**：见 D-169 回滚点。下一步候选（beyond 目标剩余项）：A2 收口复查 /
harness FIXME（插件文件→name 解析）。

## D-171（A2 收口复查 需求+设计过闸：三缺口实证 + F1-F4 修复设计）

**日期**：2026-08-27

**触发问题**：M27/M28 闭环后，beyond 目标剩余两项（A2 收口复查 / harness FIXME）；经自下而上
比对 A2（loader 层）较 harness FIXME（顶层装配）更底层；**用户确认 next_task=A2 收口复查、
fix_or_record=A 修复＋测试锁定**。工件 `.spec/a2-closure-review/requirements.md`。

**探针取证**：
- P1（红）：provider Active + 消费方 `disabled_expr` 引用注入服务 → 顶层 create 无 current fiber
  → 服务不可见 → fail-closed **误禁用**（`P1 snapshot=[]`）。fork 在 loader ctx 根化的 entry
  扩展 Context 求值则可见。
- P2（绿=既有锁定）：`interpolate` **原子**——任一 `__jsExpr` 失败整树保留原 config。
- fork 对照（index.ts:117-123）：`internal/plugin` 时 `Inject.resolve(fiber.entry.options.inject,
  fiber.inject)` **合并 entry 级 inject**；Rust `load_plugin` 未合并（entry.rs 注释未实现）。

**修复设计（F1-F4）**：F1 `Cordis::get_value_from(ctx_fiber,name)`（指定上下文解析 Value 服务）；
F2 `fiber_service_ctx` 值改**目标视图**（同 fid，修 DIV-6-2 '调用方可见≠目标可见' 错位）；
F3 `entry_disabled` 绑**入口上下文**（entry inject ∪ 插件 inject × loader 根 realm，显式键优先、
未知仍 fail-closed）；F4 `Runtime.pending_entry_inject` 并入 fiber inject（load/group_load 三径同填）。

**预期影响与回滚点**：见 D-172。工件 design.md 定稿 → 阶段 3（TDD 编码）。

## D-172（A2 收口复查 编码落定：F1-F4 红→绿 + m29 锁点）

**日期**：2026-08-27

**关键实现（TDD）**：F1 `get_value_from`（context.rs，Public，resolve_impl+Value downcast，不经
internal/get 拦截=决策快照，DIV 记录）；F2 `fiber_service_ctx` 值解析 `get_value_from(fid,n)`；
F3 `entry_disabled` 绑入口上下文 + `entry_inject_names` helper；F4 `pending_entry_inject` 字段
（runtime.rs）+ register_plugin 并入 + `load_plugin`/`load_group_plugin`/`load_group_plugin_async`
填值。锁点 **m29_a2_review 4/4**：T-L1 disabled 入口上下文 / T-L2 interpolate 原子性（既有锁定）/
T-L3 目标 realm 服务（隔离组读本地 svc） / T-L4 entry.inject 合并。

**红验证（stash 回退 F1-F4）**：T-L1/T-L3/T-L4 FAIL、T-L2 ok → 恢复全绿（三个缺口各由测试驱动）。

**阶段 4 验证**：`cargo test --workspace` EXIT=0（210 ok 块）；`cargo clippy ... -D warnings` 0；
`verify-diff.mjs` **26/26**（golden 无 `!!js` → 零回归）；serve 冒烟 HTTP 200/13270。

**预期影响与回滚点**：行为修正：① disabled 引用可见服务不再误禁用（此前 fail-closed 误禁）；
② entry 声明 inject 现参与门控（fork 行为；无 inject 声明者不受影响）。回滚 = `git revert` 本提交
（F1-F4 整体）；m29 锁点随撤；m21/m3/26 golden 与修复前基线不受影响。

## D-173（A2 收口复查 验收过闸：复核报告 + 全回归闭环）

**日期**：2026-08-27

**阶段结论**：A2 收口复查**闭环**。D-171（需求+设计，用户确认任务与 A 口径）→ D-172（编码 F1-F4 +
m29 红→绿）→ D-173（本验收）。复核报告（acceptance.md）：V1 语义保真（scope 键集/原子性）✓ ；
V2 两缺口修复（disabled 入口上下文、目标视图）✓；V3' 缺口③（entry.inject 合并）✓；V4 全回归零
破坏。证据：m29 4/4（红→绿 3 项 + 既有锁定 1）、workspace 0 失败、clippy 0、verify-diff 26/26、
serve 200/13270。

**诚实边界**：disabled/插值值解析不经 internal/get 拦截（决策/插值快照，DIV 记录）；非 Value 服务
仍不暴露（DIV-6-1）；未重评 agent-presets/standing/combo 的 row_disabled（非 loader A2 面）。

**下一步候选（beyond 目标剩余项）**：harness FIXME（插件文件→name 解析，顶层装配，独立立项）。

## D-174（harness FIXME 需求+设计过闸：插件包=文件夹 模型重构 + Q1-Q4 用户确认）

**日期**：2026-08-27

**触发问题**：beyond 目标最后一项 harness FIXME（插件文件→注册名解析）。用户先要源码对照
（真实 deepseek-harness 如何加载/区分插件）再决策；经研究真实 `Tree.import(name)`
（cordis/packages/loader/src/config/tree.ts:103-120：`cordis:`→builtins 表，否则 Node 原生
import——`./rel` 相对 baseUrl、包名走 node_modules）+ 真实 examples/headless-agent/cordis.yml
（`name: '@deepseek-ai/dsh-...'` 或 `./file.mjs`；有 `!!js` disabled/config——即我们 A2 eval_scope
镜像），结论：真实 harness **name 即模块 specifier，无 wasm/world/类型判别**，插件凭服务自我
描述运行。

**用户重构**：否决 A/B/C 三选一，定向「插件=一个文件夹，内含 wasm 组件 + 前端组件；文件夹名=
插件注册名」；随后确认四项决策（全部推荐项）：
- D1 布局：`plugin.json` 清单声明（wasm/web/caps/world 可选）+ 构建目录约定回退；
- D2 前端：静态资源目录挂接（`/plugins/<name>/**`，包 web 目录）；
- D3 范围：Rust 侧（folder→wasm 注册 + serve 挂接 + 测试）；web GUI 消费侧留后续；
- D4 loop：name=folder + **world 判别**（预检组件导出接口：`plugin-api`→Component、
  `agent-loop`→Loop）为唯一路径，**移除 config.wasm 特判**，web-cordis.yml 迁移。

**设计要点**：world 判别 = wasmtime 34 `types::Component::exports()` 遍历导出名（ABI 事实）；
registry（内置/host）优先于包解析；turn loop = 首个 dsh-loop 包（config 序）；boot 与 HMR
refresh 共用 `assemble_plugin_packages` helper（换 loop = 换 `name` 指向另一包）。

**预期影响与回滚点**：见 D-175。工件 `.spec/plugin-file-resolve/{requirements,design}.md`。

## D-175（harness FIXME 编码落定：文件夹包装配 + world 判别 + web 挂接，TDD 红→绿）

**日期**：2026-08-27

**关键实现**：wasmrt `detect_component_kind`/`ComponentKind`（lib/component.rs 导出名预检；
m30_plugin_kind 3/3：echo-loop→Loop、hello-component→Plugin、非法→Unknown）；loader
`has_plugin`；cli `plugin_pkg.rs`（`resolve_package` 清单+回退、`effective_caps` 优先级）
+ 5 单元测试；lib.rs `assemble_plugin_packages`（boot/refresh 共用；移除 load_component 死代码）
+ `Boot.packages`；web.rs `/plugins/<name>/**` 静态分支（`serve_package_asset`）；迁移
web-cordis.yml/web-smoke*.yml 与 m9_boot/m9_yaml_assemble 的 config.wasm 形态
（manifest 测试 → plugin.json 显式 wasm；swap 测试 → name 换包）。

**TDD**：红验证（stash 实现）——`boot_assembles_wasm_component_package_sibling_to_loop`
0/1 FAILED；恢复后全绿。锁点：m9_boot 23/23（含组件包装配+未知插件 fail-loud）、plugin_pkg 5/5、
m30 3/3、web serve_package_asset 1/1。

**预期影响与回滚点**：行为修正——① 插件从文件夹包按名解析（wasm+前端），`config.wasm` 键
废除（死键，name=folder 优先生效）；② world 判别替代「config.wasm=loop」；③ `/plugins/<name>/**`
前端静态挂接。回滚 = `git revert` 本提交（特征级）；旧 config.wasm 配置仍能被 name=folder 解析
（兼容降级）。

## D-176（harness FIXME 验收过闸：复核报告 + 全回归闭环，beyond 目标全部闭环）

**日期**：2026-08-27

**阶段结论**：harness FIXME **闭环** → beyond 目标 **全部完成**。D-174（需求+设计，源码对照 +
Q1-Q4 用户确认）→ D-175（编码 TDD）→ D-176（本验收）。S1 文件夹解析 / S2 loop 统一 / S3 前端
挂接 / S4 回归全绿，工件 `.spec/plugin-file-resolve/acceptance.md`。

**证据**：workspace 0 失败；clippy 0；verify-diff 26/26（golden 数据面未触）；serve 冒烟
200/13270（web-cordis.yml 纯 folder 形态）。

**诚实边界**：前端组件以静态目录挂接（D2=a），GUI 消费侧留后续（D3）；不做插件文件 watch
（HMR 显式 refresh 重解析）；`plugin.json` 为包级清单非 loader 全局 schema。

**目标收尾**：objective 的 M27/M28、A2 收口复查、harness FIXME 三项全部完成。

## D-178（服务装配单元前端 UI 方向决策：P2 声明式数据驱动）

**日期**：2026-08-27

**触发问题**：beyond 目标完成后，用户追问「服务装配单元插件若含前端 UI 如何加载渲染？Rust vs
TS 差距 + 网络类似方案？」——讲解 + 网络调研 + 方向决策任务（纯调研，不写实现代码）。

**调研结论**（真实源码 + 网络）：
- 差距本质：TS 前端插件=可执行 JS（`lib/client.js`，浏览器直接跑 + React 生态）；Rust wasm 插件=
  后端逻辑（进不了浏览器）。前端只能四选一：JS bundle 镜像标准链 / 声明式数据驱动（服务端定义
  UI、客户端通用渲染）/ 独立前端 iframe / 浏览器内 wasm（重）。
- 网络锚点：MCP Apps / A2UI（服务端定义 UI 客户端渲染，2026 趋势）、Adaptive Cards（声明式 UI
  成熟落地）、微前端/Module Federation（动态载入独立前端包；真实 harness 因 Vite 不支持 remote
  bundle 而否）、Cordis/Koishi webui + vscode-web-wasm-rust（同生态 + 浏览器内 Rust-wasm 先例）。

**用户确认（四问全答）**：① 路径 = **P2 声明式数据驱动**（否决 P1 镜像标准链 / P3 iframe /
P4 浏览器 wasm）；② GUI 边界 = **允许改/自建 GUI 壳**（含通用渲染器）——P2 因此成立；③ 不定试点
（后续立项再定）；④ 只确认方向，不写实现代码。

**关键决策与预期影响**：插件前端 = 声明（静态 `plugin.json` / 动态 RPC `describe-UI`），GUI 壳以
**单一通用渲染器**消费 JSON 描述（元素/布局/绑定/动作），动作回宿主 RPC 到 wasm 插件。天然沙箱
（渲染器只读 JSON，无任意 JS）。影响：新增插件 UI 不需改壳；代价 = 需 GUI 壳改动 + 声明 schema
子集设计 + 渲染表现力受 schema 限制。回滚点：方向未实现（纯决策，无代码）；后续立项时可在实现
前再校准 schema 形态。工件 `.spec/service-assembly-ui/requirements.md`（v2 正式）。

**下一步（建议）**：单独立项走完整瀑布流——定试点 → 设计（schema 子集 + 渲染器契约 + 发现面 +
RPC 动作面）→ TDD 实现 → 验收 demo。

---

## D-179（P2 架构模型设计定稿：声明=数据 / 前端通用渲染器 / Rust 只生声明不渲染）

**日期**：2026-08-27

**触发问题**：D-178 只冻结了 P2 方向（声明式数据驱动），方向讨论中「讲清楚但未固化」的架构模型
需要落定为设计工件；且用户追问链暴露两个易误点（「XML 翻译成 TS/CSS 交给浏览器」「由 Rust 渲染
更快」），需在文档中显式纠偏，避免后续实现走回弯路。

**用户明确**：① 更新文档以实现方向；② 交付物 = 新建 `design.md` 为主 + `requirements.md` 加
指针 + 记本条 D-179 + git 提交；③ 深度 = 架构模型 + 组件职责矩阵 + 概念纠偏，**不含**声明 schema
字段草案（试点选定后再细化）；④ SSR 首帧 = **可选加餐，非核心闭环验收项**。

**关键决策**：
- **声明是数据，不是代码**：插件侧 Rust 静态写 `plugin.xml` / 动态 `describe-UI` 组 XML，
  产物 = 数据文本。**不翻译成 TS/JS**——浏览器不执行 TS；一旦翻译产物成可执行 JS 就回到 P1
  （插件代码进浏览器 → 沙箱没了），P2 的天然沙箱（渲染器只读数据）是立身之本。
- **通用渲染器必须在浏览器（前端）实现**：「渲染」拆两半——前半（声明→HTML 字符串）Rust 能做
  = SSR 首帧加速；后半（layout/paint + 事件 + 交互）是浏览器引擎领域，Rust/wasm 够不着；
  且交互迫使渲染器在浏览器内（服务端渲染每次交互一整轮回传 = 表单刷新模型）。
- **Rust 角色** = 生声明 + 数据面/权限/动作执行（host-api 回调），独独**不做渲染**。
- **SSR 首帧 = 可选加速，非验收项**：写了合法位置（首帧更早、hydrate 续接），但不实现也不塌
  架构（可纯客户端渲染）；本轮不写实现。

**预期影响与回滚点**：新增 `.spec/service-assembly-ui/design.md`（设计工件 v1，含架构模型 +
职责矩阵 + 三条概念纠偏 + 边界），`requirements.md` §6 追加指针，本条记录。纯文档，无代码、
无运行时影响。回滚：撤本提交即可回到 D-178 后的状态；架构模型尚未实现（无代码可回滚），真正
实现代码留到试点立项后的 TDD 阶段。

## D-180（试点落地：llm-deepseek 服务装配单元——rust + ui 声明 + wasm 垂直切片）

**日期**：2026-08-28

**触发问题**：D-178/D-179 定稿 P2 架构模型后，用户要求「选一个 deepseek harness 插件作试点，
转换为定下的 rust + ui 声明 + wasm 模式」——首个服务装配单元含 UI 的可验收 demo。

**试点选择（llm-deepseek）**：HANDOFF 点名的 canonical「cordis.yml 一行插件」；Config 是纯声明
式表单（apiKeyEnv/baseURL/thinking/reasoningEffort/maxTokens/defaultContextWindow/models[]），
天然可表达为 P2 声明（数据）；与 dsh-settings/credentials/kv 契约平行；验收可做 demo（渲染 +
保存 + 发现模型，不需真实 LLM 网络）。

**关键决策**：
- **wasm world 复用 host-remote 接口身份**（`export remote` + `import host-services`）：
  宿主 `WasmRemoteEndpointPlugin` **零改动**即可加载试点组件——每服务装配单元 = 一个远程载体
  （namespace=插件名），describeUI/save/discoverModels/currentValues 动作面。
- **声明=数据，一份契约**：静态 `web/ui.json` 与 wasm `describeUI` **逐字段一致**（m32 断言），
  渲染器只读声明（无插件 JS 进浏览器 → 天然沙箱）。
- **静态声明起步 + 动态 RPC 增强**（requirements.md P2 §2 倾向）：`/plugins/llm-deepseek/ui.json`
  静态分发（D-175 serve_package_asset）+ `/api/llm-deepseek/describeUI` 动态。
- **持久化走既有 kv 后端**（key `llm-deepseek/settings`），save 白名单校验 + fail-loud，不伪造成功。
- **试点边界**：不做真实 LLM 调用 / 不做 loader entry 依赖激活（inject）——「服务插件 entry 化」
  下一阶段；不做 SSR（D-179 可选加速，非验收）。

**验收（详见 `.spec/service-assembly-ui-pilot/acceptance.md`）**：m32（wasmrt）6 断言全绿 +
dsh-cli 集成测试 `llm_deepseek_remote_routes_and_serves_static` 绿（路由 + 未装配回落
not-implemented + ui.json 静态 200）；`cargo test -p dsh-cli -p dsh-wasmrt` 225 通过（5 个
M5 bash/schedule/job 失败为基线既有环境性失败，git stash 验证与本改动无关）；clippy `-D warnings`
零告警。

**预期影响与回滚点**：新增 `wasm-plugins/llm-deepseek/`（组件 + web 声明/渲染器 demo）+ `Boot.
llm_deepseek_remote` 字段（默认 None）+ WebConfig.wasm_base 字段 + dispatch 路由（仅 namespace=
llm-deepseek 分流，既有 host-remote 路由不动）+ m32/集成测试 + 本 spec 目录。回滚：撤本提交即去
试点载体装配与路由（字段默认 None，路由回落 not-implemented），既有行为零回归。

## D-181（桌布架构定稿：双正交枚举 / 卡片壳 / 网格自适排布 / 热插拔清单口子）

**日期**：2026-08-28

**触发问题**：D-180 试点跑通「wasm 生声明 + 通用渲染器」后，用户提出把前端从「绑定 harness
现有前端」改为**桌布（Canvas）**——每个插件 UI 是一张卡片、按类型分类、左侧固定边栏、右侧网格
容器自适应排布。用户明确**非专业 UI 设计**，要求逐轮反问确认真实需求、**不要盲从其设计**。

**协商结论（六轮问答，用户逐条确认）**：
1. **桌布定位 = 独立视图**：不推翻 harness 前端（聊天/会话等成熟资产保留）；现有前端 UI 资产
   按「UI + 逻辑」**增量拆成服务装配单元**（迁一块、验一块，未迁部分继续用原前端）。
2. **右侧 = 工作台**（多卡片平铺），非单卡查看器；点左侧类型=切分类，点左侧插件名=聚焦/滚动该卡。
3. **坐标不外泄给插件**：插件只声明 `size{w,h}` + 排布顺序；**10px 网格格距**（非打印 pt），
   列数随容器宽度自适应；「插件写死坐标」被否决（重叠/空档/出界，不成立自适应）。
4. **双正交枚举强制分离**（本条核心，防架构崩塌）：`type`=侧边栏**分类**（面向用户，加值近乎
   免费）；`view.kind`=**渲染契约**（加值必须实写渲染器）。合并成单一枚举被否决——那会让
   侧栏显示 `form/list` 这种用户看不懂的值，且「加一个分类就得配一个渲染器」两条轴互相锁死。
5. **v1 单卡 + `cardId` 预留**：一个装配单元出一张卡；将来出多卡是**发现清单层面**做加法。
6. **`size` 封顶**：`w≤4`、`h≤8`，**超出由宿主裁剪 + 记录**（契约违规降级，不是渲染失败）。
7. **热插拔口子**：`/api/ui-manifest` 从**实时状态**算（非启动快照）+ `rev` 代数 + 复用既有
   `/plugins/events` SSE（D-175/D-177）广播变更；卡身份 = `(插件名, cardId)`。

**概念纠偏（防走回三条弯路，与 D-179 同性质）**：
- **`board` 否决**：「board」就是桌布/画布本身；卡片里再嵌画布 = 无限递归，v1 是纯陷阱。
  若本意是「一卡里显示一行行条目」，那已是 `list`。
- **三档制防「契约吹牛」**：写进契约的每个 `view.kind` 都等于「渲染器必须画得出」。故
  **v1 实现** `form`/`status`/`list`；**契约预留** `chat`/`chart`/`table`（渲染器待建，落
  fail-loud 元数据回落，不白屏）；**否决** `board`。「契约一次定全 + 实现逐档点亮」既满足
  用户「一步到位、不要两套并行 schema」，又不虚报能力。
- **v1 顶层只一种容器 `kind:"card"`**，D-180 的 `kind:"form"` **降级为 `view.kind:"form"`**
  并**废止旧顶层形态**——不是新旧并列（并列才是崩塌根源），只维护一棵树。

**下一步（不含本轮代码）**：新建 `.spec/service-assembly-ui-canvas/`（需求 + 设计）；试点
`service-assembly-ui-pilot` 声明形态迁移 `form → card.view.form` 列为下一编码阶段第一件事。

**预期影响与回滚点**：纯文档——本条 + `.spec/service-assembly-ui-canvas/{requirements,design}.md`
+ 试点 design 修订指针。**零代码、零运行时影响**（桌布尚未实现，无可回滚代码）。回滚：撤本提交
即回到 D-180 后状态；试点现有代码仍按 D-180 的 `kind:"form"` 运行，迁移在下一轮以 TDD 红→绿进行。

## D-182（桌布 C1 编码落定：试点声明迁移 form → card.view.form + 双模型防线）

**日期**：2026-08-28

**触发问题**：D-181 把 v2 契约定稿并声明「v1 顶层形态废止、不并存」。并存不会自己消失——
必须真把唯一的 v1 产物（llm-deepseek 试点）迁过去，并用机制护栏证明没有第二套模型残留。

**关键决策**：
- **迁移只动形态，机制零改动**：`web/ui.json` 与 wasm `ui_declaration()` 的
  `fields`/`actions` 原样搬进 `view`，外套 card 壳并加 `cardId`/`type:"model"`/`size 2×3`。
  wasm world、四端点、kv 落盘、白名单校验、`/plugins/**` 静态挂接、`dispatch` 路由**全部不动**
  ——这验证了「卡片化只影响声明形态与呈现，不影响 RPC 面」的判断。
- **新增 `view.dataRpc`（契约内字段，非扩造）**：设计 §4.1 已预留「渲染器启动时调 `view.dataRpc`
  拉宿主真实数据」。迁移时启用它显式表达 `["llm-deepseek","currentValues"]`，替掉 v1 里
  顶层 `namespace` 的隐式推导——数据面不再靠猜。
- **双模型防线 = 解析后看顶层，不 grep 文本**：新测试
  `no_legacy_v1_top_level_declaration_anywhere` 遍历全仓 `wasm-plugins/*/web/ui.json`，断言凡含
  `$schema` 者必须 v2 + 顶层 `kind:"card"`。**必须解析**：`view.kind:"form"` 是合法内容视图，
  文本 grep 会假阳性。
- **红验证**：临时放入 `kind:"form" + $schema:v1` 探针 → 防线 **FAILED** 并精确报出违规路径；
  移除后复绿。（避免「恒真断言」自欺。）
- **契约补一行 `card-kind-unknown`**：`$schema` 对但顶层容器写错时，只报
  `schema-version-unsupported` 会把诊断引向错误方向，故区分「版本错」与「容器种类错」。
  属契约正常收敛，不改「v1 已废止」的结论。

**验收**：m32 **8/8 绿**（新增 `declaration_satisfies_v2_card_contract`：cardId 非空 / type 落
闭集 / size 封顶且**声明无坐标** / dataRpc 显式；`static_ui_json_matches_describe_ui` 继续守护
「静态与 describeUI 逐字段一致」一份契约）；dsh-cli 集成测试升 v2 后绿；
`cargo clippy -p dsh-cli -p dsh-wasmrt --all-targets -- -D warnings` **0**；
dsh-wasmrt 全套 14 个测试目标全绿（含 m31 host-remote）；dsh-cli **225 通过 / 5 失败**，
5 个为**基线既有** M5 bash/schedule 环境性失败（git stash 已验证与本改动无关），计数与 C1 前一致。

**诚实交代**：`web/renderer.js` 是**包内 demo，不是桌布壳**——C1 只让它正确消费 v2 单卡并按
§7 fail-loud 表分派；侧栏分类 + 网格工作台在 C3。故 `status`/`list` 在当前 demo 落
`renderer-unimplemented` 回落（三档制回落语义正是为此设计，不虚报实现进度）。

**回滚点**：C1 集中在「声明形态 + 测试断言」——撤销本提交即回到 D-180 的 v1 形态；
`dispatch` 路由 / 载体装配 / kv 后端不受影响（未改动）。

## D-183（桌布 C2 设计定稿：uiManifest/list 原生臂 + sha256 内容哈希 rev + 清单层归一权威）

**日期**：2026-09-04

**触发问题**：接手文档 §3-A 明确「开工前必须先决策端点形状」；`rev` 语义、坏包错误码集、
disabled 交叉语义（试点未 entry 化）也须在编码前定死，否则清单层会各自发明规则。

**考虑过的选项与裁定**：
- **端点形状**：(a) 原生臂 `uiManifest/list` vs (b) 裸 `/api/ui-manifest` 路由特判。
  **选 (a)**——`/api` 路由 `trim_start_matches("/api/")` 后无 `/` 的 method 会被
  `dispatch_wasm_remote` 判 `not-implemented`（web.rs:971/4454 实证），(b) 必须为单端点
  开路由例外，破坏 `/api` 网关 `namespace/method` 约定（trust fence / RPC 信封 / 前端 rpc
  通道复用全打折）。(a) 与 `commands/list`、`settings/describe` 同形，零例外。
  **已同步回写** canvas design §6.1 + §1 架构图（原文 `/api/ui-manifest?rev=` 属 wire 表述
  纠偏，非契约变更）。
- **rev = 内容哈希而非单调计数**：单调计数重启后漂移，客户端缓存 rev 作废，违背热更前提。
  `rev = SHA-256(canonical cards[])` 全量小写 hex；**error 条目计入 rev**（坏声明修好=内容变）。
- **哈希依赖**：手写 FNV-1a vs `sha2`。**选 sha2 0.10**——已在 Cargo.lock 与本地 registry
  缓存（离线可解析，零新增供应链），成熟通用；hex 手写不拉 hex crate。「自己造轮子」的
  条件（不满足/成本过高/核心竞争力）一条都不成立。
- **清单层 = 归一单一权威**（承接手文档 §3-A 建议）：type 未知→misc+declaredType、
  size 裁剪 w∈[1,4]/h∈[1,8]+declaredSize、title 缺失回落 cardId、坐标键零输出。渲染器只
  信清单，规则不复制到 C3。
- **坏包错误码集**：`declaration-unparseable` / `schema-version-unsupported` /
  `card-kind-unknown`（复用 §7 表）+ 新增 `card-id-missing`（身份不完整无法去重/聚焦，
  fail-loud 而非静默补造；承 D-182 `card-kind-unknown` 的收敛方式）。
- **size 默认档位语义裁定**：canvas design §5.1 原文「按 type 的默认尺寸（model/config→2×3，
  status→2×2，list→4×4）」把 type 与 view.kind 混写（status/list 是 view.kind）。按语义
  裁定为**按 view.kind**：status→2×2、list→4×4、其余→2×3。契约收敛，不改已锁决策。
- **disabled 交叉（用户确认）**：loader entries 过滤 group 后按 `entry.name == pkg.name`
  匹配；**全部同名 entry 禁用才排除**，无同名 entry 出卡（兼容试点「serve 直接 push、
  未 entry 化」现状；entry 化属装配引擎侧后续）。
- **模块切分**：纯函数核心 `ui_manifest.rs::build_manifest(packages, entries)`（每请求实时
  计算，禁任何快照缓存）+ web.rs `dispatch()` 一臂。C3（桌布壳）/C5（SSE rev 广播）复用
  同一核心，规则不复制。

**预期影响与回滚点**：全增量——新模块 + dispatch 一臂 + Cargo.toml sha2 一行 + 测试；
既有 wire 面零改动、wasm 侧零改动。回滚 = 撤销本工作流提交即回到 `44f9618` 后状态。

**验收实测（2026-09-04 编码落地）**：TDD 桩红 **10/11**（唯一 pass 为负向断言被空输出平凡满足）
→ 实现转绿；**缓存探针红验证**（注入 `OnceLock` 快照缓存 → `rpc_ui_manifest_is_live_no_cache`
FAILED，证明实时性护栏非恒真）后移除复绿；11 新测试全绿（8 单元 + 3 集成）。
dsh-cli **241 通过 / 0 失败**（基线 230 + 新增 11，零劣化）、m32 8/8、dsh-wasmrt 全绿、
clippy `-D warnings` **0**、verify-diff **26/26**。逐条验收与诚实台账见
`.spec/service-assembly-ui-c2/acceptance.md`。

## D-184（桌布 C3 设计定稿：桌布壳资产宿主 = 编译进 dsh-cli 的 /canvas 独立视图 + 纯函数核心 TDD）

**日期**：2026-09-05

**触发问题**：C3 桌布壳（侧栏分类 + 10px 网格工作台 + form 渲染器 + §7 fail-loud 表）
要落地，先要回答：壳住哪（不进 harness dist？）、怎么测（无浏览器基建）、若干展示层
开放点（默认视图/侧栏序/网格几何/排布细节/轮询间隔）。用户休息中明示自主推进，需求关
自主过闸，默认值全部标注可回退。

**考虑过的选项与裁定**：
- **资产宿主**：(a) harness dist 外挂 vs (b) `include_str!` 编译进 dsh-cli + `/canvas` 路由。
  **选 (b)**——web_root 是 harness dist（外部件），插件基建不混入；且 SPA fallback
  （web.rs:1115）会把任何未路由路径吞成前端 index.html，(b) 在 fallback 前立独立路由、
  `/canvas/*` miss → 404（不回落——防「桌布失踪变前端」）。独立视图承诺 = harness 前端零改动。
- **可测性架构**：排布/校验/模型/信封全部进 `core.js` **纯函数**（零 DOM/零 fetch/零 eval），
  `node --test`（v24）断言；`app.js` 只做粘合。无浏览器基建是现实约束——把**该测的都
  挪进纯函数**，粘合层以路由冒烟 + 手测补偿并诚实声明。测试 ESM 用 assets 目录内
  `package.json {"type":"module"}` 标记（浏览器忽略该文件）。
- **排布算法契约**（可证无重叠）：`layoutGrid(cards, C)`——`w=min(w,C)` 收窄；卡顶 =
  跨列 heights 最大值；平手取最左；`heights[span]=top+h`。瀑布流 first-fit 的确定性化，
  配合 seeded-LCG 性质测试（无重叠/不出界）。
- **展示层默认值**（均可回退，回看清单在 c3 requirements §2）：默认「全部」视图、
  侧栏闭集枚举序 misc 恒末、列宽 260px/行高单元 100px/格距 10px（契约值）CSS 变量化、
  rev 轮询 4s（C5 SSE 后转兜底）。
- **error 条目落位**：清单 error 条目在壳内归 `misc` 组画 fail-loud 卡（**不发 fetch**——
  清单已判死刑），保证「装了但坏了」必然可见。
- **附带修复（发现于自下而上实证）**：试点 demo `renderer.js::callRpc` 发裸 `{args}`，
  缺 client-request 信封——真实 HTTP 下 `rpc_envelope_ok`（web.rs:1898）必 400。
  属 C1 包内 demo 的 wire 缺陷（测试都走 dispatch 直调没暴露）。本轮按同一 wire 修一行。
  `rpcEnvelope` 的红验证刻意让桩先返回裸 `{args}` 形——证明测试能抓住这类缺陷。

**预期影响与回滚点**：新增 `crates/dsh-cli/assets/canvas/` + `canvas.rs` + serve 闭包一插 +
demo renderer 一行修复；既有 wire/路由/前端零改动。回滚 = 撤销 C3 提交即回到 C2 完成态。

**验收实测（2026-09-05 编码落地）**：桩红 **12/12**（`node --test`，含 rpcEnvelope 桩刻意
复刻 demo 裸 `{args}` 缺陷被测试抓住——信封探针生效）→ 实现转绿 **12/12**；canvas.rs 3 测
（壳引用齐 + 资产 mime + 零 eval 哨兵 + miss→None）。dsh-cli **244/0**（241 + 3）、
dsh-wasmrt 全绿、clippy **0**。已知边界（诚实台账 acceptance §5）：app.js 粘合层与真实
serve 进程冒烟无自动化执行（无浏览器基建/需 boot fixture），DOM 行为待人工浏览器验证。

## D-185（桌布 C4 设计定稿：status/list 渲染器点亮 + 首个 harness 面板改写「插件清单」+ 载体泛化 remote_carriers）

**日期**：2026-09-05

**触发问题**：用户目标「完成C3后，继续将其他的 deepseek harness 插件改写为服务单元」。
改写需要：① list/status 渲染器（大量面板本质是列表）；② 新单元接入通路——若继续
`web.rs` 硬编码 namespace + 特判载体，每改一个面板都要改宿主，**违背热插拔第一等**
与「装配单元自持 UI+逻辑」的基石判断。

**考虑过的选项与裁定**：
- **接入通路**：(a) 每面板一次宿主提交（硬编码复制）vs (b) `Boot.remote_carriers`
  （namespace→载体 map）+ serve **扫描 wasm_base 发现 world:"remote" 包**挂载。
  **选 (b)**——「每装配单元一载体、namespace 分流」是接手文档 §2.2 预告的既定扩展向；
  选 (a) 等于把「改写插件」变成「改宿主」，生态命题死亡。未命中 map → host-remote
  既有语义零变（llm-deepseek 既有测试即回归锚）。
- **改写首个试点**：设置卡（form，与试点重复）vs **插件清单（list，canvas design §11
  既定建议）**——list 点亮 + 新数据面（loader 投影只读）+ 只读无副作用，风险最低收益最高。
- **数据面**：壳内直连 `pluginInventory/list`（壳变宿主 RPC 的搬运工，单元无逻辑=JSON 壳）
  vs 新单元 wasm `list` 端点内 `host_services.get("loader")` 行投影。**选后者**——
  服务装配单元=「UI+逻辑同包」本义；行语义（disabled/fiber→state 映射、group 过滤）
  归属单元自己；宿主数据面零新后端。
- **失败面纪律**：loader 服务失败 → `{ok:false}` 透传，**绝不伪造空表**（诚实空态与
  错误态在渲染器分列）。
- **扫描器坏包语义**：缺构建物/坏 plugin.json → eprintln 跳过（不炸 serve、不上死卡）；
  `host-remote` 本身（宿主桥）按名排除。
- **清单卡 type**：`runtime`（loader 装配态=运行时编排）；`capability` 留给工具/技能
  语义位。可回退一行。
- **刷新 affordance**：list/status 卡的「刷新」= 渲染器重放 dataRpc（渲染 affordance），
  不塞进契约 actions——契约不为渲染器便利扩张。

**预期影响与回滚点**：A（assets/canvas 渲染器）纯增量；B 新目录 `wasm-plugins/panel-plugin-inventory/`
（撤目录即消失）；C `Boot.remote_carriers` 替换 `llm_deepseek_remote` 字段 + serve 扫描块
（撤 C 提交回 C3 完成态 `1b0708a`）。三块各自独立可回退。

**验收实测（2026-09-05 编码落地）**：A 桩红（validate 新档 + listRows「伪造行」探针被抓）
→ node **16/16**；B m33 先红（包不存在）→ **5/5**（含 `static_ui_json_matches_describe_ui`
一份契约 + `list_service_failure_is_fail_loud` 不伪造空表；m32 双模型防线自动覆盖新声明）；
C `scan_remote_units` 判据测试（五类跳过路径）+ `scan_mounted_units_appear_in_manifest`
（发现→清单出第二卡，宿主清单层零改动）+ llm 试点测试迁移 carriers 分流（断言主体不动）。
dsh-cli **246/0**、dsh-wasmrt 全绿、clippy **0**。
**一处 wire 语义随泛化统一（已验证零宿主回归）**：未装配 boot 下 `llm-deepseek/*` 回落
从特判 `not-implemented` 并入 remote 家族统一 `internal`（"(no remote carrier assembled)"）
——D-115 既有的 pluginInventory/dynamicCordisRunner 回落文案与 code 全数不变。

## D-186（桌布 C5 落地：热插拔 watch（tick 同步 + 2s 节流）+ `ui-manifest-changed` SSE 推送）

**日期**：2026-09-05

**触发问题**：热插拔是第一等要求，但运行时没有任何包增删通路（scan 只在启动跑一次），
桌布实时性 = 纯轮询。canvas design §6.2 预留的 `ui-manifest-changed` 需要一个能感知
「装/卸/改」的宿主侧机制。

**考虑过的选项与裁定**：
- **变更感知**：(a) 独立 watcher 线程 + fs watcher 依赖 vs (b) **serve 主循环 tick 挂钩
  + 2s 节流重扫**。选 (b)——boot 非 Send（单线程宿主纪律），watch 在 accept 线程内
  零同步原语；重扫 = 目录扫描 + 小包读取，2s 窗口成本可忽略；无新依赖。（a) 需要把
  载体装配搬回主线程的消息泵，复杂度全花在省 2 秒延迟上。
- **运行时构建**：watch 重扫**绝不**触发 `cargo component build`（会阻塞 accept 分钟级）
  ——`scan_remote_units_opts(base, build_missing)`：启动 true（开发体验）、运行时 false。
- **运行时装配失败语义**：载体加载失败 → eprintln 跳过（不炸 serve、不上死卡）——与
  启动 fail-loud 区分：启动是装配决策，运行时是热插事件。
- **卸载边界**：watch 只卸 `state.mounted` 登记过的 scan 挂载包，绝不碰 boot manifest
  装配的其它 packages（测试 `unmount_only_touches_scan_mounted` 钉死）。
- **广播通道**：复用 D-099 `/plugins/events` 的 mpsc 广播面新增
  `{type:"ui-manifest-changed", rev}` 帧——不新建端点；rev 是内容哈希，天然去抖
  （改回原样 = rev 不变 = 不广播）。桌布 `EventSource` 消费即重取清单；轮询放宽 10s
  作 SSE 断线兜底（`unchanged` 协商让兜底几乎免费）。

**验收实测（2026-09-05）**：桩红（3 watch 流程 + S1 帧形状）→ 全绿；
dsh-cli **251/0**（watch 4 + hmr 帧 1；**clippy 顺手抓出一条被吞的 `#[test]`**——C2 的
`disabled_entry_excludes_card` 此前因编辑事故退化为普通函数，已复活并计入）、dsh-wasmrt
全绿、clippy **0**、node **16/16**、verify-diff ALL PASS。
**诚实台账**：真实「装/卸目录 → 浏览器卡片增删」的端到端手测未执行（无浏览器基建）；
同步/广播/挂载面已全测，EventSource 消费属 DOM 边界层。

**预期影响与回滚点**：ui_manifest 两个函数 + hmr 两个函数 + serve 一钩 + app.js 十余行；
撤提交回 `11021e8`。

## D-187（面板改写 #2：panel-runtime-status 运行时状态卡——status 渲染器首张真实卡，改写型第二次零新型复制）

**日期**：2026-09-05

**触发问题**：目标「前端全部由服务单元组成」要求面板逐块迁移。#2 候选：settings
（需要动态 fields——超出 v2 静态声明契约，属契约演进）、dynamicPlugins（列表，与 #1
形态重复）、**运行时状态卡**（点亮 status 渲染器的真实使用——C4 至今该档只有纯函数测，
无一张真实卡）。

**考虑过的选项与裁定**：
- **选状态卡**：只读、零新宿主后端（`loader`+`dynamicPlugins` 投影现成）、失败面简单、
  且把 §4.1 status 档端到端落地。settings 的「数据驱动 fields」需要新契约（fail-loud
  拒绝未定义形态优于偷偷兼容），留待独立契约决策。
- **跨服务聚合在单元内**：一个 dataRpc 端点内部聚合两个宿主投影——证明
  「数据面 = 单元逻辑」的表达力（宿主零改动）；**任一服务失败整体 fail-loud，
  不部分伪造**（缺一条腿的状态卡比诚实报错危险）。
- **tone 归单元**：`tone: ok/idle/warn` 是渲染契约字段，取值判断（disabled>0→warn）
  由单元自持——双权威禁令（渲染器不解释语义）。
- **进度台账**：`.spec/service-assembly-ui-panels/progress.md` 立账（2/N），
  含远景判定：聊天面板依赖 `chat` 契约预留渲染器点亮，届时先走契约流程。

**验收实测（2026-09-05）**：m34 先红（包不存在，5 FAILED）→ **5/5**（describeUI 契约 /
一份契约 / 聚合与 tone / 部分失败 fail-loud 无 items / 未知端点）；
`scan_mounted_units_appear_in_manifest` 扩第三卡断言（宿主清单层零改动自动上桌布）。
dsh-cli **251/0**、dsh-wasmrt 全绿（m32/m33/m34 齐）、clippy **0**。

**预期影响与回滚点**：新包目录 + m34 + 清单一行断言；撤包目录即回到 `62b7802`。

## D-188（面板改写 #3：panel-dynamic-plugins 动态插件清单卡——list 行投影 running/defined，写动作卡在契约前止步）

**日期**：2026-09-05

**触发问题**：面板迁移继续（#2 后 3/N 候选：动态插件 / 会话 / 工作区文件）。动态插件
面板数据面现成（`dynamicPlugins` 投影），但与 #1 同为 list 形态——是否值得再做一遍？

**考虑过的选项与裁定**：
- **选动态插件而非会话**：会话投影（`sessionMessages`）payload 需要 sessionId——卡级
  「选会话再展开」的交互形态超出 v1 单卡契约（要么多卡要么卡内选择器），属渲染契约
  演进；动态插件零 payload 即可行，且语义与 #1 互补（#1=静态装配了什么，#3=动态
  define/在跑什么）。
- **写动作（stop/undefine）明确不做**：宿主 `set` 面现成，但破坏性动作进卡片需要
  「确认」渲染形态先行——**渲染契约未定先不做动作卡**（fail-loud 哲学：不在未定义的
  交互上盖楼）。列为下一个契约演进候选。
- **行语义归单元**：`state = activeRun→running/否则 defined`、`name` 取
  currentPackageId 对应包（回落首包/null）——渲染器零语义（双权威禁令第三次执行）。

**验收实测（2026-09-05）**：m35 先红（包不存在）→ **5/5**；清单联动测试扩第四卡断言
（scan 挂载零宿主改动）。dsh-cli **251/0**、dsh-wasmrt 全绿（m32/m33/m34/m35 齐）、
clippy **0**、node **16/16**。

**预期影响与回滚点**：新包目录 + m35 + 清单断言行；撤包目录即回到 `013a8f1`。

## D-189（桌布 C6：rowActions 渲染 + `confirm` 契约字段——写能力卡闭环，渲染器不是安全边界）

**日期**：2026-09-05

**触发问题**：管理型面板（停/卸/删）是「前端全服务单元化」的必需组成，但 D-188 在
「卡内确认形态未定」前止步。§4.1 的 `rowActions` 形状早已定稿却无行为定义：动作参数
线形状、确认机制、渲染器职责边界都未定。

**考虑过的选项与裁定**：
- **行动作参数**：渲染器挑身份字段发送（如只发 pluginId）vs **整行入 `args.row` 原样透传**。
  选后者——渲染器不发明身份语义、不维护「哪列是身份」的第二套知识；**单元自校验**
  （row.pluginId 非空串，坏 body fail-loud 且**不触达宿主服务**）。核心纪律：
  **渲染器不是安全边界**，宿主服务同样自校验（dynamicStop 对未知 id 诚实报错）。
- **确认机制**：契约强制所有 rowActions 确认（打扰且否认单元判断权）vs 渲染器猜测动作名
  （"stop/delete" 关键词——脆弱且静默）vs **可选字段 `confirm:true`，只认严格 true**。
  选第三：契约提供机制不强加policy；无字段 = 直接执行（向后兼容，既有卡行为逐位不变）。
  v1 确认 = `window.confirm`（阻断式，最小可信形态）。
- **首个写卡选择**：panel-dynamic-plugins stop/undefine（宿主 set 面现成、破坏性最高
  ——从最危险场景验证确认机制）。
- **成功即刷新**：动作改变行数据（running→defined），不刷新 = 说谎；渲染器成功路径
  重放 dataRpc。

**验收实测（2026-09-05）**：m35 新 5 测先红（无端点，4 FAILED + 声明测红）→ **10/10**；
node C6 三测红竞速污染 → **反向桩探针补正**（`{}`/恒 true/禁用 validate 块全被抓红，
还原后 19/19）；dsh-cli **251/0**、clippy **0**。契约回写 canvas design §4.1 + §13 C6 行。

**预期影响与回滚点**：core.js 两函数 + 校验块 + app.js act()/操作列 + 单元两端点/声明 +
m35 五测；撤提交回 `ffd7e20`。confirm 为新增可选字段——旧声明零影响。

## D-190（面板改写 #4：panel-workspace-files 工作区文件卡——resource 分类首卡，两段式服务探测「不猜目录」纪律）

**日期**：2026-09-05

**触发问题**：面板迁移继续（#3 后候选：工作区文件 / 会话 / settings 动态 fields /
chat）。工作区文件面板对应 D-181 分类表的 `resource` 语义位（fs、凭据、工作区）——
**此前侧栏只有 model/runtime 两组，resource 分类还没有卡**；数据面两段现成
（`agentWorkspace` + `workspaceFiles`）。

**考虑过的选项与裁定**：
- **数据面用两段式而非卡级传参**：`workspaceFiles` 服务需要 `cwd`，但 dataRpc 只发 `{}`
  ——给契约加「dataRpc 静态参数」是表面解法（谁保证参数永远新鲜？）；**单元端点自解析**
  （先 `agentWorkspace` 拿默认工作区再列举）把「参数从哪来」的逻辑归单元——与
  panel-runtime-status 的跨服务聚合同型，零契约扩张。
- **「不猜目录」纪律**：`agentWorkspace` 失败/空 cwd → fail-loud 且**不得触达枚举服务**
  （m36 以桩调用记录断言调用序——失败后零枚举）。猜 `"."` 会把「工作区没配置」谎报成
  「当前目录就是工作区」——状态卡与文件卡 alike，缺输入就明说。
- **行投影零加工**：`{path}` 全路径直出——不发明 basename/图标/分组语义（展示加工属
  渲染器演进；双权威禁令）。空态文案「工作区没有文件」与错误态「不可读」**严格分开**。
- **type=resource**：兑现 D-181 分类表语义位（侧栏首个 resource 卡）。

**验收实测（2026-09-05）**：m36 先红（包不存在，FAILED×4+）→ **6/6**（含调用序断言与
失败零枚举断言）；清单联动测试扩第五卡。dsh-cli **251/0**、dsh-wasmrt 全绿
（m32-m36 齐）、clippy **0**。

**预期影响与回滚点**：新包目录 + m36 + 清单断言行；撤包目录即回到 `32e81bc`。

## D-191（面板改写 #5：panel-sessions 会话清单卡——session 分类首卡，发现端先行策略）

**日期**：2026-09-05

**触发问题**：会话面板因「sessionMessages 需 sessionId、卡级选择形态未定」被搁置
（D-188 记录）。但宿主 `sessionCandidates` 投影（sessionReferenceResolver 同源）
零 payload 现成——会话面板的**发现端**可先行落地，欠账只挡「打开/切换」。

**考虑过的选项与裁定**：
- **发现端先行**：列举卡（只读）与交互卡（打开/切换）解耦——列举零新契约、即刻可用；
  交互形态与 chat 渲染器同题，留同一次契约演进。**先做没有争议的一半，不为等设计
  而整块停摆**。
- **行零加工**：`{sessionId,label,createdAt}` 原样直传，epoch ms 不格式化——单元一旦
  格式化时间就把「展示权威」偷了过来（双权威禁令第五次执行）；label=id 是宿主投影
  事实，不改写、不推断标题。
- **type=session**：兑现 D-181 分类表最后一个已证语义位（侧栏四分类自此齐）。

**验收实测（2026-09-05）**：m37 先红（包不存在）→ **5/5**（含 epoch 原样与调用计数
断言）；清单联动扩第六卡。dsh-cli **251/0**、dsh-wasmrt 全绿（m32-m37 齐）、clippy **0**。

**预期影响与回滚点**：新包目录 + m37 + 清单断言行；撤包目录即回到 `cdcaf21`。

## D-192（面板改写 #6：panel-settings 设置概览卡——宿主投影器首个受测扩展，「一个视图函数两处用」）

**日期**：2026-09-05

**触发问题**：设置面板是 harness 核心面板，但写端（编辑表单）依赖「动态 fields」契约
演进（async schema，D-187 裁定独立决策）。读端今天可做：宿主 settings 域已完整
（describe_all + redact 在源头），只是 RemoteHost 投影器没接它。

**考虑过的选项与裁定**：
- **投影 vs 转发**：单元直连原生 RPC（需扩 host-services 语义到任意 RPC——面太大）vs
  **投影器加只读 arm `settingsDescribe`**。选后者：投影器就是「单元可见世界」的边界，
  新数据面在边界上显式开窗，每个 arm 可测。
- **杜绝双源漂移**：投影复用 web.rs 的 `namespace_view`（改 `pub(crate)`）——原生 RPC
  与投影**共用一个视图函数**；宿主测试断言形状（ns/applies/revision/value），并做
  **伪造空表探针**（空 namespaces 必红）验证断言非空转。
- **redact 不重做也不解除**：provider 源头已脱敏（secrets[].set 仅存在性），单元不展开
  secrets 路径、不自行脱敏、不解除脱敏——敏感面单一权威。
- **行拍平归单元**：`{ns,field,value}` 概览行是展示投影（单元语义，双权威禁令）；
  字段序不作断言（value 键序非契约，测试按 (ns,field) 查找——红之前修掉的顺序假设）。
- **装配面**：RemoteHost::new 第 4 参 `settings: Option<…>`，None → `no-settings` 诚实
  报错（不伪造空表）。

**验收实测（2026-09-05）**：宿主 2/2（形状一致 + 缺依赖诚实）+ **伪造空表探针被抓红**；
m38 先红（包不存在）→ **5/5**（拍平含非对象 value 占位行、失败透传）；清单联动扩第七卡。
dsh-cli **253/0**（+2 宿主测）、dsh-wasmrt 全绿（m32–m38 齐）、clippy **0**。

**预期影响与回滚点**：remote_host 三处 + 单元目录 + m38 + 清单一行断言；
撤本提交即回到 `36fa730`。写端（settings.update 表单）留动态 fields 契约演进。

## D-193（桌布 C8：chat 视图契约定稿——三 RPC 面 + `stream:"session-events"` 闭集；会话协议归宿主，单元只拥有声明）

**日期**：2026-09-05（设计工件；实现切片 C8-1..4 排期）

**触发问题**：「前端全部由服务单元组成」的最后一个主视图 = 聊天。流式事件不能经
wasm 单元（请求/响应模型无订阅原语）；发送长 RPC `session.prompt` 需 `&mut Boot`
而单元回调只拿 `&self`。

**考虑过的选项与裁定**：
- **(A) wasm chat 单元代理全部**（set "sessionPrompt"）：需 Arc/Box<dyn Fn> 反向钩子
  注入 turn-driver——**装配倒挂**（RemoteHost 先于 agent loop 构造），且会话域本是
  宿主概念，「单元优先」不应演变成「一切套 wasm 中转」。否决。
- **(B) 声明单元 + 宿主协议面**：`panel-chat` 单元只拥有声明（describeUI v2 chat 卡），
  三数据面（sessionSource/session·list、historyRpc/session·history 新薄臂、
  sendRpc/session·prompt slash 别名）全指宿主原生臂；流 = 渲染器直订宿主既有 SSE
  （`stream:"session-events"` **闭集单值**，契约不写 SSE 路径——宿主基建细节）。
  三条不变量不动（声明=数据、渲染=浏览器、Rust 不渲染）。选定。
- **卡内会话选择器**：sessionSource list 形状驱动 `<select>`；无源/失败 → 诚实错误态
  （不猜会话）。折叠逻辑归 core 纯函数 `chatFoldFrame`（可 node 钉死，DOM 只做接线）。
- **中断 v1 不做**：宿主中断面未实证——诚实缺省优于假按钮。

**影响**：canvas design §4.1 chat 块标注定稿 + §13 加 C7（面板 ×N 台账行）/C8 行。
**回滚点**：纯设计轮——撤本文档 + 三处标注即回到 `f8a5d68`；实现片 C8-1..4 各自独立可撤。

**实现切片实测 C8-1（2026-09-05）**：core.js chat 校验（三 `[ns,method]` 面 + `stream`
闭集，**形状校验先于渲染器保留档**——声明缺陷优先于渲染器进度）+ `chatFoldFrame`
（EventKind 规范串实证自 dsh-session types.rs：user/message、assistant/message|chunk、
turn/start|end、command/run|done；引用差 = 重绘信号；纯函数零改动入参）+ `chatOptions`。
桩红 5 → **node 26/26**；旧「九行 fail-loud」测试的裸 chat 预留行按新语义迁移
（→ chart/table 两员，齐形 chat 归 C8 专测）；canvas.rs 导出守卫补 chatFoldFrame/
chatOptions（顺手补齐 C6 漏登的 rowActionBody/needsConfirm）。C8-2..4 排期不变。

**实现切片实测 C8-2（2026-09-05）+ 一处设计回正（越级处理纪律的实证）**：原设计
「historyRpc = 新宿主薄臂 session/history（宿主折叠）」——实现后被**两条旧测红暴露**：
`session.history` 面**早已存在**（M 期：`{hasMore, events:[{event:{type,data,time,seq}}],
projections}`），自造臂遮蔽了既有臂。回正：**撤自造臂、复用既有面 + slash 别名**
（`"session.history" | "session/history"`）——旧前端与桌布卡共用同一事实源（杜绝双源
折叠漂移，同 namespace_view 纪律）；历史折叠由渲染器把 events[].event 映射成 core
`chatFoldFrame` 帧（kind=event.type 规范串本就同源）。附带：`session/list` 别名、
`session/prompt` 别名 + 简化线形状 `{sessionId,text}` 臂内映射 content 块、长 RPC
名单同步。dsh-cli **255/0**（+2）、clippy **0**。教训入册：动手前对既有方法名全表面
取证不足（只查了 sessions.list 变体未查 history）——设计文档「新薄臂」据实修正为
「复用既表面 + 别名」。

**实现切片实测 C8-3（2026-09-05）**：chat 渲染器落地（`renderChat`：会话选择器
`chatOptions(session.list.items)` + 历史 `session.history events → 归一{text} →
chatFoldFrame` + 发送乐观气泡失败标注 + 5s 轮询刷新）。**v1 诚实降级一处**：宿主
SSE 帧形状未取证（grep 无 session-event 字面量），`stream:"session-events"` SSE
直订暂缓——轮询用**同一折叠事实源**（无第二权威），取证后接入仅换驱动不改语义；
渲染器输入归一（data.content/嵌入 message.content/blocks→text）放 DOM 层传输适配，
core 折叠契约保持单一 `{text}`。core 档位：RESERVED 摘 chat → IMPLEMENTED（四档）；
C8-1 的「齐形 chat → renderer-unimplemented」测试按 C8-3 语义迁移为直通 null。
node **26/26**、dsh-cli **255/0**、clippy **0**。浏览器端到端手测仍缺（无基建；
DOM 层为已钉死纯函数的接线）。

**实现切片实测 C8-4（2026-09-05，C8 收口）**：`panel-chat` 声明单元落地——
describeUI 返回 v2 chat 声明（三数据面指宿主既表面 session·list/history/prompt），
**零自有数据端点**（`no_proprietary_data_endpoints_fail_loud` 断言 list/send/history/
status 全 fail-loud——单元不伪装能力）；ui.json == describeUI 一份契约；scan 自动
挂载 → 清单第八卡（type session）断言入 `scan_mounted_units_appear_in_manifest`。
m39 先红 → **3/3**。**C8 全链路验收**：`.spec/service-assembly-ui-c8-chat/acceptance.md`
——聊天主视图以服务单元形态运行，与旧前端同一事实源；「替代 deepseek 前端」的最后
一个主视图打通。

**补充切片 C8-3b（2026-09-05）：`stream:"session-events"` SSE 直订接入**。取证闭环
（此前暂缓的原因就此消除）：宿主帧形状 `mux_session_event_frame` = `{type:
"server-request", method:"session/event", payload:{sessionId, event:SessionEvent}}`，
且 **`session/event` 仅 `events.mux` 通道携带**（D-113 实证：host 通道下推即被前端
zod 判 malformed——渲染器只订 `/api/events.mux`）。折叠与轮询**同一事实源**（frameText
归一提到 renderChat 顶层共用 + `chatFoldFrame` 引用差重绘），轮询保留为断线兜底；
hello/keepalive/plan/approval 帧按 method 过滤忽略。诚实台账更新：浏览器端到端手测
仍缺（无基建），但帧映射两侧（宿主 `mux_session_event_frame_shape` 测 / core 折叠测）
均已钉死，渲染器为形状匹配的接线。验证：node 语法门 + dsh-cli **255/0**、clippy **0**。

## D-194（S 系列：设置编辑卡契约定稿——form 档 `fieldsFrom` 动态投影，声明单元 + 宿主既表面复用；实现切片 S1..4 排期）

**日期**：2026-09-05（契约设计轮；无代码变更）

**触发问题**：设置面板写半边。障碍 = v2 form 的 fields 是声明期静态数据，而设置字段是
运行时 schema（宿主注册表）。写协议宿主全现成（settings.update + expectedRevision 乐观锁
+ SETTINGS_CONFLICT），缺的只是渲染侧「schema+value → 表单」投影这一步。

**考虑过的选项与裁定**：
- **契约扩展形态**：新 view.kind "settingsForm"（第二套表单实现——双源漂移否决）vs
  **form 档可选 `fieldsFrom {rpc,pick}`**（fields 渲染时投影，与静态 fields 二选一，
  一个表单实现）。选后者。
- **数据面**：宿主既表面 + slash 别名（describe/update），复用不另造（D-192/193 纪律）。
- **归属**：设置域是宿主域——`panel-settings-edit` 只拥有声明（D-193-B 复制，零自有
  数据端点）。
- **诚实缺省三条**：secrets 仅显示存在性（redact 源头 set-only，写密钥需专门形态）；
  嵌套对象/数组 → 只读行不伪造输入控件；applies=restart 成功文案显式「需重启生效」。
- **并发**：describe.revision → update.expectedRevision；冲突显式呈现 + 引导重读
  （乐观锁语义原样透出，不静默重试）。

**影响**：canvas design §4.1 form 块将回写 fieldsFrom/saveRpc 形；切片 S1(core 纯函数)
/S2(宿主别名)/S3(renderForm 扩展)/S4(声明单元+第九卡) 各自红→绿独立可撤。
**回滚点**：纯设计轮——撤 `.spec/service-assembly-ui-settings-edit/` + 本条即回到
`664051c`。

**实现切片实测 S1..S4（2026-09-05，S 系列收口）**：S1 `schemaFields` + form XOR
fieldsFrom 校验（桩红 3→node 30/30，`f642496`）；S2 `canonical_rpc_method` 入口规范化
（免疫共享臂陷阱，256/0，`37717ca`；锚点侦查 `1cb6534` 先行）；S3 renderForm fieldsFrom
预载 + `{ns,patch,expectedRevision}` 乐观锁保存 + 冲突/重启文案 + checkbox（node 30/30，
`876675c`）；S4 `panel-settings-edit` 声明单元（describeUI 只拥有声明、零自有数据端点、
ui.json 一份契约；清单第九卡断言）。dsh-cli **256/0**、dsh-wasmrt 全绿（m32–m40 齐）、
clippy **0**。诚实注记：S4 未单独跑包不存在红（context 预算裁决；m33–m39 该红型已七次
实证，m40 行为断言全绿即契约钉死）。settings 面板读写两端自此服务单元化。

## D-195（panel-schedule：调度读端薄臂 + 声明单元——Boot 挂载单一权威，E2E 最大缺口划账）

**日期**：2026-09-05

**触发问题**：E2E 清单 §2 指认的最大未迁移项（调度/任务面板）。取证发现：调度只有
**M4 工具面**（schedule_create/list/delete 执行器），**无前端 RPC 臂**；状态权威 =
会话 `schedule/change` 事件日志（ScheduleHost::fold 纯重放）；`schedule.list` 点号面
不存在（遮蔽教训流程先行排除）。

**考虑过的选项与裁定**：
- **读端落点**：投影器 arm（RemoteHost 再扩构造器 + 装配时序纠缠）vs **web.rs 原生薄臂
  `schedule/list`**（handle_rpc_host 有 boot 可达，同 session.list 先例）。选后者——
  单文件、零构造器变更。
- **单一权威**：Boot 加 `schedule: Option<Arc<ScheduleHost>>`，serve 在
  `tick_schedule = Some(bundle.schedule.clone())` 同点挂载**同一 Arc**——不复制 fold
  逻辑、不新造状态源；未启用 agent loop = None → `no-schedule-host` 诚实报错。
- **归属**：声明单元照 panel-chat/panel-settings-edit 定型（零自有数据端点）。
- **写端 v1 不做**（create/delete 需表单参数与确认形态，独立切片）。

**验收实测（2026-09-05）**：宿主 `rpc_schedule_list_honest_shapes`（缺宿主诚实 + create
一条即回读 {id,kind,prompt,scheduledAt}，fold 权威闭环）；m41 3/3；清单第十卡断言。
dsh-cli **257/0**、wasmrt 全绿（m32–m41）、node 30/30、clippy **0**。诚实注记：
本包未单独跑包不存在红（红型已八次实证）；两处编译期小错（Boot 测试字面量缺字段 /
now 应 i64）即时修复。

**预期影响与回滚点**：lib.rs 两行 + web.rs 三处 + 单元目录 + m41 + 清单一行；
撤本提交回到 `5464ab2`。E2E 清单 §2 划账（调度只读子集落地；写端缺口保留标注）。

## D-196（调度写切片 A + wire 审计热修：直读字段的原生臂必须解包画布 `{args}` 信封）

**日期**：2026-09-05

**触发问题**：写切片 A（schedule/create + schedule/delete 薄臂 + 卡 rowActions delete
confirm）实现时做 wire 形状自查，发现系统性隐患：画布 `rpcEnvelope` 一律发
`payload:{args:{…}}`，而 `dispatch` 内**直读字段的原生臂**（settings/update、
session/history、session/prompt）读的是 `payload.get("ns"/"sessionId"/…)`——参数会
**静默丢失**（E2E 未跑故未暴露；wasm remote 路径早已解包、uiManifest 臂自读 args，
两者幸免）。

**裁定**：臂内一行遮蔽解包 `let payload = payload.get("args").unwrap_or(payload);`
×3——旧前端直发形 unwrap_or 原样返回 = **零回归**；否决入口全局解包（uiManifest 等
自读 args 的臂会被双解破坏，回归面大）。新臂（schedule/*）直接按画布形编写并以
`{args}` 形测试钉死。

**验收实测**：宿主 `rpc_schedule_write_roundtrip`（未挂载诚实 → create 画布形 ok →
fold 回读 1 行 → row 形 delete deleted:true → 回读空——**建一删一 fold 权威闭环**）；
m41 补 rowActions(confirm) 断言；dsh-cli **258/0**、m41 3/3、clippy **0**。
教训入册：**跨线形状（envelope）必须与每个消费臂逐一核对**，E2E 前 wire 审计是必要
关口（调度面板自此读+删可用；create 表单卡 panel-schedule-create 与审批交互仍排队）。

## D-197（panel-schedule-create：调度创建表单声明单元——静态 form × 声明单元两型合体，调度写端闭环）
**日期**：2026-09-05

**内容**：第十一个装配单元 = 静态 form 卡（kind select [after/at/every] + prompt +
afterSeconds；保存动作 `schedule/create`）+ 声明单元纪律（零自有数据端点）。保存链
`renderActions {values}` → 画布 `{args:{values}}` 信封 → create 臂（D-196 已按该形
测试钉死）→ ScheduleHost append `schedule/change` 事件 → 列表卡刷新可见——调度面板
**建/看/删** 全部以服务单元形态运行。**回滚点**：撤包目录 + m42 + 清单三行断言。
**验收**：m42 3/3（form 契约 + 一份契约 + 无自有端点）、清单第十一卡断言、全套 0 失败、
clippy **0**。审批交互为最后排队的技术项；E2E + 下线判定仍待用户。

## D-198（审批宿主切片：`approval/pending` 薄臂 + decide 行形兼容 + rowAction `args` 契约扩展）

**日期**：2026-09-05

**触发问题**：技术队列最后一项（panel-approval 种子开工）。取证：`session.approval.decide`
双臂已存在（长 RPC + dispatch，`toolCallId + decision`）；`ApprovalWire` 自持
`pending_requests()`（requested−resolved 由 wire 管理——**无需另折叠**）；wire 未装配
（无 agent loop）为常态。

**考虑过的选项与裁定**：
- **pending 数据面**：从 `frames_since` 日志自行折叠 requested−resolved vs
  **直接用 `wire.pending_requests()`**。选后者——pending 生命周期本就归 wire，
  折叠 = 第二权威（否决）。
- **decide 的 per-动作参数**（同一 rpc 的 允许/拒绝 两个动作）：rowActions 线形状
  v1 只有 `{row}`，装不下 `decision` → **C6 契约扩展：`rowActionBody(row, action)`
  合并 `action.args` 入顶层**（`{row, decision}`）——渲染器仍零语义（args 是声明里
  的字面量），decide 臂加 `row.toolCallId` 回退 + D-196 遮蔽解包（该臂正是审计清单
  里的漏网臂）。
- **归属**：decide 是长 RPC 面（&mut Boot）→ 原生臂先例（D-193-B）；卡片将照
  声明单元定型（第十二卡，下轮 m43）。

**验收实测**：宿主 `rpc_approval_pending_roundtrip`（缺 wire 诚实 → push_requested
可见（callId/toolCallId 双键行形直喂 decide）→ resolve → 清空 + canonical 别名钉死）；
node 31/31（args 合并 + 无 args 逐位不变）；dsh-cli **259/0**、clippy **0**。
诚实注记：rowActionBody 扩展未跑独立桩红（合并逻辑直白 + 双向断言钉死，注记入册）。

**回滚点**：撤本提交（宿主切片）；第十二卡（panel-approval 声明单元 + E2E 划账）下轮。

## D-199（panel-approval：第十二卡落地——技术队列清零，桌面 12 卡全服务单元）

**日期**：2026-09-05

**内容**：待审批清单声明单元（定型第十一次复制，零设计新增）：list 卡 dataRpc
`approval/pending`（D-198 臂）；行动作 允许/拒绝 = 同一 `session.approval.decide` 臂
按 `args.decision` 字面量区分（D-198 rowActionBody 扩展的首个真实消费方），**拒绝
confirm:true**（破坏性决定必确认，C6 纪律）；常量 `allowedOnce/rejected` 已对
approval.rs 源码核实（不猜字面量）。

**验收**：m43 3/3（契约含 args/confirm 断言 + 一份契约 + 零自有端点）、清单第十二卡
断言、dsh-cli **259/0**、clippy **0**。E2E 清单 §2 审批项划账。

**意义**：技术队列（面板 → 服务装配单元迁移）**全部清零**——桌面 12 卡覆盖 harness
全部核心面板域（provider/清单/状态/动态/文件/会话/设置概览/设置编辑/聊天/调度/建调度/
审批）。剩余仅流程步：真实浏览器 E2E（清单 §1 逐项）+ 用户下线拍板（清单 §3 规则）。
**回滚点**：撤包目录 + m43 + 清单四行断言。

## D-200（panel-locale-edit：多 ns 设置编辑「机械复制」兑现首卡 + define/activate 重分级）

**日期**：2026-09-05

**触发问题**：技术队列清零后削 §2 已标注缺口。原计划做 define/activate，**取证后否决**：
`dynamicCordisRunner/*` 是 vendored wasm 组件面（本仓库无源码；runHostHalf 顶层线形
`{pluginId,packageId}` 与 rowActions `{row,…}` 形不兼容，改 vendored 二进制不可行）——
该项**重分级为非机械缺口**（如需激活面须另立原生臂 + 声明单元，独立需求轮）。转做 §2
明示「机械工作」项：设置编辑多 ns。

**裁定**：locale（Live、产品向、独立）为首个复制 ns——`panel-locale-edit` 与
panel-settings-edit **逐字节同构仅换 pick**（schemaFields 投影 + 乐观锁全复用，零新机制
= 「机械」的实证）；其余 ns（llm[Restart]/shell/agent-loop/…）同法待点单。

**验收实测**：m44 3/3（fieldsFrom pick=locale 契约 + 一份契约 + 零自有端点）、清单
第十三卡断言、dsh-cli **259/0**、clippy **0**（新包无独立红跑，红型已九次实证——注记）。

**回滚点**：撤包目录 + m44 + 清单四行断言。

## D-201（fieldsFrom.nsSelect：一张通用设置编辑卡终结「每 ns 一卡」——架构优先于点单复制）

**日期**：2026-09-05

**触发问题**：D-200 后剩余 ns 的「点单式机械复制」暴露问题：每 ns 一卡 → 桌面卡膨胀，
**体验倒退于旧设置面板**（旧面板一屏编辑全部 ns）。复制越勤，架构越糟。

**考虑过的选项与裁定**：
- **每 ns 一卡**（D-200 路线的延长线）：机械、零风险，但 10+ ns = 10+ 卡——否决（省事
  ≠ 正确）。
- **v2 契约演进 `fieldsFrom.nsSelect: true`**：卡自带命名空间下拉（describe 一次拉全表，
  选项=ns 列表；切换即重投影 + meta 换 ns/revision，保存体乐观锁随动）。选此——
  新增 1 个可选声明键、渲染器一个纯函数（`nsSelectModel`）+ paint 循环，
  **pick 保持必填作初始选中**（向后兼容：无 nsSelect 键的卡逐位不变）。
- **升级落点**：panel-settings-edit 原卡升级（title 去「· ui-theme」）——不新增卡。

**验收实测**：node **32/32**（nsSelectModel 红先行：导出缺失→模块失败→实现绿；命中/
回退首项/空数据三态）；m40 3/3（fieldsFrom 含 nsSelect 断言 + 一份契约，wasm 已重建）；
canvas 导出守卫 17 名全列；全套 0 失败、clippy **0**、app.js 语法门 ✓。

**预期影响与回滚点**：core.js/app.js 通用面 + 单元声明两文件 + m40 + 导出守卫。
panel-locale-edit（第十三卡）自此**冗余**——保留可运行（固定 ns 版仍合法），建议 E2E
后合并裁撤（用户拍板项）。撤本提交回到 `53e5cc6`。E2E 清单同步（§1 设置编辑行改
「下拉选 ns」）。剩余 ns 编辑需求**一次性清零**——点单复制不再需要。

## D-202（启用动作：panel-dynamic-plugins 激活端点——「激活面原生臂」被取证证伪，零新后端）

**日期**：2026-09-05

**触发问题**：HANDOFF 优先级 ②「激活面原生臂 + 声明单元（define/activate 正解）」
开工取证发现：D-200 的分级**过于悲观**——`RemoteHost::dynamic_activate(plugin_id,
package_id)` + set 臂 `dynamicActivate`（runHostHalf 同一后端，真实装配 loader）**早已
存在且被 m31/web 测覆盖**；panel-dynamic-plugins 的 stop/undefine 本就经 host.set 走
set 臂。激活 = 同型第三复制，**不需要任何原生臂**。

**裁定**：
- **activate 端点**（单元内）：行自校验（pluginId + packageId 非空才触宿主，纪律同
  row_identity）→ host.set dynamicActivate → 透传（含 pluginRunId）。
- **行携带 packageId**：row_for 注入隐藏列（列不显示；rowAction 整行转发的既有语义
  ——零契约改动）。
- **confirm 分级**：启用=非破坏**无确认**；停止/卸载维持 confirm:true——m35 断言从
  「全 confirm」升级为**按动作分级**（这是契约语义的精化不是放宽）。
- **define（新定义写 cordis.yml）仍缺**：loader.create/update 是宿主内部面，无 RPC 面
  ——维持 §2 缺口（真正的「需设计」项，非本轮范围）。

**验收实测**：m35 **11/11**（声明三动作分级断言 + activate 缺 packageId fail-loud 不触
宿主 + 既有 stop/undefine 全绿）；一份契约（ui.json==describeUI，wasm 重建实证）；
全套 0 失败、clippy **0**。诚实注记：activate 成功路径未单测（set stub 基建只覆盖
fail-loud 型），代码与 stop 同构共享 row_action——注记入册。

**回滚点**：撤本提交回到 `d258e51`。E2E 清单同步（动态插件行 + 启用；§2 只剩 define）。

## D-203（聊天停止：cancelRpc 声明键 + session.cancel 臂遮蔽补漏——取证反转第三连胜）

**日期**：2026-09-05

**触发问题**：cancel-recon 取证（D-202 教训复用）证实 `session.cancel` 宿主面完备
（D-114 真取消、幂等、turn 中并发送达有测），D-193「中断未做」实为渲染器无入口。

**裁定**：
- **契约键 `view.cancelRpc`**（chat 视图可选）：声明即绘制「停止」按钮——渲染器零语义
  （与 sendRpc/historyRpc 同族，无 cancelRpc 的 chat 声明逐位不变）。按钮体 =
  `{sessionId: 当前}`，结果走既有 stat 行；**不删历史**（取消驱动 ≠ 删会话）。
- **D-196 补漏（审计清单第 4 臂）**：`session.cancel` 臂直读 `payload.get("sessionId")`
  → 画布 `{args}` 信封丢参，一行遮蔽解包修复（同 decide/settings/prompt 先例）。

**验收实测**：m39 3/3（cancelRpc 断言 + 一份契约，wasm 重建实证）；app.js 语法门 ✓；
全套 0 失败（遮蔽编辑零回归）、clippy **0**。诚实注记：画布形停止的**行为级**测试未加
（agent-loop mock 基建重；臂的 cancel 语义本身有 D-114/11153 测覆盖，遮蔽解包为
第四次同型模式——注记入册，E2E §1 聊天行含停止烟测兜底）。

**回滚点**：撤本提交回到 `2c4b5c0`。E2E 清单 §1 聊天行 + §2 划账（附件/审批线维持边界）。
聊天面板自此 = 选择/历史/发送/**停止** 四动作全单元形态。

## D-204（write-only 秘密字段：secrets 可设不可读——顺带封掉 D-194 遗留的意外清密闸）

**日期**：2026-09-05

**触发问题**：secrets-recon 取证（反转第四连击）证实 redact 仅读侧 wire 剥离、update
写路无闸。且读现场发现**既有事故隐患**：secret 字符串字段今天以普通空文本框渲染，
用户点保存即把 `""` 发进 patch → **覆盖已存秘密**（D-194 的「仅存在性」并未真正挡住）。

**考虑过的选项与裁定**：
- **字段直接缺席（D-194 原意的彻底版）**：安全但失去设密能力，倒退于旧前端——否决。
- **write-only 字段（采纳）**：schemaFields 把顶层 secret（path 单段）投影为
  `{type:"text", secretWriteOnly:true, value:"", exists:set}`；fieldInput →
  password 框（占位「改写（留空=不改动）/首次设置」）；**collectValues 防误清闸**：
  secretWriteOnly 且值空 → 从 patch 剔除（双向 node 测钉死）。永不明文回显 =
  describe 源剥（既有）+ 值恒空起步 + 空值不出网 三道保险。
- **嵌套 secret（path 多点）**：不提升为控件，容器维持 v1 只读（边界不变）。

**验收实测**：红先行干净（仅 2 新测红于缺行为断言，其余 32 绿）→ 绿 **34/34**；
app.js 语法门 ✓；全套 259/0、clippy **0**。

**预期影响与回滚点**：core.js 两函数 + app.js 两处（fieldsFrom 卡自动获益，声明零改动）。
E2E §1 设置编辑行加秘密写烟测（设→list 显示已设→保存留空不变）。撤本提交回到 `4d5057d`。E2E 清单已同步（13 卡基线 + §1 行）。














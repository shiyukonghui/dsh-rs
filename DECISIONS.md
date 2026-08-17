# dsh-rs 决策日志（Decision Log）

> 每一条记录对应一个关键决策点，使「为什么代码长这样」可追溯。改动 → git 提交 →
> 本日志 三者可互查（提交信息引用决策编号）。完整方案依据见
> `PLAN-rust-cordis-equivalent-migration.md` 与 `HANDOFF.md`。

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

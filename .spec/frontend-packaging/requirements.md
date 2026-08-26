# D-115-Web：前端启动修复 需求分析·证据与缺失服务清单

日期：Rust real-model 测试同期。状态：需求分析进行中（证据已收集，待设计确认）。

## 一、阶段目标（第一性原理）

让 Rust `dsh web` 服务把 vendored `deepseek-harness` 编译生成的前端（未来随 Rust 打包）
以**浏览器端完整可用**的状态跑起来：页面正常渲染、浏览器 37 个 client entry 全部激活、
remote/Host 面可调用（含真实模型会话）。抽象目标 = `dsh web` 的浏览器前端与 Rust host
之间的契约对齐。

## 二、已确立的事实链（自下而上：源码 + 报文证据）

### 1. 前端 boot 协议（vendored HEAD 0.1.1-rc.2）是「queue 门面」
- `packages/client/modules/src/index.ts`（L243-269）生成 index 注入行：queue 门面
  `window.__ModuleLoader__={mode:"queue", load, create}` + parser preload +
  graph global `window.__DSH_BOOT__`。
- `packages/client/web/src/boot.ts`（AppWebEntry.run）：读 `win.__ModuleLoader__` →
  `moduleLoader.create({boot, staticModules, ...})` → 建 cordis Context →
  `runPluginBoot` → `loader.create({name})` 逐 entry → `await loader.await()` →
  `assertEntriesActive(ctx)`（L138-157）抛「N entries did not activate」。
- `assertEntriesActive` 的判定：entry pending 时取 `entry.fiber.inject` 里
  `ctx.get(service) === undefined` 的收集 → 这就是 37-pending 的 waiting 列表来源。

### 2. Rust serve 注入的 boot 与前端协议**已匹配**（阶段1修复完成）
- `crates/dsh-cli/src/web.rs` `render_index_with_boot`（L1200-1266）注入的 queue 门面与
  vendored 源码逐字一致（比对通过）；`__DSH_BOOT__` entries 含 url/rev/inject/immediately。
- 验证：60883（web-root = vendored `dsh-web-frontend/dist`）页面已过
  `__ModuleLoader__` 阶段（不再报 double boot），进入 cordis loader 阶段，报 37-pending。
- 安装版 dist（`D:\Program Files\...`，08-14）是**新协议 N3**（自装 loader 且拒绝已存在），
  与 Rust/vendored 不匹配 → 弃用；随 Rust 打包 = vendored 编译产物。

### 3. 根因：roster 缺 base 层两个关键 client entry
- Rust `build_boot_manifest`（web.rs L1090-1164）从 **plugin_root**（= web-root 向上
  找到的 `@deepseek-ai` 目录）扫 `dsh.client.platform==="web"` 且 `lib/client.js` 存在。
- web-root 指向 `packages/bundle/web-app/node_modules/@deepseek-ai/dsh-web-frontend/dist`
  （symlink → apps/web）→ plugin_root = web-app 的 `@deepseek-ai`（72 包，web 层）。
- **base 层**（`packages/bundle/base`）的 `@deepseek-ai`（79 包）里有
  `@deepseek-ai/dsh-typert-registry`（client: `new TypertRegistry(ctx)` → 服务 `typert`）
  与 `@deepseek-ai/dsh-api-gateway`（client: `ClientRemoteService` → 服务 `remote`；
  `inject=['typert','connection']`）——二者**不在** web-app 的 `@deepseek-ai`（pnpm 隔离）→
  Rust 扫不到 → 这两个 client.js 未下发（404 实测）。
- base patch（`packages/bundle/base/cordis.patch.yml` L30-37）确认
  `typert`(←dsh-typert-registry) / `typert-gateway`(←dsh-api-gateway) 是 **base 层组合行**，
  必须在浏览器端激活。

### 4. 连锁：`typert`+`remote` 缺失 → runtime 挂 → slots/sessions/… 全缺 → 37 pending
- `dsh-client-runtime` client 半 `inject=['connection','typert','remote','remote.commands']`
  （packages/client/runtime/src/client/index.ts L183）。
- runtime **提供**：`slots`（client/slots.ts:105）、`sessions`（sessions/service.ts:348）、
  `workspaces`（workspaces/service.ts:74）、`conversationEvents/Views`
  （conversation/event-registry.ts / view-registry.ts）。
- 故 runtime 无法激活 → 这 5 个服务缺失 → 报错 37 里等这些服务的全部 pending。
- 其余待补：`locale`（dsh-client-locale）、`theme`（ui-theme）、`layout`（ui-layout）、
  `inputTriggers`（ui-input-trigger）、`commandUi`（ui-commands）、`settingsScope`/
  `settingsSchema`（ui-settings）、`remote.*`（api-remotes → 挂 7 个 host remote
  contribution，需 remote 先存在 + host /api 响应）。

### 5. Host 面远端（前端 remote.* → Rust /api）
- `remote` 服务（api-gateway ClientRemoteService）的调用经
  `connection.rpc.call('/api', endpoint, {args})`（gateway client L408）发到 host。
- Rust `dispatch`（web.rs L3215）已实现大量方法：session.prompt/create/history/cancel、
  agent.run、workspace.*、commands/execute、settings.*、goal.create/edit/clear、
  subagent.*、llm.providers/models、session.plan.mode、session.approval.decide …
  ——这是 Rust host 面现有覆盖，需与前端 remote endpoint 全集一一对齐（映射待 981500ea 报告）。

## 三、缺失服务集合（权威）：37-pending 的 waiting 并集（22 个）

`slots, theme, layout, sessions, workspaces, conversationEvents, conversationViews,
locale, settingsScope, settingsSchema, inputTriggers, commandUi, typert, remote,
remote.commands, remote.goals, remote.messageFeedback, remote.fileReferences,
remote.sessionReferenceResolver, remote.pluginInventory, remote.dynamicCordisRunner,
dynamicCordisRunner`

全部由**浏览器 client bundle 提供**（无一 host 直接 provide 到浏览器）。分类：
- ① 纯浏览器逻辑 bundle 就地提供：typert / slots / sessions / workspaces / locale /
  theme / layout / inputTriggers / commandUi / settingsScope / settingsSchema /
  conversationEvents / conversationViews / dynamicCordisRunner（其中 typert 是基座，
  runtime 依赖的 typert/remote 就位后其余自足）。
- ② remote + remote.* 服务**对象**由 api-gateway + api-remotes bundle 提供，但其
  **/api RPC 背靠**（7 个 host 服务面：commands/goals/messageFeedback/fileReferences/
  sessionReferenceResolver/pluginInventory/dynamicCordisRunner）**必须由 Rust host 实现/桥接**。
- ③ 纯 host 侧（webServer / typertGateway / connection host 半）不进浏览器，无同名服务。

## 四、关键偏差（用户方向确认后的待办）

用户确认：「Rust 侧实现 host 面 remote 端点 + wasm 插件承载」。
即除「补 roster（base 层 2 个 client entry）让浏览器自足激活」外，Rust host 还需：
- 把 Rust `dispatch` 现有方法面按前端 remote endpoint 的 wire 信封/命名对齐（待映射）；
- 对前端期望但 Rust 尚缺的 remote 命名空间（如 remote.messageFeedback /
  remote.pluginInventory / remote.dynamicCordisRunner 等）在 Rust host 面实现，
  并以 Rust 版 cordis + wasm 插件（dsh-wasmrt / dsh-loader）承载 host 侧服务。

## 五、待确认（复盘追问）

1. 「Rust 版 cordis」定位：是指复用现有 `dsh-loader`+`dsh-wasmrt` 作为 serv插件宿主、
   把 host 面 remote 端点做成 wasm 插件注册进 Rust 的组合层，还是仅要求 host 面
   实现为 Rust 函数、wasm 只是可选项？（默认：两者兼顾——wasm 插件承载 host 服务，
   现有 Rust `dispatch` 作为整合面。）
2. 修复上线形态：是继续用 `dsh web ... --web-root` 指向 vendored dist，还是
   Rust 端默认装配时需要把 base 层并入 scaffold？（默认：把 base+web 两层的
   client bundle 统一进 Rust 的 plugin_root 扫描，根因修法。）
3. 是否需要把「前端期待但 Rust 缺」的 remote 命名空间全部实现才能验收「页面渲染 +
   会话可用」，还是先保证核心（会话/聊天/模型选择/settings）？

## 六、验收标准（草案，待设计细化）

1. 浏览器加载 Rust serve 的 `/` → 37 个 client entry 全部 active（不再 37-pending）。
2. sidebar/会话/chat 正常渲染（playwright msedge headless 锚点断言）。
3. 真实模型 `session.prompt` 全链路（浏览器 prompt → Rust host → llm → 事件回流）可用。
4. `cargo test --workspace` 全绿 + clippy 0；Rust 新增 host 面测试覆盖对齐的端点。

## 七、前端期望的 remote 端点全集 vs Rust host 现状（权威，来自生成描述符）

前端 remote 端点的**权威来源** = 每个 contribution 包 `lib/typert.remote-client.js`（由
`@deepseek-ai/dsh-typert-generator` 从 Host FaceModel 生成，勿手改）。7 个命名空间的端点：

| 命名空间 | 端点 | 来源包 | Rust dispatch 现状 |
|---|---|---|---|
| commands | `commands/execute`, `commands/list` | interaction/commands | ✅ 已实现（execute 解析 payload.args） |
| goals | `goals/create\|edit\|clear\|complete\|pause\|resume` | goal/goal | ✅ 已实现（goal.create/edit/clear/pause/resume/complete） |
| messageFeedback | `messageFeedback/put\|list\|delete` | feedback/message-feedback | ❌ 缺 |
| fileReferences | `fileReferences/list` | context/file-reference | ❌ 缺 |
| sessionReferenceResolver | `sessionReferenceResolver/candidates` | context/session-reference | ❌ 缺 |
| pluginInventory | `pluginInventory/list` | host/plugin-inventory | ❌ 缺（有 dynamicCordisRunner/inventory 但非此端点） |
| dynamicCordisRunner | `dynamicCordisRunner/{getClientCode,inventory,invoke,reportClientGuardFailure,reportRenderFailure,resolveInspectQuery,resolveRequestRun,runHostHalf,settleUserRun,stopFromPanel,syncInspectManifest,undefineFromPanel}` (12) | extensions/cordis-host-runner | ⚠️ 部分（inventory/syncInspectManifest 有占位，其余缺） |

Wire 信封（已确认同构）：
- 前端 `createWebConnectionRpc.call(channel, endpoint, payload)` 发
  `POST /<channel>/<endpoint>`，body = `ClientRequest {type:'client-request', rpcId,
  method:<endpoint>, payload}`（packages/client/connection/src/client/rpc.ts）。
- 响应解析 `serverResponseSchema`——与 Rust `handle_rpc_host` 的
  `{type:'server-response', rpcId, result}` + `rpc_response` 信封**逐字段同构**。
- 结论：Rust `dispatch(method, payload)` 的方法名 = 前端 endpoint 字符串
  （`namespace/method`），wire 无需改造；仅需补齐缺的方法 + 参数/返回形状对齐。

「需要 Rust host 实现」的总量（供设计估算）：
- messageFeedback 3 + fileReferences 1 + sessionReferenceResolver 1 + pluginInventory 1
  + dynamicCordisRunner 剩 ~10 = 约 16 个新端点（多数可薄实现：pluginInventory 空表、
  messageFeedback 内存 feed、fileReferences/sessionReference 空列表、dynamicCordisRunner
  占位）——与 Rust 已有 `dynamicCordisRunner` 占位先例一致。

## 八、需求复盘（方法论）

**隐含假设（用户未明说，默认成立）**：
1. 前端 dist 必须永远来自 vendored `deepseek-harness` 编译产物（随 Rust 打包），
   不接受安装版 dist——用户已明确。
2. Rust host 的目标是「浏览器能用」，不是「前端离线/静态完整」——remote 端点要可调用。
3. 37-pending 重启无效是因为根因是「roster 缺 base 层 2 个 client entry」，非进程状态。

**缺失关键信息（可能改变方案）**：
1. 「Rust 版 cordis + wasm 插件承载 host 面」的确切形态：是复用现有 `dsh-loader`+
   `dsh-wasmrt` 把 host 面端点做成 wasm 插件注册进 Rust 组合层，还是仅要求「Rust 函数
   实现 + wasm 可选」？（默认：host 端点实现为 Rust 函数，wasm 插件承载作为可选分层。）
2. 验收的最小边界：是「会话/聊天/模型选择可用」就够，还是 settings/goals/subagents 等
   二级 remote 都要可用？（默认：核心会话可用为 P0，其余按现有 Rust 覆盖自然对齐。）
3. base 层两个 client entry（typert-registry + api-gateway）的下发方式：是扩展 Rust
   `build_boot_manifest` 支持多 plugin_root（base+web 两层），还是把 base 层 bundle
   合并/链接进一个统一的 plugin_root？（默认：前者——Rust 端加 base 层扫描。）

**处理此类问题的最常见错误**：
1. 用「安装版 dist」盲试（协议错配 double boot）——已排除。
2. 只看 37-pending 表面（以为是 UI 缺服务），不追到 root cause（base 层 roster 缺
   typert/remote 两个 entry）——已钉死。
3. 以为要多实现 host 端点才能渲染——实际上**渲染只需 roster 补齐**（浏览器自足），
   host 端点是让 remote.* 调用有后端（交互可用），是两层独立问题。

## 九、待用户确认的设计决策（进入系统设计前）

- D1：Rust `build_boot_manifest` 改造 = 支持多 plugin_root（base + web 两层）还是
  单一合并 root？（推荐：多 root——分层清晰、无需复制 node_modules。）
- D2：缺失的 ~16 个 remote 端点：全部 Rust 实现（空表/占位对齐 schema）还是仅 P0
  （核心会话相关）？（推荐：全实现但薄——空表/占位 + schema 对齐，成本低。）
- D3：host 面承载形态：普通 Rust 函数 dispatch（现有模式）还是 wasm 插件承载
  （dsh-wasmrt）？（用户倾向 wasm 插件承载，需确认具体分层。）

## 十、需求决定（用户已确认，2026-08-26）

- **D1 = 支持多 plugin_root**（base + web 两层）：Rust `build_boot_manifest` 从
  base 层 + web-app 层两个 `@deepseek-ai` 目录扫描 `dsh.client.platform==="web"`，
  合并 entries（base 层提供 typert-registry + api-gateway，web 层提供其余 11 个）。
  分层清晰、不复制 node_modules。
- **D2 = 全实现**：缺失的 messageFeedback(3) / fileReferences(1) /
  sessionReferenceResolver(1) / pluginInventory(1) / dynamicCordisRunner 剩余(~10)
  全部 Rust 实现（薄——空表/占位 + schema 对齐，与 `dynamicCordisRunner/inventory`
  返回 `[]` 先例一致）。

> ⚠️ **需求修订（2026-08-26，用户否决薄实现）**：D2 改为「**全真实实现**」——每个
> 缺失端点必须提供真实功能语义 + 真实数据源（in Rust 侧真实状态：loader entries /
> session store / settings / credentials / goal / workspace 等），返回结构严格对齐
> 生成描述符 zod schema，**禁止空表/占位/假数据**。权威语义来自 TS host 实现（逐端点
> 还原 FaceModel 真实现）。
- **D3 = 直接用 dsh-wasmrt wasm 插件承载**：host 面（含补齐的端点 + 既有 remote
  端点）承载在 Rust wasm 插件（`dsh-wasmrt` 加载的 WASM 插件）上，而非普通 dispatch
  函数。这是用户明确方向（rust 版 cordis + wasm 插件提供服务），设计阶段细化。

**「物化 vs 可用」两层验收**：
- 物化（P0）：下发 13 个最小 client bundle（base 层 2 个 + web 层 11 个）→ 浏览器
  37 entry 全 active → 页面渲染。
- 可用（P1）：Rust host 实现 `/api` Typert RPC 面（remote.* 7 命名空间全端点）+ 
  connection.api 通道 + 下行 SSE/WS + dynamicCordisRunner host 半 → 交互可用。

## 十一、设计阶段的输入（后续细化用）

1. 7 个 remote 命名空间生成描述符（`lib/typert.remote-client.js`，勿手改代码，
   只读其 wire 形状）：commands / goals / messageFeedback / fileReferences /
   sessionReferenceResolver / pluginInventory / dynamicCordisRunner。每个描述符含
   namespace/method/parameters(wire+codec+source)/result 的 zod schema → Rust 侧
   用 serde_json 等价实现。
2. dsh-wasmrt 插件 ABI：core-module C ABI + component model → 设计阶段研读
   `crates/dsh-wasmrt` 的 Plugin trait 与 loader，确定 host 面插件怎么注册 remote
   端点。
3. Rust `dispatch` 现有方法面（34+ 个）作为「既有 host 能力」基线，wasm 插件化后
   保持 wire 兼容。

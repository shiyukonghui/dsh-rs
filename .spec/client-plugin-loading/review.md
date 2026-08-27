# 调研结论：真实 deepseek-harness 前端插件加载机制（全链路 + Rust 对照）

日期：2026-08-27
任务：源码调研（`.spec/client-plugin-loading/requirements.md` v2 正式，用户确认交付物）。
范围：只理解 + 留档 + Rust 对照，**不写实现代码**。
权威来源：`deepseek-harness/.agents/notes/implemented/architecture/`
- `2026-07-23-client-plugin-loading-model.md`（加载模型，本文骨架）
- `2026-07-19-gui-web-client-architecture.md`（浏览器端 cordis 树/槽位/对象层）
- `2026-08-15-client-shells-and-dynamic-packages.md`（静态/动态包集，见 AGENTS.md 引用）
源码：`deepseek-harness/packages/client/{modules,web,hmr,connection,runtime,ui-renderer}/*` +
`cordis/packages/loader/*`（vendored Loader）。

## 1. 一句话

**浏览器端跑第二个 cordis 插件树**：每个前端 UI 能力 = 一个插件包（`package.json` 声明
`dsh.client` + 独立构建的 `lib/client.js` 工厂闭包）；宿主把已挂载条目组装成 `__DSH_BOOT__` 图
下发；浏览器用 `dsh-client-modules` 的**懒 CJS 模块表**填充 vendored Loader 的 `internal` 缝
（替代 Node 模块系统），由同一个 Loader 统一治理插件生命周期，最终经 `ui-renderer.mount()` 一次
成型。

## 2. 模块系统 vs 插件治理（核心分工）

加载模型笔记（`2026-07-23-client-plugin-loading-model.md`）：
> module system owns module identity and bytes — how code arrives, registers, and becomes an exports;
> the Loader owns plugin lifecycle — when a plugin mounts, what it waits for, and how it is torn down.

- 模块系统 = `dsh-client-modules`（双面包：node 半组装图 / browser 半模块表）。
- 治理 = vendored `@cordisjs/plugin-loader`（与宿主同版同 Byte，vendor 策略）。
- 唯一接缝 = `Loader.internal`，唯一调用点 = `tree.import`
  （`cordis/packages/loader/src/config/tree.ts:103-120`；浏览器化 `ModuleLoader.fromInternal()` 返回
  undefined = 空缝由 shell 填）。

## 3. 阶段 A — 宿主组装图（node 半）

`packages/client/modules/src/index.ts`（`ClientModuleRegistry extends Service`, `inject:
['webServer','loader']`）：

1. **条目即卷宗**（组合决策，非扫描）：`apps/cli` 的 cordis.yml 按行列出前端插件（含恒挂
   `client-hmr`）；加载链只扫「树实际挂载的条目」——包声明 `dsh.client` ≠ 本部署挂载。
2. **增量扫描**：`ctx.on('internal/plugin')` 把 fiber 的 entry name 标脏 → 微任务 flush
   （`flush()`）；激活时同步 seed 当前条目 → 首扫与稳态同一实现。每名包元数据（含「非 client 包」
   负判）缓存不过期（插件集变化需重启）。
3. **图组装**：`parseDshClient`（platform/inject/immediately/external 逐字段校验）→
   `clientExportOf`（`exports["./client"]`，缺 bundle 则 `MissingClientBundleError` 组报）→
   `graphRow`（`{id, url:"/plugins/<id>/client.js?rev=", rev, inject?, immediately?, external?}`）
   → `orderByModuleGraph`（被请求的动态行排到消费方前，**拒绝同步环**）→ `shortHash` = sha1 前 12 位
   （rev = 缓存破 + HMR diff 锚）；整行集 hash 成 `graph.rev`。
4. **路由 + 注入**：注册 webServer `prefix /plugins` 路由 → `serveBundle`（`client.js` + `.map`，
   `no-cache`）；`bootInjections(graph)` 注入 index 三件：① queue 模式 `__ModuleLoader__` 门面
   （`load` 只 push 进 pendingQueue；`create` 取 modules 注册物化并切 live）；② 阻塞 script 预载
   modules/runtime 两个普通 `lib/client.js`（`@deepseek-ai/dsh-client-modules` /
   `@deepseek-ai/dsh-client-runtime`）；③ `__DSH_BOOT__` 全局。
5. **HMR 换入口**：`rebuilt(id)` = 重读 bundle 内容重算 rev → 变化则重组装 + 通知
   `onRebuilt`/`onGraphChanged`（`clientModuleHost` 服务）——**bundle 内容到达图的唯一入口**。

## 4. 阶段 B — 浏览器：模块面（face）+ 插件面（governor）

### 注入的 HTML
queue 门面 --> `<script src="/plugins/.../client.js?rev=…">`（modules+runtime 阻塞预载）-->
`globalThis["__DSH_BOOT__"]` 图 --> Vite shell。

### 内核 `packages/client/web/src/boot.ts`（`AppWebEntry.run`）
1. `moduleLoader.create({ boot: __DSH_BOOT__, staticModules, loadBundle? })` → 构造模块系统；
   bootstrap 模块（modules 的 exports）预置进 loadCache；**门面从 queue 切 live** 并排空 pending；
2. `prefetchImmediateTier()`：并行 prefetch `.immediately` 行（仅提前到达，失败吞掉——真失败由
   第二阶段 import 负责 loud）；
3. `runPluginBoot`：`ctx.plugin(Loader)` → **`loader.internal = this.modules`**（任何 entry 存在
   前装配）→ 对每个行 `loader.create({ name })` → `loader.await()` →
   `assertEntriesActive`（**all-ACTIVE 清扫**：import 失败 / FAILED / PENDING 列名单 + 缺哪个服务，
   整体 fail-loud；无部分可用模式）；
4. `mountApp`：`ctx.inject(['uiRenderer'], scope => scope.uiRenderer.mount(container))`——一次
   挂载；`uiRenderer` 是普通 host 图行，无组装伪条目。

### `ClientModuleSystem`（`packages/client/modules/src/client/system.ts`）——懒 CJS 表
- **执行 bundle 只注册**：`window.__ModuleLoader__.load({ id, factory })`；副作用（含 CSS 注入）全在
  factory 闭包内，**首次 require 时物化**并 memo（`materialize` 带重入环守卫）。
- **到达**（`arrive`/`arriveGraphRow`）：同源外部经典 `<script async src>`（装完即移除节点防 HMR
  堆死节点）；递归先到 `external` 动态依赖；load 后校验注册 id（防未注册的仿制产物）。
- **require 解析分支**（`makeRequire`）：seed 词 → memo 记录 → 已注册 factory 物化 → **loud throw**
  （构建期 purity 门的运行时镜像）。
- **seed 静态表**（`packages/client/web/src/seed.ts`，`platform.ts` 单一事实源）：react /
  react-dom(+client) / cordis / ui-slots / ui-primitives——**仅此** shell 共享；其余裸包要么自带
  `lib/client.js` 行、要么 `dsh.client.external` 请求共享行。跨插件值导入 = 构建错误，协作走
  cordis 服务。
- HMR 三动词：`prefetch(id)`（去重加载）/ `invalidate(id)`（丢非 bootstrap 工厂）/ `import`。

### HMR 浏览器驱动（client/hmr，每帧一个插件、串行）
`invalidate`（丢旧 factory）→ `prefetch`（装新，旧 fiber 仍在服务）→ `registry.delete`（先于触
fiber，**防裸 dispose 触发 Loader 自处置分支永久禁用**）→ drain 旧 fiber disposers → 摘
`<style data-plugin>` → `entry.refresh()`（重 import 物化新 factory，CSS 以稳定 tags 重注）→
`fiber.await()`（loud）。依赖级联零代码：fiber activation epoch 串 provider uid，换 provider
fiber 即经 cordis 重载全部依赖者。粗粒度（React 有状态丢失、无回滚、无渐进渲染）为设计内成本。

## 5. 与我们 Rust harness（`crates/dsh-cli/src/web.rs`）的对照

真实链路（TS）与我们的 Rust 复刻（web.rs）逐点对应：

| 真实 harness（TS） | 我们 Rust（web.rs / hmr_events.rs） | 差距 |
|---|---|---|
| node 半扫 `dsh.client` 条目 → compose `__DSH_BOOT__` | `build_boot_manifest`（扫 `@deepseek-ai/*` 包 `lib/client.js`） | 我们的图行即 bundle；未做「包级 dsh.client 声明→图」的增量 internal/plugin 脏扫（静态全量扫一次） |
| `graphRow` rev=内容 sha1、`graph.rev` | `short_hash`（web.rs:1313）/ `rev` 全同 | 对齐 |
| `<id>/{client.js,.map}` route | `serve_plugin_bundle`（web.rs:1318+，仅 client.js；.map 未实现） | **缺口①**：source-map 路由未复刻（dev 栈/性能剖析到不了 TSX） |
| `bootInjections`（queue 门面 + 预载 + __DSH_BOOT__） | `render_index_with_boot`（web.rs:1338+，同三件） | 对齐（门面/预载/图注入一致） |
| `/plugins/events` SSE（`rebuilt` 帧） | `hmr_events`（`/plugins/events` SSE，同图/重建帧） | 对齐 |
| node 半 stat 轮询 → `clientModuleHost.rebuilt(id)` | 尚无「自身束 watch + rebuilt 触发」宿主链 | **缺口②**：Rust 侧 bundle 重建信号未实现（bundle 变更需重启；web GUI 消费侧待立项） |
| 浏览器内核两阶段（create 模块系统 → loader.internal=modules → 逐行 create → await → all-ACTIVE 清扫 → mount） | Rust 侧无浏览器运行时 | 天然无对应（我们的 GUI 是外部 SPA 消费 `/api` 而非浏览器内 cordis 树）。Rust 职责止于宿主面（图+路由+SSE+注入） |
| `dsh-client-modules/client.js`（浏览器模块表） | 无（浏览器侧由真实前端承担） | 天然无对应 |
| HMR 浏览器驱动（invalidate/prefetch/refresh 串行换 fiber） | 无（宿主只管发 rebuilt 帧） | 天然无对应 |
| 本轮新「插件包文件夹 /plugins/<name>/** 静态挂接」（D2） | web.rs 已实现（`serve_package_asset`） | 独立机制：wasm 包前端资源 vs 客户端插件 bundle |

**结论（为后续铺路）**：Rust 已完整复刻**宿主面**（图组装/路由/注入/SSE）与图增量结构；
相对真实链路的缺口集中在——① `.map` 源图路由、② bundle 变更自身 watch → rebuilt 触发
（现 `hmr_events` 依赖外部写 index/图，缺 `clientModuleHost.rebuilt` 等价物）。浏览器侧（模块表/
装载/mount）非 Rust 职责。若后续立项「web GUI 消费 `/plugins/<name>/**` 前端插件资源」，需在
本对照基础上另定（.spec 新任务）。

## 6. 诚实边界

- 未逐行通读 `client/hmr`、`connection`、`runtime`、`ui-renderer` 全源（加载链外）；HMR 行为以
  加载模型笔记为准 + `system.ts`/`boot.ts` 源码交叉确认。
- §5 缺口为「未复刻」清单（真实侧存在而 Rust 侧缺），非缺陷定性；是否补由后续立项决定。
- 未运行 TS 测试；本调研只读确认。

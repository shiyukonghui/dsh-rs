# 需求结论 v2（正式）：真实 deepseek-harness 前端插件加载机制（源码调研）

日期：2026-08-27
阶段：需求分析（瀑布流阶段 1，调研型任务）——本文档为阶段关卡工件；**已过闸（用户确认）**。
交付物：讲解（对话内，全链路 + 中文）＋ 留档（`.spec/client-plugin-loading/`）＋ Rust 对照；
不写实现代码。

## 1. 任务与目标（第一性原理）

用户：「阅读 deepseek harness 源码，了解它是如何将前端插件加载的？」——纯调研/理解任务。
目标 = 基于**真实源码**（monorepo `deepseek-harness/` + vendor `cordis/`）讲清前端插件从
**宿主组装 → 注入 HTML → 浏览器模块面 → Cordis 插件面 → UI 挂载**的完整链路，并落到关键
源码文件，而非转述。

## 2. 调研结论（核心，自下而上取证 + 自上而下章节化）

### 一句话
浏览器端跑**第二个 cordis 插件树**：每个前端 UI 能力是一个插件包（`dsh.client` 声明 + 独立构建的
`lib/client.js` 工厂），宿主组装成 `__DSH_BOOT__` 图下发；浏览器用 `dsh-client-modules` 的**懒 CJS
模块表**填充 vendored Loader 的 `internal` 缝（替代 Node 模块系统），Loader 统一治理插件生命周期。

### 关键分工（加载模型笔记）
**模块系统（`dsh-client-modules`）管模块身份与字节**——代码如何到达、注册、成为 exports；
**Loader（vendored `@cordisjs/plugin-loader`）管插件生命周期**——何时挂载、等什么、如何拆。
唯一接缝 = `Loader.internal`，唯一调用点是 `tree.import`。

### 阶段 A：宿主组装图（node 半，`packages/client/modules/src/index.ts`）
1. `apps/cli` 的 cordis.yml **行式**列出前端插件条目（含恒挂的 `client-hmr`）——「哪些插件组成
   一次部署」是**组合决策**，由条目决定、**不扫描**仓库（只扫被挂载的树）。
2. `ClientModuleRegistry extends Service`（`inject: ['webServer','loader']`）：
   - `internal/plugin` 事件把 fiber 的 entry name 标脏 → 微任务 flush（增量，无全量重扫）；
   - 读条目包 `package.json` 的 `dsh.client`（`parseDshClient`：platform/inject/immediately/external）
     → 定位 `exports["./client"]` → `graphRow` 组装 `{id, url:"/plugins/<id>/client.js?rev=", rev,
     inject?, immediately?, external?}`；`orderByModuleGraph` 把被请求的动态行排到消费方之前，
     拒绝同步环；`shortHash`=内容 sha1 前 12 位（rev = 缓存破 + HMR diff 锚）
   - 注册 webServer `prefix /plugins` 路由 → 服务 `client.js` + `.map`；
   - `bootInjections` 注入 index：**queue 模式 `__ModuleLoader__` 门面** + 阻塞 script 预载
     modules/runtime 两个 `lib/client.js` + `__DSH_BOOT__` 全局；
   - `rebuilt(id)`（内容 hash 重算）——HMR 换 bundle 内容的唯一入口；`clientModuleHost` 服务。
3. HMR node 半（`client/hmr`）：无 builder 告知，**自 stat 轮询**当前行 bundle（500ms 缺省）→
   `clientModuleHost.rebuilt(id)` → rev 变化 → 在 `GET /plugins/events` SSE 广播 `rebuilt` 帧。

### 阶段 B：浏览器模块面（face）+ 插件面（governor）
- HTML 注入：queue 门面 + `<script async src="/plugins/<id>/client.js?rev=…">` 预载
  modules+runtime → Vite shell 启动。
- 内核 `packages/client/web/src/boot.ts`（`AppWebEntry.run`）：
  1. `moduleLoader.create({boot: __DSH_BOOT__, staticModules})` → 构造 `ClientModuleSystem`
     （bootstrap 模块=modules 的 exports；**门面切 live 注册**；排空 pending queue）；
  2. `prefetchImmediateTier()`：并行 `modules.prefetch(row.id)`（`immediately` 行，仅提前到达，
     失败吞掉，第二阶段 import 再报）；
  3. `runPluginBoot`：`ctx.plugin(Loader)` → **`loader.internal = this.modules`**（在任何 entry
     存在前）→ 对每个行 `loader.create({ name })` → `loader.await()` → `assertEntriesActive`
     （**all-ACTIVE 清扫**：import 失败 / FAILED / PENDING 列名单，fail-loud；无部分可用模式）；
  4. `mountApp`：`ctx.inject(['uiRenderer'], scope => scope.uiRenderer.mount(container))` 一次挂载。
- `ClientModuleSystem`（`client/system.ts`），懒 CJS 表：
  - 执行 bundle **只注册**：bundle 调 `window.__ModuleLoader__.load({ id, factory })`；副作用
    （含 CSS 注入）都在 factory 闭包里，**首次 require 时才物化**并 memo；
  - 到达 = 同源外部经典 `<script src>`（`async`，装完即移除节点）；`arriveGraphRow` 递归先到动态
    依赖；load 后校验注册 id（防仿制产物）；
  - `makeRequire` 解析分支：seed 词 → memo 记录 → 已注册 factory 物化 → **loud throw**
    （构建期纯度门的运行时镜像）；跨插件值导入=构建错误，协作走 cordis 服务；
  - `import`/`prefetch`/`invalidate`（HMR 三动词）；`claimStyles` 标 `<style data-plugin>` 归属。
- seed = `packages/client/web/src/seed.ts` `getStaticModules()`：react/react-dom/cordis/
  ui-slots/ui-primitives（**唯一** shell 共享静态表；`platform.ts` 为单一事实源）。
- HMR 浏览器驱动（client-hmr，每帧一个插件、串行）：`invalidate`（丢旧 factory）→ `prefetch`
  （装新）→ `registry.delete`（防自处置误禁）→ drain 旧 fiber disposers → 摘 `<style>` →
  `entry.refresh()`（重新 import 物化新 factory，CSS 重注）→ `fiber.await()`（loud）。
  依赖级联零代码：fiber activation epoch 串 provider uid，换 provider fiber 即经 cordis 重载依赖者。

### 与我们的 Rust harness 镜像对应（web.rs）
`build_boot_manifest`/`BootManifest`/`serve_plugin_bundle`/`hmr_events`（`/plugins/events` + 
`/plugins/<id>/client.js`）/`render_index_with_boot`（queue 门面 + 预载 + `__DSH_BOOT__`）
即该链路的 Rust 复刻。本轮新加的「文件夹包前端静态挂接 `/plugins/<name>/**`」（D2）是另一机制
（wasm 插件包的 `web/` 资源），与客户端插件 bundle（按 `@deepseek-ai/…` id 服务 `lib/client.js`）
不冲突。

## 3. 复盘追问（已确认，2026-08-27）

| # | 问点 | 结论 |
|---|---|---|
| 1 | 交付物 | 讲解 + 留档 |
| 2 | 深度 | 全链路 |
| 3 | 价值落点 | 附 Rust 对照（为后续 web GUI 消费侧铺路） |
| 4 | 文档归属 | `.spec/client-plugin-loading/` |

## 4. 产出（本任务）

- 对话讲解：见回复（全链路，中文，源码引用）。
- 留档：本文档（requirements，调研范围+结论摘要）+ `review.md`（完整调研结论 + Rust 对照）。
- 不写实现代码；git 提交留档（D-177）。

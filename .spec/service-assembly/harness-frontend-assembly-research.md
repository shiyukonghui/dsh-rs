# 调研报告：DeepSeek Harness 如何把前端组件作为 Cordis 装配单元

（路径均相对 `deepseek-harness\`）

## A. 前端插件的「装配单元」声明格式

- **`package.json` 的 `dsh.client` 字段**：`parseDshClient` 校验 `platform`（必须为字符串，`packages/client/modules/src/index.ts:132-134`）、`inject?: string[]`、`external?: string[]`（模块表请求）、`immediately?: boolean`（阶段一预取标记）——声明接口见 `index.ts:49-63`，解析见 `index.ts:126-146`。**必须满足 `platform === 'web'` 且声明 `exports["./client"]`**，否则该包不算客户端行或直接抛错（`index.ts:447-454`；`clientExportOf` `index.ts:149-159`）。
- **`lib/client.js` 导出形态**：浏览器半是「模块级 `inject` 数组 + `apply(ctx)`」的函数插件。实例：`ui-commands/src/client/index.ts:48`（`export const inject = [...]`）与 `:55-73`（`apply` 内 `ctx.effect`/`ctx.plugin`/`ctx.slots.inject`）；`ui-layout/src/client/index.ts:108,116`；`runtime/src/client/index.ts:183,188`；`connection/src/client/index.ts:54,109`。被考察的插件均为 `inject`+`apply`，**未见导出 `name`/`Config`**（仅此样本，未全量核实）。提供服务的形态是 `ctx.provide`/`ctx.reflect.provide`（见 C）。
- 示例声明：`ui-commands/package.json:32-43`（`dsh.client.inject` 仅含包名，是信息性依赖边）、`ui-conversation/package.json:32-44`。

## B. 装配与激活机制

- **`__DSH_BOOT__` 结构** = `{ rev, entries }`（`modules/src/client/manifest.ts:66-76`）；`WebBootEntry` 字段 `id/url/rev/inject?/immediately?/external?`（`manifest.ts:51-64`），`url` 形如 `/plugins/<id>/client.js?rev=<hash>`（`modules/src/index.ts:167-176`）。
- **后端合成**：`ClientModuleRegistry`（`modules/src/index.ts:282`）启动同步扫描当前 loader entries，仅对**enabled 且有 fiber** 的 entry 收成行（`processOne` `index.ts:482-499` 的 `entry.fiber !== undefined && !entry.disabled`）；`graphRow` 生成 url+12 位 sha1 rev；`orderByModuleGraph` 按 `external` 依赖拓扑排序（`index.ts:188-220`），服务 `/plugins/*` 路由（`index.ts:340-342,529-565`）。
- **bootInjections** 依序注入三段 HTML：`__ModuleLoader__` queue 门面 IIFE → parser-preload 的 modules+runtime 两个 bundle → `globalThis["__DSH_BOOT__"]`（`index.ts:228-273`；wire 顺序在测试中锁定 `tests/node-half.client.spec.ts:106-130`）。
- **启动**：`boot.ts:61-116`——`moduleLoader.create({boot: __DSH_BOOT__, staticModules})` → `new Context()` → `ctx.plugin(Loader)` → `loader.internal = this.modules`；逐 `row` `loader.create({ name })`（`boot.ts:127-131`）→ `loader.await()` → **`assertEntriesActive`**（`boot.ts:138-158`）：任一 entry 非 ACTIVE 整页失败，PENDING 时用 `ctx.get(service)===undefined` 列缺失服务（`boot.ts:149`）。Loader 的导入经 `tree.import → ctx.loader.internal.import`（`vendor/loader/src/config/tree.ts:145-159`）即 `ClientModuleLoader.import`（`manifest.ts:257-292`；`system.ts:189-204`）。

## C. 服务提供 / 注入（provide / inject）

- **与后端同一 vendored Cordis**：`ctx.provide` 写入 `reflect.store`，只在 `get` 的 strict 模式认 ACTIVE fiber 的实现（`vendor/cordis/src/reflect.ts:277-305,237-243`）；`fiber._refresh` 对每个注入服务求 epoch，缺失则保持 `PENDING` 不执行 apply（`vendor/cordis/src/fiber.ts:597-623,576-578,147-150`）；`provide → notify → _checkImpl → _refresh → _reload` 形成「服务就绪即激活」的连锁（`reflect.ts:314-336`）。
- **客户端提供方**：connection（`connection/src/client/index.ts:109,169`）、modules（`client/modules/src/client/index.ts:57-61`）、slots（`SlotRegistry extends Service`，`runtime/src/client/slots.ts:93-107`，经 `runtime/src/client/index.ts:189 ctx.plugin(SlotRegistry)`）、sessions（`sessions/service.ts:348`）、workspaces（`workspaces/service.ts:74`）、locale（`locale/src/client/index.ts:397`）、layout（`ui-layout/src/client/index.ts:119`）、uiRenderer（`ui-renderer/src/client/index.ts:80`）。
- **slots = 可装配可替换的缝**：唯一注册 API `ctx.slots.register({name, children, store, inject, ...}, Component)`（纯核 `ui-slots/src/index.ts:741-785`；Service 层 `runtime/src/client/slots.ts:126`）；未声明的槽注册即抛错（`ui-slots/src/index.ts:789-791`）；`slots.inject(key, cb)` 等声明生命周期，声明者激活后才注册贡献、塌缩即卸载（`runtime/src/client/slots.ts:143-205`）——例：ui-commands 在其 `conversation.input.overlay` 被声明后自注册 popupSelect（`ui-commands/src/client/index.ts:58-72`）。

## D. 组合模型

- **常开 vs 条件**：web-app patch 以 `insert` 固定一批 dsh.client 行（约 40 个 `ui-*`/`client-*`，`bundle/web-app/cordis.patch.yml:153-295`），boot 全量创建且必须全 ACTIVE（`boot.ts:124-134`、`assertEntriesActive`）。行级条件用 `disabled: true` 或 `!!js` 表达式（如 `cordis.patch.yml:22-23,41`；求值在 `vendor/loader/src/config/entry.ts:104-108 disabledOf`）。
- **预取 vs 按需**：`immediately:true` 的 7 包（modules/runtime/connection/locale/ui-renderer/ui-theme/hmr，各自 `package.json:36-42`）经 `prefetchImmediateTier` 第一层提前加载（`web/src/boot.ts:97-110`；`system.ts:112-125` arrive），其余 lazy 按 import 到达。
- **占位 cardinality**：`single | list | keyed | chain`（`ui-slots/src/index.ts:88`）；scope `root | session-maybe | session`（`:91`）；`'root'` 是唯一 built-in 预声明槽（`runtime/src/client/slots.ts:41,698-704`）；single 槽同格 shadow 取低 priority（`ui-slots/src/index.ts:799-824,936-952`）；ui-layout 在 root 里声明 sidebar/conversation/details/shell.overlay 四子槽（`ui-layout/src/client/index.ts:33-84,120-127`）。

## E. 与后端服务插件的统一性

- 同一 `@deepseek-ai/cordis` 与同一 `@deepseek-ai/cordis-plugin-loader`（浏览器端先 `ctx.plugin(Loader)`，`web/src/boot.ts:114`）；entry 模型同构 `name/config/disabled/inject`（`vendor/loader/src/config/entry.ts:9-22`）；配置由 `internal/config` waterfall 在 fiber 激活前插值（`loader/src/index.ts:92-101`，`fiber.ts:641-643`）；**entry 级 `inject` 经 `internal/plugin` 并入 fiber.inject（`loader/src/index.ts:117-123`）**——前后端同一路径。前端仅替换“代码到达”这一层（`internal.import`）。
- 需分清三种边：**cordis 服务 `inject`**（fiber 等待，缺则 PENDING）、**模块图 `external`**（require 同步，缺则当场抛错，`manifest.ts:47-49`）、**`dsh.client.inject`**（仅信息性包名边，见 `packages/client/AGENTS.md` 的对比表与 `manifest.ts:43-46`）。

## F. 关键文件索引

| 关注点 | 文件（盘内源码，标注:行） |
|---|---|
| dsh.client 声明解析 / graphRow / orderByModuleGraph | `packages/client/modules/src/index.ts:49-63,126-176,188-220` |
| ClientModuleRegistry（resolveMeta/processOne/flush/compose） | `packages/client/modules/src/index.ts:282,429-463,482-527,412-415` |
| bootInjections / __DSH_BOOT__ 注入 | `packages/client/modules/src/index.ts:241-273` |
| web boot（create→loader.create→await→assertEntriesActive） | `packages/client/web/src/boot.ts:46-158` |
| ClientModuleSystem（arrive/materialize/import/prefetch） | `packages/client/modules/src/client/system.ts:53,112-212` |
| 模块表种子（react/cordis 等 platform words） | `packages/client/web/src/seed.ts:22-34`、`platform.ts:8-17` |
| cordis provide/get/strict-ACTIVE | `vendor/cordis/src/reflect.ts:233-305,314-336` |
| fiber inject 等待/PENDING→ACTIVE | `vendor/cordis/src/fiber.ts:147-150,576-623,704-710` |
| Loader→internal.import 接缝 | `vendor/loader/src/config/tree.ts:145-159`、`vendor/loader/src/index.ts:117-123` |
| entry 模型 / disabled !!js | `vendor/loader/src/config/entry.ts:9-22,84-108` |
| slots 声明与注册（SlotMap/register/slots.inject） | `packages/client/ui-slots/src/index.ts:88-91,741-789`、`packages/client/runtime/src/client/slots.ts:93-205` |
| runtime apply（slots/sessions/workspaces/events 装配） | `packages/client/runtime/src/client/index.ts:182-233` |
| 真实插件声明+装配 | `packages/client/ui-commands/package.json:32-43`、`ui-commands/src/client/index.ts:48-73`、`ui-layout/src/client/index.ts:108-130`、`ui-conversation/src/client/apply.ts:182-212` |
| web-app 组合（常开 roster / disabled） | `packages/bundle/web-app/cordis.patch.yml:43-295` |
| base bundle（typert/api-gateway 行） | `packages/bundle/base/cordis.patch.yml:30-37` |

## 摘要（5 条）

1. 前端插件与后端插件是同一个 cordis「插件=装配单元」：同一份 vendored `@deepseek-ai/cordis` Context/Fiber/Loader，前端只是把代码到达层换成 `ClientModuleSystem` 并挂到 `loader.internal`，`vendor/loader/src/config/tree.ts:145-159`。
2. 装配单元的声明 = `package.json` 的 `dsh.client`（platform='web'/inject/external/immediately）+ `exports["./client"]`（`lib/client.js` 以 `inject`+`apply` 导出），后端 `ClientModuleRegistry` 把它谱成 `window.__DSH_BOOT__` 的 `{rev, entries:[{id,url,rev,...}]}` 图，`modules/src/index.ts:167-176,429-463`。
3. 激活完全走 cordis 注入：fiber 对每个 `inject` 服务求 epoch、缺服务保持 PENDING，`ctx.provide → notify → _refresh` 自底向上连锁激活，`reflect.ts:314-336`、`fiber.ts:597-623`；boot 的 `assertEntriesActive` 强制所有 entry=ACTIVE，任一 pending 即整页失败并列出缺失服务，`web/src/boot.ts:138-158`。
4. UI 组合的“缝”是 `ctx.slots`：声明式 SlotMap + `register({name,children,store,inject})` 一次调用即贡献组件并声明子槽（single/list/keyed/chain + root/session-maybe/session），`slots.inject` 等声明生命周期实现「可装配可替换」的延迟注册，`ui-slots/src/index.ts:88,741`、`runtime/src/client/slots.ts:143-205`。
5. 「常开 vs 按需」：web-app patch 固定常开 roster（`cordis.patch.yml:153-295`），`!!js`/disabled 做行级条件装配（`entry.ts:104-108`），`immediately:true` 仅用于 7 个第一层预取基础设施包，其余 bundle lazy 到达。

未证实/局限：`ui-*` 客户端插件是否有人导出 `name`/`Config`（样本里没有，未全量核查）；`lib/client.js` 由 tsdown `clientBundle` 产出（仅由 AGENTS 指引、未逐包读 `tsdown.config.ts`）。

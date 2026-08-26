# 前端不渲染诊断记录（`dsh web` + vendored 前端 dist）

日期：Phase 4 完成后、真实模型测试同期。

## 现象（用户报告）

- 60880（默认 web-root = 已安装 DeepSeek Harness）控制台：
  `Uncaught (in promise) Error: client-modules: window.__ModuleLoader__ already installed (double boot?)`
  且前端页面空白。
- 重启相关服务**无效**（根因非进程状态，见下）。

## 根因（第一层）：协议版本错配 → double boot

事实链：

1. Rust `dsh web` 服务端 `render_index_with_boot`（`crates/dsh-cli/src/web.rs` ~L1200）
   在首页注入**旧协议** boot 三件套：
   - queue-mode 门面 `window.__ModuleLoader__ = {mode:"queue", load(registration){...}, create(options){...}}`；
   - parser preload：`/plugins/@deepseek-ai/dsh-client-modules/client.js` 与
     `.../dsh-client-runtime/client.js` 两个经典 `<script>`（用 `__ModuleLoader__.load` 注册 factory）；
   - `window.__DSH_BOOT__` boot graph（entries: rev/url/inject/immediately）。

2. 默认 web-root 指向**已安装软件**的 dist：
   `D:\Program Files\DeepSeek Harness\resources\host\node_modules\@deepseek-ai\dsh-web-frontend\dist`
   （index.html 引用 `/assets/index-aR-dA-Tc.js`，构建于 2026-08-14）。

3. 该安装版 dist 内的 client-modules 已是**新协议**：bundle 内 `N3` 类构造函数
   `if (globalThis.__ModuleLoader__ !== void 0) throw new Error("...already installed (double boot?)")`
   ——它**自己安装** `__ModuleLoader__`，发现门面已存在即判为 double boot 抛错 → 前端空白。

4. 对照：vendored 仓库 `deepseek-harness/packages/client/modules/src/index.ts`（HEAD
   `0.1.1-rc.2`, `b150a551b8`）仍是**旧协议**（queue 门面 + `createClientModuleSystem`），
   与 Rust 服务端注入逐字一致。即：**安装版 dist 是一个比 vendored 源码更未来/独立演化的
   前端**，与 Rust 服务端及 vendored 源码都不匹配。

结论：Rust 服务端注入的逻辑没错（与 vendored 源码对齐），但是
**用了错误的 web-root（安装版 dist）**，两者协议硬拼 → double boot。

## 修复方向（用户约束）：随 Rust 打包 = 用 vendored 构建的前端

用户明确：Rust 后端应使用项目下 `deepseek-harness` 编译生成的前端 dist（将来随 Rust
一起打包），而非安装软件的前端。

考察：
- `deepseek-harness/apps/web/dist`（构建于 2026-08-25）：bundle
  `index-ClqxG24t.js`，**消费** `window.__ModuleLoader__`（`if(i===void 0) throw
  "bootstrap facade is missing"`）——与 Rust 服务端旧协议匹配，无 double boot。
- 聚合发布形态 = `deepseek-harness/packages/bundle/web-app/node_modules/@deepseek-ai/dsh-web-frontend`
  （version `0.1.1-rc.2`；dist 内容与 apps/web/dist 一致，index.html 引用
  `index-ClqxG24t.js`）。其布局 `<node_modules>/@deepseek-ai/dsh-web-frontend/dist`
  恰好匹配 Rust `default_plugin_root`（从 web_root 向上找 `@deepseek-ai`）→ 插件集
  自动解析到该 node_modules（69+ 插件）。

验证（冒烟，60883 用 vendored 聚合 dist）：
- `--web-root ...\node_modules\@deepseek-ai\dsh-web-frontend\dist` 启动成功，HTTP 200；
- 首页注入门面 + `index-ClqxG24t.js` + modules/runtime preload 均正确；
- `/plugins/@deepseek-ai/dsh-client-modules/client.js` 正常下发
  （含 `__ModuleLoader__.load` + `createClientModuleSystem`，协议匹配）；
- boot graph 完整（api-remotes/connection/hmr/locale 等 entries 齐全）。
- **double boot 消除**：页面进入模块系统启动阶段。

## 现在的阻塞（第二层）：37 个 cordis entry 未激活

60883 页面报错：
```
Error: web boot: 37 entries did not activate
@deepseek-ai/dsh-client-ui-layout: pending (waiting for services: slots, theme)
@deepseek-ai/dsh-api-remotes: pending (waiting for service: remote)
...
```
全部 37 个客户端插件 entry pending，等宿主服务：`slots, theme, remote, sessions,
workspaces, typert, inputTriggers, conversationEvents, conversationViews, locale,
settingsScope, settingsSchema, commandUi, ...`

判断：前端已能加载/启动，但 **Rust 服务端注入的 boot graph 只含 client.js 插件
entries，缺少前端期望的宿主侧服务提供者**。在 TS Harness host（scaffold.ts /
`launchWebScaffold`）里这些服务由 cordis 组合 + webserver row + `dsh-web-app` 补丁
装配提供；而 Rust `dsh web` 的服务端组合未提供（或未按前端契约 seed）这些服务。
这是 Rust 服务端与 `@deepseek-ai/dsh-web-app` 前端的**服务契约缺口**，属深层改造
（service seed / host 面），暂停深入（用户指示）。

## 下一步（用户指示记录）

用户：报错后「下次告诉我去验证即可」——即此问题留待指示，不在此次继续深挖。
待办素材：
- 诊断脚本：`deepseek-harness/apps/web/render-smoke.mjs`（playwright msedge headless
  加载指定 URL，等 `[class*="frame"]`/`#root` 锚点 + 收集 console/page errors）。
- 服务现状：60880（安装版 dist，double boot，空白）已重启；60883（vendored 聚合 dist，
  double boot 已消除、卡 37 entries pending）。
- 关于「随 Rust 打包」：应逐步把 Rust 的 web-root/plugin-root 指向 vendored 构建产物
  （或引入构建步骤产出聚合 dist），并在 Rust 服务端补齐前端期望的宿主服务面。

## 关键决策/纪律

- 前端产物来源必须与 Rust 服务端 boot 协议同源（vendored deepseek-harness），
  不得回退到安装软件 dist（协议错配根因）。
- 任何中文文件改动坚持用 `edit`/`write`/`bash`（原生 UTF-8），禁止 PS 5.1 文本重写。

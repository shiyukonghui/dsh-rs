# 需求结论 v2（正式）：服务装配单元插件含前端 UI —— 声明式数据驱动路径（P2）

日期：2026-08-27
阶段：需求分析（瀑布流阶段 1）——本文档为阶段关卡工件；**已过闸（用户确认方向）**。
本文档冻结方向与边界；不进入设计/编码（用户：本轮只确认方向，不写代码）。

## 0. 决策回执（用户确认，2026-08-27）

| # | 问点 | 结论 |
|---|---|---|
| 1 | 路径取向 | **P2 声明式数据驱动**：wasm 插件**声明** JSON 描述的 UI，GUI 壳用**通用渲染器**渲染（对齐 MCP Apps / A2UI / Adaptive Cards 方向）。**否决** P1（JS bundle 镜像标准链）、P3（iframe 独立前端）、P4（浏览器内 wasm 前端） |
| 2 | GUI 边界 | **允许改/自建 GUI 壳**（含通用渲染器/插件发现机制）——P2 得以成立（现有外部 dsh-web-frontend 无通用渲染器） |
| 3 | 首个试点 | **不定试点**（后续立项时再定，把需求做成可验收 demo） |
| 4 | 本轮产出 | **只确认方向**；需求文档转正式，不写实现代码 |

## 1. 目标（第一性原理，重申）

让**服务装配单元（wasm 插件文件夹包）**携带的「前端 UI」能在 DSH Web GUI 中被**加载**（被 GUI
发现 + 拿到 UI 描述）并**渲染**（GUI 壳以通用渲染器画出），且**前端侧无 JS 编程**——
插件作者只声明 UI，渲染归 GUI 壳。这是 Rust/TS 差距之下的务实落点：wasm 进不了浏览器，就把
「UI 描述」作为数据经声明化露出。

## 2. P2 要素梳理（方向细化，进入下一任务设计前的接口面）

- **UI 声明来源**（二选一或并存）：
  a) **静态清单**：插件包 `plugin.json`（或 `web/ui.json`）声明 UI 片段 / 面板 / 卡片；
  b) **动态 RPC**：wasm 组件经现有 host-api/remote 面暴露 `describe-UI`（请求时返回 JSON 描述）。
  → 倾向：静态清单起步（轻），动态 RPC 作增强（有状态/敏感 UI）。
- **声明格式**：对齐「服务端定义 UI」业界形态——**结构化 JSON schema**（元素/布局/绑定/动作），
  参考 Adaptive Cards / JSON UI / MCP Apps A2UI 的字段子集；**不发明**私有复杂 DSL。
- **渲染器宿主**：GUI 壳新增「通用声明渲染器」（一个 slot/组件，输入 JSON 描述 + 数据 → 渲染 +
  绑定 { action → RPC }）；壳允许改（决策 2 授权）。
- **发现/清单位置**：GUI 启动时从宿主读「已装配插件清单」（对齐现有 `pluginInventory` 数据面 /
  未来的 `/plugins/<name>/**` 静态）→ 对每个含 UI 声明的包拉取描述。免重启（HMR refresh 后清单
  更新）。
- **数据与动作**：渲染所需数据走既有 RPC 通道（拉取/`pluginInventory`）；动作按钮 → 宿主 RPC →
  wasm 插件处理器（host-api 回调）。
- **安全/IPC**：渲染器只消费 JSON 描述（无任意 JS 执行 → 天然沙箱）；动作白名单/受宿主校验。

## 3. 网络锚点（方案/技术依据，2026-08 时点）

- 服务端定义 UI 客户端渲染：MCP Apps（[giantswarm mirror](https://github.com/giantswarm/muster/blob/d40decb49019a2721fbcb08df08e9f3d6102c1ed/docs/explanation/mcp-2026-07-28/03-mcp-apps.md)）、A2UI+MCP Apps（[Google](https://developers.googleblog.com/a2ui-and-mcp-apps/)）。
- 声明式 UI 成熟落地：Adaptive Cards / JSON UI（[示例](https://developer.blackbaud.com/skyapi/docs/addins/adaptive-cards/getting-started)）。
- 动态载入独立前端包（context）：微前端 / Module Federation（真实 harness 否决理由 = Vite 缺
  remote bundle；[SO](https://stackoverflow.com/questions/78536178/use-vite-as-remote-app-in-webpack-5-dynamic-remotes-container)、[mf/vite#172](https://github.com/module-federation/vite/issues/172)）。
- 同生态：Cordis/Koishi webui 与 DSH 前端视角（[腾讯云](https://cloud.tencent.com/developer/article/2728030)、
  [掘金](https://juejin.cn/post/7673438154771824674)）——P2 是其中「服务面」的声明化变体。

## 4. 边界（明确不做）

- **不做** P1（JS bundle 前端）、P3（iframe 独立前端）、P4（浏览器内 wasm 前端）——本轮否决。
- **不做**任意 JS 执行渲染（无插件 JS 进浏览器语境；渲染器只读 JSON 描述）。
- **不写代码**（本轮只确认方向与边界；设计/编码/试点在下一任务立项并走 TDD）。
- 现有 `/plugins/<name>/**` 静态挂接保留（作为 P2 静态清单/资源的载体，不冲突）。

## 5. 验收（层级，供下一任务细化）

1. 一个 wasm 插件包能声明一个 JSON UI 片段，GUI 壳发现并渲染（通用渲染器），交互动作回宿主
   RPC 到插件。首个扫描源 = 插件包静态清单（倾向）。
2. 壳改动的收敛面：渲染器为**单一通用组件**，插件 UI 声明为数据——新增插件 UI 不需改壳。
3. 声明格式子集文档化 + 校验（坏声明 fail-loud，不白屏）。
4. 全回归基线（workspace / clippy / golden / serve）不回归 + 后续试点的可验收 demo。

## 6. 下一步（后续立项建议）

单独立项（瀑布流全程）：定试点 → 设计（声明 schema 子集 + 渲染器契约 + 发现面 + RPC 动作面）
→ TDD 实现（先 wasm 插件声明面 + 壳渲染器最小集）→ 验收 demo。本轮到此为止。

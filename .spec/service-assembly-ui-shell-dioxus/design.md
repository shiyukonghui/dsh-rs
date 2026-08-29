# 系统设计：桌布壳 Rust 化（D-210 选项 C：Dioxus 壳重写）

日期：2026-09-05 | 上游：`.spec/service-assembly-ui-dioxus-research/requirements.md`
用户拍板：**动机=消灭 JS 优先；路线=直接 C**（壳重写；单元保持声明式，零迁移）。

## 1. 目标 / 非目标
- 目标：`/canvas` 渲染器（core.js+app.js 全部能力）由 Dioxus(wasm32-unknown-unknown)
  实现；纯逻辑从「node 测试」迁移到「cargo 测试（原生跑，不需要浏览器）」；JS 只剩
  **一段 <40 行的 wasm-bindgen 引导壳**（零业务逻辑——诚实口径：消灭 JS = 消灭逻辑 JS，
  胶水无法为零，除非 wasm 动态 linking 稳定）。
- 非目标：单元契约（ui.json/describeUI/组件模型）**不动**；不引入 dioxus fullstack/SSR
  （与自研 HTTP 服务不合）；不引 dx CLI 进构建主链路（wasm-bindgen-cli 单工具即可）。

## 2. 架构
- 新 crate `canvas-shell/`（**独立 workspace**，依赖树隔离，同 wasm-plugins 模式）。
- 模块划分（core.js 纯函数 → 原生可测 Rust 模块；app.js → Dioxus 组件）：
  - `model.rs`  ← buildModel/validateDeclaration/focusKey（serde_json 输入）
  - `layout.rs` ← layoutGrid/layoutMeasured/columnsForWidth（纯算术）
  - `values.rs` ← collectValues/rpcEnvelope/pollDecision/rowActionBody/needsConfirm/chatFold*
  - `schema.rs` ← schemaFields/nsSelectModel（真实 refs 表形——D-208 教训：以 wire 抓样为夹具）
  - `app.rs` + 组件（Sidebar/CardFrame/FormView/StatusView/ListView/ChatView/Relayout）
- 效果通道：fetch(POST /api/…) via web-sys；SSE=mux + /plugins/events via web-sys
  EventSource；轮询=set_timeout；重排=ResizeObserver（web-sys 绑定的 JS 互操作集中
  在 `interop.rs`，其余全纯 Rust）。
- 服务侧：构建产物（shell_bg.wasm + shell.js 引导 + 复用 canvas.css）落
  `crates/dsh-cli/assets/canvas-shell/`，include_str! 内嵌（与现行一致）；路由
  `/canvas/rust`（新）与 `/canvas`（旧 JS）**并存对比**，灰度切换权在你。

## 3. 构建链（环境事实已核）
`cargo build --target wasm32-unknown-unknown`（target 已装）→ `wasm-bindgen-cli`
（版本必须=锁定的 wasm-bindgen crate 版本；经 GitHub release 二进制获取，不编译）→
`wasm-opt`（可选，后续）→ 三件套拷入 assets。**发布产物入库**（同 wasm 组件惯例，
CI 无关）。

## 4. 功能对齐矩阵（验收=逐项过 + CDP 断言复用）
清单加载/rev 轮询/热插拔 SSE/五分类侧栏/声明校验红卡/layoutGrid+layoutMeasured+RO
重排/✕关闭+localStorage/nsSelect/secrets write-only+防误清/表单动作+confirm/行动作
confirm/schedule 轮询/chat 岛(选择/历史/发送/停止/SSE)——**每项以现行 JS 行为为规格**，
node 35 测的断言逐条移植为 cargo 测（夹具复用 core.test.mjs 的 live 抓样形）。

## 5. 风险与回滚
体积（哨兵数据随后补）；RO/EventSource 的 web-sys 绑定面→集中在 interop.rs 限爆炸半径；
`/canvas` 旧路径全程保留 → 回滚=不切默认路径即回滚；灰度通过后删除 JS 渲染器为
**独立提交**（再回滚=revert 该提交）。

## 6. 切分（每片 TDD）
S0 哨兵：最小 dioxus app 活体拉清单（本轮）→ S1 纯函数四模块移植（cargo 测先行）→
S2 壳渲染（清单/侧栏/卡框/布局/关闭）→ S3 form+status+list → S4 chat+轮询+SSE →
S5 对齐审计（CDP 双跑对比新旧壳）→ S6 切默认 + JS 渲染器退役。

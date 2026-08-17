# dsh web 完全可用——路径 A 分阶段路线

> 目标：让现有 DeepSeek Harness 前端（浏览器端 cordis 插件系统）在 Rust 后端上
> 真正跑起来——Rust 当 host，前端 UI 原样在浏览器跑，两者通过 `/api` 对接。
>
> 依据：源码 `deepseek-harness`（apps/web + packages/client/*）+ 已编译 bundle
> （`@deepseek-ai/dsh-web-frontend/dist` + `@deepseek-ai/dsh-client-*/lib/client.js`）。
> 本文件是路径 A 的权威规划；进度随实现更新。决策见 `DECISIONS.md`。

## 0. 架构事实（已核实）

- 前端不是静态 SPA，而是**浏览器端 cordis 插件系统**：
  - shell = `apps/web` → `@deepseek-ai/dsh-client-web`（boot 引擎 `AppWebEntry.run`）。
  - boot 必需 `window.__DSH_BOOT__`（host 注入的 entry graph），缺了直接抛错停在 loading。
  - 每个插件是 `/plugins/<id>/client.js?rev=<hash>` bundle，格式 `window.__ModuleLoader__.load({id, factory})`。
  - 所有 entry 必须 ACTIVE（`assertEntriesActive`），否则 boot 失败。
- 静态模块表（seed.ts）：react/jsx-runtime/react-dom/cordis/ui-slots/web-react/
  primitives/attachment/schema-form —— shell 静态打包，**不在 /plugins 范围**。
- 前端经 `connection` 插件与 host 通信：`POST /api/<method>`（client-request/
  server-response 信封）+ `/api/events.mux`/`/api/events.host`（WebSocket downlink）。
- 前端 boot 后调用的方法集是**固定清单**（见 `packages/client/connection/src/client/fixture.ts`）：
  session.list/create/history/models/selectModel/rename/fork/prompt/attachment/
  updateQueue/cancel、commands/list 等。
- **Rust 不需重写 cordis**：前端插件跑浏览器端 cordis，Rust 只需服务前端 + 注入
  manifest + 提供 /api 面 + WebSocket。

## 1. 阶段划分

### 阶段 0（已部分完成，M70/M71）：同源 HTTP 服务基线
- [x] 服务 `dsh-web-frontend/dist`（SPA fallback + MIME）。
- [x] `POST /api/<method>` 信封：version/sessions/session.create/session.history/agent-loop。
- [x] `/api/events.mux|host` SSE 下链（实时推 session/event 帧）。
- [x] HTTP 层用 `tiny_http`（成熟库，不手写轮子）。

### 阶段 1：让前端成功 boot（关键里程碑）
目标：浏览器打开 `http://127.0.0.1:PORT`，前端从白屏 → 真实 UI 出现。
- [x] **注入 `__DSH_BOOT__`**：Rust 组装 entry graph（扫描真实 bundle，38 个 web
      插件，见 D-005）注入 index.html `<head>` 首 script（对齐 `injectBootManifest`，
      `<` 转义）。
- [x] **服务 `/plugins/<id>/client.js`**：从 bundle 根目录读真实文件返回（text/javascript；
      id 含 scope 斜杠；`/plugins` 前缀路由，非 SPA fallback）。
- [x] **验证 boot**：真实前端 bundle + Edge headless 打开页面，DOM 渲染出真实 UI
      骨架（侧边栏/命令按钮/模型选择器显示 echo-loop/发送按钮）。剩余错误为
      功能级（dynamicCordisRunner 等方法未实现），非 boot 失败。
- [x] 静态模块表确认：真实 dist 由 `dsh-web-frontend` 打包，ui-slots/web-react/
      primitives/attachment/schema-form 由 shell bundle 提供（seed.ts 静态表），
      不在 `/plugins` 范围。
- 验收：页面从 loading 失败报告 → 出现真实 UI 骨架（哪怕功能未接）。

### 阶段 2：最小可交互闭环
目标：能新建会话、看到会话列表、发消息看回复。
- [x] `/api` 补方法：session.list（返回真实会话摘要）、session.create、
      session.history（surface 投影）——M70 基线已有。
- [x] `/api` 补方法：session.models（模型清单）、session.search、commands/list
      ——对齐 `UNARY_VALUE_SCHEMAS`，host.describe/workspace.list/skill.list/
      agentPreset.list 一并实现（boot 必需，见 D-007）。
- [x] WebSocket downlink：`/api/events.mux|host` 从 SSE 升级为真实 WebSocket
      （D-006）：tiny_http `upgrade()` 完成 101 握手 + tungstenite 包帧；无
      Upgrade 头回落 SSE。mux 推 `session/subscribed`+`session/event`，host 推
      `host/session-added`。已用真实前端 bundle + `ws` 客户端验证。
- [x] 验证：`session.prompt` 发消息 → echo-loop 驱动 → `session.history` 返回完整
      turn 流（turn/start→step/start→user/message→assistant/message→step/end→
      turn/end）；WebSocket 把同批事件推成 `session/event` 帧 → 前端可实时显示。
- 验收：基本聊天闭环可用（echo-loop 或 llm-loop）。

### 阶段 3：会话/工具功能扩展
目标：核心 Harness 交互（工具调用、多会话、模型选择）。
- [ ] `/api` 补：session.selectModel/rename/fork/prompt/cancel、tools 相关、
      agent 状态、settings 读。
- [ ] 工具调用在 UI 可见（tool/call + tool/result 事件已可推）。
- [ ] 验收：多会话切换、模型选择、工具调用展示。

### 阶段 4：完善与加固
- [ ] trust fence（Host 头校验 / loopback 判定，对齐 `api-request-trust.ts`）。
- [ ] 全量方法面覆盖（goals/jobs/approvals/skills/credentials/subagents...）。
- [ ] `events.host` host 帧（session-added/status 等）。
- [ ] HMR（模块级 / 配置级，可选）。
- [ ] 验收：`dsh web` 覆盖 Harness 主要交互，回归全绿。

## 2. web 插件清单（`__DSH_BOOT__.entries` 数据源）

共 34 个 `dsh.client` web 插件（来自已安装 bundle 的 package.json `dsh.client`）。

**immediately: true（阶段 1 优先，6 个）**：
`connection`, `hmr`, `locale`, `modules`, `runtime`, `ui-theme`

**immediately: false（28 个）**：
`ui-agent-preset, ui-commands, ui-conversation, ui-cordis, ui-deliverables,
ui-directory-picker-browse, ui-directory-picker-native, ui-goal, ui-input-trigger,
ui-jobs, ui-layout, ui-message-feedback, ui-model-selection, ui-permission-presets,
ui-plan, ui-settings, ui-settings-general, ui-settings-models,
ui-settings-plugin-inventory, ui-settings-plugins, ui-sidebar, ui-skill, ui-subagent,
ui-tool, ui-trajectory, ui-user-questions, ui-workflow-run, ui-workspace`

> 注意：清单中 inject 引用 `@deepseek-ai/dsh-api-remotes`、`dsh-typert-registry`、
> `dsh-cordis-client-runner` 等非 dsh-client-* 包——需确认它们是否也在 bundle 服务
> 范围，或由 shell/静态表覆盖。

## 3. 关键决策记录

- D-003（M70）：`dsh web` 同源服务前端 + /api RPC。
- D-004（M71）：用 tiny_http + SSE 下链（不手写 HTTP）。
- 后续阶段决策（注入 manifest / WebSocket / trust fence）实现时逐条入 DECISIONS.md。

## 4. 验收方式

- 每阶段：`cargo test --workspace` 全绿 + `cargo clippy -D warnings` 零警告。
- 手工冒烟：`dsh web <cordis.yml>` 后浏览器打开，观察 boot 进度。
- 可用 curl 验证：`/` 返回注入后的 index.html（含 `__DSH_BOOT__`）、
  `/plugins/<id>/client.js` 返回真实 JS、`POST /api/session.list` 返回信封。

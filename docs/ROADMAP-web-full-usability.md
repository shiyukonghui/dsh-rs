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
- [x] `/api` 补：session.selectModel/rename/fork/prompt/cancel（对齐 schemas，
      prompt 驱动 turn）、settings 读、agentPreset、llm、goal、subagent、
      credentials、dynamicCordisRunner（D-007/D-003 续）。
- [x] 工具调用在 UI 可见：`tool/call` + `tool/result` 事件经 mux WebSocket 推成
      `session/event` 帧（tool-loop 已注册工具，宿主 mux 帧带 data 透传）。
- [x] 验收（模型选择 + 工具调用展示 + 多会话）：UI 显示 echo-loop 并可从
      session.models 选；tool-loop 会话历史含 turn/start→user→tool/call→
      tool/result→assistant→turn/end，mux WebSocket 实时推 `session/event` 帧；
      多会话切换（D-010）：web 层 `SessionRegistry`，session.create mint 新 id、
      session.list 多会话、history/prompt 按 sessionId 路由（各会话历史独立）。

### 阶段 4：完善与加固
- [x] trust fence（Host 头校验 / loopback 判定，对齐 `api-request-trust.ts`，
      D-009）——`/api` 与 `/plugins` 仅接受 loopback Host，否则 403。
- [x] 方法面覆盖主要 UI 方法（对齐 `UNARY_VALUE_SCHEMAS`）：
      settings/credentials/llm/goal/subagent/agentPreset（select/read/copy/
      remove）、dynamicCordisRunner 等——空实现 ok:true，不 fail loud，UI 不再
      报错。jobs/approvals/skills 等留待后续按需补齐。
- [x] `events.host` host 帧：/api/events.host WebSocket 握手推 `host/session-added`
      （对齐 hostFrameSchema，D-006）。session-status 等按需扩展。
- [ ] HMR（模块级 / 配置级，可选）。
- [x] 验收（主交互）：真实浏览器 boot 零错误（仅前端 slot 冲突警告），模型选择
      显示 echo-loop，聊天闭环通，`cargo test --workspace` 全绿 +
      `cargo clippy -D warnings` 零警告。

## 2. web 插件清单（`__DSH_BOOT__.entries` 数据源）

共 38 个 `dsh.client` web 插件（真实 bundle 扫描结果，见 D-005；早期规划记为 34）。

**immediately: true（8 个）**：
`api-gateway`, `api-remotes`, `connection`, `locale`, `modules`, `runtime`,
`ui-theme`, `typert-registry`

**immediately: false（30 个）**：
`ui-agent-preset, ui-commands, ui-conversation, ui-cordis, ui-deliverables,
ui-directory-picker-browse, ui-directory-picker-native, ui-goal, ui-input-trigger,
ui-jobs, ui-layout, ui-message-feedback, ui-model-selection, ui-permission-presets,
ui-plan, ui-settings, ui-settings-general, ui-settings-models,
ui-settings-plugin-inventory, ui-settings-plugins, ui-sidebar, ui-skill, ui-subagent,
ui-tool, ui-trajectory, ui-user-questions, ui-workflow-run, ui-workspace`

> 注意：清单中 inject 引用 `@deepseek-ai/dsh-api-remotes`、`dsh-typert-registry`、
> `dsh-cordis-client-runner` 等非 dsh-client-* 包——已确认（D-005）：它们也是
> `dsh.client.platform==web` 的包，`build_boot_manifest` 扫描 `@deepseek-ai` 目录
> 自动收录（dsh-api-gateway/api-remotes/typert-registry 均在 immediately 集）。

## 3. 关键决策记录

- D-003（M70）：`dsh web` 同源服务前端 + /api RPC。
- D-004（M71）：用 tiny_http + SSE 下链（不手写 HTTP）。
- 后续阶段决策（注入 manifest / WebSocket / trust fence）实现时逐条入 DECISIONS.md。

## 4. 验收方式

- 每阶段：`cargo test --workspace` 全绿 + `cargo clippy -D warnings` 零警告。
- 手工冒烟：`dsh web <cordis.yml>` 后浏览器打开，观察 boot 进度。
- 可用 curl 验证：`/` 返回注入后的 index.html（含 `__DSH_BOOT__`）、
  `/plugins/<id>/client.js` 返回真实 JS、`POST /api/session.list` 返回信封。

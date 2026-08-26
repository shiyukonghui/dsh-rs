# D-115-Web：前端启动修复 系统设计

日期：2026-08-26。上一阶段（需求分析）已由用户验收（requirements.md 定稿，D1/D2/D3 已确认）。
本设计仅供评审；编码实现（TDD）在其验收后开始。

## 1. 设计目标（对齐需求验收标准）

让 Rust `dsh web` 服务 + vendored 编译前端以「浏览器完整可用」状态运行：
- 物化（P0）：浏览器 37 个 client entry 全 active，页面渲染（sidebar/会话/chat）。
- 可用（P1）：host 面 `/api` Typert RPC（remote.* 全端点）+ connection 通道 + 下行，交互可用。

## 2. 整体架构（三个改动面，互不耦合可分批推进）

```
┌─ 浏览器 ─────────────────────────────────────────────┐
│  vendored dist (apps/web) — 旧协议 queue 门面，已匹配 │
│  13 个 client bundle：modules/runtime/connection/    │
│  api-gateway/api-remotes/typert-registry/ui-*        │
│              │  /api <ns>/<method>  (ClientRequest)   │
└──────────────┼───────────────────────────────────────┘
               ▼
┌─ Rust host（dsh-cli web） ────────────────────────────┐
│  build_boot_manifest ── D1 多 plugin_root (base+web)  │
│  serve: / → 注入 boot；/plugins/*/client.js          │
│  /api RPC: 信封已同构 → 路由到 EndpointHost           │
│    EndpointHost ── D3: wasm 插件（host-remote world） │
│    已实现: commands/goals/session/agent/workspace…    │
│    补齐: messageFeedback/fileReferences/… (D2 全实现) │
└───────────────────────────────────────────────────────┘
```

**关键判断**：三层互相独立——
- 物化只依赖 D1（roster 补齐），不碰端点；
- 端点 D2/D3 是「可用」面的增强，不阻塞渲染；
- 分阶段落地：先 D1 验渲染 → 再 D2/D3 验交互。

## 3. 改动面 1 — Rust 插件组合（Rust 版 cordis）

Rust 已具备完整 cordis 原型：
- `dsh_core::Cordis` + `Plugin` trait（`apply(&self, ctx, config)`）+ `ctx.provide/on` +
  fiber 可逆副作用（registry.rs）；
- `Cordis.plugin(Arc<dyn Plugin>, config)` 注册（dsh-cli web.rs `boot_with_sessions` 先例）；
- `dsh-loader` = Cordis loader 的 Rust 移植（entry tree/groups/transactional/self-dispose）。

**设计决策 D-R1**：web server 的 boot 组合显式装配一个「端点宿主」插件树：
```
EndpointsPlugin(core)  → 注册 remote 端点路由（宿主原生实现，基础面）
WasmRemotePlugin(wasm) → D3: 经 dsh-wasmrt 加载 host-remote 组件，承载补齐端点
```
两者同一 Cordis 组合，`ctx.provide("remoteEndpointRouter", ...)` 统一路由面。

## 4. 改动面 2 — D1：`build_boot_manifest` 多 plugin_root

**现状**：`build_boot_manifest(&cfg.plugin_root)` 从单一 `@deepseek-ai` 目录扫；
`plugin_root = default_plugin_root(web_root)` 只达 web-app 层 → 缺 base 层。

**改造**：`WebConfig` 新增 `plugin_roots: Vec<PathBuf>`（有序；base 在前，web-app 在后）。
- `main.rs`：`web_main` 从 vendored 布局推导两个 root：
  - base：`packages/bundle/base/node_modules/@deepseek-ai`
  - web：`packages/bundle/web-app/node_modules/@deepseek-ai`（= 现 default）
  - 均可被 `DSH_PLUGIN_ROOT` / `DSH_WEB_ROOT` 系列 env 覆盖（保持既有点位）。
- `build_boot_manifest(roots: &[&Path])`：遍历每个 root 的 `read_dir`，按
  `dsh.client.platform=="web"` + `lib/client.js` 收录；**遇同 id（重名包）后者覆盖**
  （保持「分层补丁」语义，与 cordis patch 后层覆盖先层一致）；`HOST_COMPOSITION_EXCLUDED_CLIENTS`
  过滤保留。
- rev 计算基于合并后 entries（现有实现不变）。

**验证（TDD）**：新增测试断言——两 root 布局（base 含 typert-registry+api-gateway、
web 含 runtime/ui-*）→ entries 覆盖 13 个最小集；重名后者覆盖；rev 变化。

## 5. 改动面 3 — D2/D3：host 面 remote 端点

### 5.1 端点路由（wire 已同构）

前端 `POST /<channel>/<endpoint>`（endpoint = `ns/method`），body = ClientRequest。
Rust `dispatch` 现有实现已按 `ns/method` 处理（commands/list、goal.pause…）。
改造：`dispatch` 增加「未命中已知方法 → 转交 EndpointHost」回落，避免硬编码全表。

### 5.2 端点全集与实现（全真实实现，不做空表/占位/假数据）

**原则（用户明确）**：不做薄实现——每个端点都要**真实的功能语义 + 真实数据源**。
数据存在 Rust 侧真实状态（loader entries / session store / settings / credentials /
goal service / workspace registry 等），返回结构**严格对齐**生成描述符 zod schema。
权威语义来自 TS host 实现（subagent 正在逐一还原 7 命名空间的 FaceModel 真实现）。

| 命名空间 | 端点 | 真实实现方向（数据源） |
|---|---|---|
| commands | execute, list | ✅ 已有（execute 解析 payload.args；list 真实命令目录） |
| goals | create^edit^clear^complete^pause^resume | ✅ 已有（dsh-goal 真实状态机） |
| messageFeedback | put, list, delete | 🆕 真实现：持久 KV 表（按 sessionId）+ 校验（note-maxBytes/blank/toolarge、ifVersion 乐观并发、target 须存在于 session events 的 append-origin assistant/message、session identity 按 createdAt+cwd 匹配、no-op 幂等）；返回 success/rejected union（细分支见 TS `MessageFeedbackService`，packages/feedback/message-feedback/src/index.ts，已亲自还原） |
| fileReferences | list | 🆕 真实现：按 agent cwd + query（`@`/`@"`）扫描 dsh-fs 路径候选（deterministic path-only；signal 可取消）；TS `FileReferenceService.list`（packages/context/file-reference/src/index.ts） |
| sessionReferenceResolver | candidates | 🆕 真实现：列出全部会话（排除自身 agent），按 cwd 亲缘度排序，query 时大小写不敏感过滤（id/cwd/title 子串），limit 截断 → `{sessionId,label,cwd?,createdAt}` + canonical mention；TS `listCandidates`（packages/context/session-reference/src/index.ts） |
| pluginInventory | list | 🆕 真实现：`dsh-loader::entries()` 实时只读投影，跳过 group 行 → `{entries:[{entryId,moduleName,enabled,fiberPhase}]}`；每次调用实时读（TS `PluginInventoryGateway.list`） |
| dynamicCordisRunner | 12 个 | ⚠️ 真实现困难分级待定（见 5.5）：动态 cordis 插件宿主运行 + 审批状态机；Rust 有 Cordis.plugin() + loader.create/update 基础，但 sandbox 代码求值（evaluateHostCode）等 TS 特有宿主面需 not-implemented fail-loud |
| session.*/agent.*/workspace.*/settings.*/llm.*/subagent.*/schedule/job | 既有 | ✅ 不动（已真实） |

**已亲手还原的 TS 真实语义样本**（5/7 端点，见上表内引用的源文件）；剩余
goals 参数结构 + dynamicCordisRunner 难度分级待 subagent 报告补全（编号待填）。

**「真实实现」验收红线（用户否决薄实现后确立）**：
- ✅ 有真实数据源：数据来自 Rust 侧真实状态（loader entries / session store / settings /
  credentials / goal / workspace / 持久 KV 等），不是硬编码假值。
- ✅ 有真实业务规则：校验（版本冲突/目标存在/请求合法性）真实执行并返回真实错误分支，
  不是无脑 ok。
- ✅ 有真实持久（该持久时）：messageFeedback 等应落真实存储（session store / KV），
  重启可读回。
- ❌ 禁止：`[]`/`null`/固定假结构冒充可用；渲染与交互层面出现「看起来能用实际是假」。
- ⚠️ 明确不实现的（如 dynamicCordisRunner 中依赖 TS 特有宿主 sandbox 求值的子集）：
  **显式返回 not-implemented 错误**（fail-loud 诚实），绝不返回假成功。
  若某方法 Rust 有真实能力子集（如 inventory/syncInspectManifest → 真实 loader 状态），
  实现该子集并保持 wire 兼容。

## 5.5 dynamicCordisRunner 真实实现难度分级（定向判定）

基于 `packages/extensions/cordis-host-runner/src/index.ts` 的 `runHostHalf`（L324-374：
createAttempt / activate / 审批 awaited 状态机）+ sandbox.ts 的既有判断：

- **可真实实现（映射 Rust 现有真实能力）**：
  - `inventory` → 枚举 Rust Cordis/loader 的已装动态插件（真实状态）。
  - `syncInspectManifest` / `resolveInspectQuery` → 真实 inspect 查询注册/解析
    （Rust Cordis 服务/事件面可投影）。
- **部分可（Rust 有基础、需补状态机）**：
  - `runHostHalf` / `settleUserRun` / `stopFromPanel` / `undefineFromPanel` → 动态插件
    运行 + 审批生命周期（Rust `Cordis.plugin()` + `loader.create/update` 有基础；
    需补审批 pending/awaited 状态机）。
- **高度 TS 宿主依赖（honest not-implemented，拒绝假数据）**：
  - `getClientCode`（动态 client 包代码拉取——Rust 无 dynamic client 包机制）；
  - `invoke`（sandbox `evaluateHostCode` 求值宿主方法——TS 特有 sandbox）；
  - `reportRenderFailure` / `reportClientGuardFailure`（TS 渲染/guard 沙箱报告）。
  这些返回规范化的 `not-implemented` 业务错误（对齐 RpcError 形状），前端按错误处理，
  绝不回假成功。

**设计说明**：dynamicCordisRunner 的「真实实现」边界 = 用户驱动可见的真实插件清单/
状态投影 + 明确 not-implemented，而非把 TS 动态沙箱搬到 Rust。这与「真实现」红线一致
（真实的部分真实做，做不到的诚实报错，不伪造）。

## 5.6 设计最终确认（用户裁定，2026-08-26）

- dynamicCordisRunner 中依赖 TS sandbox 的 4 方法（getClientCode / invoke /
  reportRenderFailure / reportClientGuardFailure）：**接受显式 not-implemented 错误**，
  问题记录留待后续调研解决（列入 DECISIONS 待办）。
- **D3 承载范围 = 所有新增端点全部放 wasm**（not: 部分宿主部分 wasm）。即
  messageFeedback / fileReferences / sessionReferenceResolver / pluginInventory /
  dynamicCordisRunner 的全部新实现 + 其余新增端点，逻辑写在 wasm 组件
  （host-remote world）内；既有大面（commands/goals/session/agent/workspace/settings/
  llm/subagent）保持宿主原生，统一经 EndpointHost 路由。wasm 组件是新增端点的唯一实现地。

### 5.3 D3 —— wasm 插件承载（host-remote world）

**沿用 dsh-wasmrt 组件模型路径**（对齐 `WasmLoopPlugin`/`dsh-loop.wit` 先例）：

新增 `crates/dsh-wasmrt/wit-dsh/host-remote.wit`：
```wit
interface remote {
  /// 处理一个 host side 端点：namespace/method/body(JSON 字节) → result(JSON 字节)。
  handle: func(namespace: string, method: string, body: list<u8>) -> list<u8>;
}
interface host-services {
  /// wasm 端点可反向访问宿主：读 session/settings 等（JSON 字节）。
  get: func(service: string, payload: list<u8>) -> list<u8>;
}
world host-remote {
  export remote;         // 端点实现（wasm 侧）
  import host-services;  // wasm → 宿主
}
```

宿主侧新增 `crates/dsh-wasmrt/src/remote.rs`：`WasmRemoteEndpointPlugin`（组件加载 +
`handle` 导出绑定），适配 `dsh_core::Plugin`——apply 时 `ctx.provide("remoteEndpoints", …)`
并在 `dispatch` 回落路径被 `EndpointHost` 调用。

**承载范围（第一版）**：messageFeedback / fileReferences / sessionReferenceResolver /
pluginInventory / dynamicCordisRunner 全端点走 wasm；commands/goals/session 等既有大面
留在宿主原生（作为 base，稳定性优先）。这符合「rust 版 cordis + wasm 插件承载服务」——
端点既有宿主原生 + wasm 插件两层，统一经 EndpointHost 路由。

**构建**：wasm 端点插件的载荷（Rust wasm32 + wit-bindgen + wasm-tools component new）按
仓库既有 echo-loop 构建流程新增一个 build target。**已验证链路可用**：`wasm-plugins/`
下已有多套 wasm32-wasip1 组件产物（echo-loop/hello-component/llm-loop 等），
`echo-loop` 为 `crate-type=["cdylib"]` + 引用 `dsh:dsh`/`dsh:plugin` wit——D3 完全可复制
该模板（新建 `wasm-plugins/host-remote/`，同 Cargo + wit 布局）。

### 5.4 EndpointHost（统一路由）

`dispatch` 回落逻辑封装：`fn route_remote(host: &EndpointHost, ns, method, body) -> Value`：
1. 查 wasm 端点插件 EndpointHost 表 → 命中 → 调用插件 `handle`；
2. 否则按已在 dispatch 的既有方法处理；
3. 否则命中占位（空表/not-implemented）。

## 6. 测试策略（TDD，本阶段编码阶段执行）

- D1：`build_boot_manifest` 多 root 单测（两 root 布局 → 13 集；覆盖；rev）。
- D2（真实现）：
  - messageFeedback：put→list 读回（持久）、版本冲突、note-blank/too-large、target 不存在、
    no-op 幂等、delete absent；（断言真实数据而非空表）。
  - fileReferences：构造临时 cwd + 文件 → query 前缀 → 返回真实路径候选。
  - sessionReferenceResolver：多会话 + cwd → 排序/过滤真实会话候选。
  - pluginInventory：loader 注入真实 entries → 断言行数/字段。
  - dynamicCordisRunner：inventory/syncInspectManifest 返回真实状态；not-implemented
    类返回规范化错误（断言不是假 ok）。
- D3：WasmRemoteEndpointPlugin 单元测试（加载组件 → handle 路由 → 结果）；一个最小
  host-remote 组件（echo 载荷）做加载冒烟。
- 集成：`assemble_server_runtime` + dispatch 回落 → 未命中→wasm 端点，命中→结果。
- 浏览器：playwright msedge headless 验证渲染锚点 + 37 全 active（复用 render-smoke.mjs）。

**goals 结构说明**：Rust `dsh-goal` 本身就是 TS `dsh-goal` 的对齐实现（CAS 状态机 +
事件源 fold + 投影 + round-driver），其参数/返回即权威结构，无需另查。

## 7. 验收对照（需求验收标准）

| 需求验收 | 设计对应 |
|---|---|
| 浏览器 37 entry 全 active | D1 roster 补齐（base 2 + web 11） |
| 页面渲染 | 物化后 playwright 锚点断言 |
| session.prompt 全链路可用 | 既有大面不动 + EndpointHost 路由不破坏 |
| cargo test 全绿 + clippy 0 | 各改动面 TDD + 关闸 |

## 8. 决策日志草稿（追加 DECISIONS.md）

- D-115-Web-D1：多 plugin_root——为「随 Rust 打包」的 vendored 两层（base+web）设计；
  被否决：合并复制 node_modules（维护包袱、占盘）。
- D-115-Web-D3：host-remote world 承载补齐端点——被否决：宿主原生函数直接实现
  （D3 用户指定 wasm 插件承载；组件模型路径复用既有 dsh-loop 范式）。
- D-115-Web-D2：全实现但薄（空表占位对齐 schema）——被否决：只 P0 核心（用户要求全实现，
  且薄实现成本低、fail-loud 诚实）。

## 9. 风险

- wasm 端点插件的载荷构建链路（rust→wasm32→component）：**已验证可用**——仓库
  `wasm-plugins/` 有多套组件产物 + echo-loop 模板（cdylib + dsh:dsh/dsh:plugin wit）。
  第一版即用 wasm 承载（D3 达成），native 插件仅留作临时 fallback（若某个端点无法
  wasm 化，DECISIONS 记录权衡，不静默降级）。
- pnpm 层间 node_modules 隔离：多 root 后 base 层的 gateway/typert 已 build（实测存在
  lib/client.js），无构建缺口。
- 端点「薄实现」被前端做真实交互：messageFeedback 等若前端必须持久，进程内 Vec 足够
  会话级；若需跨会话/重启持久，再评估存储后端（设计留口，不现在扩大）。

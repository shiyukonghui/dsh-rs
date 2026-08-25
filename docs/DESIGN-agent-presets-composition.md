# DESIGN：Rust 插件组合 —— per-session agent preset（D-102 前置调研稿）

> 状态：**调研稿（未定稿）**——目标不是「预设清单」，而是在 Rust 里实现可类比 TS
> cordis 的「插件组合」体系：一个会话选择一份 `agent.cordis.yml` 组合，组成该会话
> 独有的工具面 / 提示词 / 服务实例，隔离域（isolate realm）内私有，会话结束/重选时
> 可卸载（teardown）。本稿先给「大概方案」供用户定夺下一步，**不写实现代码**。

---

## 0. 为什么这件事在 Rust 是可行的（第一性原理，自上而下）

**目标**：`agent.preset`（= 一份 per-session 组合）必须能做到 TS 能做到的三件事：
1. **选择**：会话归属某 preset id（会话级选择 → settings 默认 → 部署默认）。
2. **组合**：把该 preset 的 `agent.cordis.yml` 顶层层列（plugin rows）挂载成该会话
   的**私有**工具面 + 提示词 + 服务实例；`isolate` 域保证不同会话同一 preset 互不
   干扰，宿主平面（注册表/agent-loop/沙箱）保持全局共享。
3. **卸载/重选**：会话结束或 `agentPreset.select` 重选（仅 blank 会话，
   `agent-preset-locked`）时，按 disposer 顺序回收该会话的组合。

**向下看（自下而上），Rust 仓库已具备 90% 的机制性底座**（详见 §1），所以这不是
从零重写 cordis，而是「组合编排层 + 一组 Rust 原生插件实现」：

| 机制 | TS | Rust 现状 |
|---|---|---|
| 插件行/组合解析（EntryOptions、include、patch、`!!js`） | cordis loader + `@deepseek-ai/cordis-plugin-include` | `dsh-loader`（`crates/dsh-loader`，含 `entry/include/group/hmr`、M62 isolate/intercept 差分、事务+回滚）+ `dsh-eval`（`!!js` 表达式子集） |
| 服务注册表 + 隔离 realm（isolate 标签/实例） | Cordis registry + realm 7 步转移 | `dsh-core`（M3：`ScopeId`、按作用域 `resolve_impl`、`ctx.plugin/provide/get`、LocalRealm/GlobalRealm、realm GC） |
| 作用域键控的工具面（per-session 可见性） | scope register/shadow | `dsh-tools`（`register(scope)`、`schemas(scope)`、`known_names(scope)`、`presentAs(scope)`、`ScopedLayers`） |
| 作用域键控的系统提示词 | scope sections/persona | `dsh-system-prompt`（`tools(scope, provider)`、scoped sections/contexts shadow globals、`assemble(&AssembleContext{scope})`） |
| per-session agent 生命周期 | Session→Agent→mount | D-101 已实现：`AgentLoopHost::register_session_agent` / `ensure_session_agent` / `runtime_agents`；`AgentLoopHost` 已持 disposer 列表 |
| 设置域 | settings namespace | `dsh-settings` + D-095 已注册 `agent-loop` 等 namespace（`agent-presets` 待加） |
| 工具实现 | 各 `dsh-tool-*` TS 插件 | 33 个 Rust crate ≈ 1:1：`dsh-shell/bwsh←dsh-shell`、`dsh-fs`、`dsh-jobs`、`dsh-workflow`、`dsh-subagent`、`dsh-terminal`、`dsh-plan`、`dsh-goal`、`dsh-code-runtime`、`dsh-compaction`、`dsh-llm-deepseek` … |

## 1. Rust 现状盘点（自下而上，含引用）

### 1.1 组合解析：dsh-loader
- `dsh_loader::EntryOptions{id,name,config,disabled,disabled_expr,group,inject,isolate,intercept}`（`crates/dsh-loader/src/entry.rs:15`）。
- `Loader::new(&Cordis)` + `register_plugin(name, Arc<dyn Plugin>)` + 编程式 `create/update/remove/sync`（`crates/dsh-loader/src/loader.rs:362,378,416,458,577,606`）；`Include`（读文件 patch 层，`crates/dsh-loader/src/include.rs`）。
- serve boot 已用同一条路径加载顶层 cordis.yml：`Loader::new(&cordis)` → `register_plugin("dsh:services", …)` → `Include::new(...)`（`crates/dsh-cli/src/lib.rs:137-174`）。→ **顶层缺省组合与 per-session preset 组合共用同一 loader 语义**。
- `isolate`/`intercept` 已在 loader 层透传/实现（M3 + M62 差分场景，`crates/dsh-loader/src/isolate.rs`）。

### 1.2 隔离 realm：dsh-core
- `Cordis::plugin()/plugin_arc()/provide()/provide_service()/get()/get_typed()`（`crates/dsh-core/src/context.rs:412+`、`1199-1252`），`provide` 随 fiber 挂卸载（disposer）。
- M3 已实现 isolate 作用域：`FiberData.isolate: HashMap<String, ScopeId>`（`crates/dsh-core/src/fiber.rs:74`）、服务注册表按 `(ScopeId, name)` 隔离（`crates/dsh-core/src/runtime.rs:99,133,275,283` `alloc_scope`/`resolve_scope`/realm GC）——同名服务在不同作用域 = 不同实例，即 isolate realm 语义；`ctx.get` 是**全局 store 按 isolate 标签查询**（非 fiber 链），与 Cordis `reflect.get` 对齐。

### 1.3 作用域工具面：dsh-tools + dsh-scope
- `register(scope: Option<&ScopeKey>)`、`schemas(scope)`、`known_names(scope)`、`get(name,scope)`、`presentAs(scope,…)`、`mode_for(scope)`（`crates/dsh-tools/src/runtime.rs:262,410,419,405,456,435`）——"全局基 + 祖先链 + 自有层 shadow"（`ScopedLayers`/`NamedEntries`，`dsh-scope`）。
- **结论**：per-session 工具集 = 在会话 `ScopeKey` 层注册，天然独立于全局/他会话。系统提示词同理（`dsh-system-prompt` `tools(scope, provider)`、scoped sections，`crates/dsh-system-prompt/src/lib.rs:447,499-523`）。

### 1.4 per-session agent：D-101
- `AgentLoopHost{config, store, bus, registry, llm, tools, prompt, agents, runtime_agents, disposers}`（`crates/dsh-agent-loop/src/host.rs`）；`register_session_agent` 幂等、`ensure_agent`、`followup`。
- web `session.create/fork` → `ensure_session_agent(boot, sid, cwd)`（D-101）；`run_rust_loop` 经 `configured_for_session` 路由。
- **当前缺口**：所有会话共享同一个 `prompt`/`tools`（with_store 各建一份全局），尚未从**会话的作用域**组装工具/提示词——这正是组合层要接的缝。

### 1.5 工具实现覆盖
33 crate 已含 m4/m5 全部工具（bash/pwsh、fs、jobs、schedule、todo、terminal、subagent、workflow、goal、plan、sandbox 策略、code-runtime…），web 侧以 `register_m4_tools_with_host`/`register_m5_tools_with_host`（`crates/dsh-cli/src/web.rs`）注册到全局 registry。→ 变成「可组合插件行」只需包装成按 scope 注册的函数/插件实现，而非重写。

## 2. TS 语义基准（权威：standing mount + scope parentage + 单飞队；已核对 mount.ts + TS 深度调研）

**来源**：`packages/preset/agent-presets/{mount,index,session,discovery,preset}.ts` +
`vendor/loader/src/config/isolate.ts` + `packages/host/apiproxy/src/api-proxy.ts` +
`apps/cli/config/agent-presets/README.md`。要点（全部带 file:line 引用）：

- **standing mount（每次每个 preset id 只挂载一次，单飞队）**：`index.ts:252 standings:
  Map<presetId, Promise<StandingMount>>`，`ensureStanding`（index.ts:491-534）按 preset id
  单飞队创建 standing `key={agentPreset:id}`（每代新铸纯对象）→ `createScope` → 挂载。
  **组合文件变化**（mtime+size 戳，index.ts:546-560）→ 新**代（generation）**：之后创建的
  会话 join 新代；已 join 的会话保持旧代；被取代的代只在整树 teardown 时释放
  （index.ts:490-512,562-570）。
- **join = 父链重载（bindScopeParent，只绑一次）**：`mount(agentCtx,id?)`（index.ts:275-288）→
  `bindScopeParent(agentKey, standing.key)`，绑定记录在 `bindings: WeakMap<ScopeKey, ScopeParentBinding>`
  （index.ts:260）。`composeFrom(parentCtx)`（index.ts:316-325）让子 agent 继承父 agent 的
  **同一代数**组合（同步绑定父→子）。`recompose(agentCtx,id)`（index.ts:458-472）先解析+ensure
  新 standing，再 `binding.rebind(standing.key)`——**是父链重载，不是卸载**；旧组合仍服务其
  他 agent；失败时 agent 保持原样。blank 检查是调用方职责（api-proxy 在入队操作内再查）。
- **挂载阶梯（mount.ts:332-380）**：① 拒绝无 scope 上下文（否则注册落到每 agent）；
  ② `Include.Config={path}` 并记录 host baseUrl（harnessBase，供裸包名解析）；
  ③ `agentCtx.plugin(PresetTree, config)` 直接挂载（非 loader 注册）→ await 子树；
  ④ 审计 inactiveRows（任一行未激活/缺注入 → 整挂载拒绝，指名行）；⑤ 审计 leakedServices
  （任一行把服务发进 root realm → 拒绝）；⑥ 成功记入 mounts `{presetId, fiber, key}`。
  任何失败先 dispose 子树再抛 `PresetMountError`（broken preset = 部署须修，粗粒度错误
  `UnknownPresetError` 与坏 preset 区分，preset.ts:71-93）。
- **isolate realm（vendored cordis-loader，config/isolate.ts，非 harness 扩展）**：
  `Context[symbols.isolate]: Dict<symbol>`（context.ts:18,42-52）；`ctx.isolate(name,label)`
  子上下文按符解析。`isolate:{name:true}` → LocalRealm `#<entry id>`（per-entry 新符号）；
  `isolate:{name:"label"}` → GlobalRealm `@<label>`（同 label 共享同一符号——**共享 label 不
  池化实例**：`provide` 同符号二次注册 `throw 'service has been registered'`，reflect.ts:289-291）。
  服务 store 进程全局，隔离图把名字约束到实现：host 注册表保持全局、preset 服务私有到本
  mount。**同一 preset 的所有会话共享 standing 实例**；per-session 分离靠 scope 层（每个
  agent 目录/提示词走 `agent→preset→global` 链）+ 插件内部按 Session/Agent 键控状态。
- **会话预设解析（session.ts:48-54 + api-proxy）**：最新 `agent-preset/selected` 日志事件
  胜出，否则用 creation header（`header.agentPreset` 深冻结）。新建：`resolve(id?)→defaultId`
  （defaultId = settings `agent-presets.default` else config.default，index.ts:191-193）写入
  header（api-proxy.ts:1610-1619），发布前 `presets.mount(agentCtx,id)`——组合失败整体回滚
  会话创建（与 agent-loop `setupAndPublish` 发布前 setup + 失败 dispose 一致，agent-loop/
  index.ts:625-645）。续接：stored preset（日志）优先 + `assertPresetUnchanged` 拒绝不匹配。
- **重选/锁定**：`agentPreset.select` 在入队操作内查 `sessionBlank`（无 `turn/start`，
  api-proxy.ts:448-450），非 blank → `agent-preset-locked`（api-proxy.ts:2987-3015）；成功
  `recompose` + append `agent-preset/selected` 事件。
- **设置默认流**：`agent-presets` namespace（index.ts:40）、schema `{default?}`（index.ts:43-51）、
  注册 base=组合层默认 + user 层覆盖（settings 三态：schema default → base → user layer，
  settings/src/index.ts:102-129）；客户端写 `settings.update({ns:'agent-presets'})`；删除预设
  后 `unset default` 回落到部署默认（index.ts:400-416）。
- **`{{model}}`/`{{cwd}}` 不是 loader 插值**——是 system-prompt 变量（每 loop 注册一次，
  agent-loop/src/index.ts:351-353；`cwd` 来自 `session.header.cwd`）；严格渲染。
- **每行效果面**（§3b 全表）：persona → `systemPrompt.section('deployment:persona', order 0)`
  scope-only（预设内 shadow 全局 persona；全局挂载抛错）；`tool-*` → scoped `tools` 层；
  isolate 组承载服务 realm（planMode/compaction/workflowEngine/terminals/fs shadow）；
  host 平面经 scope 视图被每会话消费：tools/systemPrompt/skills/subagents/jobs/web/fs/shell/llm/codeRuntime。

## 3. 差距清单（待实现，按难度升序）

0. **【架构关键点】两套作用域系统尚未打通 —— 已核实为「两套运行时织物」的既有缺口**
   （架构偏离程度分析，2026 定稿时复核到 src）：
   - **事实 A · 双运行时并存（有记录的设计）**：`dsh-core`（Cordis 移植）是**组合运行时**
     ——boot 顶层组合（`lib.rs:136-137 Loader::new(&Cordis)`）+ WASM 循环
     （`run_turn(&boot.ctx)`）；`dsh-agent-loop`（ReactLoopAgent）是**agent 平面驱动**——
     生产路径 `serve()` 设 `boot.agent_loop=Some`（web.rs:266），`session.prompt/agent.*`
     走 `run_rust_loop`（lib.rs:468+），**不经 `boot.ctx`**。二者分派由 D-077 记录为
     显式分叉（agent_loop.is_some → native，否则 WASM）。
   - **事实 B · 闭环服务面不解析自 dsh-core**：`build_loop_deps`（service.rs:41-128）把
     `SystemPrompt`/`LlmRuntime`/`ToolRegistry` 以 **Rc 闭包注入（LoopDeps）**，不走
     `Cordis::get()`；闭环事件（`agent/request` 水岭）在 **dsh-agent 自己的 `AgentBus`**
     （agent_bus.rs:33「对齐 Cordis ctx.events 的最小…」；dsh-agent 不 use dsh_core）——
     即**第二套事件织物**。
   - **事实 C · 作用域语义半采纳**：dsh-tools/dsh-system-prompt 的作用域层用 **dsh-scope
     ScopeKey**（可见性/覆盖语义等价 TS）；但 dsh-core 的 **ScopeId 服务隔离 realm**
     （服务按作用域实例）在 agent 闭合内**完全不可达**（dsh-core 服务经 Loader/Cordis
     才解析）。
   - **偏离程度逐缝裁定**：
     | 缝 | TS（参考架构） | Rust 现状 | 判定 |
     |---|---|---|---|
     | 组合/装载权威（boot） | cordis loader 组 host | dsh-core+loader 组 host | ✅ 对齐 |
     | 循环控制流（turn 状态机） | ReactLoopAgent 自驱 | 同（M2e 设计即自驱） | ✅ 对齐 |
     | 闭环服务解析 | 经 composed ctx.get | Rc/LoopDeps 注入，不经 Cordis | ❌ 旁路 |
     | 事件/水岭（agent/request 等） | cordis ctx.waterfall/on | dsh-agent AgentBus（第二织物） | ⚠️ 语义影子，织物相异 |
     | 工具/提示词作用域 | scope 链（agent→preset→global） | dsh-scope ScopeKey 链 | ⚠️ 可见性等价；键空间 ≠ dsh-core ScopeId |
     | isolate 服务实例（realm） | isolate 组内私有实例可达 loop | dsh-core realm 存在但 loop 不可达 | ❌ 旁路 |
     → **「绕过 dsh-core 核心」= 真实存在，但只在 服务解析/事件/isolate 面（agent 平面
       消费组合的那层）**；组合权威与循环驱动未被绕过。它是**有记录的分阶段中间态**
       （D-032/033 LoopDeps 缝、D-035 宿主装配、D-077 双驱动分叉），并非事故；但它是
       真缺口——**presets 是第一个需要「服务平面（isolate realm）语义」的特性，恰好把
       它逼了出来**。
   - 收敛路径/决策（见 §5 增补「架构收敛」）：**A 仅作用域层献祭服务行（不收敛，缺口照旧）；
     B 服务桥接——preset 在 dsh-core/loader 挂载（真 entry tree + realm + 守卫 + 代），
     经**窄桥**把每行的模型可见效果投影进 loop 已消费的 scope 层、把 service 行映射成
     loop 可消费的句柄（推荐先做，符合架构也解锁 presets）；C 全收敛——循环开进 dsh-core
     （agent.ctx=dsh-core ctx.extend({agent})、服务全走 Cordis::get、AgentBus 折入 dsh-core
     事件、每 agent fibre 挂 entry tree；计划弧终点，量/险大，作 presets 之后独立架构
     里程碑）。**

1. **preset 解析 + 解析器**：把 `agent.cordis.yml`（同 EntryOptions 的顶层层列，含
   group/disabled/`isolate`/`{{model}}`/`{{cwd}}` 插值）解析为可挂载行。reuse
   `dsh-loader` 的 Include/EntryOptions 解析（已验证 `include.rs:308-355` 读顶层
   YAML 数组）；`{{var}}` 整文展开需小实现（现仅 `__jsExpr` 节点内插值）。
   注意 vendored preset 用字面 `!!js` YAML 标签，Rust YAML 库不认 → 复制时转译为
   `{"__jsExpr": "..."}`（对齐 include.rs:6 的既有 M3 差异）。
2. **组合挂载器（核心编排）—— standing mount + join（对齐 mount.ts）**：
   - **standing 层**：每个 preset id 在 dsh-tools / dsh-system-prompt 下建一个
     **稳定的 standing ScopeKey 层**（解出的 entry tree 逐行挂载：工具
     `register(Some(standing_key))`、prompt `sections/tools(Some(standing_key))`、
     persona 覆盖 `deployment:persona`），**只挂载一次**；
   - **join**：每个会话 agent 的 ScopeKey（D-101 每 agent `ScopeKey::new()`，
     host.rs:289）**parent 到该 preset 的 standing key**（dsh-scope 祖先链现成），
     会话自有层可 shadow——两会话角色分工：standing 持有组合，会话层持自身状态；
   - **守卫（mount 即验证，fail loud）**：行未激活（缺注入服务）→ 组合标记 `broken`；
     `isolate: true` 行的服务实例不得落 root——Rust 侧以「该行注册未带 scope」为
     泄漏判据，拒绝挂载；
   - 返回 disposer 列表；**standing 生命周期** = 全树存活期间（进程级，首次挂载到
     teardown），**per-session JOIN 撤销** = 会话结束/重选时（撤销 agent→standing
     的 parent 链接——**spike-2 已核实：dsh-scope 原生支持运行时重绑/断链**，
     `bind_scope_parent`/`ScopeParentBinding::rebind`（lib.rs:134,140）+ 每 scope
     disposer（lib.rs:276），无需新 API、只需接线）。**per-session disposer 桶当前缺失**
     （teardown 仅 host 全局，host.rs:151,338-344）→ 需加会话级回收。
3. **Rust 原生插件行实现**（shipped preset 引用名→Rust 插件）：
   `dsh-persona`（scope 化 persona，`{{model}}`/`{{cwd}}`，`deployment:persona` 被
   scope 覆盖）、`dsh-agent-instructions`（user instructions, maxBytes）、`dsh-tool-*`
   （把 §1.5 工具包成按 scope 注册）、`dsh-agent-tool-presentation{mode:code}`、
   `dsh-plan-mode`/`dsh-compaction-*`/`dsh-skill-*`/subagent 行；`disabled: !!js …`
   → `dsh-eval` 求值（`disabled_expr` 机制已存在，loader.rs:83-119）。
4. **agent-loop 接线**：让每个会话 agent 从**其 scope** 组装 tools/prompt（ReactLoopAgent
   现经 `LoopDeps` 直驱共享 prompt/tools，`service.rs:41-128, agent.rs:718-867`）——
   组合「生效」的必经关，也是语义风险最高处（`request/header` 快照与 tool 语义回归）。
    **spike-3/5 已降险**：`agent.rs:664` 已用 `assemble_context_for(&agent)` 装配 SystemPrompt、
    `host.rs:185-187` 已按 `ctx.scope` 决议 `tools.schemas(scope)`+`known_names(scope)`、
    `tool_exec` 已传 `Some(&scope)`——**每 agent scope 已贯通 assemble/工具决议**；P4 只需
    在 standing/会话层填充作用域注册（P2/P3），未 join 的 default 保持全局=安全基线。
5. **agentPreset RPC + settings**：`agentPreset.list/read/select/copy/remove/openDocument`
   真实语义（`agent-preset-not-found`/`agent-preset-locked` 错误码、`agentPreset` header、
   `agent-preset/selected` 事件、`agent_preset` 持久化字段——**dsh-session 已具备这些
   线头**，`types.rs/format.rs`）；注册 `agent-presets` settings namespace（D-095 样式，
   `lib.rs:296-372` 现有槽可照抄）；`authorable`= 用户根存在、`hasDocument`= 可打开。
6. **内置/自定义根**：内置根 = vendored `apps/cli/config/agent-presets/*`（真正交付时
   复制进 Rust 项目自持；vendored 参考树不下发）；自定义根 = `<cwd|home>/.agent-presets/*`。
   运行时**不可**依赖 vendored 参考树。
7. **测试/验收**：TDD——组合解析单测、per-session 工具/提示词投影（两会话互不可见）、
   卸载 disposer、preset RPC surface、`agent-preset-locked` 语义；回归 149 全绿 + clippy。

## 4. 目标架构（分层）

```
web serve (60165)
  ├─ SessionHost（store 持久化，含 agent_preset meta）←events→ AgentLoopHost（D-101 per-session agent）
  │                                              │ 每个会话 agent 从其 scope 组装（join standing）
  │                                              ▼
  │   AgentLoopHost
  │     ├─ tools:  dsh-tools（ScopedLayers：global 基 + preset standing 层 + 会话自有层 shadow）
  │     ├─ prompt: dsh-system-prompt（scope 化 sections/contexts/persona，deployment:persona 被覆盖）
  │     └─ 组合挂载器 PresetComposer（对齐 mount.ts）
  │           ├─ standing mount：preset id → 稳定 ScopeKey 层（只 mount 一次）
  │           ├─ join：会话 agent ScopeKey parent → standing key（会话层可 shadow）
  │           ├─ presets/registry：内置（vendored 副本 minimal/standard/code/cordis）+ 用户 .agent-presets
  │           ├─ 守卫：未激活行/root 泄漏行 → broken（fake 不挂载）
  │           └─ 卸载：per-session join 撤销 + disposers；standing 随整树 teardown
  └─ agentPreset.* RPC + settings descriptor（agent-presets namespace，默认持久化）
```

## 5. 阶段划分 / 工作量 / 风险（供决策）

| 阶段 | 内容 | 相对量 | 风险 |
|---|---|---|---|
| P1 | preset 解析（EntryOptions + 插值）+ 用户/内置根发现 + `agentPreset.list/read` + settings namespace | 小 | 低 |
| P2 | 组合挂载器（standing mount + join + 守卫 + disposer） | 中 | 中（scope parent 链接语义） |
| P3 | Rust 插件行实现（persona/instructions/工具行/disabled 求值） | 中 | 中（`!!js` 标签转译边界） |
| P4 | agent-loop 按 scope 组装 tools/prompt（`request/header`/`tool/call` 语义回归） | 中 | 高（既有闭环语义） |
| P5 | `select/copy/remove/锁定` + 端到端验收 | 小 | 低 |

**关键诚实点**：P4 是「组合真正生效」的必经关，也是语义风险最高处（不能破坏既有
`request/header` 快照与 tool 语义）；P3 的 shipped preset 每行都要有 Rust 实现，
否则该行直接 `broken`（复用 TS discovery 的 broken 语义，诚实展示而非假挂载）。

### 5.1 架构收敛（把 dsh-core 请回 agent 平面的消费路径；与 P 阶段并行喂入）

先决：§3.0 的偏离记录意味着「presets」不能止步于 A（仅 scope 层、service 行 broken），
否则把两个运行时织物的缺口固化成永久缺口。推荐路径：

- **B（跟随 P1–P3 落地）· preset 的组合权威归位 dsh-core/loader**：preset 用
  `dsh-loader` 在 dsh-core 上挂真 entry tree（standing mount + 守卫 + 代 + LogRealm/
  GlobalRealm 服务实例，全程复用 dsh-core 现成机制），把**每行的模型可见效果**
  （tools/prompt/persona/skills）投影进 dsh-tools/dsh-system-prompt 的 scope 层（loop
  已消费）；**service 行**经**窄服务桥**映射成 loop 可消费的句柄（plan-mode/compaction
  pruner/terminals/fs shadow/workflow engine…——先支持 shipped preset 实际用到的子集，
  其余 `broken` 诚实展示）。
- **C（P5 之后独立架构里程碑）· 循环开进 dsh-core（计划弧终点）**：
  （i）loop 的全部服务访问从 LoopDeps Rc 线程换 `Cordis::get`（agent 作用域上下文）；
  （ii）`ReactLoopAgent.ctx = dsh-core ctx.extend({agent})`，让组合行 `ctx.on/provide/
  get` 直达 loop（对齐 TS `agent.ctx`）；
  （iii）AgentBus 折入 dsh-core 事件命名空间（agent/request、tools/pre-execute 等水岭
  同源）；
  （iv）每 agent 一 fiber + entry tree，isolate realm 服务实例原生可达。
  量/险最大，但「dsh-core 为核心运行时」的架构终态由此成立；presets 先交付 → 收敛有
  明确的原地迁移靶（组合已挂在 dsh-core，只换 loop 的消费面）。

> 决策影射：若用户选 A，则 preset 交付最快但 §3.0 的缺口被固化；选 B，preset 交付同时
> 把 dsh-core 请回组合权威位（推荐）；选 C，则是带收敛终点的完整架构修复，单独排期。

## 6. 决策点（已收用户定夺；执行细节转 PLAN-BC-presets-execution.md）

- **A · 内置预设根的载体（已决）**：把 vendored `apps/cli/config/agent-presets/*` **复制进
  Rust 项目自持**（推荐采纳），**并且**支持与 deepseek-harness 一致地读取**自定义 agent**
  （用户根 `.agent-presets/*` 发现+读取）。
  - **落地状态 ✅（D-102）**：`resources/agent-presets/{minimal,standard,code,cordis}/` 已
    自持（`agent.cordis.yml` 忠实转译 + `preset.yml` 字节复制 + cordis skills），
    `tools/translate-agent-presets.ps1` + `tools/verify-agent-presets.py` 可复跑；
    customized 根发现 / P1 接线待实现（见 PLAN）。
- **B · 推进粒度（已决）**：**直通 P4**——P1–P4 连续推进，组合最终真实改变会话行为。
- **C · `!!js` 求值边界（已决）**：`dsh-eval` 现成子集 + vendored 预置平台静态预排除。
- **D · 自定义预设作者体验（已决，按推荐）**：`.agent-presets/` 目录发现前置交付
  （authorable 即真），`copy/remove/openDocument` 作者流并入 P5。
- **路径（已决）**：B（组合权威归位 dsh-core/loader + 窄服务桥）→ C（循环开进 dsh-core
  收敛，独立架构里程碑）。

> 全部问题/改动点/风险/spike/阶段草稿集中在 `PLAN-BC-presets-execution.md`，供深入分析后
> 定稿。定稿 → DECISIONS D-103 + git → TDD 分段实现。所有决策遵循方法论四：关键决策落
> DECISIONS + git 互查；key 永不落盘；内置组合以「复制自持」为准，不依赖 vendored 参考树运行。

# 需求结论：服务装配单元（Phase 1 = 服务插件 entry 化 + A1 身份键 + A7 持久化写回）

日期：2026-08-26
阶段：需求分析（瀑布流阶段 1）——本文档为阶段关卡工件。
状态：**定稿（决策 D-S1..D-S5 已由用户确认；D-S5 已落 commit c76d37d）**
依据：`docs/SERVICE-ASSEMBLY-HANDOFF.md`（项目核心目标交接文档，含缺失清单 A1-A7 / 决策点 / Sprint 0）。

---

## 1. 目标（Top-down）

第一性原理：项目创建的根本意义是把 Rust 插件变成像 cordis 服务插件一样的**服务装配单元**——
配置驱动、依赖激活、可热更、可持久化回写，与 TS cordis 语义等价。它不是翻译 API，而是复刻装配模型。

自上而下拆解本次目标（收敛到可验收的最小闭环）：

- **总目标**：`cordis.yml` 里声明一个服务插件 entry（如 `dsh:services`，及未来 `llm-pi-ai`/genai
  适配器/自定义服务）→ Rust 运行时**按名解析** → **按依赖自动激活** → **配置驱动 apply** →
  **可热更**（loader create/update/remove + HMR）→ 语义与 TS cordis 等价（dsh-diff golden）。
- **本次里程碑**（用户确认范围 D-S1=A 且 D-S4=A）= handoff §7 Sprint 0 的「第一阶段落地：**服务插件
  entry 化**」**扩大版**：
  1. **entry 化**——消除 `boot()` 中对 `dsh:services` 的名称特判、以及「非 services entry 必是
     `config.wasm` loop」的假设，让服务插件（含**新增**的自定义服务 entry）经「cordis.yml entry →
     loader 按名解析 → apply」装配，作为「插件=装配单元」成立的最小闭环；
  2. **A1 身份键对齐**（D-S2=与 deepseek harness 一致）——插件身份 = **解析后的插件实现本体**
     （Arc 指针/新生代 uid），name 是解析键但同名同实现=同身份、同名新实现=新身份（cordis
     `registry.has(callback)` 的回调指针身份等价物）；
  3. **A7 持久化写回**（D-S4=A）——运行时 loader 更新（create/update/remove）除记录外**真实写回
     cordis.yml**（对齐 cordis `internal/update` → `tree.write()` 落盘）；Config.simplify 反解随
     对齐面处理。
- **判定成功的直接证据**：向 cordis.yml 追加一个**新增服务插件 entry**（受控自定义服务，
  非 `dsh:services`）也能 boot 成功、按名解析、apply 生效、其服务依赖可见——全程零新增
  boot() 特判代码；且运行时对其 update/remove 后 cordis.yml 真实落盘、重启后按落盘配置恢复。

## 2. 非目标（明确不做）

- 不做 A6 `[Service.init]` 生成器 effect、A5 intercept 合并对齐、A3/A4 依赖激活核对、B 类对齐项
  （extend/invoke、Group 折叠核对、HMR 模块热更、config simplify）——全部留后续阶段。
- 不做 A2 `!!js` 条件装配（D-S3 已定：**记录为边界**，spike 另立）。
- 不做「前端组件行的 Rust 引擎激活」（§6b 分支判断——浏览器内激活引擎是 TS 自带 cordis；Rust
  重写前端 cordis 是另一条大线，D-S1 显式排除，如需另行立项）。
- 不改 WASM loop 承载（`WasmLoopPlugin` 仍是 loop 引擎；本次仅把「哪些 entry 是 loop」的
  判定从「非 services 即 loop」改为「声明 `config.wasm` 才是 loop」）。
- 不做模型配置 CRUD / wasm 端点承载 / 前端包装（与本次装配单元开发平行的其它线，不混入）。
- 不引入 tokio/runtime 级重构（D-004/D-006 判决不变；D-115 的 Send/worker 纪律属请求面，
  不动 loader/fiber 这里）。
- 不实现「插件实现可用性」的 Node 模块系统等价物（Rust 的等价物 = 静态链接注册 +
  cordis.yml + `register_plugin`（含 WASM component 动态包，D-115-Web 已有），记为文档化偏差）。

## 3. 假设（用户需确认）

- **H1（范围）**：本次「服务装配单元开发」= Phase 1 后端服务插件 entry 化（最小闭环），而非整个
  A1-A7 缺口链。这是 handoff §7.4 的建议切入；且实证（§6b）：前端组件行的激活引擎在浏览器内
  （TS 自带 cordis），Rust 侧的后端服务插件 entry 化才是「Rust 插件成为装配单元」的直接表现。
- **H2（身份键，D-S2 已定）**：插件身份 = **解析后的插件实现本体**（与 deepseek harness 一致：
  TS `registry.has(callback)` / re-import=新身份）。Rust 等价物 = Arc 指针/新生代 uid；
  name 仍是解析键，但「同名同实现=同身份、同名新实现=新身份」——为 HMR 换代 / case-4
  （插件自处置 vs 模块消失）提供身份判定基础。<br/>**注意**：A1 的「仓库键模型」从平名
  单实现仓库升级为「实现为本的身份」，属深水区改动，**由本阶段设计（阶段 2）细化，编码阶段
  按 TDD 落地**。
- **H6（A7 落盘形态）**：运行时 loader 更新写回的目标文件 = 主 cordis.yml（含经 overlays/Include 的
  合并语义）；落盘走原子写 + 反解 Config.simplify；与模型配置 CRUD 的 `SettingsProvider::file`
  各自独立（alignment 不混线）。
- **H3（可用性来源）**：插件实现的「可用性」来自 Rust 静态注册（`register_plugin`）+ 动态注册
  （cordis.yml/WASM 组件），不是 TS 模块系统——这构成与 TS 的文档化偏差，可接受。
- **H4（等价性口径）**：等价 = 行为级（dsh-diff golden：TS 原版 cordis trace vs Rust trace
  逐行），不是字节级 API 翻译。
- **H5（新服务插件的 handles 来源）**：服务插件提供服务的**具体句柄**由装配方（host）在注册/
  apply 时提供（如现 `DshServicesPlugin::all()` 在 boot 构造），不是由配置字面声明 handles。

## 4. 硬约束

- 与 TS cordis 语义等价：`crates/dsh-diff`（TS 原版 cordis 跑同一 JSON 剧本 → 规范化 trace →
  golden；Rust dsh-core 跑同一剧本 → 逐行对比）。**每个新增语义必须补一条 dsh-diff golden**。
- m 系列装配测试：`crates/dsh-loader/tests/{m2_loader,m3_isolate,m3_include,m3_expr,m7_await,
  m14_loader_async,m15_hmr}.rs`——新增语义必须有（红→绿）对应。
- 回归：`cargo test -p dsh-cli -p dsh-loader -p dsh-wasmrt -p dsh-core` 全绿 + clippy `-D warnings` 零。
- 既有 `dsh web` / `--agent-loop`（Rust 原生 loop 驱动）路径零回归。
- 决策日志：本任务每个关键决策追加 DECISIONS.md，改动 → git 提交 → 决策条目可互查。
- key 纪律（`DEEPSEEK_API_KEY` 等仅进程环境，永不落盘/入 git）与许可证纪律照旧。

## 5. Rust 现状缺口（自下而上核实）

| 项 | 现状（已读源码实证） | 结论 |
|---|---|---|
| loader 按名解析 | `register_plugin(name, Arc<dyn Plugin>)`（loader.rs:385，`plugins: HashMap<String,Arc<dyn Plugin>>`）；`create → start_entry → load_plugin`（loader.rs:423/674）→ `plugins.get(&name)`（loader.rs:724-728）→ `ctx.plugin_arc(plugin, config)` | ✅ **不是缺口**，按名解析已存在 |
| 服务插件形态 | `DshServicesPlugin`（services.rs:20-233）`impl Plugin`，`name()="dsh:services"`，apply → `ctx.provide("sessions"/"tools"/"llm", ...)` + 声明式 tools/llm | ✅ 已是插件形态 |
| cordis.yml 声明 | `target/web/cordis.yml` 已有 `services` entry（`name: dsh:services, config: {services: [sessions]}`）+ `loop` entry | ✅ 声明已存在 |
| **boot 名称特判** | lib.rs:174 `loader.register_plugin("dsh:services", Arc::new(DshServicesPlugin::all()))` —— 服务插件「实现可用性」在 boot 代码硬编码，handles 在 lib.rs 内联构造（`new_session()/new_tool_registry()/new_llm()`） | ⬜ **缺口 1**：`dsh:services` 名称在 boot 被特判 |
| **boot 非 loop 假设** | lib.rs:176-198 `for entry { if entry.name == "dsh:services" { continue } ; let wasm = config.wasm ...ok_or("needs config.wasm")?; ...WasmLoopPlugin::new_owned... }` | ⬜ **缺口 2**：任何非 `dsh:services` entry 强制要求 `config.wasm` → **声明第二个/任意服务插件 entry 会 boot 失败** |
| **A1 身份键** | `PluginHandle = String`（registry.rs:34，注释「M0=名称键；M2 起扩展 manifest hash」）；仓库是平名 `HashMap<String, Arc<dyn Plugin>>`（loader.rs:40）——同名多实现/新实现替换无法表达 | ⬜ **需实现**（D-S2 已定：实现为本的身份，与 harness 一致；设计阶段细化） |
| **A7 持久化写回** | `LoaderState.writes: Vec<String>`（loader.rs:41-42，注释明言「写回记录（持久化 no-op）」）；`write()`（loader.rs:662-664）/`take_writes()`（loader.rs:416-418）只记录不落盘；无 YAML 回写路径 | ⬜ **需实现**（D-S4=A：运行时更新真实写回 cordis.yml） |
| 等价性基建 | `scenarios/` 有 17 个剧本（含 `06-dependency-gate.json`、`07-intercept-merge.json`、loader-*/include-*）+ `diff/ts-host/verify-diff.mjs`（TS host 差分编排）；loader 单测 m 系列齐全 | ✅ 可扩展「服务插件依赖激活」剧本 |
| 依赖激活核心 | handoff §1.1 已确认：`check_impls`/`refresh_fiber`（epoch）/`begin_load/finish_load`（notify）/`fail_fiber`（runtime.rs:574-702）+ `ctx.provide/get/inject` | ✅ 已存在（Phase 1 复核是否足用，见测试） |
| 工作树基线 | `crates/dsh-cli/src/lib.rs` 原有 **157 行未提交改动**（`register_model_config_settings`——模型配置 CRUD 线的 WIP） | ✅ **已处置**（D-S5=A：commit `c76d37d` 保留为模型配置 CRUD 线检查点；工作树现干净） |

## 6. 已确认事实（关键澄清）

- **loader 按名解析不是本轮要做的事**：它已成立（loader.rs:724-728）。本轮真正的缺口在
  **boot() 装配循环的假设**（缺口 1+2），即「哪条 entry 是 loop、哪条是服务」的判定。
- `dsh:services` entry 目前经 `include.load()` → loader 正常路径 apply 已经发生；「entry 化」
  的实质是：**让自定义服务插件 entry（非 `dsh:services`）也能被声明而不会 boot 失败**，并把
  「名称 → 实现」的登记从 boot 内联特判收敛为可扩展的注册面。
- WASM loop 与服务的分野：WASM plugin（`WasmLoopPlugin`/`WasmComponentPlugin`/
  `WasmRemoteEndpointPlugin`）都 `impl Plugin`，可按名进 loader 仓库；loop 引擎的 `run_turn`
  需要具体类型（`WasmLoopPlugin`）——故「loop entry」检测以 `config.wasm` 为标记是正当的
  （良性分型），而「其余必是 loop」是错误假设。

## 6b. 关键调研结论：deepseek harness 如何把前端组件也作为装配单元（实证）

调研来源：`.spec/service-assembly/harness-frontend-assembly-research.md`（子代理对 harness fork
`deepseek-harness/` 的只读检索；全部断言带 文件:行号）。

**核心答案：前端插件与后端服务插件是同一个「插件=装配单元」模型——同一份 vendored
`@deepseek-ai/cordis` 的 Context/Fiber/Loader 运行时，唯一区别是「代码到达层」**：
前端用 `__DSH_BOOT__` 清单 + `ClientModuleSystem`（引擎内 lazy CJS 模块表）挂到
`ctx.loader.internal`（`vendor/loader/src/config/tree.ts:145-159` → `ClientModuleLoader.import`
`packages/client/modules/src/client/system.ts:189-204`），替代 Node 的 ESM loader；后端用 Node
内部 ESM。装配单元的五要素在前后端完全一致：

| 要素 | 前端（web 组件） | 后端（服务插件） | Rust 现状 |
|---|---|---|---|
| 声明 | package.json `dsh.client`（platform='web'/inject/external/immediately，`modules/src/index.ts:49-63,126-146`）+ `exports["./client"]` 导出 `inject` 数组 + `apply(ctx)`（`ui-commands/src/client/index.ts:48-73`） | 插件行 `{id, name(模块 specifier), config, inject?}`（`vendor/loader/src/config/entry.ts:9-22`） | `Plugin` trait：`name()/inject()/config_schema()/apply(ctx,config)`（registry.rs:14-32）——**同构** |
| 按名解析 | `ClientModuleRegistry` 把 loader entries 谱成 `__DSH_BOOT__ {rev, entries:[{id,url,rev,inject?}]}`（`modules/src/index.ts:167-176,429-463`），url=`/plugins/<id>/client.js?rev=` | `tree.import(name)` 模块解析（`tree.ts:145-162`） | `loader.load_plugin` 按 `entry.name` 查 `plugins.get(name)`（loader.rs:724-728）——**已成立** |
| 依赖激活 | fiber 对每个 inject 服务求 epoch，缺→PENDING；`provide→notify→_refresh` 自底向上连锁（`reflect.ts:314-336`、`fiber.ts:597-623`） | 同一 cordis | `check_impls` / `refresh_fiber`（epoch）/ `begin_load-finish_load`(notify)（runtime.rs:574-702）——**核心已存在** |
| 配置驱动 apply | `internal/config` waterfall 在 fiber 激活前插值（`loader/src/index.ts:92-101`） | 同一 | `apply(ctx,config)` + `config_schema` 校验（registry.rs:23-31）——**已存在** |
| 可装配可替换缝 | `ctx.slots`：声明式 SlotMap + `register({name,children,store,inject})`，single/list/keyed/chain + root/session-maybe/session（`ui-slots/src/index.ts:88-91,741-789`）；`slots.inject` 声明生命周期延迟注册（`runtime/src/client/slots.ts:143-205`） | cordis 服务（provide/inject/intercept） | Rust `ctx.provide/get/intercept`（context.rs）——服务缝已存在；slots 是 UI 层缝 |

**Rust 侧对应的现实（关键澄清，直接回答用户之问）**：
- Rust `dsh web` **已经把「前端组件作为装配单元」落实在承载面**：`build_boot_manifest` 扫描
  `dsh.client.platform=="web"` 包生成 `__DSH_BOOT__` roster（D-005/D-115-Web D1），
  `/plugins/<id>/client.js` 服务 bundle；浏览器内的**激活引擎是 TS harness 自带的 JS cordis**
  （`assertEntriesActive` 强制全 ACTIVE，`web/src/boot.ts:138-158`）。Rust 只做「配置驱动的
  roster 生成 + bundle 服务」，不重写前端 cordis。
- 因此「服务装配单元」在 Rust 侧的真正待办是**后端服务插件**：让 Rust 插件（`dsh:services`、
  未来的 llm-pi-ai/genai 适配器/自定义服务）成为与前端行**同型**的「cordis.yml 声明 →
  按名解析 → 依赖激活 → 配置驱动 apply」装配单元——这正是 Phase 1 entry 化的对象。
- **分支判断**：若用户心中「服务装配单元」包含「前端组件行也由 **Rust 引擎**激活」
  （即重写浏览器端前端插件激活引擎），那是与后端 entry 化**不同的一条大线**——不作为
  Phase 1 隐含范围，需用户明示（见 D-S1 的 A/B/C 之外显式排除）。

## 7. 测试与验收标准（阶段关卡）

- **红 → 绿（TDD 主证据）**：向 cordis.yml 追加一个受控「新增服务插件 entry」（fixture 服务，
  提供可被消费者观察的服务）→ 修改前 `boot()` 红（`needs config.wasm` 失败）→ 修改后绿
  （按名解析 + apply 生效 + 服务依赖可见 + `dsh:services` 不回归）。
- **A1（身份键，D-S2）**：红测锁定「同名**新实现**重新注册/换代 → 新身份」与「同名同实现 →
  同身份（幂等）」；HMR/换代语义按 harness 口径（`registry.delete(旧)+registry.plugin(新)`）验证。
- **A7（持久化写回，D-S4）**：红测锁定「运行时 create/update/remove → cordis.yml 真实落盘（原子写）；
  重启读回落盘配置 → 装配恢复」；Config.simplify 反解随对齐面处理。
- **等价性（handoff §5 机制）**：新增/扩展一条「服务插件依赖激活」dsh-diff 剧本
  （TS 原版 cordis 跑同一剧本 → golden，Rust 对齐逐行）；或扩展 `scenarios/06-dependency-gate.json`。
- **m 系列**：对应新增语义补 m 系列红测（红→绿），覆盖 entry 创建/apply/disabled/hot 更新路径。
- **回归**：本任务 4 crate（dsh-cli/dsh-loader/dsh-wasmrt/dsh-core）全绿 + 全 workspace 无失败
  + clippy `-D warnings` 零。
- **live 复验（部署阶段）**：`dsh web target/web/cordis.yml --agent-loop ...` 含新增服务 entry
  时 boot 成功、UI/请求面零回归（按既有门控冒烟纪律）。

## 8. 决策收敛记录（已定稿，用户全部确认）

| 决策 | 选项 | 结论（用户裁决） |
|---|---|---|
| **D-S1 本次范围** | A) 仅 Phase 1 后端服务插件 entry 化 / B) entry 化 + A3/A4 依赖激活核对 / C) 全部 A1-A7（前端 component 行的 Rust 引擎激活显式排除，需则另立项） | **A**：Phase 1 entry 化。经实证（§6b）「Rust 装配单元 = 后端插件走 cordis.yml→按名→apply」 |
| **D-S2 A1 身份键** | A) 二维键(来源,name)+版本 / B) 文档化偏差（平名） / C) 与 deepseek harness 一致（实现为本身份） | **与 deepseek harness 一致**：身份 = 解析后的插件实现本体（Arc 指针/新生代 uid）；name 为解析键，同名同实现=同身份、同名新实现=新身份 |
| **D-S3 A2 `!!js` 条件装配** | A) 本轮实现 / B) 记录为边界，spike 另立 | **B**：记录为边界 |
| **D-S4 A7 持久化写回** | A) 本轮做（YAML 落盘 + Config.simplify 反解） / B) defer（与模型配置 CRUD 对齐另立） | **A**：本轮做 |
| **D-S5 未提交 WIP** | A) commit 保留 / B) stash / C) 忽略 | **A**：commit `c76d37d`（模型配置 CRUD 线检查点） |

> 阶段结论：需求分析关闸工件定稿 → 进入阶段 2（系统设计）。A1/A7 的实现细节（身份键结构、
> 落盘事务/反解、与既有 include/HMR 的接线）在设计阶段细化并按 TDD 落地。

## 9. 遗留边界（如实记录，非本次目标）

- A2 `!!js`（D-S3=边界）、A3/A4（依赖激活核对）、A5（intercept 合并）、A6（生成器 effect）、B 类
  对齐项（extend/invoke、Group 折叠、HMR 模块热更、config simplify 完整版）仍为后续阶段
  （handoff §3 完整清单）。
- TS 的「插件实现可用性 = Node 模块系统」在 Rust 是静态注册 + 动态包（WASM component），
  与 cordis 的模块 specifier 语义存在**文档化偏差**（A1 身份键定稿后并入决策日志）。
- 前端组件行的激活引擎在浏览器内（TS 自带 cordis）；Rust 侧「重写前端 cordis 引擎」显式
  排除（D-S1），需则另行立项。
- A7 落盘与模型配置 CRUD 的 `SettingsProvider::file` **各自独立**（alignment 不混线但语义对齐）。
- dsh-diff 的 TS host 差分（`diff/ts-host`）与 in-crate 差分两套并存；新增剧本归属以
  handoff §5 的机制为准。
- 多进程/多租户 / 插件热二进制动态加载（非 cordis.yml 声明型）不属本次。

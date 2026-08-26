# 服务装配单元（Service as Assembly Unit）架构接手文档

日期：2026-08-26
作者：dsh-web 项目组
定位：项目核心目标的技术交接文档。目标读者 = 接手该架构演进的新 agent。
前提：读本文档前先读 `.spec/models-config-crud/requirements.md`（模型配置 CRUD 对齐）与
`docs/ARCHITECTURE.md`（若存在）· `docs/DECISIONS.md`。

---

## 0. 为什么这是项目的根本目标

用户裁定：**「把 Rust 插件变成像 cordis 服务插件一样的『服务装配单元』是项目创建的根本
意义和基石。这个不完成，其他所有操作（模型配置 CRUD、wasm 端点承载、前端包装……）都
是在偏离核心目标。」**

换句话说：DeepSeek Harness（TS）的价值不只是「能聊天」，而是**一切能力都是可装配、可
替换、可组合的 cordis 服务插件**（llm-deepseek / llm-pi-ai / settings / credentials /
goals / … 都是 cordis.yml 里的一行插件）。**Rust 重写（dsh-rs）的根本意义是让这套「配置
驱动、依赖激活、可热更」的插件模型在 Rust 里成立**——不是翻译 API，而是复刻装配模型。

因此，本任务的验收不是「某个端点可用」，而是：**cordis.yml 里声明一行插件（如 llm-pi-ai 或
自定义服务），Rust 运行时能按名解析、按依赖自动激活、配置驱动、可热更、可持久化回写——与
TS cordis 语义等价**。

---

## 1. 现状：Rust 已有什么（自读实证，非推测）

以下全部来自 `crates/` 源码核读，是**已有、可复用**的：

### 1.1 装配引擎（dsh-loader + dsh-core）核心已在
- **Plugin trait**（`crates/dsh-core/src/registry.rs:14-32`）：`name() / inject() /
  config_schema() / apply(ctx, config)` —— 与 Cordis 插件契约同构（Base.name/inject/
  Config/apply）。
- **Cordis loader Rust 移植**（`crates/dsh-loader/`）：Entry / EntryGroup / EntryTree /
  update 四分支事务 / include / isolate / intercept / HMR（`hmr.rs` 存在）。
- **LoaderState.plugins 仓库**：`HashMap<String, Arc<dyn Plugin>>`（`loader.rs:40`）按名解析：
  `load_plugin`（`loader.rs:715-786`）从仓库查名 → isolate/intercept → `ctx.plugin_arc`
  → fiber 关联。
- **响应式依赖激活**（`crates/dsh-core/src/runtime.rs:574-702`）：
  `check_impls`（fiber 按 scope 解析 inject）、`refresh_fiber`（epoch = 依赖提供者 uid 拼接，
  变化→Load/Unload）、`begin_load/finish_load`（apply + notify 依赖方）、`fail_fiber`。
  这是 Cordis `_checkImpl/_refresh/_setEpoch` 的等价核心。
- **服务提供**：`ctx.provide/get/intercept/isolate`（`context.rs:1270,1356,1475,454`）；
  `ctx.inject(deps, callback)`（`context.rs:510-516`）包装为 InjectPlugin。
- **fiber/effect**：`FiberState`（`fiber.rs:15-22`）、`EffectOutcome`（`fiber.rs:42-53`，
  含 `Await` = M27 的 `[Service.init]` 等价子集）。

### 1.2 已实现的服务插件
- **`DshServicesPlugin`**（`crates/dsh-wasmrt/src/services.rs:20-233`）：`impl Plugin`，
  `name()="dsh:services"`，apply 里 `ctx.provide("sessions"/"tools"/"llm", Arc)` +
  声明式 tools/llm（`register_declared_tools/llm`）。**这就是 Cordis 服务提供者插件的 Rust 版**。
- **wasm 插件**：`WasmLoopPlugin`（`loop.rs:476`）、`WasmComponentPlugin`
  （`component.rs:200`）、`WasmRemoteEndpointPlugin`（`remote.rs:228`）都 `impl Plugin`。

### 1.3 装配的「当前形态」（关键缺口的来源）
- **web cordis.yml 已有配置声明**（`target/web/cordis.yml`）：
  ```yaml
  - id: services
    name: dsh:services
    config: { services: [sessions] }
  - id: loop
    name: echo-loop
    config: { wasm: echo-loop }
  ```
- **但装配是代码特判**（`crates/dsh-cli/src/lib.rs:170-198`）：
  - L174：`loader.register_plugin("dsh:services", Arc::new(DshServicesPlugin::all()))` ——
    服务插件**硬编码**注册，不走「cordis.yml entry → loader 按名解析」。
  - L176-197：对非 services 的 entry **特判 `config.wasm`**（从文件读组件构造
    `WasmLoopPlugin::new_owned`），不是按 `entry.name` 从插件仓库解析。
- **`--agent-loop` 时实际推理由 Rust 原生 loop 驱动**（session.prompt → run_rust_loop 走
  m6_llm 的 DeepSeekAdapter），WASM loop 仅是 boot 必需占位（cordis.yml 注释明说）。

### 1.4 等价性测试基础设施（极有价值）
- **`crates/dsh-diff/`**：用 **TS 原版 cordis 跑同一 JSON 剧本** 输出规范化 trace，Rust
  `dsh-core` 跑同一剧本，逐行对比（golden 固化 TS 输出）。这是「Rust 装配行为 ≡ TS cordis」
  的**现成验收机制**。
- **m 系列测试**：`crates/dsh-loader/tests/m2_loader.rs`、`m3_isolate.rs`、`m3_include.rs`、
  `m7_await.rs`、`m14_loader_async.rs`、`m15_hmr.rs` 等大量装配语义测试。

---

## 2. 对标基准：Cordis 装配契约（deepseek-harness/vendor/cordis/，subagent 权威提取）

### 2.1 插件定义
- `Plugin = Function | Constructor | Object`（`registry.ts:92-96`），Base = `{ name?, Config?,
  inject?, provide?, intercept? }`（`registry.ts:100-111`）。
- `inject`：`string[] | { [k]: intercept }`（数组=无拦截配置，`registry.ts:19,71-89`）。
- `Config` 校验：StandardSchema，激活时（非注入前）校验，失败→ValidationError→FAILED
  （`fiber.ts:50-62,641-655`）。
- **身份键 = 解析后的回调函数指针**（`registry.ts:197,322`）：re-import=新插件身份，
  HMR/自处置 case 4 靠 `registry.has(callback)` 判定（`loader/index.ts:137-140`）。

### 2.2 装配管线
- cordis.yml 扁平 entry 列表：`{ id, name(模块 specifier!), config?, group?, disabled?,
  inject?, intercept?, isolate? }`（`config/entry.ts:9-22`、`config/isolate.ts:6-9`）。
  **`name` 是 Node 模块 specifier（如 `@deepseek-ai/dsh-llm-deepseek`），不是展示名**。
- Include（文件树）→ EntryGroup（并行 create，失败回滚）→ `Entry._init`：
  `tree.import(name)` 按名模块解析 → `registry.plugin(plugin, config)` → `fiber.await()`
  （`config/entry.ts:259-302`）。
- **import = 按名做模块解析**（`config/tree.ts:145-162`）：`cordis:` 内建 / `internal.import` /
  相对或裸 specifier。
- **依赖顺序 = 隐式等待**：无显式拓扑排序；提供者先 Active→`notify` 拉起等待的依赖方。

### 2.3 依赖与服务
- `ctx.provide(name, value, check?)`（`reflect.ts:277-305`）：effect 包装，ACTIVE 时
  `notify([name])`；unprovide 先 notify 唤醒依赖方再自清（`reflect.ts:297-303`）。
- `_getImpl` strict 要求提供者 ACTIVE（`reflect.ts:237-243`）；check 谓词（`reflect.ts:124`、
  `Service[check]`、loader 的 `await` 门 `loader/index.ts:166-170`）。
- **epoch**：依赖提供者 uid 拼接（`fiber.ts:611-623`），reload 快照注入实现
  （`fiber.ts:647`）。
- Service：构造即 `ctx.reflect.provide(name, self, Service[check])`（`service.ts:42-59`）；
  `[Service.resolveConfig]` 合并祖先 intercept 链 + base/head（`service.ts:86-102`）。
- 长效插件形态是** async generator `[Service.init]`**：`yield () => this.stop()` 先注册停止
  再 await 启动体（`group.ts:125-128`、`include.ts:273-289`、`hmr.ts:199-205`）。

### 2.4 配置式装配
- **「插件名→实现」= 模块 specifier**：`id`（诊断/补丁 id）+ `name`（模块）+ `config`（Config
  schema 输入）+ `!!js`（config/disabled 的 `with(ctx) eval`，`config/utils.ts:5-9`）。
- patch 层：bundle 的 `cordis.patch.yml` → profile → 用户层，单遍 id 索引
  （`include.ts:58-128`、`profile.ts`）；裸包名经 `profiles/node_modules` 扁平回退。
- disabled/conditional：沿父链继承（`entry.ts:84-108`）。

### 2.5 动态性
- fiber 级 `update(config)`（`fiber.ts:736-753`）；entry 级四分支事务（`config/entry.ts:142-246`，
  含 patchContext(仅 config) vs 替换(rollback)）；group 级并行事务（`group.ts:59-106`）。
- HMR：config 文件→Include.refresh→root.update；插件模块→清 ESM/CJS 缓存→重新
  import→`registry.delete(旧)+registry.plugin(新)`（`hmr/index.ts:400-549`）。

---

## 3. 缺失清单（核心装配契约缺口，按优先级）

> 标注：✅ = 已有（自读确认）；⬜ = 缺失/需实现；⚠️ = 部分有、需核对对齐。

### A. 核心契约缺口（优先做）

**A1 ⬜ 插件身份键模型**（最深的差异）
Cordis 以「解析后的回调」为身份键（re-import=新身份，HMR/case-4 依赖它）；Rust 是平名仓库
`name → Arc<dyn Plugin>`，无法表达「同名多实现/新实现替换」。落地：仓库键改
`(baseUrl/来源, name) → 实现` + 版本号；或显式声明为文档化偏差。影响 HMR 语义与 case-4
「插件自处置 vs 模块消失」。

**A2 ⬜ `!!js` 求值作用域缺 ctx 服务**
Cordis `with(ctx) eval(expr)` 可读注入服务（`config/utils.ts`）；Rust `eval_scope`
（`loader.rs:121-133`）的 `ctx` 是空对象。凡依赖服务的 config 表达式语义缺失。需把求值
作用域绑定到当前 fiber 的注入就绪上下文。

**A3 ⚠️ 提供者可用性谓词 check + strict-active**
Cordis `provide(name, value, check)` + `_getImpl` strict（提供者必须 ACTIVE）。Rust
`provide` 是否有 check 谓词待核对（`context.rs:1270`）；若无，依赖等待的准确性受影响。

**A4 ⚠️ 注入快照与 unprovide 唤醒顺序**
Cordis reload 快照注入 + unprovide「先 notify 依赖方再自清」（`reflect.ts:297-303`）；
跨隔离边界 `ctx.get` 沿父 fiber walk（`reflect.ts:153-167`）。Rust `refresh_fiber`/`notify`
已有核心（`runtime.rs:585-702`），但 unprovide 顺序与父链 walk 需逐一对齐。

**A5 ⚠️ 拦截配置合并 `[Service.resolveConfig]`**
Cordis 祖先 intercept 原型链 + base/head + `Config.merge`/`Object.assign`（`service.ts:86-102`）。
Rust `intercept`（`context.rs:1475`）存在，合并层级/优先级需对齐。影响 pi-ai 这类
「inject 对象形态 intercept 配置」的服务。

**A6 ⬜ 类插件 `[Service.init]` 生成器形态**
Cordis 长效插件 `async* [Service.init] { yield () => stop(); await … }`（先注册停止再启动）。
Rust `EffectOutcome::Await` 只覆盖 await 一个 future，**无**迭代器/异步迭代器 effect 的逐项
收集与 epoch 中途取消（`fiber.ts:375-395`）。需实现生成器 effect（等价 M27 承诺的完整形态），
否则卸载顺序（先卸子项）与「init 失败前 disposer」语义缺失。

**A7 ⬜ 持久化写回**
Cordis `internal/update` 把 config 写回 entry 并经 `tree.write()` 落盘（Include 防抖原子写
`include.ts:344-374`）；Rust `writes` 只是记录（`loader.rs:42,416`）。运行时更新不落盘。
需实现 YAML 落盘（含 `Config.simplify` 反解回写，`loader/index.ts:106`）。

### B. 已实现需对齐（谨慎核对，非核心）

**B1 ⚠️** `[Service.extend]` 派生作用域实例 + 可调用服务（`service.ts:65-73`）。
**B2 ⚠️** Group 折叠差异：Rust 在 loader 层展开、无独立 Group 插件 fiber（dsh-loader
`lib.rs:5`）；Cordis Group 是带 init 的插件并可共享 isolate realm（事件顺序/`[Service.init]`
await/「group 与消费者同 realm」约定需验证）。
**B3 ⚠️** HMR 模块热更：Rust `hmr.rs` 存在，但无 Node 依赖图 accepted/declined 分类与双
模块缓存清理——需换「插件身份版本替换 + 受影响 entry reload」等价合约；externals→全重载
语义亦需同构。
**B4 ⚠️** config `simplify` 回写 unparse：Rust 落盘需 dsh-schema 提供 simplify。

### C. 已确认已有（不用重做，只需接线）
- Plugin trait / entry 树 / update 事务 / include / isolate / intercept / notify / epoch /
  fiber / 服务提供者（DshServicesPlugin）。

---

## 4. 关键架构决策点（接手先行确认）

1. **A1 的仓库键模型**：二维键（来源,name）+ 版本 vs 文档化偏差。这决定后续所有 HMR/动态
   装配的实现形态，必须先定。
2. **服务插件 entry 化**：消除 `lib.rs` 的 `if name == "dsh:services"` 特判与 `config.wasm`
   特判——让服务插件（DshServicesPlugin、genai 适配器包）也走「cordis.yml entry → loader
   按名解析 → apply」。这是「插件=装配单元」的直接表现，建议作为第一阶段落地。
3. **A2 的 `!!js`**：若产品需要条件装配（`!!js process.env.X === 'y'`）则必须做；否则记录为
   边界。
4. **持久化（A7）**：结合模型配置 CRUD 的 `SettingsProvider::file`（用户已选 Rust 自有文件）
   与凭据落盘——entry 配置持久化与其对齐。

---

## 5. 验收与验证方法

- **行为等价**：`crates/dsh-diff`（TS 原版 cordis vs Rust 跑同一剧本对比 trace）——每个新增
  语义必须补一条 diff 剧本/golden。
- **m 系列装配测试**：`m2_loader`（entry/update）、`m3_isolate`、`m7_await`（依赖等待）、
  `m14_loader_async`、`m15_hmr`——新增语义必须有对应（红→绿）。
- **真实场景**：cordis.yml 声明 llm-pi-ai（或自定义服务）→ Rust 运行时按名解析、依赖
  激活、配置生效、可热更、持久化回写。
- **回归**：`cargo test -p dsh-cli -p dsh-loader -p dsh-wasmrt -p dsh-core` 全绿 + clippy 0。

---

## 6. 相关文件索引

- CRust 装配引擎：`crates/dsh-core/src/{registry,context,runtime,fiber}.rs`、
  `crates/dsh-loader/src/{loader,entry,group,include,hmr}.rs`
- Rust 服务插件：`crates/dsh-wasmrt/src/{services,loop,component,remote}.rs`
- Rust 装配特判点：`crates/dsh-cli/src/lib.rs:160-230`（boot 装配）、`target/web/cordis.yml`
- TS 权威基准：`deepseek-harness/vendor/cordis/src/{registry,fiber,reflect,context,service}.ts`、
  `vendor/loader/src/config/{entry,group,isolate,tree}.ts`、`vendor/include/src/index.ts`、
  `vendor/hmr/src/index.ts`
- 等价性测试：`crates/dsh-diff/`
- 前置模型配置需求：`.spec/models-config-crud/requirements.md`
- 决策日志：`docs/DECISIONS.md`

---

## 7. 接手建议的第一步（Sprint 0）

1. 读本文件 + `crates/dsh-loader/src/loader.rs`（重点 load_plugin/start_entry/update 事务）
   + `crates/dsh-core/src/runtime.rs`（重点 notify/check_impls/refresh_fiber）。
2. 用 `dsh-diff` 跑一个「服务插件依赖激活」剧本，确认当前 trace 与 TS 的差异点（找到第一
   个行为缺口）。
3. 按 A1→A2→A7→A6→A5 排序，每项先写红测试（m 系列或 dsh-diff golden）再实现。
4. 第一阶段落地「服务插件 entry 化」：让 `dsh:services` 在 cordis.yml 声明即装配（消除
   lib.rs 特判），作为「插件=装配单元」成立的最小闭环。

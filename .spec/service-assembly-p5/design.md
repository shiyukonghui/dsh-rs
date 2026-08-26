# 设计：服务装配单元 Phase 5 — A5 对象形态 inject 拦截配置合并（[Service.resolveConfig] + pi-ai）

日期：2026-08-27
阶段：系统设计（瀑布流阶段 2，Phase 5）——本文档为阶段关卡工件。
依据：`.spec/service-assembly-p5/requirements.md`（需求定稿，A5-SCOPE=A / A5-GOLDEN=A 用户确认）。
参照：npm `cordis` 4.0.0-rc.8 `lib/index.js`（fiber 构造/`Inject.resolve`）与 `@deepseek-ai/cordis src/service.ts`。

---

## 1. 设计目标

给 dsh-core/dsh-loader 补「**对象形态 inject**」：插件可声明 `inject: { 'svc': cfg }`——除依赖名外
携带该服务的 intercept 配置；装载时配置写入**本 fiber 自身 intercept 层最内层**，服务
`resolve_config` 以最高优先级合并（对齐 cordis `Inject.resolve`/fiber 构造 + `[Service.resolveConfig]`）。
新增 1 个对象形态 inject golden（TS 原版 ↔ Rust 逐行）作等价主证据；m-series 锁定。不改既有
`ctx.intercept()` 层叠合并不动（07 已对齐）。

## 2. 自下而上锚点（本阶段核实）

| 锚点 | 基址 | 用途 |
|---|---|---|
| cordis `Inject.resolve` | npm cordis `lib/index.js:1274-1288`：array→`{name:null}`；object→`{name:cfg??null}` | 依赖名集 = 全部键；对象条目即拦截配置 |
| cordis fiber 构造 | `lib/index.js:699-705`：`this.ctx[intercept] = Object.create(parent[intercept])`；`layer[name]=cfg`（本 fiber 自身最内层） | 注入配置落点 |
| cordis `[Service.resolveConfig]` | `@deepseek-ai/cordis src/service.ts:86-102`：链 walk（`in` + `hasOwn` unshift，深层显）/ base 前置 / head 后置 / `Config.merge`\|`Object.assign` | base/head 语义（本项目仅浅合并，DIV） |
| Rust `Plugin::inject()` | `dsh-core registry.rs:18-21`（名字数组） | 新增可选 `inject_configs()` |
| Rust `runtime.register_plugin` | runtime.rs:516-580（`inject = plugin.inject()…`；`pending_intercept`→`f.intercept`） | S1 注入钩子 |
| Rust `resolve_config` | context.rs:1636（父链 walk + per-layer 值 + base/head + 浅合并） | 无需改（数据进入 f.intercept 即生效） |
| DSL/TS host | scenario-host.mjs `buildPlugin`（`desc.inject` 数组）`resolve-config` op（94-104）| S3：`desc.injectConfig`（对象）+ resolve-config 已可链 walk |

## 3. 设计分解

### S1（dsh-core + dsh-loader：Plugin 注入配置通道 + 装载最内层）

```text
// registry.rs（Plugin trait，新可选方法，默认空，不破坏既有实现）
fn inject_configs(&self) -> Vec<(String, Value)> { Vec::new() }

// runtime.rs register_plugin（S1 钩子）
inject（依赖名）= plugin.inject() 名字 ∪ inject_configs() 键（去重）
fiber 自身 intercept 层 = pending_intercept（entry 声明）+ plugin.inject_configs()（后 append 者同名胜）
```

- 依赖名集合并：插件对象形态 inject 的键**同时是依赖**（cordis `Object.keys(this.inject)` = deps，
  `lib/index.js:724/932`）——`register_plugin` 把配置键并入 `inject`（去重）。
- 最内层：`f.intercept.extend(pending_ic)` 后 `extend(own_cfgs)` → `resolve_config` 的 per-layer
  值（后者覆盖）+ 父链 walk（对子代可见）→ 注入配置以**最高优先级**进入本 fiber 的
  `resolve_config`，且子代沿父链可见（T1/T2/T3）。`resolve_config`/`ctx.intercept()` 本体不改。

### S2（m-series 红测，crates/dsh-loader/tests/m20_object_inject.rs）

| # | 红测 | 断言（绿） |
|---|---|---|
| T1 | 父 `ctx.intercept(srv,{a:1,p:1})` + 子插件 `inject_configs={srv:{a:9,b:2}}` + 子 `resolve_config(srv)` | `{a:9,b:2}`（子注入配置最内层 > 父 intercept + 同键后者覆盖） |
| T2 | `resolve_config(srv, Some(base{b:0}), Some(head{h:9}))`（base → 注入层 → head） | `{b:0,a:9,h:9}`（base 最低 / head 最高） |
| T3 | 父 `inject_configs={srv:{p:1}}` → 子 `resolve_config(srv)` | 子读到 `{p:1}`（注入配置沿父链对子代可见） |

- 需提供 `srv`（注入键即依赖）：m20 用 provider 插件 `ctx.provide("srv", …)` 使消费者 Active。
- `common` 的 `FnPlugin` 增 `inject_configs` 字段 + 构造器（默认空，不影响既有测试）。

### S3（DSL + TS host + golden）

- **DSL**：`PluginDesc` 增 `inject_config: Option<serde_json::Map<String, Value>>`（对象形态）；Rust
  `ScenarioPlugin`：`inject()` 含该配置键（泄漏扩展）；`inject_configs()` 返回其条目。
- **TS host**：`buildPlugin` 对 `desc.injectConfig` 设 `plugin.inject = desc.injectConfig`（对象；
  cordis `Inject.resolve` 原生处理），`resolve-config` op 已支持链 walk（含本 fiber 注入层）。
- **golden**：`scenario-13-object-inject-config` —— 父 provide+srv + `ctx.intercept(srv,{a:1,p:1})`
  + 挂载子（子 `injectConfig={srv:{a:9,b:2}}` + `resolve-config:srv`）→ `{"a":9,"b":2}`（TS 原版 ↔ Rust
  逐行）；与 07 平行但最内层由**对象形态 inject** 提供（A5 本质）。

### S4（回归 + 可观测）
- `verify-diff.mjs` 23 场景全通过（22 既有逐字节不变 + 新增）。
- 受影响 crate + workspace + clippy `-D warnings` 0；serve 冒烟 HTTP 200（无运行面破坏）。

## 4. 实现顺序（TDD）

1. **S1**：`Plugin::inject_configs()` + `runtime.register_plugin` 编排（m20 引用缺失 API → E0599 红 →
   绿）。独立提交。
2. **S2**：m20 T1-T3 全绿（随 S1 或独立，回滚点内）。
3. **S3**：DSL/TS-host + `scenario-13` golden（TS 原版生成）→ Rust 逐行一致。独立提交。
4. **S4→阶段 4**：workspace + clippy + verify-diff 23 全过。
5. **阶段 5**：serve 冒烟 + acceptance 收口。

## 5. DIV / 让步清单

- **DIV-5-1**：合并为浅合并（`Object.assign` 语义）；cordis `Config.merge`（深合并）不做——缺 pi-ai
  深合并证据，用户确认按浅合并范围（需求 §2）。
- **DIV-5-2**：依赖注入模型 = 配置键并入依赖名集（cordis 语义）；若插件只想拦截不想依赖，cordis 无此
  区分（键即依赖），对齐原样——需求文档记录。
- **DIV-5-3**：`Plugin::inject_configs()` 返回 `Vec`（每次调用构造）；插件元数据量小，性能可忽略；
  plugin 内部跨调用一致（确定性元数据约定）。

## 6. 部署与回滚（阶段 5 预案）

- 部署：`Plugin::inject_configs()` 为可选新方法（默认空，既有实现零改动）；装载并入最内层；pi-ai 类
  插件可经此通道声明拦截配置。无运行面破坏。
- 回滚：`git revert` 本阶段提交（S1+S2 随特征级整体；S3/golden 可独立回滚）。

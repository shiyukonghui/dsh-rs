# 需求结论：服务装配单元 Phase 5 — A5 对象形态 inject 拦截配置合并（[Service.resolveConfig] + pi-ai）

日期：2026-08-27
阶段：需求分析（瀑布流阶段 1，Phase 5）——本文档为阶段关卡工件。
状态：**定稿（范围用户确认：A5-SCOPE=A 对象形态 inject 全流程；A5-GOLDEN=A golden 等价证明）**
依据：`docs/SERVICE-ASSEMBLY-HANDOFF.md` §3 A5 + §0 验收（「配置式装配」维度）+ HANDOFF §2.3。

---

## 1. 目标（Top-down）

第一性原理：cordis 插件可写 **`inject: { 'svc': cfg }`**（对象形态）——除声明依赖名外，还**携带
该服务的 intercept 配置**；cordis fiber 装载时把它写入**本 fiber 自身的 intercept 层最内层**
（`fiber.ts:700-705`：`Object.create(parent[intercept])` + `layer[name] = cfg`），于是服务
`[Service.resolveConfig]` 在该 fiber 内读到的 config = `base → 根 → … → 本 fiber 注入层 → head`，
**注入配置在最内层（最高优先级）**。此类 thi/i 主动态——`pi-ai` 服务即「inject 对象形态 intercept
配置」的消费者（HANDOFF A5）。Rust `Plugin::inject() -> &[&str]` 只有名字数组，**缺失配置通道** →
pi-ai 语义无法表达。A5 补齐并用 TS 原版 golden 证明逐行等价。

**验收** = 对象形态 inject 红测（注入配置成为最内层 + `resolve_config` 最高优先级合并 + 对子代可见）
全绿 + 1 个对象形态 inject golden（TS 原版 ↔ Rust 逐行） + 既有 22 场景零回归 + 受影响 crate/
workspace 全绿 + clippy 0 + serve 冒烟。

## 2. 非目标（明确不做）

- **不**做 cordis `Config.merge` **深合并**（service.ts:97-98 静态深合并；JSON Schema 可深合并非默认）——
  本轮以 `Object.assign`（浅合并）为口径；仅当 pi-ai 实际配置含嵌套对象且期望深合并证据出现时后续立项
  （DIV-5-x）。用户已确认当前范围。
- **不**改既有的 `ctx.intercept()` 层叠合并（runtime `resolve_config` 父链 walk + per-layer 值、base/head）——
  该路径已由 07-intercept-merge golden 对齐且本轮不动。
- **不**用 apply 内 `ctx.intercept()` 打桩模拟「对象形态 inject」（把装载元数据与运行期副作用混为一谈是
  常见错，见 §复盘）。
- **不**做 `[Service.extend]`（B1）派生作用域/可调用服务——B 类后续。

## 3. 假设（含复盘确认）

- **H1 建模**：对象形态 inject = `Plugin::inject_configs() -> Vec<(String, Value)>`（默认空，新可选方法，
  不破坏既有实现）；装载时并入本 fiber 自身 intercept 层（最内/最高优先级）。
- **H2 优先级**：`resolve_config` 合并序 = `base`（最低）→ 根 → … → 本 fiber（含注入配置，最内） →
  `head`（最高）；同层同名后者覆盖；浅合并。
- **H3 对子代可见**：注入配置随本 fiber 的层进入父链，子代 fiber 的 `resolve_config` 亦可见（对齐 cordis
  原型链继承）。
- **H4 等价证据**：1 个对象形态 inject golden（TS 原版↔Rust 逐行）+ m-series 红→绿 + 既有 22 零回归。

## 4. 硬约束

- 每个新语义落 m 系列红→绿；新 golden 由 `verify-diff.mjs` 全量通过（TS 原版生成、Rust 逐行一致）。
- 既有 22 golden 逐字节不变；`cargo test` 相关 crate + workspace 全绿；clippy `-D warnings` 0。
- DECISIONS 追加；改动 → git 提交 → 决策条目互查。

## 5. 现状缺口（自下而上核实，带依据）

| 项 | 现状（源码实证） | 结论 |
|---|---|---|
| cordis 对象形态 inject | `service.ts:86-102` `[resolveConfig]`：原型链 walk + base/head + `Object.assign`/`Config.merge`；`fiber.ts:700-705` 把 inject 对象条目写进本 fiber 自身层 | ✅ 参照已锁定 |
| Rust `intercept()` | `context.rs:1606` `ctx.intercept(name, config)` 存在；`resolve_config`（context.rs:1636）父链 walk + per-layer 值 + base/head + 浅合并 | ✅ 已有（07 golden 对齐） |
| Rust `Plugin::inject()` | `registry.rs:18-21` **仅名字数组**（`&[&str]`），无配置通道 | ⬜ **缺口：对象形态 inject 配置缺失** |
| fiber 装载 | `runtime.register_plugin`（runtime.rs:524 `inject = plugin.inject()…`）只建依赖名；`pending_intercept`（loader:897-906）只带 entry options 声明的 intercept | ⬜ 需在 fiber 自身层并入插件 inject-configs |
| 对子代可见 | `resolve_config` 沿 `fd.parent` 收集（context.rs:1641-1654）——本 fiber 注入项在链上即对子代可见 | ✅ 机制已有，只缺注入数据 |
| DSL/TS host | `scenario-host.mjs` `buildPlugin` 支持 `desc.inject`（名字）；`applyOp` 有 `intercept`/`resolve-config`（scenario-host:86-104） | ⬜ 需支持插件级 inject 配置条目 → 注入到 fiber 自身层 |
| 测试落点 | m 系列在 `dsh-loader/tests/`（m16/m18/m19 先例） | m20 |

## 6. 测试与验收标准（阶段关卡）

- **T1 最内层注入**：插件 A 声明 `inject_configs = { srv: cfg }` → 装载后 `resolve_config("srv")` 含 `cfg`
  且优先级**高于**父纤维的 `ctx.intercept(srv, …)`（最内层；cf. 07 的 child 覆盖 parent）。
- **T2 base/head 序**：`resolve_config(srv, base, head)` → `base → 注入层 → head` 合并序正确。
- **T3 对子代可见**：父声明注入配置 → 后代 fiber 的 `resolve_config` 读到（父链 walk 含注入项）。
- **T4 golden**：对象形态 inject 场景（父插件 + inject 配置 + 子 resolveConfig，含 base/head）TS 原版
  ↔ Rust 逐行一致。
- **回归**：既有 22 golden 零回归；workspace + clippy 0；serve 冒烟（部署阶段）。

## 7. 决策收敛

| 决策 | 结论 |
|---|---|
| A5-SCOPE | **A：对象形态 inject 全流程**（用户确认）——`Plugin::inject_configs()` 新可选方法 + 装载并入 fiber 自身最内层 + `resolve_config` 最高优先级 + 等价证明；不破坏既有实现 |
| A5-GOLDEN | **A：新增对象形态 inject golden**（用户确认）——TS 原版↔Rust 逐行一致；m-series 作锁定 |

## 8. 遗留边界

- `Config.merge` 深合并不做（缺 pi-ai 深合并证据；DIV-5-x 后续可按需立项）。
- B1 `[Service.extend]`/B2 Group 折叠/B4 config simplify/A3 动态 check 按目标优先级后续（本轮=A5 先行）。
- 浏览器 E2E（`--dump-dom`）按仓纪律代偿。

## 复盘追问结论（需求阶段已向用户确认）
- **假设**：对齐口径 = 对象形态 inject 最内层（H1-H3）；浅合并口径（H4 证据）。
- **缺失信息**：pi-ai 实际配置是否含嵌套深合并需求——已向用户点明，用户确认当前按浅合并范围走；
  若后续出现 pi-ai 深合并证据，回本阶段更新范围。
- **常见错误**：把「插件声明时的注入配置（装载元数据）」与「apply 内 `ctx.intercept()`（运行期副作用）」
  混为一谈——本轮以装载层/最内层语义实现，不用 apply 打桩。

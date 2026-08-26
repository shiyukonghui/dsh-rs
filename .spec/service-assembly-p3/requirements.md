# 需求结论：服务装配单元 Phase 3 — B3 HMR 模块热更（身份换代 → 受影响 entry reload）

日期：2026-08-26
阶段：需求分析（瀑布流阶段 1，Phase 3）——本文档为阶段关卡工件。
状态：**定稿（范围由用户确认：B3 HMR 模块热更）**
依据：`docs/SERVICE-ASSEMBLY-HANDOFF.md` §3 B3 + §0 验收（「可热更」维度）+ §5 验收方法。

---

## 1. 目标（Top-down）

第一性原理：验收五维（按名解析/依赖激活/配置驱动/**可热更**/持久化回写）中，前四维已在 Phase 1/2
落地或核对，「**可热更**」仍缺**插件实现级**热更——现 `hmr.rs` 只做**配置文件** watcher
（registerConfig 层），不做**插件实现体**热更。Phase 1 的 A1（`PluginIdentity`/`generation`/
`Entry.identity`）恰为此铺好检测基础。本阶段落地 handoff B3 等价合约：

- **插件身份版本替换**：宿主对某 name 注册**新实现**（A1：新身份 + generation 递增）→ 所有以
  **旧身份**加载的 entry（`Entry.identity == 旧`）**自动 reload 新实现**（同 id/options 保 entry，
  仅换 fiber 起的实现）。
- **受影响 entry reload**：reload 后服务提供者以新实现 apply；**服务依赖方经 epoch 自动重估**重激活
  （「externals→全重载」的 Rust 同构——Rust 无 Node 模块图，依赖方重活等价 importers 重载，记 DIV）。
- 与 cordis `registry.delete(旧)+registry.plugin(新)`（hmr/index.ts:400-549）语义同构。

**验收** = 身份换代红测（T1 换实现重载 / T2 依赖方重活 / T3 同实现幂等）全绿 + `dsh-diff` 21 场景
零回归 + 受影响 4 crate + workspace 全绿 + clippy `-D warnings` 0 + serve 冒烟零回归。

## 2. 非目标（明确不做）

- 不实现 Node 模块图（imports graph）的精确同构——Rust 无模块系统；「externals→全重载」以
  「服务依赖方经 epoch 重活」等价承载（DIV-3-1）。
- 不做 A6 生成器 effect `[Service.init]`、A5 intercept resolveConfig、A2 `!!js`（仍为边界）——后续。
- 不改 hmr.rs 配置文件 watcher 机制（registerConfig 层保持）；本阶段只加**实现级** replace/reload 层。
- 不重写 dsh-core 依赖激活核心（epoch/notify 已正确，Phase 2 已证等价）。

## 3. 假设

- **H1 身份换代入口** = `Loader::replace_plugin(name, new_impl)`（或等价公开 API）：内部走 A1
  `register_plugin` 语义（同名同 Arc 幂等/新 Arc 新身份+generation），随后驱动受影响 entry reload。
- **H2 entry 保真**：reload 保持 entry id/options/group 归属不变，仅 fiber 以新实现重启
  （与 remove+create 的破坏性区分；避免 dispose/group 归属抖动）。
- **H3 依赖方传播**：reload 后提供者 fiber 换代 → 依赖方 epoch 变化 → 自动重活（等价 cordis
  importers 重载）；若 epoch 因 fiber uid 复用而不变，则由 `reload_stale` 显式刷新受影响依赖方
  （设计阶段以自下而上核实为准）。
- **H4 等价口径**：行为级（m-series 断言 + dsh-diff 既有零回归）；本例无新 dsh-diff golden——
  DSL 无法表达「同 name 换实现」（DIV-3-2），等价证据以 m-series 红→绿为准。

## 4. 硬约束

- 每新增语义补 m 系列红→绿；`node verify-diff.mjs` 21 既有场景零回归（golden 逐字节不变）。
- `cargo test -p dsh-core -p dsh-loader -p dsh-diff -p dsh-wasmrt -p dsh-cli` 全绿 + workspace +
  clippy `-D warnings` 0。
- 决策日志 DECISIONS 追加；改动 → git 提交 → 决策条目互查。

## 5. 现状缺口（自下而上核实，带行号）

| 项 | 现状（已读源码实证） | 结论 |
|---|---|---|
| A1 检测数据 | `register_plugin` 同名新 Arc → 新身份+generation（loader.rs:392-408）；`Entry.identity` 记录解析身份（loader.rs，Phase 1）；访问器 `plugin_identity/plugin_generation/entry_identity` | ✅ 就绪（Phase 1） |
| update replace 分支 | `loader.update` 的 `replace = diff.name \|\| diff.group \|\| diff.inject`（loader.rs:575,1121）——**同 name 换实现不触发 replace**（config-only → fiber.update，runtime 键不变） | ⬜ **缺口 1**：无「同 name 换实现」重载路径 |
| remove+create | 可换新实现（重解析当前注册），但**破坏性**（dispose/group 归属抖动、entry 生命周期重启） | ⬜ 缺 entry 保真 reload |
| 实现级热更 API | 无 `replace_plugin`/`reload_stale`（identity 换代驱动）层；hmr.rs 只做配置文件 watcher | ⬜ **缺口 2**：需新增实现级 replace/reload 层 |
| dynamic 替换 | `dynamic_activate`：`register_plugin(新实现)` + `create(新 entry_id)`（remote_host.rs:203-206）；**无同 entry 换代** | ⬜ 可扩展面 |
| 依赖方重活 | epoch = owner uid 拼接（runtime.rs:633-666）；reload 后 uid 是否换 → 依赖方 epoch 是否变 **待设计期自下而上核实** | ⚠️ 设计期定 |

## 6. 测试与验收标准（阶段关卡）

- **T1 换实现重载**：register v1 → create entry（apply 记 v1、`Entry.identity=v1`）→ region v2（不同
  Arc，generation 递增）→ `replace_plugin(name, v2)` → entry 自动 reload：apply 记 v2、`Entry.identity
  = v2`、fiber Active（同 id/options 保真）。
- **T2 依赖方重活**：provider 提供 svc + consumer inject svc 均 Active → `replace_plugin(provider)` →
  consumer 经历 Unload/Pending→（provider 新实现 apply）→ Active（依赖方 epoch 重估 = externals 同构）。
- **T3 幂等**：`replace_plugin(name, 同一 Arc)` → 无换代（generation 不变）→ 无 reload（no-op）；
  `reload_stale` 对无 stale entry 为 no-op。
- **m 系列**：上述红测落 dsh-loader 新测试或 m16 扩展；任一缺行为先红。
- **回归**：`verify-diff.mjs` 21 零回归；4 crate + workspace + clippy 0；serve 冒烟（部署阶段）。

## 7. 决策收敛

| 决策 | 结论 |
|---|---|
| P3-SCOPE | **A：B3 HMR 模块热更**（用户确认） |
| DIV-3-1 externals→全重载 | Rust 无模块图 → 以「服务依赖方经 epoch 重活」等价承载；若 uid 复用阻断 → 显式刷新受影响依赖方 |
| DIV-3-2 等价证据 | 本例无新 dsh-diff golden（DSL 无法表达同 name 换实现）；以 m-series 红→绿为等价主证据 + 既有 21 场景零回归 |

## 8. 遗留边界

- A6/A5/A2 仍后续；cordis 模块图 externals 精确同构不成立（DIV-3-1）；浏览器 E2E 通道按仓纪律代偿。

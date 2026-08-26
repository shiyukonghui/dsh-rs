# 需求结论：服务装配单元 Phase 2 — A3 提供者 check/strict-active + A4 注入序列对齐

日期：2026-08-26
阶段：需求分析（瀑布流阶段 1，Phase 2）——本文档为阶段关卡工件。
状态：**定稿（范围由用户确认：A3+A4 依赖激活核对）**
依据：`docs/SERVICE-ASSEMBLY-HANDOFF.md` §3 A3/A4 + §5 验收方法；`.spec/service-assembly/`（Phase 1 工件）。

---

## 1. 目标（Top-down）

第一性原理：Phase 1 已把「服务插件 entry 化 + 身份键 + 持久化」落地；「**按依赖自动激活**」这一
验收维度仍缺**等价性证明与顺序对齐**。本阶段把 Rust 依赖激活核心（provide/check_impls/refresh_fiber/
notify/unprovide/resolve）在下列子点上与 TS cordis **逐一对齐**，并用 dsh-diff golden 固化，发现的
分歧以 TS 为权威修复 Rust（对齐意为 trace 级行为一致，handoff §5 口径）：

- **A3a 提供者可用性谓词 check**：实现在场但谓词不成立 → 依赖方保持 PENDING；谓词成立（或缺省）→
  激活。等价证据：provide-with-check 场景 golden（check=false 时 consumer 不激活）。
- **A3b strict-active**：仅 ACTIVE 提供者的实现可作为依赖（loading/unloading 不喂依赖等待）；provide
  仅允许 ACTIVE fiber 调用（`InactiveEffect`，context.rs:1284）。等价证据：已有 06 的 Loading→Active
  时序 + 补 strict 面断言。
- **A4a unprovide 顺序**：unprovide 触发依赖方重估（缺失 → PENDING），事件序与 TS「先 notify 再自清」
  （reflect.ts:297-303）一致。Rust disposer = `remove_impl→notify`（context.rs:1308）——**待 golden
  判定是否存在可观察次序差，有则对齐**。
- **A4b 注入快照 / epoch**：reload 依赖重估与重启语义（`refresh_fiber` epoch 已有，runtime.rs:633-666）；
  补充 golden。
- **A4c 跨隔离父链 walk**：隔离 realm 内 `ctx.get`/依赖解析沿父 fiber 链落到祖先/根作用域
  （`resolve_scope` 父链 walk，runtime.rs:301-315）。等价证据：跨 realm 解析 golden / m3_isolate 扩展。

**验收** = 每子点至少一条 dsh-diff golden（TS 生成、Rust 逐行一致）+ 需要的 m 系列红测；发现的
顺序/谓词分歧在 Rust 侧修复（TS 为权威并记录 DIV）；`dsh-diff` 全量（18→21+ 场景）零回归 +
`cargo test -p dsh-core -p dsh-loader -p dsh-diff -p dsh-wasmrt -p dsh-cli` 全绿 + clippy `-D warnings` 0。

## 2. 非目标（明确不做）

- 不做 A6 生成器 effect `[Service.init]`、A5 intercept `resolveConfig` 合并、B 类对齐（B1-B4）——后续。
- 不重写依赖激活核心（自下而上证实 check/strict-active/epoch/notify/父链 walk 核心**已存在**）——
  只做核对、缺口修复、golden 固化、顺序对齐。
- 不改 Plugin trait / loader 仓库 / A7 持久化（Phase 1 面零改动）。
- 不引入新依赖（纯现有 crate + diff 基建）。

## 3. 假设

- **H1 等价口径**：行为级 trace（dsh-diff golden），handoff §5 机制；「分歧」= trace 行序列不同，
  以 TS 为权威（仓库既有 vendored-TS-权威纪律）。
- **H2 check 谓词形态**：Rust `CheckFn = Box<dyn Fn()->bool>`（reflect.rs:11）；dsh-diff 场景 DSL 用
  **静态布尔** `check` 表达（CheckFn 的动态态变无法在静态剧本表达）——check=false 锁定「缺谓词→
  PENDING」等价，check 动态翻转后续（spike 观察）。
- **H3 父链 walk**：等价判定 = 隔离 realm 内的依赖解析最终落到哪个 impl（祖先 realm/根 realm），与
  块级时序；以 loader 级 isolate 场景 + m3_isolate 现有覆盖扩展为准。

## 4. 硬约束

- 每个新增语义补一条 dsh-diff golden；m 系列红→绿；`dsh-diff` 18 既有场景**零回归**（golden
  逐字节不变）。
- `cargo test -p dsh-core -p dsh-loader -p dsh-diff -p dsh-wasmrt -p dsh-cli` 全绿 + workspace 全量 +
  clippy `-D warnings` 0。
- 决策日志 DECISIONS 追加；改动 → git 提交 → 决策条目互查。

## 5. 现状缺口（自下而上核实，带行号）

| 子点 | 现状（已读源码实证） | 结论 |
|---|---|---|
| A3a check 谓词 | `CheckFn`/`Impl.check`/`check_ok()`（reflect.rs:11-34）；`check_impls` 用 `check_ok()`（runtime.rs:616）false→依赖方 PENDING；`provide_with(name,value,check)`（context.rs:1275） | ✅ 核心已有；**零 golden 覆盖**（18 剧本均无 check 用例）→ 补 |
| A3b strict-active | provide 仅 ACTIVE fiber 可调（context.rs:1284 `InactiveEffect`）；Loading→Active 时序在 06 golden 可见 | ✅ 已有；补 strict 面断言/golden |
| A4a unprovide 顺序 | disposer = `remove_impl→notify`（context.rs:1308）；TS「先 notify 再自清」（reflect.ts:297-303） | ⚠️ **顺序待 golden 判定**，若有可观察差 → 对齐 |
| A4b epoch/注入快照 | `refresh_fiber` epoch（runtime.rs:633-666）、check_impls 重算 store | ✅ 已有；补 reload golden |
| A4c 父链 walk | `resolve_scope` 沿父链找 isolate 映射（runtime.rs:301-315）；`resolve_impl` 经 scope 查全局 store（runtime.rs:412） | ✅ 已有；补跨 realm golden + m3_isolate 扩展 |
| dsh-diff 基建 | scenario-host.mjs/loader-host.mjs `provide` op 无 `check` 参数；Rust `ApplyOp::Provide{service,value}` 无 check | ⬜ **DSL 需扩展**：provide op 增可选 `check` 静态字段（TS+Rust 两侧对称） |

## 6. 测试与验收标准（阶段关卡）

- **新增 dsh-diff 剧本**（每子点 ≥1，TS 生成 golden、Rust 对齐）：
  - `scenario-10-provide-check-gate`：provider `provide svc` 带 `check:false` → consumer(`inject:["svc"]`)
    保持 PENDING（provider Active 但 consumer 不激活）；对照 `check` 缺省/true 激活（06 已证）。
  - `scenario-11-strict-active-gate`（或并入 10）：补 strict 面（Loading 期不可当依赖）断言。
  - `loader-14-unprovide-order`：loader 入口 provider unprovide/unload → consumer 重估 PENDING，
    事件序（Active:Unloading→Pending）与 TS 一致（06 的 unload 已有部分覆盖，本剧本以 loader 按名
    + disposer 路径锁序）。
  - `loader-15-cross-realm-walk`：隔离 realm consumer 依赖在父/根 realm 的 provider 服务 → 解析落
    对 realm（父链 walk）；含 get 跨 realm。
- **m 系列红→绿**：m3_isolate 扩展（跨 realm get）+ m7_await 补 check 谓词红测；任一新增语义先红。
- **DSL 对称扩展**：scenario-host.mjs 与 Rust `ScenarioPlugin::apply_op` 的 `Provide` 增可选 `check`
  （bool）——红因缺字段（TS 侧解析失败/ Rust 侧未知字段）→ 绿。
- **回归**：`node diff/ts-host/verify-diff.mjs` 全量（含既有 18）PASS；4 crate + workspace + clippy 0。
- **部署冒烟（阶段 5）**：既有 `dsh web` serve 冒烟零回归（Phase 1 acceptance §3 口径，无 key 门控）。

## 7. 决策收敛

| 决策 | 结论 |
|---|---|
| P2-SCOPE | **A：A3+A4 依赖激活核对**（用户确认） |
| DIV-2-1 顺序分歧处置 | 以 TS 为权威修复 Rust（继承「vendored TS 权威」纪律），修复记入 DECISIONS |
| DIV-2-2 check 谓词 golden 形态 | 静态 bool（动态态变 spike 另立）；DSL provide op 增可选 check |
| DIV-2-3 父链 walk 等价判定 | 以「解析落 realm + 块级时序」为准，loader 级 isolate 场景承载 |

## 8. 遗留边界

- A6/A5/B1-B4 仍后续；A3 的动态 check 谓词（态变）spike 另立；浏览器 E2E 通道按仓纪律代偿。

# 需求结论：服务装配单元 Phase 10 — A3 动态 check spike（谓词运行时再求值）

日期：2026-08-27
阶段：需求分析（瀑布流阶段 1，Phase 10）——本文档为阶段关卡工件。
依据：`docs/SERVICE-ASSEMBLY-HANDOFF.md` §A3 + cordis reflect/fiber 源码实证 + §0 验收。

---

## 1. 目标（Top-down → Bottom-up）：spike 问题定义

HANDOFF A3：「Cordis `provide(name, value, check)` + `_getImpl` strict（提供者必须 ACTIVE）。Rust
`provide` 是否有 check 谓词待核对；若无，依赖等待的准确性受影响。」

**自下而上核对结论**：
- **HANDOFF 的直接问题已否**——Rust `provide` **有** check 谓词：`ctx.provide_with(name, value,
  Some(CheckFn))`（context.rs:1406）+ `check_ok()`（reflect.rs:29）+ `runtime::check_impls`
  （runtime.rs:630，按作用域解析 impl 并求值谓词）+ 静态门已被 `m7_await::await_gated_by_check_predicate`
  与 `scenario-10-provide-check-gate.golden` 锁定。
- **剩「动态 check」spike**（目标原文）：验证谓词在**运行时**的再求值触发点与 cordis 对齐。

**cordis 再求值触发点（源码实证）**：
1. provide 时提供者 ACTIVE → `notify`（reflect.ts:294-296）；
2. unprovide（disposer）→ `notify`（reflect.ts:297-303）；
3. 提供者 fiber ACTIVE↔NON-ACTIVE 翻转 → `notify`（fiber.ts:588-594 状态变更钩子）；
4. notify → 依赖方按注入名 `_checkImpl`（**重求值 check**，fiber.ts:597-609；不成立删除 store →
   epoch INACTIVE）→ `_refresh` → 转换。

**Rust 对照（自读）**：provide-while-Active → notify（context.rs:1427）；unprovide disposer → notify
（context.rs:1437）；finish_load(→Active) → 通知已提供服务（runtime.rs:703-722）；**reload = 卸载
（run_unload 跑 provide disposer → remove_impl+notify）→ 重 apply 再 provide → finish_load notify**——
触发点 1/2/3 在 Rust 由 produce-disposer 驱动的卸载路径覆盖。纯谓词翻转（无 notify 触发点）在 cordis
**非反应式**——Rust 须同位。

**验收（pass criterion）** = m25 spike 测试 5 断言绿：静态门 / 纯翻转非反应式 / 重载+true→依赖方激活 /
重载+false→依赖方返回 Pending / 往返再激活。无生产代码改动（机制已存在——spike 锁定，非修复）。

## 2. 非目标

- **不改** dsh-core / loader 生产代码（spike 验证；若 m25 红再评估，红期回归需求）。
- **不做** golden 于动态翻转（TS 场景 DSL 无法表达运行期 flag 翻转；m-series 证据，A 类 spike 例外）。
- **不做** 谓词翻转的自动反应式（cordis 亦非反应式；不引入 notify 广播——超出 cordis 语义）。

## 3. 假设（复盘确认）

- **H1**：Rust reload（`update_with`）路径 = 卸载（disposer 运行）→ 重 apply——谓词翻转经 re-provide
  + finish_load notify 生效（cur. cordis `_reload` 的 provide 效应重注册同径）。
- **H2**：`update_with` 对 provider 无需 config 变更也重载（不依赖 diff）。
- **H3**：m-series 证据（动态翻转不可 golden；spike 以 m25 锁定 parity）。

## 4. 硬约束

- m25 5 断言绿；203→205 目标递增（m24 204 + m25）；workspace + clippy 0；23 golden 零回归。
- DECISIONS 追加；改动 → git 提交 → 决策条目互查。

## 5. 复盘追问结论

- 直接问题（「有无 check」）已答——**有**（对照 m7/golden/源码）。合法再触发点对齐；纯翻转非反应式
  系 cordis 语义非缺口。
- 常见错误：误以为「check 翻转应立即反应」——cordis 只在 notify 触发时再求值，若把纯翻转做成反应式
  反而**越界**。

## 6. 遗留边界

- 谓词内状态变更不自动广播（cordis 同位，非缺口）。组中组/嵌套隔离的 check 覆盖由 m3_isolate +
  loader-15 同 realm 机制保障（非本 spike 范围）。

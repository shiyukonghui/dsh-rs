# 需求结论：beyond 目标 Phase A4 — 注入快照 / unprovide 唤醒顺序 / 父链 walk 逐一收口

日期：2026-08-27
阶段：需求分析（瀑布流阶段 1，Phase A4）——本文档为阶段关卡工件（用户已确认）。
依据：`docs/SERVICE-ASSEMBLY-HANDOFF.md` §A4 + fork reflect/fiber 源码实证 + 现有实现自读。

---

## 1. 目标（第一性原理 + 双视角）

HANDOFF A4 ⚠️：「Cordis reload 快照注入 + unprovide『先 notify 依赖方再自清』（reflect.ts:297-303）；
跨隔离边界 `ctx.get` 沿父 fiber walk（reflect.ts:153-167）。Rust `refresh_fiber`/`notify` 已有核心
（runtime.rs:585-702），但 unprovide 顺序与父链 walk 需逐一对齐。」

**自下而上（Rust 现状实证）**：
- **unprovide 顺序**：cordis provide disposer = 删 impl 记录 → `notify` 唤醒依赖方（`await
  allSettled`）→ **最后**删自身 `fiber.store`（"ensure self access before dependencies cleanup"）。
  Rust `provide_with` disposer（context.rs:1436-1445）= `remove_impl`（global services + 同帧删 self
  `f.store`）+ `notify` + run transitions。cordis 的「stale 自访问窗口」仅存在于 provide disposer 自身
  异步体内；后续 disposer 串行运行在它完成后 → **observable 契约 = ① notify 先于后续 disposer（依赖方
  先于提供者剩余 teardown 放松）② 端态 ctx.get → None**——两侧应一致。
- **父链 walk**：Rust `resolve_scope`（runtime.rs:307-321）沿父链查 isolate 映射取作用域 → 全局表
  `services[(scope,name)]`（resolve_impl）。cross-realm 被 loader-15/m3_isolate 锁定；3 层嵌套 +
  inject 名边界 stop 未专门验证。
- **reload 注入快照**：cordis `_reload` `this.store = { ...this._store }`（快照后重跑 apply，
  fiber.ts:647）。Rust reload = 卸载→重 apply；`f.store` 由 `check_impls`/`refresh_fiber` 重算——需
  验证依赖方在提供者 reload 期间的去活/重活与快照一致性。

**收敛（用户确认 Q1=B：m27 + TS golden）**：A4 = 三件以 **golden + m-series 双证据**逐一验证/对齐；
golden 现只能用「卸载自访问 + 排序 + 3 层 walk + reload」场景集表达，需先扩 loader-host/dsh-diff
DSL（新增 `dispose-check` apply op，两侧同构）。

## 2. 目标 / 非目标

- **目标**：
  1. DSL 扩展（loader-host.mjs TS + crates/dsh-diff Rust）：plugin apply 新 op
     `{op:'dispose-check', service:'svc'}`——在 apply 序列该位置注册一 disposer，卸载时
     `trace: dispose-check:{service}:{JSON(get)}`（读 ctx.get(service)）。
  2. Golden 场景（新增，入 verify-diff 23→N）：
     - G1 unprovide 唤醒排序：provider apply=[dispose-check, provide]（dispose-check 先注册 →
       逆向后运行）→ loader-remove provider；断言 notify（依赖方卸载）先于 dispose-check、
       dispose-check 端态 null、不落 disabled。
     - G2 walk 边界 3 层：嵌套 groups（3 层）+ isolate，consumer 沿父链取到外层 provider；另一
       consumer 的注入名在边界隔离 → Pending（walk 停止）。
     - G3 reload 注入快照：loader-update 提供者 config → 依赖方去活/重活序列 + dispose-check 排序。
  3. m27（core/loader 层锁定，确定性）：unprovide 自访问端态 / walk 边界 / reload 快照三断言。
  4. 若 golden 暴露真实偏差 → 修 skill 对齐（红期回归需求）。
- **非目标**：不改 scope 分配/服务表结构；不动 replace_plugin/remove_plugin 既有 API；不引入
  「stale 窗口」的绕行机制（仅当 golden 明确驱动才对齐）。

## 3. 约束

- 新语义 green + workspace + clippy 0；**既有 23 golden 零回归**；serve 冒烟。
- DECISIONS 追加；改动 → git 提交 → 决策条目互查。

## 4. 验收标准（阶段关卡）

- DSL 双侧同构；A4 goldens（≥3 场景）双侧逐行一致；m27 断言绿；全回归。
- G1 若显示 notify 与 dispose-check 的排序/端态与 cordis 不一致 → 对齐后重验。

## 5. 复盘追问（已确认 Q1=B）

- **默认成立的假设**：cordis「stale 自访问窗口」仅存在 provide disposer 异步体内，dispose-check
  （后续独立 disposer）在窗口外 → 两侧同为 null（若 golden 反证则对齐）。父链 walk 停止性 = 注入名
  边界（isolate 映射）+ 作用域落根；Rust resolve_scope 已同构（loader-15 佐证）。
- **缺失信息**：cordis 在 unload 时 notify 依赖方后的「await allSettled」在 Rust sync disposer 不可
  await——但排序契约（notify 事件先行、端态一致）可 golden 验证；严格 await 对齐仅在 async 卸载
  （unload_async）已有。
- **常见错误**：把「stale 自访问窗口」当成必须给 Rust 的 `ctx.get` 加 per-fiber store 回退——该窗口
  在 observable 层不可达（后续 disposer 运行在窗口后），应先用 golden 实证再决定，避免改结构。

## 6. 遗留边界

- A4 只做「验证 + 有偏差才对齐」；结构性 per-fiber store 回退不做（除非 golden 反证）。

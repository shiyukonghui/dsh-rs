# 设计：beyond 目标 Phase A4 —— 注入快照 / unprovide 唤醒顺序 / 父链 walk（golden + m27）

日期：2026-08-27
阶段：系统设计（瀑布流阶段 2，Phase A4）——本文档为阶段关卡工件。
依据：`.spec/service-assembly-a4/requirements.md`（定稿）+ fork 源码实证。

---

## 1. 设计目标

A4 三件以 **TS golden + m27** 双证据逐一验证/对齐。golden 需要把「卸载期自访问 + 排序」表达进场景
DSL → 先扩 `loader-host.mjs`（TS）与 `crates/dsh-diff`（Rust）的 plugin apply op。

## 2. 自下而上锚点

| 锚点 | 基址 | 用途 |
|---|---|---|
| cordis collect/dispose 逆序 | fiber.ts:356-400（effect/return 收集→逆序）、415-…（effect） | 排序语义 |
| Rust collect → Many 逆序 | fiber.rs:132-164（`collected.iter().rev()`） | 排序同构 |
| cordis provide disposer | reflect.ts:297-303（删 store → notify → await 依赖方 → 删 self store） | unprovide 契约 |
| Rust provide_with disposer | context.rs:1436-1445（remove_impl + notify + run transitions） | 对照 |
| cordis ctx.get walk | reflect.ts:153-167（沿 fiber.store 上走 + isolate 门 + inject 名 stop） | walk 边界 |
| Rust resolve_scope/resolve_impl | runtime.rs:307-321/418-422（父链 isolate → 全局表） | walk 同构（loader-15 佐证） |
| reload 快照 | cordis fiber.ts:647（`store = {..._store}`）；Rust reload=卸载→重apply | G3 |
| loader-host DSL | loader-host.mjs（apply op: log/log-config/provide；steps: sync/create/update/remove） | 扩点 |

## 3. 设计分解

### S1（DSL 扩展，双侧同构）

plugin apply 新 op `{ op: "dispose-check", service: "svc" }`：
- **语义**：在 apply 序列**该 op 位置**注册一个 disposer；卸载时 `trace: dispose-check:svc:{JSON(ctx.get("svc") ?? null)}`。
- **TS（loader-host buildPlugin）**：op 若在 `ctx.provide` 之前 → 先 `ctx.fiber.effect(() => () => { trace.push(...read ctx.svc...) })`；之后同序完成（收集逆序）。
- **Rust（dsh-diff apply 解释）**：op 位置调用 `ctx.effect(...EffectOutcome::One(disposer))` 注册；与 provide op 的 `provide_with` 同按调用序收集，卸载逆序执行（fiber.rs:132-164）。

### S2（A4 goldens，3 场景，create 到 `scenarios/` + `.golden`）

| 场景 | 结构 | 断言本质 |
|---|---|---|
| `loader-22-unprovide-wakeup-order` | provider apply=[dispose-check, provide(svc)]；consumer inject svc；loader-sync → **loader-remove provider** | 卸载逆序：provide-disposer（remove→notify→consumer Unloading）**先于** dispose-check；`dispose-check:svc:null`；consumer 未落 disabled |
| `loader-23-walk-3level-boundary` | 3 层嵌套 groups + isolate；consumer 沿父链得外层 provider（Active）；另一 consumer 注入名在边界隔离 → Pending（walk 止） | walk 边界 = isolate 门/inject 名 stop |
| `loader-24-reload-store-snapshot` | provider（provide svc + dispose-check）+ consumer inject；loader-sync → **loader-update provider config** → reload；依赖方去活/重活序列 | reload 注入快照：consumer reload、dispose-check 排序、端态重活 |

- 生成：`node diff/ts-host/generate-goldens.mjs <新场景>`；验证并入 verify-diff（23→26）。

### S3（m27，core/loader 层 m-series，确定性）

| # | 断言 |
|---|---|
| T1 | provider 卸载（remove_plugin → unload）时：与其同类效果的后来 disposer 里 `ctx.get(provided)` → None（端态）；依赖方先 Unloading（notify 早于后续 disposer） |
| T2 | 3 层嵌套 + isolate：内层 consumer 沿父链取到外层 provider（Active）；注入名被边界隔离的 consumer → Pending |
| T3 | provider reload（update_with）→ 依赖方去活→重活；apply 重跑一次（注入快照一致，非旧态） |

- 红→绿；若 G1/T1 暴露排序或端态偏差 → 回需求 root-cause 后对齐（红期回归流程，不在此偷偷打补丁）。

### S4（回归与文档）

workspace + clippy + verify-diff（26/26，含 3 新 golden）+ serve 冒烟；如未发现偏差 → DECISIONS 记
「A4 parity 实证」；发现偏差 → 对齐提交 + 记录。

## 4. 实现顺序（TDD）

1. S1 双侧 DSL → 3 个新场景 JSON → TS 侧 generate golden → Rust 侧 verify（红=未实现 op/或输出差）。
2. S3 m27 红→绿（对齐点出现则修）。
3. S4 全回归 + 阶段 5 + acceptance。

## 5. DIV / 让步清单

- **DIV-A4-1**：cordis「stale 自访问窗口」在 observable 层不可达（后续 disposer 串行运行在其后）——
  不引入 per-fiber store 回退；以 G1/A4-T1 端态+排序为契约。（若 golden 反证则撤销并 root-cause。）
- **DIV-A4-2**：`await allSettled(依赖方)` 的严格 await 仅异步卸载（unload_async）覆盖；同步路径以
  事件排序+端态等价。
- **DIV-A4-3**：A4 golden 只覆盖三件可表达面；结构性 per-fiber walk（Rust get 走全局表）维持
  resolve_scope 同构（不复制 cordis 的 store-walk 结构）。

## 6. 部署与回滚（阶段 5 预案）

- 部署：A4 为验证型（golden+m27）；若产生对齐修复则随迭代集成。
- 回滚：撤 DSL/场景/测试提交。

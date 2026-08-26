# 设计：服务装配单元 Phase 4 — A6 异步生成器 effect（[Service.init] 完整形态）

日期：2026-08-27
阶段：系统设计（瀑布流阶段 2，Phase 4）——本文档为阶段关卡工件。
依据：`.spec/service-assembly-p4/requirements.md`（需求定稿，A6-SCOPE=A / A6-GOLDEN=A 用户确认）。
参照：npm `cordis` 4.0.0-rc.8 `lib/index.js`（scenario-host 运行的原版）与
`@deepseek-ai/cordis src/fiber.ts`（两者 `_execute` 逐字一致）。

---

## 1. 设计目标

给 dsh-core 补「异步生成器 effect」形态：插件 apply 返回**逐项产生 disposer** 的流（pull 式），
驱动方在 sync（`now_or_never`）/async（`drive_async_loads`）两条路径逐项**立即收集**、卸载**逆序**
执行、**epoch 中途取消**、**失败前 disposer 保留**——语义 = cordis `_execute` async-iterator 分支
（`lib/index.js:798-840`）。新增 1 个 async-generator golden（TS 原版 ↔ Rust 逐行）作等价主证据；
m-series 锁定。不改现有 `Group`/`Await` 路径（A6-SCOPE=A）。

## 2. 自下而上锚点（本阶段核实）

| 锚点 | 基址 | 用途 |
|---|---|---|
| cordis `_execute` async-iterator 分支 | npm `cordis/lib/index.js:798-840`；`@deepseek-ai/cordis src/fiber.ts:356-400` | 语义基准：`await Promise.resolve()`；每轮先 `if (runner.epoch !== oldEpoch) return`（中途取消）→ `await iter.next()` → `safeCollect(值)` |
| fiber 级 runner.epoch | npm `cordis/lib/index.js:957-976`（`_reload`：`oldEpoch=_runner.epoch`；catch → `_runner.epoch=INACTIVE`）| 中途取消/失败的**判定键 = epoch 变化**（非仅 uid）；Rust 对应 `fiber.epoch: Option<String>` |
| 收集/卸载序 | `effect()` `collect` 顺序 push（lib:448-454）、`dispose` `splice(0).reverse()`（lib:431）| 已收集 disposer 注册序 → 卸载逆序 |
| Rust `EffectOutcome` | fiber.rs:42-53（None/One/Many/Async/Await）| 新增 `Stream` 变体 |
| Rust `FiberData.disposers` | fiber.rs:87 + `collect_effect`(121) + `take_disposers`(159) | 逐项收集直插 `disposers`（注册序），卸载逆序已由调用方保证 |
| Rust sync 驱动 | context.rs `apply_body`(707-744) + `drain_phase1`(757-784) | sync Stream 驱动点 |
| Rust async 驱动 | context.rs `drive_async_loads`(569-653) `Apply(fid)` 分支 | async Stream 驱动点 |
| Rust fiber.epoch | runtime.rs `refresh_fiber`（epoch=依赖提供者 uid 拼接，633-666）| 中途取消判定键 |
| 生成器插件表达 | scenario-host.mjs `buildPlugin`(23-34) 只生成**函数** apply | 需扩：`desc.gen` 生成 async-generator apply |
| 依赖面 | dsh-core Cargo.toml: futures-util 0.3 `["std","alloc"]`（已用 LocalBoxFuture/now_or_never/yield_now）| `LocalBoxStream`/`StreamExt` 同 crate 可取 |

## 3. 设计分解

### S1（dsh-core：Stream effect 形态 + 双驱动）

```text
// fiber.rs
pub type GenItem = Result<Disposer, CordisError>;      // 一次 next 的产出；流结束 = 正常完成
EffectOutcome::Stream(LocalBoxStream<'static, GenItem>)  // 异步生成器 effect（pull 逐项）

// FiberData
fn push_gen_disposer(&mut self, d: Disposer)   // 直接入 self.disposers（注册序）+ effect 元数据
                                               // （不复用 collect_effect 的「整批包装」——逐项经时收集）
```

- **驱动循环（async，`drive_async_loads` 的 `Apply(fid)` 分支，取代当前对 `Await` 的特判；`Await` 保留）**：
  ```text
  1. outcome = apply_body(...)
  2. 若 outcome == Stream(s)：
     old = rt.fiber(fid).epoch.clone()
     loop {
       if rt.fiber(fid).epoch != old → 中途取消：停止收集（已收集保留）；
          进入该 fiber 的卸载路径（disposers 逆序执行；等价 cordis `_setEpoch(变更)`→`_unload`）；break
       item = s.next().await            // StreamExt::next；其间让出（复用 yield_now 时序）
       match item:
         Some(Ok(d))  => fiber.push_gen_disposer(d)   // 逐项立即收集
         Some(Err(e)) => rt.fail_fiber(fid, e); break // 失败前已收集保留（T3）
         None         => break                         // 生成完成 → 继续 Finish
     }
     未中途取消 → 照常 Finish（finish_load + notify）
  ```
- **驱动循环（sync，`drain_phase1` 的 Apply 分支 + `apply_body` 特判）**：同样循环，但每步
  `now_or_never(s.next())`：`Some(Ready(item))` 处理如上；`None`（真 pending）→ **停**，fiber 保持
  Loading、不 finish（与既有 `Await` sync 同限：无事件循环不重驱；DIV-4-2）。
- **中途取消判定键 = `fiber.epoch`（Option<String>）变化**，忠实反映 cordis `runner.epoch` 语义
  （依赖方 epoch 由 `refresh_fiber` 维护，Phase 2 已证等价）。驱动期间 epoch 变（如依赖提供者被
  卸/换代）→ 取消后续收集。
- **失败保留**：`Some(Err(e))` 前已 `push_gen_disposer` 的项留在 `disposers`（`fail_fiber` 只清
  epoch/置 Failed，不吞 disposers），卸载/重载时照常执行。

### S2（m-series 锁定，crates/dsh-loader/tests/m19_async_gen.rs）

红测（用 FnPlugin + 构造 `LocalBoxStream` 的插件 apply；`common` 加 stream 构造辅助）：

| # | 红测 | 断言（绿） |
|---|---|---|
| T1 | 生成器 yield A → await 边界 → yield B → yield C → done → fiber Active → 卸载 | A/B/C **逐个**收集（注册序）；卸载 `dispose:` 逆序 **C,B,A** |
| T2 | generator 在某 await 步内**同步翻转自身 epoch**（如经捕获 loader 卸掉其依赖提供者）→ 下一步预检查命中 | 后续 yield **不再收集**；已收集 A 保留；fiber 进入卸载而非 Active |
| T3 | 生成器 yield A → 某步 Err("boom") → fiber Failed → 卸载 | A 保留注册并在卸载执行（`dispose:A`）；与 TS 失败路径一致 |

### S3（DSL + TS host + golden 等价证明）

- **场景 DSL 扩展**：插件描述新增 `gen` 数组（async-generator 体），步进 op：
  - `{"op":"yield","text":"X"}` → 产出 disposer（收集即 trace **`effect-reg:X`**；运行 trace **`dispose:X`**）；
  - `{"op":"await","text":"m"}` → 生成器内 await 边界（trace **`gen-await:m`**；TS 用 `await Promise.resolve()`，
    Rust 用之产生流内边界）；
  - `{"op":"throw","text":"boom"}` → 该步抛错（trace **`gen-fail:boom`**；TS `await` 到 reject / `throw`，
    Rust 产出 `Err`）。
- **TS host**（scenario-host.mjs）：`buildPlugin` 对 `desc.gen` 生成 `function (ctx, config) { return (async function* () { …遍历 desc.gen：yield→trace effect-reg + yield disposer；await→trace gen-await + await Promise.resolve()；throw→trace gen-fail + throw })() }`。
  收集序/失败由 cordis `_execute` 原样驱动（不注入药）。
- **Rust dsh-diff 解释器**：同 DSL 建 `LocalBoxStream`（每项闭包按序 trace `effect-reg`/`gen-await`/
  `gen-fail`），驱动同上 → 与 TS 逐行一致。
- **golden 场景**：`scenario-12-async-generator.sync`/`.golden` —— **T1+T3 融合**：
  `yield A → await m1 → yield B → await m2 → throw boom`：
  断言 TS/Rust 逐行：`effect-reg:A`、`gen-await:m1`、`effect-reg:B`、`gen-await:m2`、`gen-fail:boom`、
  `status …:Failed`、随后 unload 步 `dispose:B`、`dispose:A`（逆序 + 失败前保留）。
- **T2 中途取消不进 golden**（单 `await ctx.plugin()` 步内无法从外部同步中断生成器；DIV-4-3）→
  仅 m-series。既有 21 场景零回归（新增场景后 verify-diff 全量通过）。

### S4（回归 + 可观测）

- `verify-diff.mjs` 22 场景全通过（21 既有逐字节不变 + 1 新增）。
- 受影响 4 crate + workspace + clippy `-D warnings` 0。
- serve 冒烟 HTTP 200（无运行面破坏；Stream 为增量能力，既有路径不动）。

## 4. 实现顺序（TDD）

1. **S1**：`GenItem`/`EffectOutcome::Stream` + `push_gen_disposer` + async 驱动（红测 T1-T3 引用缺失
   变体/API → E0599 红 → 绿）。独立提交（D-134）。
2. **S2**：m19 T1-T3 全绿（随 S1 或独立，回滚点内）。
3. **S3**：DSL+TS host 扩展 + `verify-diff` 生成 golden（TS 原版）→ Rust 逐行一致。独立提交（D-135）。
4. **S4→阶段 4**：workspace + clippy + verify-diff 22 全过。
5. **阶段 5**：serve 冒烟 + acceptance 收口（D-136）。

## 5. DIV / 让步清单

- **DIV-4-1**：异步生成器的 Rust 表达 = `LocalBoxStream`（pull 逐项），非 async-generator 语法；
  等价判定 = 与 `_execute` async-iterator 分支的可观测行为逐行一致。
- **DIV-4-2**：真 pending（需真实事件循环暂停）的生成器仅 async 模式可推进；sync 驱动驱到 ready
  边界停（与既有 `Await` 同限）。golden/m-series 全覆盖不依赖外部时钟。
- **DIV-4-3**：T2 中途取消以「生成器体内同步翻转自身 epoch」确定性表达（m-series）；golden 只承载
  T1+T3 融合场景。
- **DIV-4-4**：不改 Group/Await 现有路径（A6-SCOPE=A）；Group 迁生成器形态留后续。

## 6. 部署与回滚（阶段 5 预案）

- 部署：Stream effect 为 dsh-core 增量能力（新变体 + 驱动分支），公开给插件作者（FnPlugin/宿主
  apply 返回 Stream）。无运行面改动。
- 回滚：`git revert` 本阶段提交（S1+S2 随特征级整体；S3 DSL/golden 独立回滚点）。

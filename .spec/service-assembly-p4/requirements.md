# 需求结论：服务装配单元 Phase 4 — A6 异步生成器 effect（cordis `[Service.init]` 完整形态）

日期：2026-08-27
阶段：需求分析（瀑布流阶段 1，Phase 4）——本文档为阶段关卡工件。
状态：**定稿（范围用户确认：A6-SCOPE=A 核心能力补齐；A6-GOLDEN=A golden 等价证明）**
依据：`docs/SERVICE-ASSEMBLY-HANDOFF.md` §3 A6 + §0 验收（「依赖激活」维度）+ 本月度遗留清单。

---

## 1. 目标（Top-down）

第一性原理：cordis 长效插件的标准形态是 **`async* [Service.init]` 生成器**——`yield disposer`（先注册
清理项）→ `await 启动体` → 继续 yield/await——group/include/hmr 均以此形态书写
（group.ts:125-128 / include.ts:273-289 / hmr.ts:199-205）。Rust `EffectOutcome::Await` 只是 M27
承诺的**等价子集**（await 一个 future → 最终 outcome），无法表达「逐项 yield 的 disposer 在 await
之间**逐个立即收集**、epoch 中途取消、失败前 disposer 保留」。A6 补齐这一核心 effect 能力并给出
与 TS 原版**逐行等价**的证据。验收五维中「依赖激活」维度的最后一块拼图。

**验收** = async-generator effect 红测（逐项收集/逆序卸载/epoch 中途取消/init 失败前 disposer）全绿
+ **1 个 async-generator dsh-diff golden**（TS 原版 cordis ↔ Rust 逐行一致，用户确认 A6-GOLDEN=A）
+ 既有 21 场景零回归 + 受影响 crate/workspace 全绿 + clippy 0 + serve 冒烟零回归。

## 2. 非目标（明确不做）

- **不**把现有 `Group`（loader.rs:304-344 `Await` + `ctx.effect("group-stop")` 近似）迁移/改写为生成器
  形态——保持兼容，只新增能力（用户确认 A6-SCOPE=A；迁移列为后续可选）。
- **不**追求 JS async-generator 的字面语法等价——Rust 无该语法；以**语义等价**为准
  （pull 式「逐项产生 disposer」表达）。
- **不**做背压/完整流运行时/可取消 token 的外部化——驱动沿用 dsh-core 既有
  `now_or_never`（sync）/`drive_async_loads`（async）机制。
- **不**重写 epoch/notify 依赖激活核心（已等价，Phase 2 已证）；不改 `check_impls/refresh_fiber`。
- **不**扩 sync-iterator 与 thenable 形态——`EffectOutcome::Many`（同步生成器逐项=一次给出全部）
  与 `Await→最终 outcome`（thenable→单 disposer）已覆盖（见 §5 缺口核对）。

## 3. 假设（含复盘确认）

- **H1 语义等价口径**：Rust 用 pull 式生成器/流（`next() -> Option<Disposer>`）表达「逐项产生
  disposer」；等价判定 = 与 cordis `_execute` async-iterator 分支的可观测行为一致（逐项收集序、
  卸载逆序、中途取消点、失败前保留集）。
- **H2 中途取消口径**：`epoch 变更`（fiber uid 失效/换代 = `dispose`/`restart` 路径）→ 生成器循环
  **停止后续收集**、**已收集 disposer 保留**（对齐 cordis `if (runner.epoch !== oldEpoch) return`）。
- **H3 失败前 disposer 口径**：生成器在某 await 步**抛错前已 yield** 的 disposer 保留注册，fiber 按
  fail-loud（`fail_fiber`）失败，卸载时这些 disposer 照常逆序执行（对齐 cordis：collect 即时发生，
  失败只是中断后续迭代）。
- **H4 等价证据**：1 个 async-generator golden（TS 原版↔Rust 逐行）+ m-series 红→绿 + 既有 21
  场景 zero-regression 承接（与 Phase 3 DIV-3-2 口径一致，但本例**有** golden，等价证据最强档）。

## 4. 硬约束

- 每新增语义落 m 系列红→绿；golden 由 `node diff/ts-host/verify-diff.mjs` 全量通过（新 golden 由
  TS 原版生成、Rust 逐行一致）。
- `cargo test -p dsh-core -p dsh-loader -p dsh-diff -p dsh-wasmrt -p dsh-cli` + workspace 全绿 +
  `cargo clippy --workspace --all-targets -- -D warnings` 0。
- 决策日志 DECISIONS 追加；改动 → git 提交 → 决策条目互查。既有 21 golden 逐字节不变（zero-regression）。

## 5. 现状缺口（自下而上核实，带依据）

| 项 | 现状（已读源码实证） | 结论 |
|---|---|---|
| cordis 生成器语义 | `_execute`（npm cordis 4.0.0-rc.8 `lib/index.js:798-840` 与 `@deepseek-ai/cordis src/fiber.ts:356-400` **完全一致**）：async-iterator 分支 `await Promise.resolve()` 后才逐 `await iter.next()` + `safeCollect(value)`，**每步先查 `runner.epoch !== oldEpoch` 则 return（中途取消）**；`effect` 内 disposables 顺序收集、卸载 `splice(0).reverse()` 逆序执行 | ✅ 参照已锁定 |
| EffectOutcome 形态 | `None/One/Many/Async/Await`（fiber.rs:42-53）；`Many`=同步生成器整批；`Await`=await 一 future→最终 outcome | ⬜ **缺 async 生成器逐项形态**（跨 await 逐项收集+中途取消+失败前保留） |
| sync 驱动 | `apply_body`（context.rs:707-744）：`Await(fut)` 用 `now_or_never`（sync） | ✅ 可扩展驱动点 |
| async 驱动 | `drive_async_loads`（context.rs:569-653）：`Apply(fid)` 内 await `Await(fut)`，`finish_load`+notify 在 `Finish`；`await_children` 等 Loading 后代 | ✅ 可扩展驱动点（逐项 await 处复用 yield_now 顺序） |
| Group 现状 | loader.rs:304-344 用 `Await` + 独立 `ctx.effect("group-stop")` 近似「先注册清理再挂子项」 | ⚠️ 已近似，**不改**（A6-SCOPE=A） |
| 生成器插件表达 | scenario-host.mjs `buildPlugin` 生成**函数** apply（同步 applyOp 序列）；`desc.apply` 无 yield/await 表达 | ⬜ **需扩 DSL + TS host**（A6-GOLDEN=A）：插件 apply 可返回生成器 + `yield-impl`/`await` 步进 + trace 收集/插入序 |
| 测试落点 | dsh-core 无独立 tests 目录；m 系列在 `dsh-loader/tests/`（m16/m18 先例，FnPlugin + Cordis） | ✅ m19_async_gen.rs |

## 6. 测试与验收标准（阶段关卡）

- **T1 逐项收集 + 卸载逆序**：插件 apply 返回生成器，yield disposer A → await 步 → yield B → done。
  断言：A/B **逐个**被收集（注册序），fiber Active；卸载 → 逆序 B 后 A。
- **T2 epoch 中途取消**：生成器 yield A → await →（期间 fiber 被 unload/restart）→ 后续 yield 不再
  收集；已收集 A 保留并在卸载执行；不出现「死fiber 收集」。
- **T3 init 失败前 disposer 保留**：生成器 yield A → 某 await 步抛错 → 生成器中止；fiber
  `Failed`；A 保留注册（卸载时执行）。
- **T4 golden**：1 个 async-generator 场景（含逐项收集 trace、中途取消 trace、失败 trace）TS 原版
  golden ↔ Rust 逐行一致（`verify-diff.mjs` 全量含新场景）。
- **回归**：既有 21 golden 零回归；5 个受影响 crate + workspace + clippy 0；serve 冒烟（部署阶段）。

## 7. 决策收敛

| 决策 | 结论 |
|---|---|
| A6-SCOPE | **A：核心 effect 能力补齐 + 等价证明**（用户确认）——新增 async 生成器 effect 形态 + sync/async 双驱动 + m-series；不改 Group/Await 现有路径 |
| A6-GOLDEN | **A：新增 1 个 async-generator golden**（用户确认）——扩展 DSL + scenario-host 支持生成器插件，TS 原版 golden ↔ Rust 逐行一致；m-series 作锁定 |

## 8. 遗留边界

- Group/include/hmr 迁移到生成器形态（现有近似已够用，列为后续可选优化）。
- 背压/流式取消 token 外部化；浏览器 E2E（`--dump-dom`）按仓纪律代偿；Node 模块图精确同构不成立
  （既有 DIV）。

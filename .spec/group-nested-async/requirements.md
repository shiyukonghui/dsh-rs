# 需求结论：group 嵌套异步时序（M27/M28）—— 聚焦 Finish 时序

日期：2026-08-27
阶段：需求分析（瀑布流阶段 1）——本文档为阶段关卡工件。
任务归属：beyond 目标「继续任务…从更底层做起」——用户确认起点 = M27/M28（核心运行时）。
状态：**用户已确认**（order=A：从 M27/M28 做起；target=A：聚焦 Finish 时序，不动 disposer 并发）。

## 1. 目标（第一性原理：真正要达成的）

让 Rust 异步驱动在**嵌套 group 装载/卸载的 Finish 时序**上对齐 cordis 的可观测序：
- 现象类（A4 实测）：cordis 中嵌套组的 `status:Group:Loading:Active` **批量聚到装载末尾**（所有
  group 在各自 Loading/未决后裔 settle 之后才 finish），且**不早于**子入口 Active；
  Rust 存在「Group 提前 Active」（如 loader-23 中 gIso：唯一子入口 b1 为 Pending、无 Loading 后裔
  → 立即 finish）与「Group Active 先于子入口 Active」两类偏离。
- 根本目的：嵌套组场景的 golden 能字节级双对齐、装载序可预测，消除 A4 因该缺口把嵌套组 golden
  降级到 m-series 的妥协。

## 2. 非目标（划界）

- **不重写 disposer 并发调度**（cordis `_unload` 的 `Promise.all(disposers)` 微任务交错 = DIV-A4-5
  类，consumer Pending 与 dispose-check 行序）：保留文档化边界、另行立项。
- 不动 unprovide 唤醒顺序（已闭环 A4/m27 T1）、不动父链 walk（已闭环 m27 T2）、不动静态语义。
- 不做 harness FIXME（插件文件→name）、不做 A2 收口复查（本轮不展开）。

## 3. 自下而上核对（现状证据）

| 锚点 | 位置 | 事实 |
|---|---|---|
| Finish 转换 | runtime.rs:37/46（`Finish(FiberId)` 转换） | apply 后让出 → finish_load |
| Finish 延迟 | runtime.rs:255 `fiber_chain_contains`、232/706 `await_children` | M27：Finish 仅当无 Loading 后裔时才前进 |
| finish_load | runtime.rs:703-722 | Active + 清状态 + notify 依赖方 |
| Group 装载 | loader.rs GroupPlugin.apply（397-440）：`EffectOutcome::Await` 挂子入口、Finish 延迟保子先 Active | 子入口 parent=Group fiber |
| 实测偏离 | loader-23（嵌套组，gIso isolate 边界） | Rust gIso 提前 Active；cordis 全部聚末尾 |
| DIV-A4-5 | DECISIONS D-163 | 交错已文档化；本轮不触碰 |

## 4. 自上而下分解（成功标准）

- **S1 语义定位**：精确读出 cordis `EntryGroup` 的 finish 门槛（是「无 Loading 后裔」还是「同批
  子入口全 settle 后」——决定 await_children 判据是否需要改为批量/集合语义）。
- **S2 判据修正**：把「Finish 延迟」从「无 Loading 后裔」对齐为 cordis 门槛（含 Pending-only 组
  不提前 finish、Group 不早于子入口 Active）。
- **S3 校验面**：恢复嵌套组 golden（3 层 + isolate 边界，`group:true` 已可用）字节级双对齐；
  25 个既有 golden 不回归；m-series 全绿；clippy 0；serve 冒烟基线一致。

## 5. 假设与约束

- 约束：只动 async 驱动 Finish 调度（M27/M28 面）；sync 路径、notify/唤醒、disposer 逆序不动。
- 假设：cordis 的「Group 聚末尾」来自其 group update 的 `Promise.allSettled(children)` 批量 await +
  fiber.init 在批后 resolve（待 S1 实证；若反证则据实修正假设）。

## 6. 验收标准（阶段关卡）

> 更新（D-169/D-170）：编码期实测 `status:p:Loading:Active` 落点 = DIV-nested-2（mount
> 时序），用户裁决扩口径到 **B（mount 时序）**；验收标准 1 已按 B 达成——`loader-25`（3 层嵌套
> + isolate 边界 + Pending-only 组）**字节级 PASS**（verify-diff 26/26），详见 design.md §5b/5c。

1. 新增/恢复嵌套组 golden（≥1，含 isolate 边界与 Pending-only 组）在 verify-diff 字节级 PASS。
2. `cargo test --workspace` 0 失败；`cargo clippy -- ---D warnings` 0；既有 25 golden 全绿。
3. M27/M28 修复点有明确 DECISIONS 条目与回滚点；DIV-A4-5 边界保持文档化不受影响。

## 7. 复盘与待确认结论（已确认）

- 假设 A（隐含）：A4 观察到的嵌套组交错 = Finish 时序缺口而非 disposer 并发缺口 → 已由用户定
  口径 target=A（聚焦 Finish 时序）锁定边界。
- 信息缺口修正：最初 A4 以为「组内嵌组装载失败」——实为场景缺 `group:true`（D-163 已澄清）；
  本轮无需再疑。
- 常见错误规避：不因「组 Pending-only 提前 finish 无害」就跳过对齐（正是 byte-parity 目标）；
  不以重写调度换取短期通过（用户已否决 B 全量口径）。
- 排序确认（用户）：order=A（M27/M28 最底层先做）。

## 8. 产物归属

后续文档（design/acceptance）入 `.spec/group-nested-async/`，DECISIONS 从 D-167 起按闸门追加。

# 验收：beyond 目标 Phase A4 —— 注入快照 / unprovide 唤醒顺序 / 父链 walk

日期：2026-08-27
阶段：部署与维护（瀑布流阶段 5，Phase A4）——本文件为该阶段关卡工件。
上游：`.spec/service-assembly-a4/{requirements,design}.md`（D-161）→ 编码（D-163）。

## 1. 验收标准与证据

| 标准（需求文档 §验收） | 证据 | 结论 |
|---|---|---|
| DSL 扩 `dispose-check`（非 strict）双侧同构 | loader-host.mjs + dsh-diff `ApplyOp::DisposeCheck`（D-163） | ✅ |
| golden 1：unprovide 自访问 null（remove 后 disposer 后行） | `loader-22-unprovide-self-access` 11 行字节一致 | ✅ |
| golden 2：reload 应用新 config + teardown 快照（provider 单独） | `loader-24-reload-store-snapshot` 17 行字节一致 | ✅ |
| m27 三断言核心锁定 | `m27_a4.rs`：unprovide_wakeup_order / group_isolate_boundary_and_3level_walk / reload_applies_new_config_and_reactivates_dependent | ✅ 3/3 |
| 组隔离边界真实生效（修复项） | 修复前：b1 越界 Active（golden 探针）；修复后：Pending（m27 T2 + golden 语义） | ✅ |
| 全回归 | cargo test --workspace **209 目标 0 失败**；clippy **0**；verify-diff **25/25** | ✅ |
| serve 冒烟 | `dsh web target/web/cordis.yml` → GET / **HTTP 200 len 13270**（与基线一致） | ✅ |

## 2. 编码期发现与取舍

- **发现真实偏差（group 入口 isolate 未应用）**：`load_group_plugin`（sync+async）原先不设
  pending_isolate；m3_isolate / loader-15 的兄弟节点无自身 isolate → 两路皆 Active、测试无法分辨。
  A4 探针（嵌套 gIso 边界）首次暴露 → 修复 `entry_isolate_map`（D-163 / DIV-A4-4）。
- **异步交错边界（DIV-A4-5）**：cordis `_unload` = `Promise.all` 并发 disposers（各先让出微任务）；
  Rust 顺序逆序（依赖方完整 settle 后才自清，语义 ⊇ cordis）。`consumer Unloading:Pending` 与
  `dispose-check` 行序不可字节对齐 → **不改全局调度**；golden 收窄剔除窗口；唤醒顺序由 m27 T1
  （确定性）承担。
- **dsh-diff update 惯例**：update options 须带 id/name（归一化全量语义），与 loader-12 一致。

## 3. 诚实边界

- golden 层：嵌套组（组内嵌组）的异步装载/finish 交错尚未字节对齐（M27/M28 纵深）——A4 未触碰，
  相关语义由 m27 T2（3 层 walk + 边界）确定性锁定；golden 用两层/多顶层组表达 walk 与边界。
- 未做结构性 per-fiber store walk（Rust `get` 走全局表，resolve_scope 同构 cordis）；
  非 strict 读取语义等价已由 loader-22/24 + m27 T1 实证。

## 4. 部署与回滚

- 部署：loader 修复随迭代集成；golden/DSL 增量为测试面。
- 回滚：`git revert`（D-163，连同 loader.rs 修复）；决策链 = DECISIONS D-161 → D-163 → D-164。

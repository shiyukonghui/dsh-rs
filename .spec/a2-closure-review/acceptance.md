# 验收：A2 收口复查——eval_scope 服务绑定一致性修复 + 锁点

日期：2026-08-27
阶段：测试验证（4）→ 验收（5）——本文档为该两阶段关卡工件。
依据：`.spec/a2-closure-review/{requirements,design}.md`（D-171）+ 探针/锁点（D-172）+ 全回归。

## 1. 验收结论：**PASS**

用户确认口径：next_task=A2 收口复查（loader 层更底层）；fix_or_record=A 修复＋测试锁定。
复核发现 **3 个真实缺口**，全部修复（TDD 红→绿）并有锁点测试；既有行为（interpolate 原子性）
锁定。全回归零破坏。

## 2. 复核报告（V1-V4 结论）

| 维度 | 结论 | 证据 |
|---|---|---|
| V1 语义保真 | scope 键集（config/process/env/ctx+裸标识符，显式键优先）= fork `with(ctx)` 的 Value 服务子集（DIV-6-1）✓；`interpolate` **原子**（任一节点失败整树保留原 config） | 探针 P2/T-L2 |
| V2 绑定 | 修复 2 处：disabled 绑 current_fiber→**入口上下文**（fork loader-ctx 根化求值）；fiber_service_ctx 值按 current→**目标视图**（同 fid，DIV-6-2 错位消除） | 探针 P1/T-L1 红、T-L3 红 |
| V3 应用面/新增 | 缺口③：`entry.options.inject` 未并入 fiber inject（fork `internal/plugin` `Inject.resolve(...)` 同径；entry.rs 注释未实现）→ 修复并测试 | fork index.ts:117-123；T-L4 红 |
| V4 回归 | m21 3/3、m3 3/3、m29 4/4、dsh-loader 全绿、verify-diff 26/26、clippy 0、serve 基线一致 | 见 §4 |

## 3. 修复清单（D-172，TDD）

- **F1** `Cordis::get_value_from(ctx_fiber, name)`：指定上下文解析 Value 服务（目标视图；
  不经 internal/get 拦截——决策期快照，DIV 记录）。
- **F2** `fiber_service_ctx`：名字与值都按同一 fid（目标视图）——修 config-interp 的 DIV-6-2 错位。
- **F3** `entry_disabled`：绑入口上下文——names = entry.ts inject ∪ 插件 inject；values =
  `get_value_from(loader_fid=根 realm)`；显式键优先；未知标识符仍 fail-closed（m3 语义保持）。
- **F4** `Runtime.pending_entry_inject` + `register_plugin` 并入 fiber inject（load_plugin /
  load_group_plugin / load_group_plugin_async 同径填）——entry 声明依赖参与门控与 `!!js` 绑定。

## 4. 阶段 4 证据（测试验证）

- 锁点 **m29_a2_review** 4/4：T-L1（disabled 入口上下文，红→绿）/ T-L2（interpolate 原子性，
  既有行为锁定）/ T-L3（目标 realm 服务，红→绿）/ T-L4（entry.inject 合并，红→绿）。
  红验证：stash 回退 F1-F4 → T-L1/T-L3/T-L4 FAIL、T-L2 ok → 恢复全绿。
- **dsh-loader 全量**：24 test 二进制全 ok（含 m29 4/4、m21 3/3、m3 3/3、m28 2/2）。
- **全 workspace**：EXIT=0 零失败；`cargo clippy --workspace --all-targets -- -D warnings` **0**；
  `verify-diff.mjs` **26/26**（golden 无 `!!js` → 零回归）；serve 冒烟 **HTTP 200/13270**。

## 5. 诚实边界

- disabled/插值值解析**不经 internal/get 拦截**（决策/插值时刻快照；DIV 记录）。
- 非 Value 服务仍不暴露（DIV-6-1）；`get_value_from` 属 Impl/Value 直达，accessor 属性不经。
- 未重评 agent-presets/standing/combo 的 row_disabled 语义（非 loader A2 面）。
- harness FIXME（插件文件→name）留待后续（顶层装配，独立立项）。

## 6. 决策链互查

D-171（需求+设计：A2 收口复查，用户确认任务与 A 口径）→ D-172（编码：F1-F4 + m29 红→绿）→
D-173（本验收）。工件 requirements/design/acceptance + DECISIONS + git 提交互查（commit 见 git log）。

## 7. 部署与回滚

- **部署**：改动集中在 dsh-core（get_value_from + pending_entry_inject 并入）+ dsh-loader 绑定两个
  求值点 + entry.inject 合并；无配置迁移。行为修正点：① disabled 引用可见服务不再误禁用
  （此前 fail-closed 误禁）；② entry 声明 inject 现参与门控（fork 行为；无 inject 声明者不受影响）。
- **回滚**：撤 D-172 提交（F1-F4 整体特征级）。m29 锁点随回滚移除；m21/m3/26 golden 与行为基线
  （A2 修复前）不受影响。既有配置若依赖「有 inject 声明但不门控」的旧行为需知悉（fork 下本就门控）。

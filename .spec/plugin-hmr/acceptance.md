# 验收：beyond 目标 Phase HMR —— 宿主侧插件模块热更 + 删除后 entry 处置

日期：2026-08-27
阶段：部署与维护（瀑布流阶段 5，Phase HMR）——本文件为该阶段关卡工件。
上游：`.spec/plugin-hmr/{requirements,design}.md`（D-162）→ 编码（D-165）。

## 1. 验收标准与证据

| 标准（需求/设计 §验收） | 证据 | 结论 |
|---|---|---|
| 宿主入口 `sync_plugin(name, PluginEvent)` | `Loader::sync_plugin` + `PluginEvent{R,Replace,Delete}` + `PluginSyncOutcome{reloaded,disposed,retained}`（lib.rs 导出） | ✅ |
| Register 幂等（同 Arc） / 换代（新 Arc）+ reload | e2e 步骤 2-3：幂等 reloaded=[]、generation=2、entry_identity=新 | ✅ |
| Replace hot-swap 语义 | e2e 步骤 3：reloaded=["a"]、身份换代、g&Active | ✅ |
| Delete 删除后「保留但 inert」处置 | e2e 步骤 4：fiber Disposed、entry 保留、**不 disabled**、无 `disable:` 写回 | ✅ |
| Revive：再 Register 恢复 | e2e 步骤 5：新 fiber、新身份、新实现（重载 lineage=全新记录） | ✅ |
| 全回归 | cargo test --workspace 0 失败；clippy 0；verify-diff 25/25 | ✅ |
| serve 冒烟 | `dsh web target/web/cordis.yml` → GET / **HTTP 200 len 13270**（基线一致） | ✅ |

## 2. 编码期发现与取舍

- **Delete 后 `fiber()` 字段不清空**：沿用 m26 不变量（fiber_state==Disposed 判「逝」，字段保引用）。
- **re-register = 全新 lineage**：Delete 清空了插件记录 → 再 Register 是全新记录（generation 重置 1、
  新身份 token）。诚实语义（cordis delete+import 同径）；测试修正非实现补丁。
- **DIV-HMR-1/2 落实**：删除后 entry 保留但 inert（可 revive/remove，不自禁用=case-4 保持）；
  薄封装（A1/B3 零改动）；未做文件 watcher（harness 装配层 FIXME 保持不变）。

## 3. 诚实边界

- `sync_plugin` 为**事件封装**，不做插件**文件**→ Arc 解析（specifier 解析留 harness）；
  配置 HMR（Include）另由 hmr.rs/m15 覆盖。
- 未新增事务引擎/并发控制；宿主需在事件间串行调用（与 B3 单线程装载一致）。

## 4. 部署与回滚

- 部署：纯增量 API + 契约文档；既有 replace/remove（A1/B3/m18/m26）零行为变化。
- 回滚：`git revert`（D-165）；决策链 = D-162 → D-165 → D-166。

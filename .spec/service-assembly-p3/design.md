# 设计：服务装配单元 Phase 3 — B3 HMR 模块热更（身份换代 → 受影响 entry reload）

日期：2026-08-26
阶段：系统设计（瀑布流阶段 2，Phase 3）——本文档为阶段关卡工件。
依据：`.spec/service-assembly-p3/requirements.md`（需求定稿）+ `docs/SERVICE-ASSEMBLY-HANDOFF.md` §3 B3。

---

## 1. 设计目标

给 dsh-loader 增「插件实现级热更」层：宿主对某 name 注册新实现（身份换代）→ 以旧身份加载的
entry 自动 reload 新实现（entry 保真），服务依赖方经 epoch 自动重活（externals 同构）。API 公开、
m-series 红→绿验收、既有 21 场景零回归。

## 2. 自下而上锚点（本阶段核实）

| 锚点 | 基址 | 用途 |
|---|---|---|
| `register_plugin` 换代（新 Arc → 新身份+generation） | loader.rs:392-408 | A1 语义复用（检测数据就绪） |
| `Entry.identity` 记录解析身份 | loader.rs（Phase 1） | 旧身份检测 |
| `dispose_entry`（卸 fiber、保 entry 记录） | loader.rs | entry 保真 reload 的卸半 |
| `start_entry`（按当前注册实现重挂载） | loader.rs | entry 保真 reload 的重半 |
| **fiber uid 换代**：dispose 置 None（runtime.rs:755）、重载重新分配（runtime.rs:208-209） | dsh-core | **依赖方 epoch 自动变 → 自动重活**（externals 同构自然成立；DIV-3-1 无需显式刷新） |
| 依赖方重活 = epoch owner_uid 拼接 | runtime.rs:633-666 | T2 传播机制 |

## 3. 设计分解

### S1（loader 层，公开 API）

```text
// B3 主入口：同 name 换实现（身份换代）→ 受影响 entry reload；同实现幂等返回 Ok(0)。
pub fn replace_plugin(&self, name: &str, plugin: Arc<dyn Plugin>) -> Result<usize, CordisError>

// 可观测：name 下以「旧身份」加载的 entry 数/ids（供宿主/HMR 判断）。
pub fn stale_entry_ids(&self, name: &str) -> Vec<String>
```

- `replace_plugin` 逻辑：
  1. 读当前注册：同 Arc（`Arc::ptr_eq`）→ **幂等**（无换代）返回 `Ok(0)`（T3）。
  2. 不同 Arc → 走 A1 `register_plugin`（铸新身份 + generation 递增）。
  3. 收集受影响 entry：`entry.options.name == name && entry.identity.is_some() && entry.identity != 新身份`
     （即曾以旧实现加载）→ 逐个 `reload_entry(id)`（计数返回）。
  4. 失败 → `?` 传播（fail-loud），已 reload 计数不计失败项（诚实）。
- `reload_entry(id)`（私有）：disabled → no-op；否则 `dispose_entry(id)?`（卸旧 fiber，保 entry 记录）
  + `start_entry(id)?`（按当前注册实现重挂载，identity 重新记录为新身份）。
- 依赖方传播：提供者 reload 后 uid 复位重分配 → 其 svc impl owner uid 变 → 依赖方 `refresh_fiber`
  epoch 变 → 自动 Load（**无需显式刷新**——DIV-3-1 定案）。

### S2（m 系列红测，crates/dsh-loader/tests/m18_hmr_impl.rs 或 m16 扩展）

| # | 红测 | 断言（绿） |
|---|---|---|
| T1 | impl v1 → create entry(apply 记 v1) `replace_plugin(v2)` | entry 自动 reload：apply 记 v2、`entry_identity==v2`、fiber Active、id/options 保真 |
| T2 | provider(v1)+consumer 均 Active `replace_plugin(provider, v2)` | consumer 经历 Unload/Pending →（provider 新实现 apply）→ Active（epoch 重活 = externals） |
| T3 | `replace_plugin(name, 同一 Arc)` | 返回 0、generation 不变、无 reload |
| T4 | `replace_plugin` 返回受影响数 | 2 个 entry 用同名 → 返回 2；无 stale → 0 |

### S3（回归 + 可观测）
- `verify-diff.mjs` 21 场景零回归（本阶段不改 cordis 核心/dsh-diff DSL）。
- 受影响 4 crate + workspace + clippy `-D warnings` 0。
- 部署冒烟：serve HTTP 200 零回归（无运行面改动，仅 loader 增量 API）。

## 4. 实现顺序（TDD）

1. **S1** `replace_plugin`/`stale_entry_ids`/`reload_entry`（红测 T1-T4 引用缺失 API → E0599 红 →
   绿）。独立提交。
2. **S2** 回归门槛 + clippy 0 + verify-diff 21 零回归。随 S1 同提交或独立均在回滚点内。
3. **S3** 部署冒烟 + acceptance 收口。独立提交。

## 5. DIV / 让步清单

- DIV-3-1（定案）：externals→全重载 = 「依赖方经 fiber uid 换代/epoch 自动重活」（Rust 无模块图，
  不等价 cordis importers 图；语义同构：换实现后消费方重取新实现重活）。
- DIV-3-2：本例无新 dsh-diff golden（DSL 无法表达「同 name 换实现」）；等价主证据 = m-series 红→绿
  + 既有 21 场景零回归。
- DIV-3-3：`replace_plugin` 只换「同一 name 的实现」；entry 的其它配置不变（reload 保持 options）。
  group 入口（其 fiber 是合成 GroupPlugin）不参与实现热更（GroupPlugin 非注册表实现）——B2 后续。

## 6. 部署与回滚（阶段 5 预案）

- 部署：`loader.replace_plugin(name, 新实现)` 为宿主可调用公开 API（serve/dynamic runner 可选接线）；
  配置文件 watcher（hmr.rs）保持。
- 回滚：`git revert` 本阶段提交（loader 增量 API + m_series，独立回滚点）。

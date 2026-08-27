# 设计：beyond 目标 Phase HMR —— 宿主侧插件模块热更 + 删除后 entry 处置（集成测试 + 文档）

日期：2026-08-27
阶段：系统设计（瀑布流阶段 2，Phase HMR）——本文档为阶段关卡工件。
依据：`.spec/plugin-hmr/requirements.md`（定稿，用户确认 HMR=A）。

---

## 1. 设计目标

把「插件模块 add/change/delete」接到 loader 既有 replace_plugin/remove_plugin（B3/A1 已完备），
定义为宿主-facing 契约 + 删除后 entry 处置策略（**保留但 inert**）+ 一条 e2e 集成测试锁定宿主序列。

## 2. 自下而上锚点

| 锚点 | 基址 | 用途 |
|---|---|---|
| replace_plugin（新实现→entry reload） | loader.rs:512 + m18（T1-T4） | change 态 |
| remove_plugin（删除→fiber dispose、entry 不自禁用） | loader.rs:491 + m26（T1-T3） | delete 态 |
| register_plugin（同 Arc 幂等/新 Arc 换代） | loader.rs:467-483 | add 态 |
| stale_entry_ids / reload_entry | loader.rs:514-527 / reload | 观测 |
| 配置 HMR watcher | hmr.rs + m15（Include::refresh） | 参照（本阶段不扩文件 watch） |

## 3. 设计分解

### S1（宿主契约：`LoaderPluginApi` 观测/入口，dsh-loader，小）

- 在 Loader 上把「插件事件」做薄封装，供宿主一个明确入口：
  ```text
  pub enum PluginEvent { Register(Arc<dyn Plugin>), Replace(Arc<dyn Plugin>), Delete }
  impl Loader { pub fn sync_plugin(&self, name: &str, event: PluginEvent) -> Result<PluginSyncOutcome, CordisError> }
  pub struct PluginSyncOutcome {
      pub reloaded: Vec<String>,   // Replace：受影响 entry（reload 新实现）
      pub disposed: usize,         // Delete：被 dispose 的 fiber 数
      pub retained: Vec<String>,   // Delete：受影响 entry（保留但 inert）
  }
  ```
  - Register → `register_plugin`（幂等/换代）；Replace → `replace_plugin`（换代+reload）；
    Delete → `remove_plugin`（记 retained=受影响 entry id 列表 + disposed fiber 数）。
- 兼容：不破坏既有 replace_plugin/remove_plugin API（薄封装委托）。

### S2（删除后处置策略：保留但 inert）

- `remove_plugin` 后受影响 entries：**保留**（options/identity 不变、不自禁用、不写盘）+ fiber 已逝。
  Document：宿主可再 `sync_plugin(name, Register/Replace)` 恢复（case-4 的模块再现可 revive）或显式
  `remove(id)` 移除。不新增破坏性「整体移除」语义（cordis delete 同径，DIV-HMR-1）。

### S3（e2e 集成测试 m27b / 并入 m27？—— 独立 `m27_hmr_host.rs`）

```text
sequence:
  register_plugin p(v1) → create entry a(p) → sync_plugin(p, Register同Arc) [noop]
  → sync_plugin(p, Replace v2) → entry a reloads 新实现（identity 更新、generation+1）
  → sync_plugin(p, Delete) → fiber 逝、entry retained&inert（不 disabled、无 disable 写）
  → sync_plugin(p, Register v3) → entry a reloads v3（revive 恢复）
断言：各 phase 的 reloaded/disposed/retained + generation/identity + disabled=false。
```

### S4（文档）

- identity.rs 或 loader 模块 doc 补「宿主插件模块 HMR 序列」契约 + retained 策略；DECISIONS 记
  DIV-HMR 条目（无文件 watcher——specifier 解析留 harness）。

## 4. 实现顺序（TDD）

1. S1 `sync_plugin` + `PluginSyncOutcome`（compile）。
2. S3 e2e 红→绿（先写期望序列）。
3. S2/S4 文档。
4. 阶段 4 回归（206→207 目标 + 23 golden）+ 阶段 5 serve + acceptance。

## 5. DIV / 让步清单

- **DIV-HMR-1**：删除后 entry 「保留但 inert」+ 显式 revive/remove；不做整体自动移除（cordis
  delete 不动 entry.options；避免破坏 case-4）。
- **DIV-HMR-2**：不做插件**文件** watcher / specifier→Arc 解析（harness 装配层 FIXME）；
  宿主靠事件总线调 `sync_plugin`。配置 HMR（Include）已闭环（B3/m15）。
- **DIV-HMR-3**：`sync_plugin` 是薄事件封装（复用 B3/A1 API），非新事务引擎。

## 6. 部署与回滚（阶段 5 预案）

- 部署：纯增量 API + 契约文档；既有 replace/remove/m18/m26 零行为变化。
- 回滚：撤 sync_plugin + e2e 提交。

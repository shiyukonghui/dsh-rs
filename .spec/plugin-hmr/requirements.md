# 需求结论：beyond 目标 Phase HMR — 更完整 HMR（宿主侧插件模块热更 + 删除后 entry 处置）

日期：2026-08-27
阶段：需求分析（瀑布流阶段 1，Phase HMR）——本文档为阶段关卡工件（用户已确认）。
依据：`docs/SERVICE-ASSEMBLY-HANDOFF.md` §A1/DIV-A1-3、B3 既有 + 现有实现自读。

---

## 1. 目标（第一性原理 + 双视角）

第一性：`replace_plugin`/`remove_plugin` 已是 loader API（B3/A1，m18/m26 绿）但**仅测试调用**——
宿主/触发链未接入（无生产调用者＝「更完整 HMR」缺口）。目标 = **宿主 facing 的插件模块热更入口**：
把「插件文件 add/change/delete」事件接到 loader 的 replace_plugin / remove_plugin，并定义
**删除后受影响 entry 的处置策略**。

**自上而下（host-facing 契约）**：
- 插件模块 **change** → `replace_plugin(name, 新实现)` → 旧身份 entry 自动 reload（B3 已验证）。
- 插件模块 **delete** → `remove_plugin(name)`（A1 已验证：fiber dispose、entry 不自禁用）+
  定义该 entry 后续状态。
- 宿主需要可观测：replace/remove 影响哪些 entry（`reload_entry` 计数 / `remove_plugin` 返回 fiber 数）。

**自下而上（现有）**：
- `replace_plugin`（loader.rs:512）→ 换代 + stale_entry_ids + reload_entry；`remove_plugin`
  （loader.rs:491）→ 先删记录后 dispose（case-4 合法）。
- HMR watcher（hmr.rs，m15）服务**配置文件**热更（Include::refresh）；**插件模块**热更无接入。
- dsh-cli / 宿主无 PluginManager 层。

## 2. 目标 / 非目标

- **目标**：
  1. loader 增宿主-facing 接入点（轻量，避免万物进 loader）：把插件模块变更语义封装成
     `apply_plugin_event(name, event)`（event = Register(Arc) / Delete）→ 内部委托
     register_plugin + replace_plugin / remove_plugin，返回影响 entry 集；
     或直接文档化「宿主调 replace/remove_plugin」并提供一个观测助手（stale/reloaded 集）。
  2. **删除后 entry 处置策略**（用户确认可选：保留但 inert / 整体移除）——**选：保留但 inert**：
     entry 保留（options/身份不变、不自禁用），fiber 已逝；宿主可后续 remove 或重新注册恢复。
     理由：与 cordis delete 语义一致（不动 entry.options），且不破坏 case-4（module 再现可 self-revive）。
  3. 集成测试 + 文档：宿主链路（register→reload→delete sequence + release 恢复）在一测试锁。
- **非目标**：不做内存映射 plugin-file→name 的文件 watcher（B3/m15 已覆盖配置 HMR；插件文件
  watcher 泛化 FIXME 留宿主自治）；不改 loader 内部事务。

## 3. 约束

- 宿主接入点 ≥1、集成测试绿；既有 206 目标 + 23 golden 零回归；clippy 0；serve 冒烟。
- DECISIONS 追加；改动 → git 提交 → 决策条目互查。

## 4. 验收标准（阶段关卡）

- `apply_plugin_event`（或等价宿主入口）三态（Register 新 / Change 替换 / Delete 移除）链路 e2e；
  删除后 entry inert（不 disabled、fiber 逝、记录可再注册恢复）；观测助手返回受影响集。

## 5. 复盘追问（已确认 Q2=A：host 接入 + entry 处置策略 + 集成测试）

- **入口形态候选**：`Loader::sync_plugin(name, event)` vs 文档化直调 replace/remove +
  `reloaded_entries(name)`/`stale_entry_ids` 观测。**推荐最小侵入**：文档化直调 +
  retention 策略文档 + 集成测试（e2e sequence）——replace/remove 已完备，宿主只需要「何时调用」
  的契约与可观测结果。
- **删除后处置**：保留但 inert（cordis 同径）——不新增「整体移除 entry」的破坏性语义。
- **缺失信息**：宿主真正的「插件文件→name」解析（specifier→Arc<dyn Plugin>）属 harness 装配层；
  本阶段不虚构映射，只定 loader 侧契约。

## 6. 遗留边界

- tar 插件文件 watcher / specifier 解析（harness FIXME）；Include 配置 HMR 已闭环（B3/m15）。

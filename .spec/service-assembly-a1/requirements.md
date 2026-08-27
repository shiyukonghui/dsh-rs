# 需求结论（定稿）：beyond 目标 —— A1 插件身份键模型收口

日期：2026-08-27（确认 2026-08-27）
阶段：需求分析（瀑布流阶段 1，Phase A1）——本文档为阶段关卡工件（用户已确认）。
依据：`docs/SERVICE-ASSEMBLY-HANDOFF.md` §A1 + fork registry/fiber 源码实证 + 现有实现自读。

---

## 1. 目标（第一性原理 + 双视角）

第一性原理：把 A1 剥到基本事实——Cordis 的插件身份 = **解析后回调指针**；「同一实现=同身份，
re-import=新身份」驱动两块语义：**HMR 实现替换**（registry.delete(旧)+registry.plugin(新)）与
**case-4**（插件模块从 registry 删除后，该插件的 fiber self-dispose 是合法卸载、不入 disabled）。

- **自上而下**：A1 的顶层成功标准 = 三条契约都成立（① 同名同实现幂等；② 同名新实现=新身份；
  ③ 模块消失 → fiber self-dispose 合法）。
- **自下而上（现有实现自读）**：
  - Rust 已实现 ①②：「平名仓库 + `PluginIdentity`(Arc 指针) + `generation`」，
    `register_plugin` 同 Arc 幂等、新 Arc 换代（loader.rs:467-483）；`replace_plugin`（B3/HMR）
    换代 → stale entry 自动 reload（loader.rs:492-512）；m16（T1-T5）+ m18（T1-T4）已绿。
  - ② 的「新实现替换」= 顺序替换（一名一记录）。**「同名多实现同时共存」未支持**。
  - ③ **case-4 有分支无触发**：seven_case case-4（loader.rs:220-227）判
    `registry.contains_key(runtime_key)`——但**无 `remove_plugin`（registry.delete）API** →
    「模块消失」路径不可达、无测试。

**收敛**：A1 核心（身份换代 + HMR）已闭环；**剩余 = case-4 可触发化 + 验证测试 + A1 偏差文档化声明**，
并按 HANDOFF 给出的「或显式声明为文档化偏差」分支定界「同名多实现」归属。

## 2. 目标 / 非目标

- **目标**：
  1. 补 `remove_plugin(name)`（cordis `registry.delete(plugin)` 语义：移除记录 + dispose 该名
     所有存活 fiber → 其 entry 若 self-dispose 走 case-4 合法路径，**不**落 disabled+写回）。
  2. case-4 专用 m-test：插件被删 → 该 entry fiber self-dispose 后 entry **不**被标 disabled；
     插件仍在（不会被误判）对照位。
  3. A1 身份模型文档化（DECISIONS + `.spec/` README 区段或 identity.rs doc）：
     「名字→当前实现」+ 身份 token = 回调身份 的 Rust 同构；同名多实现 = 宿主层责任（文档化偏差）。
- **非目标**：
  - 不做注册表键结构改（(来源, name)+版本 多实现共存）——除非用户选择分支 B（见 §6 待确认）。
  - 不改 HMR 文件 watcher（B3 已闭环）。
  - 不做 TS golden 于 case-4 —— 除非用户要求建 delete-plugin 场景工厂（见 §6）。

## 3. 约束

- 新语义落 m26 红→绿；既有 205 目标 + workspace + clippy 0；23 golden 零回归；serve 冒烟。
- DECISIONS 追加；改动 → git 提交 → 决策条目互查。

## 4. 验收标准（阶段关卡）

- m26 绿：`remove_plugin("x")` → x 的存活 fiber 被 dispose、记录移除；该 entry 不自禁用；
  `stale_entry_ids`/replace 语义不受影响；删除后再 `replace_plugin` 幂等边界验证。
- 全回归 + clippy + golden + serve。

## 5. 复盘追问（方法论二·强制动作）

**（1）用户没明说、但默认成立的假设**
- H1：dsh-rs 宿主（harness）以「名字→实现」的**扁平注册集**装配；同名多实现**同时**共存不是注册表
  契约（宿主在 import 层负责去重/选择）——这是走「文档化偏差」分支的前提。
- H2：`remove_plugin(name)` 语义 = cordis `registry.delete`：**移除记录 + dispose 该名所有 fiber**
  （而非仅删注册留 fiber 存活）。
- H3：case-4 判定用 name（runtime_key）查 registry；「模块消失」= name 被删。

**（2）缺失的关键信息（可能改变答案）**
- 宿主/HMR 目前**从不删除插件**（只有 replace）？若未来需要「移除模块 → 该名 entry 整体卸载」，
  case-4 只是其中一环（entry 侧处置还要定）。→ 决定 case-4 现在是「可触发+有测试」还是「纯文档化」。

**（3）处理这类问题最常犯的一个错误**
- 把「同名多实现同时共存」误当成必要契约，去改 (来源, name)+版本 键——而 dsh-rs 是**宿主-owned
  注册表**，多实现由宿主在 import 层消解；注册表层做成多实现反而破坏「名字→当前实现」的确定性
  （load_plugin 按名取一家）。另一个反向错误：把 case-4 误解为「模块消失=入口应禁用」——实际 cordis
  语义是 self-dispose **合法**、不落 disabled（index.ts:140-156 case-4 早退）。

## 6. 待确认问题（用户已确认 2026-08-27）

| 问 | 结论 |
|---|---|
| Q1 范围分支 | **A 文档化偏差收口**：补 `remove_plugin` + case-4 m-test + A1 偏差显式文档化；注册表键结构不动；同名多实现=宿主层责任。 |
| Q2 case-4 证据 | **m-series**：m26 直接测 remove_plugin 后 entry 不自禁用 + 对照片仍禁用。 |
| Q3 remove_plugin 语义 | **移除记录（loader st.plugins + core registry）+ dispose 该名所有存活 fiber**（cordis registry.delete 对齐）；其 entry 若 self-dispose 走 case-4 合法路径、不落 disabled+写回。 |
本文档定稿 → 系统设计（阶段 2）。

## 7. 遗留边界

- 多实现共存（分支 B 才展开）；entry 级「模块被删→入口处置」策略（case-4 外的宿主自治）。

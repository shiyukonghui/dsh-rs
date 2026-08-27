# 验收报告：beyond 目标 Phase A1 — 插件身份键模型收口（remove_plugin + case-4 + 文档化偏差）

日期：2026-08-27
阶段：测试验证（阶段 4）+ 部署与维护（阶段 5）——本文档为阶段关卡工件（验收收口）。
依据：`.spec/service-assembly-a1/requirements.md` + `design.md`（定稿，用户确认）+
`docs/DECISIONS.md` D-158/D-159/D-160。
范围：A1 = 身份模型已形核（既有）；收口三件——`remove_plugin`（registry.delete 同径）→ case-4
可触发 + m26 验证 + 文档化偏差声明。

---

## 1. 交付范围（对需求/设计逐条核对）

| 项 | 要求（Q1-Q3 确认） | 交付 | 证据 |
|---|---|---|---|
| `remove_plugin(name)` | 移除记录（core+loader）+ dispose 该名存活 fiber（cordis delete 同径） | ✅ `Loader::remove_plugin` | m26 T1/T3 |
| case-4 合法路径 | 模块消失 → self-dispose 合法：entry 不自禁用、无 `disable:` 写回 | ✅（先删记录后 unload，顺序不变量） | m26 T1 |
| case-7 对照 | 插件仍注册时 self-dispose → entry disabled + 写回（既有语义复锁） | ✅ | m26 T2 |
| 文档化偏差 | 一名一实现（顺序换代）+ 同名多实现=宿主层责任 | ✅ identity.rs doc + D-158/159 | DIV-A1-1 |
| 幂等 | 未注册名 → Ok(0) | ✅ | m26 T3 |

## 2. 阶段 4（测试验证）证据

- **m26 3/3 红→绿**：红 = `no method named remove_plugin`（编译期缺方法）；绿 = T1（remove 后
  entry 不自禁用、无 disable 写回、identity None）、T2（对照 disabled + disable 写回）、T3（ghost
  幂等）。
- **`cargo test --workspace`**：EXIT=0，**206 目标 0 失败**（205 + m26）。
- **`cargo clippy --workspace --all-targets -- -D warnings`**：EXIT=0。
- **`node diff/ts-host/verify-diff.mjs`**：**23/23 PASS**（replace/HMR/m16/m18 零回归——remove_plugin
  纯增量 API，不改既有换代路径）。

## 3. 编码期发现与取舍（如实记录）

- **A1 已形核（非从零）**：身份 = Arc 指针 token + generation + `replace_plugin`（B3/HMR）早已实现
  （m16/m18）；真实剩余仅「case-4 触发路径不可达（无 delete API）+ 无测试 + 偏差未文档化」。
- **顺序不变量实证**：`remove_plugin` 必须先删 record 再 unload——否则 seven_case case-4 在
  `internal/plugin(dispose)` 时看到 name 仍注册 → 误落 case-7（entry 被禁用）。设计层显式锁定，
  m26 T1 印证。
- **对照 T2 语义**：外部 `ctx.unload(fid)` 触发与 fiber 自处置同事件（internal/plugin dispose）；
  插件仍注册 → case-7 disabled——cordis index.ts:140-156 同径复锁。

## 4. 阶段 5（部署与维护）证据

- **部署冒烟**：`dsh web target/web/cordis.yml --port 60891` → `GET /` **HTTP 200**（len 13270 基线
  一致），进程干净停止。
- **部署面**：纯增量 API + case-4 触发路径（此前不可达）；既有 replace/HMR/等断言零变化。
  回滚 = `git revert e169ef3`（remove_plugin + m26 + identity.rs doc）。

## 5. 诚实边界（未做 / 延后）

- 同名多实现**同时共存**（(来源,name)+版本 键）未做——用户确认走「文档化偏差」分支（DIV-A1-1）；
  宿主 import 层负责多实现消解。
- case-4 无 TS golden（DIV-A1-2；DSL 无 delete-plugin 操作）。
- `remove_plugin` 不写持久化（DIV-A1-3；cordis delete 不动 entry.options；宿主自治后续处置）。
- A1 验收为 beyond 目标首步；我此前报告的目标（A5/A2/B1/B2/B4/A3）为独立闭环。

## 6. 决策链互查

`D-158 需求+设计（36b0b6e）→ D-159 编码（e169ef3）→ 本验收（D-160，待提交）`。
改动 → git 提交 → DECISIONS 条目一一对应。

## 7. 结论

**通过**：A1（插件身份键模型收口）验收完成。`remove_plugin`（registry.delete 同径）+ case-4 合法路径
可触发并有 m26 三断言锁定（含 case-7 对照）；文档化偏差显式声明（DIV-A1-1）；workspace 206 目标全绿、
clippy 0、23 golden 零回归、serve 冒烟 HTTP 200。

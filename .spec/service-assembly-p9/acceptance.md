# 验收报告：服务装配单元 Phase 9 — B4 config simplify 回写 unparse

日期：2026-08-27
阶段：测试验证（阶段 4）+ 部署与维护（阶段 5）——本文档为阶段关卡工件（验收收口）。
依据：`.spec/service-assembly-p9/requirements.md` + `design.md`（定稿）+ `docs/DECISIONS.md` D-152/D-153/D-154。
范围：B4 = dsh-schema `simplify`（schemastery 语义）+ loader write_back 运行时更新写回简化（m-series，
无 golden——DIV-9-2 + B 类先例）。

---

## 1. 交付范围（对需求/设计逐条核对）

| 项 | 要求 | 交付 | 证据 |
|---|---|---|---|
| `dsh_schema::simplify(schema, value)` | schemastery `Schema.prototype.simplify` 逐字 | ✅ `crates/dsh-schema/src/lib.rs` | m24 T3（十分支） |
| 写回简化（运行时更新） | `internal/update`（noSave=false）→ `e.options.config = simplify(...)` | ✅ loader `write_back` | m24 T1 |
| 无 schema 原样 | 无 schema → 不简化 | ✅ | m24 T2 |
| create 不简化 | cordis `_patchContext` 的 `fiber.update(cfg,true)` noSave=true 跳过 | ✅ 同径保持 | m24 T1 首断 |
| persist 闭合 | 简化仅存 `e.options.config`→`entry_options()`（内存=落盘） | ✅ | m24 T1（`entry_options()` 断言） |

## 2. 阶段 4（测试验证）证据

- **m24 3/3 绿**：T1 `update_with(fid,{def:5,other:2},false)` → 写回 `{other:2}`（def==默认删）+ create
  首断原样（cordis noSave=true 同径）；T2 无 schema `{k:2}` 原样；T3 simplify 十分支（object 删默认键 +
  未声明键删、全删 `{}`==`{}` 默认→**Null**、README `{foo:'',bar:1}`→`{bar:1}`、dict 保 null、array 逐项、
  union try resolve、顶层默认→null、null 透传、原始型原值）。
- **`cargo test --workspace`**：EXIT=0，**204 目标 0 失败**（含 m24 + m17_persist 等既有）。
- **`cargo clippy --workspace --all-targets -- -D warnings`**：EXIT=0（红期 unused import 修复后）。
- **`node diff/ts-host/verify-diff.mjs`**：**23/23 PASS**——无 schema 插件路径与既有 goldens 零回归。

## 3. 编码期发现与取舍（如实记录）

- **简化触点修正（红期实证）**：`internal/update` 仅**运行时 `update_with(fid,cfg,false)`** 触发
  （cordis core 的 `ctx.update()`）；`_patchContext` 的 `fiber.update(cfg,true)` 带 noSave=true →
  write_back 跳过；`create()` 直接 `self.write` 存原样。即 cordis **create 不简化，运行时更新才简化**——
  修正 T1 为 `update_with` 驱动（初版用 create 断言 `{def:5,other:1}`→原始，已修订）。
- **全默认对象塌缩 Null（红期实证）**：`Schema::object()` 默认即 `{}`（schemastery defineMethod 同）——
  object 全键删后 `{}`==默认`{}` → **Null**（schemastery deepEqual(result, default) 收尾）。初版断言
  `{a:{}}` 系错误预期；按 schemastery 修订（嵌套 `{a:{x:5}}` 全默认 → Null）。
- **未声明对象键删**：`schema?.simplify` 缺 key → undefined → object 分支删键（README `<bar>` 保留即
  因声明且≠默认）——对应 dsh-schema None 分支 → Null → 删。
- **依赖**：dsh-loader 增 `dsh-schema` 直接依赖（此前经 dsh-core 间接使用）。

## 4. 阶段 5（部署与维护）证据

- **部署冒烟**：`dsh web target/web/cordis.yml --port 60889`（本轮含 simplify 语义）→ `GET /`
  **HTTP 200**（len 13270 基线一致），进程干净停止。
- **部署面**：仅增「声明 schema 插件在运行时配置更新时写回简化配置」语义；create/无 schema/goldens
  零变化。回滚 = `git revert e65546a`（dsh-schema simplify + write_back + m24 + 依赖）。

## 5. 诚实边界（未做 / 延后）

- 无 golden（DIV-9-2；loader-host 插件无 Config schema）；m-series。`deepEqual` dict-default 特判降级
  （DIV-9-1）；`Lazy` 透传（DIV-9-4）。
- A3 动态 check spike（后续优先级）。

## 6. 决策链互查

`D-152 需求+设计（901612c）→ D-153 编码（e65546a）→ 本验收（D-154，待提交）`。
改动 → git 提交 → DECISIONS 条目一一对应（本阶段需求+设计并闸——验证型任务，较轻）。

## 7. 结论

**通过**：B4（config simplify 回写 unparse）验收完成。`dsh_schema::simplify` 全部语义对齐
schemastery（红期两处实证修正：运行时更新触点 + 全默认塌缩 Null）；loader 写回接入 cordis 同径；
m24 3/3 绿、workspace 204 目标全绿、clippy 0、23 golden 零回归、serve 冒烟 HTTP 200。

# 需求结论：服务装配单元 Phase 9 — B4 config simplify 回写 unparse

日期：2026-08-27
阶段：需求分析（瀑布流阶段 1，Phase 9）——本文档为阶段关卡工件。
依据：`docs/SERVICE-ASSEMBLY-HANDOFF.md` §3 B4 + fork/schemastery 源码实证 + §0 验收。

---

## 1. 目标（Top-down → Bottom-up）

第一性原理：cordis-plugin-loader 每次配置更新后把**简化配置**写回并落盘——fork `internal/update`
监听器（index.ts:103-109）：
```js
await next()
const unparse = this.runtime?.Config?.['simplify']
this.entry.options.config = unparse ? unparse(config) : config
this.entry.parent.tree.write()
```
`Config.simplify` = schemastery `Schema.prototype.simplify`（@deepseek-ai/schemastery src/index.ts:407-442）：
**值与 schema 默认相等 → 删（对象分支去 null/undefined 键）**；Rust `write_back`（loader.rs:263-285）存
**原始配置**（未简化）→ 落盘含默认值键（与 cordis 持久化形态不一致）。B4 补齐：dsh-schema 实现
`simplify(schema, value)`（schemastery 语义移植）+ loader write_back 接入（插件声明 config_schema 时
简化写回）。

**验收** = `dsh_schema::simplify` 语义= schemastery 逐分支（对象删默认键/字典保 null/数组逐项/intersect
合并/union 试解析），m24 红→绿（写回落盘含简化）+ m17_persist 回归 + 全 workspace + clippy 0 + serve
冒烟。

## 2. 非目标

- **不做** TS/golden 于简化写回（loader-host 插件无 Config schema；B 类先例 m-series）。
- **不做** `Config.merge`（深合并回填）——A5/DIV-5-1 已定浅合并范围；简化后的配置在下次装载经
  `validate_config` 自动补回默认（语义闭合）。
- **不改** in-memory diff 语义的意图偏差：`e.options.config` 存简化值**与 cordis 一致**（同字段
  内存=落盘）；配置-only 差异走 branch-3 fiber.update（cordis `_patchContext` 同径）。
- **不做** A3 动态 check（后续优先级）。

## 3. 假设（复盘确认）

- **H1**：`simplify` 只对**声明 config_schema 的插件**生效（无 schema → 原样，零影响既有测试/golden
  的 no-schema 插件）。
- **H2**：write_back 的 `args[1]` = 经过 internal/config 插值 + schema 校验（默认已填充）的配置——
  simplify 借默认值判定删键（cordis 同径：fiber 已解析配置）。
- **H3**：写法语义照抄 schemastery `Schema.prototype.simplify`（含对象删 null/undefined 项、dict 保
  null、末尾对象与 default 全等→null、union 用 `resolve` try）。
- **H4**：证据 m-series（B1/B2 先例——B 类非核心）。

## 4. 硬约束

- dsh-schema `simplify` 不改 `resolve`/`Meta` 现有语义（纯增量函数）；`deepEqual` 用 serde_json 相等
  （分派 type==dict 时对 default 的差异按 schemastery `dict` 参数语义降级为常规深等，DIV-9-1）。
- 新语义落 m24 红→绿；既有 203 目标 + workspace + clippy 0；23 golden 零回归。
- DECISIONS 追加；改动 → git 提交 → 决策条目互查。

## 5. 现状缺口（自下而上核实，带依据）

| 项 | 现状（源码实证） | 结论 |
|---|---|---|
| fork simplify | index.ts:103-109（internal/update → `Config['simplify'](config)` → options.config → tree.write） | ✅ 参照已锁定 |
| schemastery simplify | src/index.ts:407-442（默认相等→null；object 删 null/undefined；dict 保；array/tuple 映射；intersect 合并；union 试 resolve；else 原值） | ✅ 语义已提取 |
| dsh-schema | Meta.default（lib.rs:26-43）+ resolve（415），**无 simplify** | ⬜ 缺 simplify 函数 |
| loader write_back | loader.rs:263-285（`e.options.config = cfg.clone()` 原始写回） | ⬜ 需接 schema simplify |
| persist 回归 | m17_persist（机制锁定：权威列表/顺序/fail-loud；no-schema 插件） | ✅ 不受影响 |

## 6. 测试与验收标准（阶段关卡）

- **T1**：schema 含默认（`def=5`），config `{def:5, other:1}` → `simplify` → `{other:1}`（默认键删）。
- **T2**：无 schema 插件 → 写回 config 原样（raw）。
- **T3**：嵌套对象删默认键 + dict 保 null 项 + array 逐项简化。
- **回归**：m17_persist 三测 + 23 golden + workspace + clippy 0 + serve 冒烟（阶段 5）。

## 7. 决策收敛

| 决策 | 结论 |
|---|---|
| B4 载体 | **dsh-schema `simplify` + loader write_back 接入**（cordis 同字段内存=落盘语义） |
| B4 证据 | **m-series**（m24 T1-T3 + m17 回归；B 类先例；golden 需 TS Config schema 成本高） |

## 8. 遗留边界

- `deepEqual` 的 dict-default 特判降级（DIV-9-1，JSON 值域无 undefined）。
- 简化配置在下次装载经 validate_config 补默认（语义闭合）；无需回填 merge。

## 复盘追问结论
- **假设/缺失**：简化仅作用于 schema 插件（H1）；golden 不可行（TS 无 Config schema 场景）→ m-series
  （延续 B 类先例）。
- **常见错误**：把 simplify 做成「只在落盘 YAML 序列化前删键」而不动 `e.options.config`——会导致内存
  与落盘分离、下次 write 又从内存全量导出（简化失效）；需与 cordis 一致**写回同字段**。

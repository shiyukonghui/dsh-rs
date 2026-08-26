# 验收报告：服务装配单元 Phase 5 — A5 对象形态 inject 拦截配置合并（[Service.resolveConfig] + pi-ai）

日期：2026-08-27
阶段：测试验证（阶段 4）+ 部署与维护（阶段 5）——本文档为阶段关卡工件（验收收口）。
依据：`.spec/service-assembly-p5/requirements.md`（定稿）+ `design.md`（定稿）+ `docs/DECISIONS.md` D-139/D-140。

---

## 1. 交付范围（对需求/设计逐条核对）

| 项 | 要求 | 交付 | 证据 |
|---|---|---|---|
| S1 Plugin 配置通道 | 对象形态 inject（`{ svc: cfg }`）可表达 | ✅ `Plugin::inject_configs()`（registry.rs，默认空，不破坏既有实现） | 编译 + m20 |
| S1 装载最内层 | 注入配置入本 fiber 自身 intercept 层最内层（最高优先级/子代可见） | ✅ `register_plugin`：依赖名集 = `inject()` ∪ 配置键；`f.intercept` extend pending_ic + own_cfgs | m20 T1-T3 + golden |
| T1 | 子注入配置 > 父 `ctx.intercept`（同键后者覆盖） | ✅ m20 | `obj_inject_config_wins_over_parent_intercept` |
| T2 | base 最低 / head 最高 / 注入层中间（浅合并） | ✅ m20 | `obj_inject_base_head_ordering` |
| T3 | 父注入配置沿父链对子代可见 | ✅ m20 | `obj_inject_config_visible_to_child_via_parent_chain` |
| S3 golden | 对象形态 inject 场景 TS 原版 ↔ Rust 逐行一致 | ✅ `scenario-13-object-inject-config`（14 行） | verify-diff 23/23 |

## 2. 阶段 4（测试验证）证据

- **m20 TDD 红→绿 3/3**：红测两处断言修正（诚实记录）——
  (1) T2 首版把 `base` 当「最高优先级」→ 对照 cordis `Object.assign({}, base, …)` 修正为 base **最低**
  （被注入层覆盖：`b`=2 而非 0）；(2) T1 首版断言 `{a:9,b:2}` 精确匹配 → 父 intercept 的 `p:1` 应**保留**
  （非覆盖）→ `{a:9,b:2,p:1}`。
- **`cargo test --workspace`**：EXIT=0，**200 目标 0 失败**（+m20，既有全量回归零破坏）。
- **`cargo clippy --workspace --all-targets -- -D warnings`**：EXIT=0。
- **`node diff/ts-host/verify-diff.mjs`**：**23/23 PASS**——22 既有 golden 逐字节不变 + `scenario-13`
  （`plugin:prov` / `status` / `apply:prov` / `provide:srv:{}` / Active / parent / `intercept:srv:{"a":1,"p":1}`
  / child（injectConfig） / `apply:child` / `resolve-config:srv:{"a":9,"b":2,"p":1}` / Active×2）；
  子注入配置最内层合并 → TS 原版 cordis 与 Rust 逐行一致。

## 3. 编码期发现与关键取舍（如实记录）

- **serde 字段失配**：JSON `"injectConfig"`（camelCase）与 Rust 字段 `inject_config` 失配 → 注入配置
  未生效（首轮红）；`#[serde(rename="injectConfig")]` 修复。
- **键序规范化（DIV-5-4，追证后的取舍）**：初试全局 serde_json `preserve_order`（插入序）→ **破坏 9 个
  既有 golden**（loader/include/session 宿主键序为**排序**，非插入序；`config,id,name`）。回退；改为仅对
  scenario-host `resolve-config` trace 用 `stableStringify`（对象键递归字典序）。JSON 键序无语义，
  两侧键序确定性一致；既有 golden 零回归。**不启用全局 preserve_order 是本轮的重要架构保守决策**。
- **dsh-diff DSL**：`PluginDesc.inject_config` + `inject()` 含配置键 + `inject_configs()`；
  scenario-host `plugin.inject = { ...injectConfig }`（cordis `Inject.resolve` 原生处理对象形态）。

## 4. 阶段 5（部署与维护）证据

- **部署冒烟**：`dsh web target/web/cordis.yml --port 60885`（本轮含 dsh-core register_plugin 改动）
  → `GET /` **HTTP 200**（len 13270，与基线一致），进程干净停止——真实启动链路零回归。
- **部署面**：`Plugin::inject_configs()` 为可选新方法（默认空）——所有既有插件实现（FnPlugin/
  DshServicesPlugin/WasmLoopPlugin/ScenarioPlugin…）零改动；pi-ai 类插件可经此通道声明拦截配置。
  对象形态 inject 的键即依赖（DIV-5-2），配置入最内层。
- **回滚**：`git revert 260f031`（D-139，core/loader/diff/host/golden 特征级整体）；`scenario-13-*`
  可独立删除。

## 5. 诚实边界（未做 / 延后）

- `Config.merge` **深合并**不做（DIV-5-1：缺 pi-ai 深合并证据；需求已向用户确认浅合并范围）。
- golden 键序经 stableStringify 规范化（resolve-config trace）；其余 scenario-host trace 值（provide/
  intercept 的 op.value）保持文件声明序——当前全部已排序/单一键，矛盾暴露即扩展同规范化（DIV-5-4）。
- 浏览器 E2E（`--dump-dom`）按仓纪律代偿。
- B1 `[Service.extend]` / B2 Group 折叠 / B4 config simplify / A3 动态 check = 后续优先级目标。

## 6. 决策链互查

`D-137 需求（cb28285）→ D-138 设计（3b7b782）→ D-139 编码+golden（260f031）→ 本验收（D-140，待提交）`。
改动 → git 提交 → DECISIONS 条目一一对应。

## 7. 结论

**通过**：A5（对象形态 inject 拦截配置合并）五阶段闭环。Rust 现可按 cordis 语义承载「插件声明
`inject: { svc: cfg }` → 配置入本 fiber 最内层 → `resolve_config` 最高优先级合并（含 base/head）/
子代可见」，pi-ai 前置能力就位；新 golden 14 行 TS 原版 ↔ Rust 逐行一致，既有 23 场景零回归、
200 目标全绿、clippy 0、serve 冒烟 HTTP 200。

# 设计：A2 收口复查——绑定一致性修复 + 锁点

日期：2026-08-27
阶段：系统设计（瀑布流阶段 2）——本文档为阶段关卡工件。
依据：`.spec/a2-closure-review/requirements.md`（D-171 需求）+ fork 源码 + 探针实测。

## 1. 复核结论（V1-V4 实证）

| 维度 | 结论 | 证据 |
|---|---|---|
| V1 语义保真 | `eval_scope_with_services` 键集/裸标识符/显式键优先 = fork `with(ctx)` 子集（Value 服务，DIV-6-1）✓；`interpolate` **原子**（任一节点 Err → 整树保留原 config） | 探针 P2：两 `__jsExpr` 全保留 |
| V2 绑定 | **缺口①（实）**：`entry_disabled` 绑 `current_fiber`——顶层 create 无 current → 空服务 → 服务引用 fail-closed **误禁用**（fork 在 loader ctx 根化的 entry 扩展 Context 求值则可见）。**缺陷②（DIV-6-2 错位）**：`fiber_service_ctx` 名字取 `fiber(fid).inject`、值却按 `current_fiber` 解析（应同一目标视图）。| 探针 P1 FAIL（`P1 snapshot=[]`）|
| V3 应用面 | loader 求值点仅 2 处（config 插值 args[0]=fid ✓ / disabled best-effort ✗）；agent-presets/standing/combo 非 A2 面（确认无 loader 侧遗漏） | crates 全量 grep |
| V4 回归 | m21 T1-T3 / m3 不回归需保持；golden 26/26 无 `!!js`（A2-SCOPE=B）→ 改 config-interp 值解析零 golden 风险 | verify-diff 26/26 |
| V3'（新增，fork 对照） | **缺口③（实）**：fork `internal/plugin` 时 `Inject.resolve(fiber.entry.options.inject, fiber.inject)` **合并 entry 级 inject** 进 fiber（index.ts:117-123，fork 自带 "FIXME merge config"）；Rust `load_plugin` 未合并 → entry 声明依赖不参与服务门控与绑定（entry.rs 注释「合并进插件 inject」未实现） | fork index.ts:117-123 ↔ loader.rs:1073-1147 |

## 2. 修复设计（3 处，TDD）

**F1 — `Cordis::get_value_from(ctx_fiber: Option<FiberId>, name)`**（dsh-core/public）：
`resolve_impl(name, ctx_fiber)` → `Arc<dyn Any>` downcast `Value`（DIV-6-1 同 `get_value` 的 Value 暴露面；
不经 internal/get 拦截——disabled 决策期取决策时刻快照，文档化为该边界）。

**F2 — `fiber_service_ctx(ctx, fid)` 值解析改"目标视图"**：名字与值都按 **同一个 fid**——
`get_value_from(fid, name)`（替代 `get_value(name)`/current_fiber）。效果：config-interp 绑目标纤维
自身的 realm 链（父链+isolate），消除「调用方可见 ≠ 目标可见」（DIV-6-2 错位）。风险：golden 无
`!!js`；m21 顶层 `parent=None` 目标=根=调用方 → 值不变。m3 用显式键（config/process）不受影响。

**F3 — `entry_disabled` 绑入口上下文（fork 保真）**：names = `entry.options.inject` ∪ 插件
`inject()`；values = `get_value_from(st.loader_fiber（根 realm，None→根作用域）, name)`——fork
`Entry.evaluate` 在 loader ctx 根化的扩展 Context 求值，同径。显式键 config/process/env 仍优先；
未知标识符仍 fail → fail-closed（m3 语义保持）。

**F4 — `entry.options.inject` 并入 fiber inject（fork index.ts:117-123 同径）**：
`Runtime` 增 `pending_entry_inject: Vec<String>`（镜像 pending_isolate/intercept 模式）；`load_plugin`
/`load_group_plugin` 挂载前从 `e.options.inject` 填充；`register_plugin` 取走并入 fiber inject。
效果：entry 声明依赖参与服务门控（fork 行为）且进入 config/disabled 绑定名单。无 inject 条目的
既有配置零影响；有 inject 且依赖未提供者由「不门控即装载」修正为 fork 的「门控等待」。

## 3. 锁点测试（TDD，红→绿）

- **T-L1（红→绿）**= 探针 P1：provider Active + 消费方 `disabled_expr` 引用注入服务 → 服务可见应
  求值**不**禁用（fork）；修复后 apply 发生。
- **T-L2（直接绿锁定）**= 探针 P2：多 `__jsExpr` 节点原子回退（整树保留）。
- **T-L3（红→绿，DIV-6-2）**：构造「目标隔离 realm 本地服务」——组 gIso( isolate svc ) 内：提供方
  `local` 提供 svc、消费方 `p`(inject svc, config `{"__jsExpr":"svc.k"}`) 同组；应解析到 **gIso 本地
  svc**（目标视图），而非根 svc。修复前（current=调用方→根视图）会取错 → 红；修复后取本地 → 绿。

## 4. 验收

- T-L1..T-L3 全绿；m21/m3 不回归；`cargo test --workspace` 0；clippy 0；verify-diff 26/26；
  serve 冒烟基线一致。
- 工件：`.spec/a2-closure-review/{requirements,design,acceptance}.md` + 复核报告（acceptance 含
  V1-V4 表）+ DECISIONS D-17x + git 提交互查。

## 5. 划界（诚实边界）

- disabled/插值值解析**不经 internal/get 拦截**（决策时快照；DIV 记录）。
- 非 Value 服务仍不暴露（DIV-6-1）。
- 未重评 agent-presets/standing row_disabled 语义（非 loader A2 面）。
- harness FIXME（插件文件→name）留待后续（顶层装配，独立立项）。

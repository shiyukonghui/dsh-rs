# 需求结论：A2 收口复查（`!!js` eval_scope 服务绑定的复核与锁点）

日期：2026-08-27
阶段：需求分析（瀑布流阶段 1）——本文档为阶段关卡工件。
任务归属：beyond 目标「继续任务…从更底层做起」——M27/M28 已闭环（D-169/D-170）；两个剩余候选
经自下而上比对，**A2 收口复查（loader 层）较 harness FIXME（顶层装配胶水）更底层**，按「从更底层」
应先做 A2。状态：**用户已确认**（next_task=A2 收口复查——较 harness FIXME 更底层；fix_or_record=A 修复＋测试锁定）。

## 1. 目标（第一性原理：收口的本意）

A2（D-141..144，`.spec/service-assembly-p6`）已闭环：把 `!!js`/`__jsExpr` 求值作用域绑定到**目标
fiber 的注入就绪上下文**（`eval_scope_with_services` + `fiber_service_ctx`；m21 T1-T3；无 golden，
证据退档 m-series）。**收口复查** = 复核该闭环实现是否与其全契约一致、无残留缺口、边界清晰，并
**用新测试把关键行为锁死**（防后续 HMR/mount 演进回归）。若复核发现真实语义缺口 → 修复（TDD）。
产出：复核报告 + 锁点测试 + （可选）修复，而非新功能面。

## 2. 非目标（划界）

- **不**做 TS host / golden（延续 A2-SCOPE=B；fork 语义重建成本论证不变）。
- **不**做 harness FIXME（插件文件→name，顶层依赖 loader API）——本轮不展开。
- **不**暴露非 Value 服务（DIV-6-1 保持）；不引入 `with` 语句 / 任意 JS 引擎（沿用 dsh_eval）。
- **不**改 `internal/config` 触发时序（仍先 waterfall 插值后 push current，args[0]=fid 机制）。
- **不**触碰 agent-presets `row_disabled` / standing / combo 求值面（属 P2/K4 面，非 loader A2 面；
  仅复核确认无 loader 侧遗漏，不重评其语义）。

## 3. 自下而上核对（现状证据）

| 锚点 | 位置 | 事实 |
|---|---|---|
| scope 构造 | loader.rs:143-161 `eval_scope_with_services` | `{config, process, ctx=services, env:{}}` + 服务名顶层裸标识符（显式键优先） |
| 服务上下文 | loader.rs:165-179 `fiber_service_ctx` | `fiber(fid).inject` 名单 × `ctx.get_value(name)`；fid=None→空 |
| config 插值绑点 | loader.rs:304-334 `internal/config` 监听器 | 经 `args[0]=fid` 精确绑**目标纤维**（apply_body 单发射点 context.rs:799，waterfall 早于 current.push） |
| disabled 绑点 | loader.rs:89-123 `entry_disabled` | 经 `ctx.current_fiber()` **best-effort**（D-142 注释）；失败 fail-closed（truthy→禁用） |
| 可见性 | context.rs:1623-1643 `get_value` | 经 `resolve_impl(name, current_fiber)`——**监听时刻当前 fiber** 的 realm 链可见性（DIV-6-2 边缘） |
| interpolate | dsh-eval:536-553 | 递归构建新 Value（非原地突变）；任一节点 Err → 整树 Err → 监听器回退原 config（**原子**） |
| 原子性回退 | loader.rs:324-330 | 失败保留原 config + `writes.push("eval-error:…")`（fail-loud） |
| 求值点清点 | crates 全量 grep | loader 层仅 2 处：config 插值（精确绑）+ disabled 门控（best-effort）；均已在 bind |

## 4. 自上而下分解（复查维度与成功标准）

- **V1 语义保真（fork `with(ctx)` 对照）**：scope 键集合与 fork `new Function('ctx','expr','with(ctx)
  {return eval(expr)}')`（fork config/utils.ts）- 目标纤维扩展 Context 暴露面的差异逐一核验——
  （a）裸标识符读服务 ✓；（b）`ctx.<svc>` 成员 ✓；（c）显式键 config/process/env/ctx 优先 ✓；
  （d）`env` 恒定 {} 与 fork 面的差异——确认属既有确定性设计还是缺口；（e）interpolate 多 `__jsExpr`
  节点的**原子性**（全有或全无）用测试锁定。
- **V2 绑定正确性**：
  - `internal/config` args[0]=fid 全路径核验（唯一发射点，已确认；重写/重载/嵌套装载路径一致）。
  - `get_value` 监听时刻可见性（DIV-6-2）：目标纤维注入值 = 祖先/兄弟提供时可见性如何；「目标可见
    而调用方不可见」（isolate 边界）极端——确认是否真缺口还是已文档化边界。
  - **disabled best-effort 不对称**：config 精确绑 args[0]、disabled 绑 current_fiber——disabled 期间
    current 为调用方（loader/父组），引用服务不可见 → fail-closed → 误禁用风险；确认是否真实缺口。
- **V3 应用面完备**：loader 全部求值点均已绑定（config/disabled 两处）；确认无第三条遗漏路径
  （write_back / seven_case / update 一 致性）。
- **V4 回归一致性**：m21 T1-T3 / m3（disabled_expr + config 插值）不回归；workspace + clippy 0；
  verify-diff 26/26 不回归（A2 无 golden，但整体基线保持）。

## 5. 待确认（复盘追问——向用户确认后再进入设计）

1. **任务顺序**：两项剩余候选（A2 收口复查 / harness FIXME）——按「更底层」先做 **A2 收口复查**
   （loader 层；harness FIXME 为顶层装配，留待下轮）。需用户确认。
2. **修正口径**：若 V2 实测发现真实缺口（如 disabled best-effort 误禁用、get_value 可见性错位）——
   - **A（推荐）**：修复 + TDD 测试锁定（符合「收口」本意：把闭环真正封死）；
   - **B**：仅记录为文档化边界（不动代码），本轮只做复核报告 + 锁点测试。
3. **lock 测试范围**：默认新增 Tests（候选：interpolate 多节点原子性 / 重载 update 插值仍绑目标纤维 /
   disabled+服务 best-effort 行为 / 未注入服务 fail-loud）——在 V1/V2 结论上收敛为最小锁点集。

## 6. 验收标准（阶段关卡，S3）

1. 复核报告（`.spec/a2-closure-review/`）：V1-V4 每维给「确认/缺口/边界」结论 + fork 对照证据。
2. 新增锁点测试全绿（红→绿 或 直接绿＝既有行为锁定）；m21/m3 不回归；workspace + clippy 0；
   verify-diff **26/26**；serve 冒烟基线一致。
3. 若选择修正口径 A：修复有独立 TDD 红→绿 + DECISIONS 条目 + 回滚点；若 B：缺口各以 DECISIONS 分支
   记录、不改行为。
4. 决策链互查：DECISIONS D-17X + 工件 + git 提交互查。

## 7. 产物归属

工件事务入 `.spec/a2-closure-review/`（requirements/design/acceptance/review）；DECISIONS 自 D-17X 追加。

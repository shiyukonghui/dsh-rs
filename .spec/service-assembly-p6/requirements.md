# 需求结论：服务装配单元 Phase 6 — A2 !!js 求值作用域绑定注入服务

日期：2026-08-27
阶段：需求分析（瀑布流阶段 1，Phase 6）——本文档为阶段关卡工件。
状态：**定稿（范围用户确认：A2-SCOPE=B 仅 Rust 侧；A2-BARE=A ctx 成员 + 裸标识符）**
依据：`docs/SERVICE-ASSEMBLY-HANDOFF.md` §3 A2 + fork 源码实证 + §0 验收。

---

## 1. 目标（Top-down → Bottom-up）

第一性原理：cordis-plugin-loader 的 `!!js`/`__jsExpr` 求值 = `new Function('ctx','expr','with(ctx){ return eval(expr) }')`
（fork `config/utils.ts`），`interpolate(ctx, config)` 递归替换 `{__jsExpr}` 节点；`evaluate(this.ctx, expr)`
中的 ctx = 入口的**扩展 Context**（注入服务以属性混入 → **裸标识符** + `ctx.svc` 均可读，fork
`lib/index.js:338/370`）。Rust `eval_scope`（`loader.rs:124-136`）现绑定 `ctx: {}`（空）→ 凡依赖注入
服务的 config 表达式语义缺失。A2 把求值作用域绑定到**目标 fiber 的注入就绪上下文**（Value 服务通道），
对齐 `with(ctx)` 语义。

**验收** = config `!!js` 读注入服务红测全绿（ctx 成员 + 裸标识符 + 显式键优先 + 失败保留）+ m-series
（m21）+ 既有全量回归 + clippy 0 + serve 冒烟（部署阶段）。

## 2. 非目标

- **不做** TS host / golden（A2-SCOPE=B，用户确认——TS host 现无 `!!js` 支持，重建 fork 语义成本高；
  等价证据退一档，由 m-series + 单测锁定）。
- **不**做非 Value 服务（`Arc<dyn Any>` 不可 JSON 化）的暴露——仅 `get_value`（Value 型）服务入 ctx
  （DIV-6-1；cordis 暴露任意对象，Rust 子集限 JSON）。
- **不**引入 `with` 语句 / 任意 JS 引擎——沿用 dsh_eval 受限求值器（白名单。
- **不**动 `internal/config` 的触发时序（apply_body 先 waterfall 插值后 push current——插入绑定读
  `args[0]=fid`，不改顺序）。
- **不**做 B 类 / A3（后续优先级）。

## 3. 假设（复盘确认）

- **H1 绑定对象**：`internal/config` 监听器从 `args[0]` 取目标 fiber → 该 fiber 的 `inject` 名单 →
  `get_value` 逐个取值 → `ctx` = `{ 注入服务名 → 值 }`（本 fiber 注入就绪上下文）。
- **H2 裸标识符**：服务名作为顶层作用域标识符（`with(ctx)` 语义）；与显式键
  （`config`/`process`/`env`/`ctx`）冲突时**显式键优先**（A2-BARE=A）。
- **H3 失败保留**：求值失败 fail-loud（保留原 config + `eval-error` 写回标记，与现状一致）。
- **H4 可见性**：`get_value` 基于当前（监听时刻=父）纤维的 store 可见性解析——祖先提供的服务可读；
  仅目标深层隔离可见的极端情况作 DIV。

## 4. 硬约束

- 复用 dsh_eval（不改求值器核心，只扩 scope 构造）；`eval_scope_with_process` 保持（既有测试）。
- 新语义落 m21 红→绿；workspace + 相关 crate + clippy 0。
- DECISIONS 追加；改动 → git 提交 → 决策条目互查。

## 5. 现状缺口（自下而上核实，带依据）

| 项 | 现状（源码实证） | 结论 |
|---|---|---|
| fork eval | `new Function('ctx','expr','with(ctx){return eval(expr)}')` + `interpolate(ctx, value)`（fork config/utils.ts:5-22） | ✅ 参照已锁定 |
| fork ctx | 入口扩展 Context（`lib/index.js:338` `ctx.extend({[Entry.key]:this})`；混入服务属性） | ✅ 参照已锁定（成员+裸标识符） |
| Rust eval_scope | `{config, process, ctx:{}, env:{}}`（loader.rs:124-136）——**ctx 空** | ⬜ **缺口：ctx 未绑注入服务** |
| internal/config 触发点 | apply_body：`waterfall("internal/config",[fid, config0])` 早于 `current.push(fid)`（context.rs:742-753） | ⬜ 需经 `args[0]=fid` 取目标纤维（不可用 current_fiber） |
| 服务值通道 | `get_value`（context.rs:1513，经 internal/get 拦截）+ `get_raw_value`（accessor/Value 服务） | ✅ 可用；Value 型才可暴露 |
| 求值器 | dsh_eval 平坦 Scope（HashMap<String,Value>）标识符/成员访问 | ✅ 裸标识符 = 顶层键可直接支持 |
| 既有测试 | m3_expr（disabled_expr + config.k 插值）；TS host 无 !!js | ✅ 模型在 m3；golden 无（A2-SCOPE=B） |

## 6. 测试与验收标准（阶段关卡）

- **T1**：入口 plugin 注入 Value 服务 `svc`；config `{"__jsExpr": "svc.k * 2"}` → apply 得到正确值
  （ctx/裸标识符读注入服务）。
- **T2**：`ctx.svc.k` 成员访问 + 服务名与显式键（如服务名=config）冲突时显式键优先（裸标识符不覆盖）。
- **T3**：未注入的服务名 → 求值失败 → fail-loud（保留原 config + `eval-error` 写回标记）。
- **回归**：m3_expr 三测不回归；workspace + clippy 0；serve 冒烟。

## 7. 决策收敛

| 决策 | 结论 |
|---|---|
| A2-SCOPE | **B：仅 Rust 侧**（用户确认）——ctx 绑定 + 成员/裸标识符 + m-series/单测；无 golden |
| A2-BARE | **A：ctx 成员 + 裸标识符**（用户确认）——服务名注入顶层作用域，显式键优先 |

## 8. 遗留边界

- 非 Value 服务不暴露（DIV-6-1）；仅目标深层隔离可见的服务极端情况（DIV-6-2）。
- 后续优先级：B1 `[Service.extend]` / B2 Group 折叠 / B4 config simplify + A3 动态 check spike。

## 复盘追问结论（需求阶段已向用户确认）
- **假设**：绑定对象 = 目标 fiber 注入就绪上下文（H1-H4）。
- **缺失信息**：TS 侧能否对齐（fork 语义重建成本）——用户确认 A2-SCOPE=B（Rust 侧，证据退档 m-series）。
- **常见错误**：把 ctx 绑到「装载时空 ctx」或「全局 store」而非「目标 fiber 注入就绪上下文」，或在
  `current.push(fid)` 之后才绑定（时序错位）——本轮经 `args[0]=fid` 精确绑定，不动触发时序。

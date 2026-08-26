# 验收报告：服务装配单元 Phase 6 — A2 !!js 求值作用域绑定注入服务

日期：2026-08-27
阶段：测试验证（阶段 4）+ 部署与维护（阶段 5）——本文档为阶段关卡工件（验收收口）。
依据：`.spec/service-assembly-p6/requirements.md`（定稿）+ `design.md`（定稿）+ `docs/DECISIONS.md` D-143/D-144。
范围：A2-SCOPE=B（仅 Rust 侧）+ A2-BARE=A（ctx 成员 + 裸标识符）——用户确认，无 golden。

---

## 1. 交付范围（对需求/设计逐条核对）

| 项 | 要求 | 交付 | 证据 |
|---|---|---|---|
| S1 scope 构造 | ctx 绑注入服务 + 成员访问 + 裸标识符 + 显式键优先 | ✅ `eval_scope_with_services`（loader.rs） | 编译 + m21 |
| S1 API 兼容 | 空 services = 现状（m3/单测零回归） | ✅ `eval_scope_with_process` 委托空 services | m3 三测 + workspace |
| S2 绑定对象 | internal/config 插值绑**目标纤维**注入服务 | ✅ 监听器经 `args[0]=fid` + `fiber_service_ctx` | m21 T1-T3 |
| S2 disabled | disabled 表达式绑当前纤维（best-effort） | ✅ `entry_disabled` 参数化 | 编译 + 回归 |
| T1 | 裸标识符读注入服务 | ✅ m21 | `js_expr_reads_injected_service_bare_identifier` |
| T2 | ctx 成员 + 显式键优先 | ✅ m21 | `js_expr_ctx_member_and_explicit_key_precedence` |
| T3 | 未注入服务 fail-loud 保留 | ✅ m21 | `js_expr_unknown_service_fails_loud_keeps_config` |

## 2. 阶段 4（测试验证）证据

- **m21 3/3 绿**：T1 `{"__jsExpr":"svc.k"}` → apply 得 42；T2 `ctx.config.tag`=SVC（服务值）/ `config.tag`=CFG
  （显式键优先）；T3 `nope.x`（未注入）→ 求值失败 → 原 config 保留 + `eval-error` 写回标记。
- **`cargo test --workspace`**：EXIT=0，**201 目标 0 失败**（+m21，既有全量回归零破坏）。
- **`cargo clippy --workspace --all-targets -- -D warnings`**：EXIT=0。
- **`node diff/ts-host/verify-diff.mjs`**：**23/23 PASS**——listener 新增 `internal/get` 读取（内部事件，
  无 trace）与 `entry_disabled` 参数化对既有 23 golden **逐字节零回归**。

## 3. 编码期发现与取舍（如实记录）

- **时序约束**：`apply_body` 的 `internal/config` waterfall **早于** `current.push(fid)`（context.rs:
  742-753）——若用 `current_fiber()` 拿到的是父纤维（错位）；绑定必须经 `args[0]=fid` 显式取目标纤维。
- **waterfall 重入**：`get_value`（internal/get waterfall）在 internal/config 监听器内嵌套调用——已核实
  `waterfall` 每次构造独立 `WfChain`（可重入安全）。
- **dead-code**：生产路径统一走 `eval_scope_with_services` 后 `eval_scope` 仅剩测试使用 → `#[cfg(test)]`。
- **A2-SCOPE=B 边界**：等价证据由 m21 + 单测锁定（无 golden；TS host 无 `!!js` 支持，fork 语义重建
  成本高——用户确认取舍）。

## 4. 阶段 5（部署与维护）证据

- **部署冒烟**：`dsh web target/web/cordis.yml --port 60886`（本轮含 dsh-loader internal/config 改动）
  → `GET /` **HTTP 200**（len 13270 与基线一致），进程干净停止——真实启动链路零回归。
- **部署面**：空注入上下文时行为与现状逐字节一致（唯一变化仅在「配置表达式 + 该纤维有 Value 注入服务」
  同时成立时）；纯增量。回滚 = `git revert 1dd6476`。

## 5. 诚实边界（未做 / 延后）

- 仅 Value 型服务暴露（DIV-6-1；`Arc<dyn Any>` 非 JSON 不暴露）。
- `get_value` 按监听时刻 store 可见性解析（DIV-6-2；祖先提供可读，深层隔离极端场景受限）。
- 非 B 类 / A3 未做（后续优先级：B1 extend / B2 Group 折叠 / B4 config simplify + A3 动态 check spike）。

## 6. 决策链互查

`D-141 需求（1593264）→ D-142 设计（f3f2257）→ D-143 编码（1dd6476）→ 本验收（D-144，待提交）`。
改动 → git 提交 → DECISIONS 条目一一对应。

## 7. 结论

**通过**：A2（`!!js` 求值作用域绑定注入服务）五阶段闭环。Rust 可按 fork `with(ctx)` 语义在 config
表达式中读取当前纤维注入服务（裸标识符 + `ctx.svc`，显式键优先，失败 fail-loud 保留）；m21 3/3、
201 目标全绿、clippy 0、23 golden 零回归、serve 冒烟 HTTP 200。

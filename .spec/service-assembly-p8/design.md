# 设计：服务装配单元 Phase 8 — B2 Group 子入口失败 fail-loud（loader 装载事务层）

日期：2026-08-27
阶段：系统设计（瀑布流阶段 2，Phase 8）——本文档为阶段关卡工件。
依据：`.spec/service-assembly-p8/requirements.md`（需求定稿）+ fork 源码实证。

---

## 1. 设计目标

Rust 组装载在**子入口失败**时按 cordis 语义 **fail-loud + 回滚**：`load_group_plugin`(sync) /
`load_group_plugin_async` 在 Group fiber 自身无错后，检查组内子入口纤维；任一 Failed → 返回其错误 →
`create`/`update` 的既有回滚（`dispose_entry(g)` 级联停止子入口）完成清理。不动 dsh-core fiber 层、
不动正常分组路径。

## 2. 自下而上锚点（本阶段核实）

| 锚点 | 基址 | 用途 |
|---|---|---|
| fork 子失败 → reject | fork `lib/index.js:522-533` `_start`: `await fiber.await()`（子失败 reject）→ `_init`/`update` 抛 → group.ts:71-80 allSettled 抛 | 装载期失败语义 |
| Rust 组装载吞错 | loader.rs `load_group_plugin`(875)/`load_group_plugin_async`(1519) 只查自身 `fiber_error`；GroupPlugin Await 恒 None；child `start_entry(...).ok()` 吞 | 修正点 |
| create 回滚 | loader.rs:630-638（start_entry Err → `dispose_entry(id)` → entries.remove） | 复用回滚 |
| dispose_entry(g) 级联 | loader.rs:1078-1096（group 先 `for c in children { dispose_entry(c) }` 再卸自身） | 已加载兄弟清理 |
| 子入口记录 | `st.entries[c].fiber` + `st.entries[g].subgroup → st.groups[sg].data` | 组内子列表 |
| 失败判定 | `ctx.fiber_error(fid)`（仅失败纤维有值，与 m20 T3 / loader-02 fail-loud 约定一致） | H3 |

## 3. 设计分解

### S1（dsh-loader 组装载 fail-loud）

```text
// loader.rs
fn group_child_error(&self, gid: &str) -> Option<CordisError>:
    children = entries[gid].subgroup → groups[sg].data   // 组内子入口 id
    for c in children:
        fid = entries[c].fiber
        if let Some(err) = ctx.fiber_error(fid) { return Some(err) }   // 首个失败子返回错误
    None

// load_group_plugin（sync）/ load_group_plugin_async：
    ... fiber_error(fid) 检查 ...
    if let Some(err) = self.group_child_error(id) { return Err(err) }   // B2
```

- 调用时机 = `plugin_arc`/`plugin_arc_async` 返回后（子入口已 settle：sync 两阶段 / async Finish
  均跑完，H2）。
- `create` / `update` 的既有 Err 路径（sync-630 / async-rest ∈ start_entry_async 1505-1511）自动回滚。

### S2（m-series 红测，crates/dsh-loader/tests/m23_group_failure.rs）

| # | 红测 | 断言（绿） |
|---|---|---|
| V1 | 组 [c1=ok, c2=bad] → `loader.create(g)` | `unwrap_err` 含 "boom"（fail-loud）；`applied>=1`（c1 先加载）；`fiber("c1")/fiber("c2")/fiber("g")` 均 None（回滚清理：已加载兄弟被停止 + 失败子 + 组入口） |

- 首版红（修复前）：create 返回 Ok、group Active —— 实锤吞错。

## 4. 实现顺序（TDD）

1. **S1**：`group_child_error` + 两处调用（compile）。
2. **S2**：m23（fail-loud 断言）红→绿。
3. **回归**：`cargo test -p dsh-loader` + `verify-diff`（loader-10 等 23 golden 零回归——正常分组路径
  不受新检查影响：healthy 子入口无 fiber_error）+ workspace + clippy。
4. **阶段 5**：serve 冒烟 + acceptance。

## 5. DIV / 让步清单

- **DIV-8-1**：子失败场景无 golden（TS loader-sync reject）；m-series 证据（B2 非核心先例）。
- **DIV-8-2**：组中组（多层嵌套）子失败由传递性覆盖（子组自身按本设计 fail-loud → 父组 `group_child_error`
  视其为 Failed 子）。本设计仅直接检查**一层** children；多层靠子组自身装载失败传播（`entry.fiber` 的
  子组 fiber 会 Failed——断言层用 `fiber_error` 覆盖）。
- **DIV-8-3**：`group_child_error` 返回**首个**失败子之错误（fail-loud；多失败聚合不做——r.n. 单错误
  契约与 loader-02 一致）。

## 6. 部署与回滚（阶段 5 预案）

- 部署：语义对齐（子失败 → 装载失败 + 回滚）；正常分组路径零改动（healthy 子无 error）。
- 回滚：`git revert` 本阶段提交（loader 两组检查 + m23）。

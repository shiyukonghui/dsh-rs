# 验收报告：服务装配单元 Phase 8 — B2 Group 折叠验证与子入口失败 fail-loud

日期：2026-08-27
阶段：测试验证（阶段 4）+ 部署与维护（阶段 5）——本文档为阶段关卡工件（验收收口）。
依据：`.spec/service-assembly-p8/requirements.md` + `design.md`（定稿）+ `docs/DECISIONS.md` D-149/D-150/D-151。
范围：B2 = 三约定核对（既有证据收口）+ 子入口失败 fail-loud 修复（m23 m-series，无 golden——DIV-8-1）。

---

## 1. 交付范围（对需求/设计逐条核对）

| 项 | 要求 | 交付 | 证据 |
|---|---|---|---|
| 事件顺序 / init await 核对 | Group Active 在 children 后；stop 逆序 | ✅ 既有 | `loader-10-group-nested.golden`（async 驱动逐行） |
| group 与消费者同 realm 核对 | 组 realm 内 provider/consumer 同域 | ✅ 既有 | `m3_isolate::group_realm_walk_resolves_parent_provider` + `loader-15-cross-realm-walk.golden` |
| 子失败 fail-loud（缺口） | 子入口失败 → 组装载失败 + 回滚 | ✅ `group_child_error`（sync+async load_group_plugin） | m23（红→绿） |
| 回滚清理 | 已加载兄弟被停止 + 失败子/组清理 | ✅ 复用 `create`→`dispose_entry(g)` 级联 | m23（fiber 均 None） |

## 2. 阶段 4（测试验证）证据

- **m23 红→绿**：修复前 `create(g)` 返回 **Ok**、group **Active**（m23 首版红断言 `Some(Active)`≠`Failed`
  ——实锤吞错）；修复后断言按 **fail-loud + 回滚**契约：`create(g)` `unwrap_err()` 含 `"boom"`、
  `applied >= 1`（c1 先加载成功）、`fiber("c1")/fiber("c2")/fiber("g")` 均 None（回滚停止已加载兄弟 +
  失败子 + 移除组入口）。
- **`cargo test -p dsh-loader`**：EXIT=0（含 m23 + 既有全量）。
- **`cargo test --workspace`**：EXIT=0，**203 目标 0 失败**。
- **`cargo clippy --workspace --all-targets -- -D warnings`**：EXIT=0。
- **`node diff/ts-host/verify-diff.mjs`**：**23/23 PASS**——正常分组路径（loader-10/15 等）零回归
  （healthy 子入口无 `fiber_error`，新检查不触发）+ 子失败场景无 golden（TS loader-sync reject，DIV-8-1）。

## 3. 编码期发现与取舍（如实记录）

- **HANDOFF 描述过时**：B2 条目称「Rust 在 loader 层展开、无独立 Group 插件 fiber」——实际 M22 起
  `GroupPlugin`（loader.rs:341）为真实 fiber，三约定前两条早已由既有 golden/m-series 对齐；B2 之
  剩余 = **子入口失败吞错**这一真实缺口。
- **语义修订（m23 红期）**：初版断言「Group 纤维应 Failed」→ 对照 fork 确认 cordis 是**装载事务失败**
  （`_start await fiber.await()` reject → loader-sync/update reject + 回滚），非「Group 留存 Failed」——
  修订为 fail-loud + 回滚（m20 T3 / loader-02 同契约）。
- **修复定位**：loader 装载事务层（`group_child_error` 前置检查）而非 dsh-core fiber 层——与 cordis
  «装载期 reject» 语义一致，且复用既有 `create/update` 回滚，零侵入正常分组路径。

## 4. 阶段 5（部署与维护）证据

- **部署冒烟**：`dsh web target/web/cordis.yml --port 60888`（本轮含 dsh-loader 组装载检查）→
  `GET /` **HTTP 200**（len 13270 与基线一致），进程干净停止——真实启动链路零回归。
- **部署面**：family 语义新增「组内任一子入口失败 → 该组装载 fail-loud + 回滚」；正常分组路径行为
  不变。回滚 = `git revert 746e982`（loader 两组检查 + m23，特征级整体）。

## 5. 诚实边界（未做 / 延后）

- 子失败场景无 golden（DIV-8-1；TS loader-sync 对失败装载 reject）：m-series 证据。
- 多层嵌套（组中组）子失败由传递性覆盖（DIV-8-2）；多失败聚合不做（DIV-8-3，返回首个失败子错误）。
- B4 config simplify / A3 动态 check spike 未做（后续优先级）。

## 6. 决策链互查

`D-149 需求+设计（e770bc2）→ D-150 编码（746e982）→ 本验收（D-151，待提交）`。
改动 → git 提交 → DECISIONS 条目一一对应（本阶段需求+设计并闸——验证型任务，较轻）。

## 7. 结论

**通过**：B2（Group 折叠）验收完成。三约定（事件顺序 / `[Service.init]` await / group 同 realm）经既有
golden+m-series 交叉锁定零回归；真实缺口「组内子入口失败被吞」已按 cordis 装载事务语义修复——
m23 红→绿（fail-loud + 回滚）、203 目标全绿、clippy 0、23 golden 零回归、serve 冒烟 HTTP 200。

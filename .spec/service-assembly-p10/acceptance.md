# 验收报告：服务装配单元 Phase 10 — A3 动态 check spike

日期：2026-08-27
阶段：测试验证（阶段 4）+ 部署与维护（阶段 5）——本文档为阶段关卡工件（验收收口）。
依据：`.spec/service-assembly-p10/requirements.md` + `design.md`（定稿）+ `docs/DECISIONS.md`
D-155/D-156/D-157。
范围：A3 = 直接问题（Rust `provide` 有 check 谓词——已证）+ 动态 check spike（m25 parity 锁定；
零生产代码改动）。

---

## 1. 交付范围（对需求/设计逐条核对）

| 项 | 要求 | 交付 | 证据 |
|---|---|---|---|
| 谓词存在性（HANDOFF 直接问题） | Rust `provide` 有 check | ✅ `provide_with`+`check_ok`+`check_impls` | m7_await + scenario-10 golden（既有） |
| 静态门 | check=false → 依赖方 Pending/不激活 | ✅ 既有（本 spike 复锁） | m25 断言 1 |
| 纯翻转非反应式 | 谓词翻转变无 notify 触发点 → 保持原状（cordis 同位） | ✅ | m25 断言 2 |
| 动态激活 | reload + check=true → 依赖方 Active | ✅ | m25 断言 3 / 5 |
| 动态失效 | reload + check=false → 依赖方回 Pending | ✅ | m25 断言 4 |
| 零生产改动 | spike 验证非修复 | ✅ | m25 仅测试 |

## 2. 阶段 4（测试验证）证据

- **m25 1 测试 / 5 断言全绿**（红→绿：机制已在，前 2 断言锁定既有语义、后 3 断言锁定动态 parity）。
- **`cargo test --workspace`**：EXIT=0，**205 目标 0 失败**（m24 204 + m25）。
- **`cargo clippy --workspace --all-targets -- -D warnings`**：EXIT=0（红期 doc 列表缩进 lint 修复）。
- **`node diff/ts-host/verify-diff.mjs`**：**23/23 PASS**（零回归）。

## 3. spike 结论（如实记录）

- HANDOFF A3 字面问题（「Rust provide 是否有 check 谓词」）**已否**——谓词存在，静态门由 m7 +
  scenario-10 golden 锁定。
- cordis 动态再求值触发点（源码实证）：provide-while-Active / unprovide / 提供者 ACTIVE↔NON-ACTIVE
  翻转 → `notify` → 依赖方 `_checkImpl`（重求值 check，不成立删 store → epoch INACTIVE）。
- Rust 由 produce-disposer 驱动的卸载/重载路径（unload → `remove_impl+notify` → 重 apply re-provide
  → `finish_load` notify）+ `check_impls`/`refresh_fiber` 覆盖同语义——**m25 实证 parity**。
- **纯谓词翻转非反应式** 系 cordis 语义（无 notify 触发点不广播），非缺口；Rust 同位（断 2）。
- 未引入任何生产代码改动（若 m25 红本应回需求重评估——未发生）。

## 4. 阶段 5（部署与维护）证据

- **部署冒烟**：`dsh web target/web/cordis.yml --port 60890` → `GET /` **HTTP 200**（len 13270 基线
  一致），进程干净停止。无生产路径变化（纯测试锁定），回滚 = 撤 m25 + acceptance 工件提交。
- **部署面**：零运行时行为变化；A3 闭环 = 证据性（存在性 + parity）。

## 5. 诚实边界（未做 / 延后）

- 动态翻转不可 golden（TS 场景 DSL 无运行期 flag 翻转；DIV-10-1）；m-series 锁定。
- 谓词翻转不加自动 notify 广播（DIV-10-2，cordis 非反应式同位）。

## 6. 决策链互查

`D-155 需求+设计（fba9f86）→ D-156 spike（52f812e）→ 本验收（D-157，待提交）`。
改动 → git 提交 → DECISIONS 条目一一对应。

## 7. 结论

**通过**：A3（动态 check spike）验收完成。Rust `provide` 的 check 谓词存在（m7/golden）且动态再求值
触发点与 cordis **全面 parity**（m25 5 断言）；零生产改动；workspace 205 目标全绿、clippy 0、
23 golden 零回归、serve 冒烟 HTTP 200。**至此目标全部 HANDOFF 缺口（A5/A2/B1/B2/B4/A3）闭环。**

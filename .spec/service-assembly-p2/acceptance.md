# 验收报告：服务装配单元 Phase 2 — A3+A4 依赖激活核对

日期：2026-08-26
阶段：测试验证（阶段 4）+ 部署与维护（阶段 5，Phase 2）——本文档为阶段关卡工件。
依据：`.spec/service-assembly-p2/requirements.md`（需求定稿）· `design.md`（设计定稿）。

---

## 1. 交付范围（对接 requirements.md §1 的 A3/A4 子点）

| 子点 | 等价证据（dsh-diff golden） | m 系列锁定 | 结论 |
|---|---|---|---|
| **A3a check 谓词门** | `scenario-10-provide-check-gate.golden`：provider `provide svc`（check:false）→ provider Active、consumer 保持 PENDING（6 行，TS↔Rust 一致） | m7_await `await_gated_by_check_predicate`（check=false → 依赖方 Pending） | ✅ 等价 |
| **A3b strict-active** | 既有 `06`/`loader-13` golden 已证「wait-then-Active」（consumer 于 provider Loading 期不执行，Active 后才激活） | — | ✅ 已有覆盖 |
| **A4a unprovide 顺序** | `scenario-11-unprovide-order.golden`：provide 后立即 unprovide → 后续依赖方 PENDING、provider 保持 Active（6 行一致）。**判明 Rust `remove_impl→notify` 与 TS「先 notify 再自清」在 trace 级无可观察差** | — | ✅ 等价，无需修复 |
| **A4b 注入快照/epoch** | 既有 06/08/loader-13 golden 覆盖 reload 依赖重估 | — | ✅ 已有 |
| **A4c 跨隔离父链 walk** | `loader-15-cross-realm-walk.golden`：group isolate realm 内 provider provide svc → 子 consumer 沿父链 walk 落组 realm → Active+apply/log（14 行一致） | m3_isolate `group_realm_walk_resolves_parent_provider` | ✅ 等价 |

**关键结论**：A3/A4 核心在 Rust 本就具备且与 TS 等价；**唯一的真缺口 = 等价覆盖为零**（18 剧本无
check 用例）。本阶段补齐覆盖 + DSL 表达能力，**无需 dsh-core 核心修复**。

## 2. 测试验证（阶段 4）逐条证据

1. `node diff/ts-host/verify-diff.mjs` → **21/21 PASS**：新增 `scenario-10`/`scenario-11`/
   `loader-15` 三个 golden（TS 原版 cordis 生成）与 Rust **逐行一致**；既有 18 场景 golden 零回归。
2. `cargo test --workspace` → EXIT=0 全绿（含 m7_await 5/5 + m3_isolate 3/3 新用例）。
3. `cargo clippy --workspace --all-targets -- -D warnings` → EXIT=0 零告警。
4. 红→绿纪律：DSL `check` 字段缺失阶段新剧本无法表达语义（红）→ 两侧对称补齐 → golden 对齐（绿）。

## 3. 部署与维护（阶段 5）

- **serve 冒烟**：`dsh web target/web/cordis.yml`（port 60882）`/` HTTP 200、进程干净退出——Phase 2
  未改任何运行面（dsh-core/boot/serve 零改动），serve 零回归（冒烟后进程已停）。
- **运行方式/回滚**：运行方式同 Phase 1（`dsh web <cordis.yml>`）；本阶段改动全在 dsh-diff 基建 +
  测试，`git revert e97dc05`（独立回滚点）即回。
- **维护**：dsh-diff 剧本现 21 个；新增语义先补 golden 再实现（本章纪律不变）。

## 4. 诚实边界（非本次交付）

- A3 的**动态 check 态变**（谓词随时间/上下文翻转）无法在静态剧本表达——spike 另立（DIV-2-2）；
  静态 check=false 门已锁定等价。
- A5（intercept `resolveConfig` 合并）、A6（生成器 effect `[Service.init]`）、B1-B4（extend/Group
  折叠/HMR 模块热更/config simplify）——仍后续阶段。
- A3b strict-active 无独立新剧本（06/loader-13 已证 wait-then-Active），诚实不重复造。

## 5. 决策链与 git 互查
Phase 2：requirements（`ba6d78d` D-124）→ design（`3cbd48b` D-125）→ 编码（`e97dc05` D-126）→
本验收工件。改动 → 提交 → DECISIONS 逐条可互查。

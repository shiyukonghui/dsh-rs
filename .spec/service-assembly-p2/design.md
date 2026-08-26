# 设计：服务装配单元 Phase 2 — A3+A4 依赖激活核对

日期：2026-08-26
阶段：系统设计（瀑布流阶段 2，Phase 2）——本文档为阶段关卡工件。
依据：`.spec/service-assembly-p2/requirements.md`（需求定稿）+ `docs/SERVICE-ASSEMBLY-HANDOFF.md` §3 A3/A4。

---

## 1. 设计目标

把 A3（提供者 check/strict-active）与 A4（unprovide 顺序/注入快照/父链 walk）与 TS cordis 的等价性
**用 dsh-diff golden 固化**；任何 trace 分歧以 TS 为权威修复 Rust。设计零新 crate、零新依赖，只动
diff 基建（scenario-host.mjs / Rust ScenarioPlugin）+ 可能的 dsh-core 顺序微调 + m 系列红测。

## 2. 自下而上锚点（本阶段核实）

| 锚点 | 基址 | 用途 |
|---|---|---|
| `provide_with(name,value,check)` | context.rs:1275 | A3 check 谓词入口（已有） |
| `check_ok()` / `Impl.check` | reflect.rs:11-34 | A3 谓词求值（已有） |
| `check_impls` 用 check_ok | runtime.rs:610-630 / 616 | check=false→依赖方 PENDING（已有） |
| disposer = remove_impl→notify | context.rs:1305-1314 | A4a 顺序（待 golden 判定） |
| `refresh_fiber` epoch | runtime.rs:633-666 | A4b 注入快照（已有） |
| `resolve_scope` 父链 walk | runtime.rs:301-315 | A4c（已有） |
| dsh-diff `provide` op 无 check | scenario-host.mjs:80-82 / dsh-diff lib.rs:610-618 | DSP 需对称扩展 |
| scenario-host `dispose-effect` | scenario-host.mjs:48 / dsh-diff lib.rs:532 | A4a unprovide 剧本载体 |

## 3. 设计分解

### S1（前置）：dsh-diff DSL 对称扩展——`provide` op 增可选 `check`

- **TS（scenario-host.mjs）**：`provide` case 解析 `op.check`（可选布尔）；`ctx.provide(op.service,
  op.value, () => op.check !== false)`；trace 仍只打 `provide:{service}:{value}`（check 改变的是
  「依赖方是否激活」这一可观察结果，不打进 provide 行——与 06 一致）。
- **Rust（dsh-diff lib.rs `ApplyOp::Provide`）**：增字段 `#[serde(default)] check: Option<bool>`；
  `apply_op` 在 `check=Some(false)` 时用 `ctx.provide_with(service, value, Some(Box::new(|| false)))`，
  否则 `ctx.provide(service, value)`。红测：新剧本（check:false）在无该字段时 TS/Rust 解析不一致
  或 consumer 错误激活 → 绿。
- 依赖 `loader-host.mjs` 与 `scenario-host.mjs` 两侧都加（loader 剧本若用 check 亦可用）。

### S2：等价 golden 集（每条 TS 生成、Rust 对齐）

| 剧本 | A 子点 | 骨架 | 期望 trace 要点 |
|---|---|---|---|
| `scenario-10-provide-check-gate.json` | A3a | consumer(`inject:["svc"]`) + provider(`provide svc:"v1", check:false`)；顺序 plugin consumer → plugin provider | consumer 创建后保持 Pending（**无** apply:consumer）；provider Active；provide 行存在 |
| `scenario-11-unprovide-order.json` | A4a | provider(`apply:[provide svc]`)，consumer(`inject:["svc"]`)；步骤 plugin consumer → plugin provider；`dispose-effect index 0`（unprovide，不卸 fiber） | provider provide → consumer Active；dispose-effect → consumer Pending/Loading→失依赖（卸载依赖方侧 Active:Unloading→Pending），provider 仍 Active |
| `loader-13b-strict-active-order.json`（或并入现有 loader-13/06 断言） | A3b | consumer 先挂（PENDING）→ provider 后挂（provide）→ consumer Active；Loading 期不满足 | 同 06 的 wait-then-activate（strict 面 = provider Active 前不喂执行） |
| `loader-15-cross-realm-walk.json` | A4c | group 入口带 `isolate:{svc:true}`；子 provider（provide svc 入院 realm）+ 子 consumer（`inject:["svc"]` 无自身 isolate）；loader-sync 挂载 | consumer 沿父链 walk 落 group realm → 解析 provider 的 svc → Active；证明跨隔离边界可见性等价 |

- 每条先红（DSL/场景缺字段 → TS 或 Rust 解析失败；或不预期激活）→ 绿（golden 精确一致）。
- `nome` 及既有 18 场景零回归（verify-diff 全量）。

### S3：m 系列红测（阿里对齐修复的锚点）

- `m7_await.rs` 扩展：`provide_with(check=false)` → inject 该服务的 `ctx.await` 永不 resolve /
  依赖方不激活；check=true → resolve/激活（红→绿，TDD 驱动无 DSL 的纯核心路径）。
- `m3_isolate.rs` 扩展：group isolate realm 内 consumer `get("svc")` 沿父链 walk 落 group realm 的
  provider impl（红→绿）。
- 若 S2 任一 golden 暴露**顺序/可见性分歧** → 在 dsh-core 对齐（以 TS reflect.ts 为权威），加对应
  红测并记 DECISIONS（DIV-2-1）。

## 4. 实现顺序（TDD）

1. **S1** DSL `check` 对称扩展（TS + Rust）——先红（缺字段）后绿。独立提交。
2. **S2** golden 集：`scenario-10`（A3a）→ `scenario-11`（A4a）→ `loader-15`（A4c）→
   strict-active 断言并入 06/loader-13（A3b）。逐条 verify-diff 生成并对齐。独立提交。
3. **S3** m 系列红测（m7 check await / m3_isolate 跨 realm）+ 发现的分歧修复。独立提交。
4. 回归门槛 + clippy 0 + 部署冒烟（serve 零回归）。

## 5. DIV / 让步清单

- DIV-2-1 unprovide 顺序：以 TS「先 notify 再自清」为权威；若 golden 显示可观察差 → 改 disposer
  为 notify-先（依赖方重估用当前表），并 m 系列锁序。
- DIV-2-2 check 谓词 = 静态布尔（剧本可表达）；动态态变 check（如随时间翻转）spike 另立。
- DIV-2-3 父链 walk = 解析落 realm + 块级时序（loader 级 isolate/golden 承载）。
- DIV-2-4 strict-active 不单独造剧本（06/loader-13 已证 wait-then-Active）；仅复核断言。

## 6. 部署与回滚（阶段 5 预案）

- 部署冒烟：既有 `dsh web` serve（生产 target/web/cordis.yml）零回归（无 key 门控同 Phase 1）。
- 回滚：S1/S2/S3 各自独立 `git revert`（DSL/golden/m 系列，互不耦合）。

# 需求结论：服务装配单元 Phase 8 — B2 Group 折叠验证与子入口失败 fail-loud

日期：2026-08-27
阶段：需求分析 + 系统设计（瀑布流阶段 1-2，Phase 8）——本文档为阶段关卡工件。
依据：`docs/SERVICE-ASSEMBLY-HANDOFF.md` §3 B2 + fork 源码实证 + §0 验收。

---

## 1. 目标（Top-down → Bottom-up）

第一性原理：B2（HANDOFF：「Rust 在 loader 层展开、无独立 Group 插件 fiber」）**已过时**——M22 起 Rust
`GroupPlugin`（loader.rs:341）以真实 fiber 形态注册（`plugin:Group`/`status:Group`），children 挂载于
Group fiber 之下。B2 实为**三约定核对 + 一个真实缺口**：

| 约定 | 证据（既有） | 结论 |
|---|---|---|
| 事件顺序 / `[Service.init]` await（group Active 在 children 之后；stop 逆序） | `loader-10-group-nested` golden（TS 原版↔Rust 逐行，async 驱动） | ✅ 已对齐 |
| 「group 与消费者同 realm」 | `m3_isolate::group_realm_walk_resolves_parent_provider`（组 isolate → 子 provider 供 svc → 子 consumer 沿父链 walk 激活）+ `loader-15-cross-realm-walk.golden` | ✅ 已对齐 |
| **子入口失败 → 组装载失败 + 回滚**（真缺口） | fork `_start`（`lib/index.js:522-533`）`await fiber.await()` —— 子 fiber 失败 → **reject** → `entry.update` 抛 → group `update`（allSettled，group.ts:71-80）抛 → group init 失败 → loader 装载失败 | ⬜ **Rust `load_group_plugin` 只查 Group 自身 fiber_error，组内子失败被吞**（`.ok()` / Await 恒 None）→ group 保持 Active（m23 首版红证） |

**验收** = m23「组内子入口 apply 失败 → create fail-loud（保留子错误）+ 回滚（已加载兄弟/全部子入口被
停止清理）」绿 + 既有 3 约定零回归（loader-10 golden / m3_isolate / loader-15）+ 全 workspace + clippy 0
+ serve 冒烟。

## 2. 非目标

- **不做** dsh-core fiber 状态层改动（group 失败不依赖核心层 Failed 检测——失败判定与清理都在 loader
  层，语义对齐 cordis loader 装载事务）。
- **不**动正常分组路径（loader-10 的 Group Active 时序 / dispose_entry 级联卸载）。
- **不做** TS golden 于子失败场景——TS `loader-sync` 对失败装载会 **reject**（无法产出稳定 golden）；
  等价证据用 m-series（延续 A2/B1 的 B 类非核心先例）。
- **不做** B4 config simplify / A3（后续优先级）。

## 3. 假设（复盘确认）

- **H1**：组内子入口失败以**装载失败**（create/update 返回 Err）呈现，回滚经既有
  `dispose_entry(g)`（loader.rs:1078 级联子入口）完成。
- **H2**：检测点在子入口**完全 settle 之后**（sync 两阶段 / async Finish 皆跑完）——不可提前
  （否则刚注册未 apply 的子入口误判）。
- **H3**：`fiber_error(fid)` 仅失败纤维有值（为 Null 的骨骼），可作为「子入口 Failed」判定。
- **H4**：证据 m-series（B2 非核心，历史先例 A2-SCOPE=B / B1-PROOF=A 语义一致）。

## 4. 硬约束

- 新语义落 m23 红→绿（首版红 = 当前吞错行为实锤）；24...实际 23 golden 零回归（loader-10 等）；
  workspace + clippy 0。
- DECISIONS 追加；改动 → git 提交 → 决策条目互查。

## 5. 决策收敛

| 决策 | 结论 |
|---|---|
| B2 缺口处理 | **fail-loud + 回滚**（cordis 装载事务语义）：`load_group_plugin`(sync) 与 `load_group_plugin_async` 增 `group_child_error` 前置检查 → Err → `create/update` 既有回滚清理 |
| B2 证据 | **m-series**（m23 fail-loud + 回滚断言）；既有 3 约定由 golden/m-series 交叉引用锁 |

## 6. 遗留边界

- 子失败场景无 golden（TS reject）；三级以上嵌套子失败（组中组）由传递性覆盖（子组自身失败 → 父组
  视同子失败）。
- B4 / A3 后续。

## 复盘追问结论
- **假设/缺失**：三约定已由既有证据覆盖；真缺口 = 子失败吞错（红证）。B 类非核心 → m-series 证据
  （延续用户此前 A2/B1 确认先例；如要 golden 需建 TS 失败场景工厂，成本高）。
- **常见错误**：把修复错放 dsh-core fiber 层（改「Group 纤维 Failed」）而非 loader 装载事务层——
  后者与 cordis `_start await fiber.await()` 的**装载期 reject** 语义一致，且复用既有回滚。

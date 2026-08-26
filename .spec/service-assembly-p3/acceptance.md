# 验收报告：服务装配单元 Phase 3 — B3 HMR 模块热更（身份换代 → 受影响 entry reload）

日期：2026-08-26
阶段：测试验证（瀑布流阶段 4）+ 部署与维护（阶段 5）——本文档为阶段关卡工件（验收收口）。
依据：`.spec/service-assembly-p3/requirements.md`（定稿）+ `design.md`（定稿）+ `docs/DECISIONS.md` D-130。

---

## 1. 交付范围（对需求/设计逐条核对）

| 项 | 需求/设计要求 | 交付 | 证据 |
|---|---|---|---|
| S1 `replace_plugin` | 同 name 换实现（身份换代 → 受影响 entry reload）；同实现幂等 `Ok(0)`；返回受影响数 | ✅ `Loader::replace_plugin` | m18 T1/T3/T4 |
| S1 `stale_entry_ids` | name 下以旧身份加载的 entry 集（宿主/HMR 观测） | ✅ `Loader::stale_entry_ids` | m18 T4 |
| S1 `reload_entry` | disabled no-op；否则 dispose_entry+start_entry（entry 保真） | ✅ 内部实现 | m18 T1 |
| T1 | v1 注册→create→`replace_plugin(v2)` → entry 自动 reload 新实现（apply v2、identity=新、fiber Active、id/options 保真） | ✅ | `replace_plugin_reloads_entry_with_new_impl` |
| T2 | 换代提供者实现 → 依赖方经 uid/epoch 自动重活（DIV-3-1） | ✅ | `replace_plugin_revives_dependency_consumer` |
| T3 | 同实现（同一 Arc）→ 幂等：`Ok(0)`、generation 不变、无 reload | ✅ | `replace_plugin_same_impl_is_noop` |
| T4 | 受影响计数；换代前后 stale 集观测 | ✅ | `replace_plugin_counts_stale_entries` |

**等价主证据**（DIV-3-2）：本例无新 dsh-diff golden（DSL 无法表达「同 name 换实现」）；等价性 =
m-series 红→绿 + 既有 21 场景逐字节零回归（见 §3）。

## 2. 阶段 4（测试验证）证据

- **m18 TDD 红→绿**：T1/T4 首轮**红**（reload 取到旧实现）→ 根因修复 → 4/4 绿。红测证明
  行为缺口真实存在（非测试自误）：红时 reload 后 apply 仍记 v1，白盒定位到 runtime 按名模块缓存
  未随 re-import 更新。
- **`cargo test --workspace`**：EXIT=0，198 个测试目标全部 ok、0 失败（含 m18 4/4、既有 m1-m17/dsh-core/
  dsh-diff/dsh-wasmrt/dsh-cli 全量回归零破坏）。
- **`cargo clippy --workspace --all-targets -- -D warnings`**：EXIT=0，零告警。
- **`node diff/ts-host/verify-diff.mjs`**：21/21 PASS，golden 逐字节不变（零回归）。

## 3. 编码期发现（对设计的自下而上修正，诚实记录）

- **发现**：D-129 设计假定 reload 后 entry「以当前注册新实现重挂载」，但编码期 TDD 红测暴露
  dsh-core 深层缺口——runtime `register_plugin` 的按名**模块缓存**（`registry[name].plugin`，
  供 `begin_load` 取插件）仅在**首次**注册（`or_insert_with`）时写入；同名 re-import 不覆盖 →
  `begin_load` 取到陈旧实现（`replace_plugin` reload、`remove+create` 重载、`dynamic_activate` 新 entry
  三类路径均受影响）。这与 cordis `registry.plugin(name, cb)` **按名替换**语义相悖。
- **修复（D-130，触及 dsh-core）**：runtime `register_plugin` 处始终 `record.plugin = plugin.clone()`
  （按名覆盖）。零回归（既有用例无「同 name 换实现」），m16 A1-c（loader 级身份断言）不受影响。
- **影响面**：本阶段实际修改 2 个 crate（dsh-core 运行时模块缓存 + dsh-loader 热更层）+ 新增 m18，
  比设计预判（仅 loader）多一处 core 修复——已按「越级」纪律先定位、再修复、后重跑全量验证，未跳过任何关卡。

## 4. 阶段 5（部署与维护）证据

- **部署冒烟**：`target\debug\dsh.exe web target/web/cordis.yml --port 60883`（真实 `dsh web` serve，
  本次含 dsh-core 运行时改动）→ `GET /` **HTTP 200**（len 13270），进程干净停止
  （`Get-Process -Name dsh` 无存活）。证明运行时模块缓存替换不破坏真实启动链路。
- **部署**：`replace_plugin(name, 新实现)` 为宿主可调用公开 API（serve/dynamic runner 可选接线）；
  配置文件 watcher（hmr.rs，registerConfig 层）保持不动。
- **回滚**：`git revert a8793e3`（D-130，loader 层 + core 缓存替换 + m18 特征级整体回滚，独立回滚点）；
  文档回滚 = `git revert` D-128/D-129 对应提交。core 修复若单独回滚会使 B3 退回「reload 取旧实现」。

## 5. 诚实边界（未做 / 延后）

- 无新 dsh-diff golden（DIV-3-2，DSL 无法表达同 name 换实现）——等价以 m-series + 既有 21 零回归承载。
- Node 模块图（imports graph）精确同构不成立（DIV-3-1）——以「依赖方经 fiber uid/epoch 自动重活」等价承载。
- group 入口不参与实现级热更（合成 GroupPlugin 非注册表实现，DIV-3-3）——B2 Group 折叠后续。
- A6（生成器 effect `[Service.init]`）/ A5（intercept resolveConfig）/ A2（`!!js` 边界）/ B1（extend）/
  B4（config simplify）仍按 HANDOFF 后续立项；浏览器 E2E（`--dump-dom`）按仓纪律代偿。
- 注：T2 的「依赖方自动重活」在根因修复**前后**均可观测到（epoch 变迁机制本就正确，Phase 2 已证）；
  修复的意义是确保重活的是**新实现**（本轮 T1/T2/T4 的 apply 断言直接锁定）。

## 6. 决策链互查

`D-128 需求（5839181）→ D-129 设计（42bfcf5）→ D-130 编码+核心修复（a8793e3）→ 本验收（D-131，待提交）`。
改动 → git 提交 → DECISIONS 条目一一对应，可沿提交历史回溯。

## 7. 结论

**通过**：B3 HMR 模块热更五阶段全部闭环。实现级身份换代→受影响 entry reload 行为级等价达成，
既有 21 场景零回归、198 目标全绿、clippy 0、serve 冒烟 HTTP 200；编码期发现的 dsh-core 模块缓存
缺位已按「先红测定位→根因修复→全量重验」收口并如实记录。

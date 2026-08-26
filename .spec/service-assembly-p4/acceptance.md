# 验收报告：服务装配单元 Phase 4 — A6 异步生成器 effect（[Service.init] 完整形态）

日期：2026-08-27
阶段：测试验证（瀑布流阶段 4）+ 部署与维护（阶段 5）——本文档为阶段关卡工件（验收收口）。
依据：`.spec/service-assembly-p4/requirements.md`（定稿）+ `design.md`（定稿）+ `docs/DECISIONS.md` D-135。

---

## 1. 交付范围（对需求/设计逐条核对）

| 项 | 需求/设计要求 | 交付 | 证据 |
|---|---|---|---|
| S1 `EffectOutcome::Stream` | async 生成器 effect 形态（逐项产出 disposer） | ✅ `fiber.rs` `Stream(LocalBoxStream<'static, GenItem>)` + `GenItem` + `push_gen_disposer` | 编译 + m19 |
| S1 sync 驱动 | `now_or_never` 逐项收集；Pending 保持 Loading | ✅ `Cordis::drive_stream_sync`（context.rs） | m19 T1-T3 |
| S1 async 驱动 | 逐项 await + epoch 中途取消 + 失败保留 | ✅ `Cordis::drive_stream_async` + `drive_async_loads` 插桩 | `run_step_async` 侧（golden 走 sync；async 路径编译+插桩） |
| S1 中途取消 | epoch 变化 → 停止后续收集、已收集保留、`run_unload` 逆序执行 | ✅ `run_load` MidCancelled 分支（对齐 cordis `_reload` 的 `_unload`） | m19 T2 |
| S1 失败保留 | `Err` 项 fail_fiber（失败前已收集保留） | ✅ `process_gen_item`（`Err` → `fail_fiber`） | m19 T3 |
| T1 | 逐项收集（跨 await 边界）+ Active + 卸载逆序（C,B,A） | ✅ m19 + golden | `gen_stream_collects_in_order_and_unloads_reversed` |
| T2 | epoch 中途取消 + 保留 | ✅ m19 | `gen_stream_mid_cancel_on_epoch_change` |
| T3 | init 失败前 disposer 保留 + fail-loud | ✅ m19 | `gen_stream_fail_retains_collected_disposers` |
| S3 golden | TS 原版 ↔ Rust 逐行一致 | ✅ `scenario-12-async-generator`（14 行） | verify-diff 22/22 |

## 2. 阶段 4（测试验证）证据

- **m19 TDD 红→绿**：T2 首版断言与 cordis `_execute` 循环语义矛盾（红）→ 对照 `lib/index.js:798-840`
  修正断言（flip 步产出 B 先收集、此后停止）；T3 首版与 loader fail-loud 约定冲突（红）→ 修正为
  「`create` Err + 回滚 + 失败前 A 保留」。3/3 绿——红测证明行为缺口真实、修正方向由 TS 语义裁定。
- **`cargo test --workspace`**：EXIT=0，199 目标全部 ok、0 失败（含 m19 3/3、既有 m1-m18/dsh-core/
  dsh-diff/dsh-wasmrt/dsh-cli 全量回归零破坏）。
- **`cargo clippy --workspace --all-targets -- -D warnings`**：EXIT=0。
- **`node diff/ts-host/verify-diff.mjs`**：**22/22 PASS**——21 既有 golden 逐字节不变（零回归）+
  `scenario-12-async-generator` 新增（TS 原版 cordis async generator ↔ Rust LocalBoxStream 逐行一致：
  `plugin:g` / `status:g:Pending:Loading` / `apply:g` / `effect-reg:A` / `gen-await:m1` /
  `effect-reg:B` / `gen-await:m2` / `effect-reg:C` / `status:g:Loading:Active` /
  `status:g:Active:Unloading` / `dispose:C` / `dispose:B` / `dispose:A` / `status:g:Unloading:Disposed`）。

## 3. 编码期发现（对设计的自下而上修正，诚实记录）

- **T2 语义**：设计初版「flip 步产出不收集」与 cordis `_execute` 循环（collect 在 pre-check 之后）
  不符——修正为「flip 步产出的项**先**收集，循环顶 pre-check 之后再停止（后续项不收集）」。
- **T3 载体**：生成器抛错 → cordis `await ctx.plugin()` **reject**（scenario-host 取不到 Failed fiber
  引用）→ golden 只承载 T1 排序场景；失败路径由 m19 T3 锁定（DIV-4-5）。
- **TS host**：cordis `isConstructor` 对 `function` 声明走 `new` 分支并丢弃返回对象 → 生成器插件
  用**箭头函数**（非构造器）保证返回对象经 cordis 传播。
- **Rust 驱动**：`run_load` 中途取消分支补 `run_unload`（等价 cordis `_reload` 在 `_execute` 早退后
  的 `_unload`），否则已收集 disposer 永不运行。

## 4. 阶段 5（部署与维护）证据

- **部署冒烟**：`target\debug\dsh.exe web target/web/cordis.yml --port 60884`（本轮含 dsh-core 运行时
  改动）→ `GET /` **HTTP 200**（len 13270），进程干净停止——真实启动链路零回归。
- **部署**：`EffectOutcome::Stream` 为插件作者可返回的新 effect 形态（dsh-core 增量能力）；dsh-diff
  DSL 新增 `gen`（yield/await/throw）供等价场景书写。无运行面破坏（既有路径不动）。
- **回滚**：`git revert dd9cd1a`（D-135，核心 + m19 + DSL/golden 特征级整体回滚）；`scenario-12-*`
  可独立删除。文档提交可独立撤。

## 5. 诚实边界（未做 / 延后）

- golden 只承载 T1 排序场景（DIV-4-5：scenario-host 无法稳健处置 reject 的 Failed fiber）；T2/T3
  由 m-series 锁定。
- 真 pending（需真实事件循环暂停）的生成器仅 async 模式可推进；sync 驱动与既有 `Await` 同限
  （DIV-4-2）——m-series/golden 均为确定性同步步进，不依赖外部时钟。
- Group/include/hmr 现有 `Await + ctx.effect` 近似不改（A6-SCOPE=A），迁移到生成器形态列后续。
- Node 模块图精确同构不成立（既有 DIV）；浏览器 E2E（`--dump-dom`）按仓纪律代偿。
- `dsh-diff` 新增 `futures-util` 依赖（`std`+`alloc`，与 dsh-loader/dsh-core 同源同 features）。

## 6. 决策链互查

`D-132 需求（b1d1cfd）→ D-133 设计（34629a9）→ D-135 编码+golden（dd9cd1a）→ 本验收（D-136，待提交）`。
改动 → git 提交 → DECISIONS 条目一一对应。

## 7. 结论

**通过**：A6（异步生成器 effect，[Service.init] 完整形态）五阶段全部闭环。Rust 现可按 cordis
`_execute` 语义承载「逐项收集/卸载逆序/epoch 中途取消/失败前 disposer 保留」，唯一 golden 14 行
TS 原版 ↔ Rust 逐行一致；既有 22 场景零回归、199 目标全绿、clippy 0、serve 冒烟 HTTP 200。

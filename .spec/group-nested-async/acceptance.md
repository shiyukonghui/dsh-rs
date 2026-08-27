# 验收：group 嵌套异步时序（M27/M28）—— B 口径 mount 时序（DIV-nested-2 解决）

日期：2026-08-27
阶段：测试验证（4）→ 验收（5）——本文档为该两阶段关卡工件。
依据：`.spec/group-nested-async/{requirements,design}.md`（D-167/D-168）+ D-169 编码 + 全回归证据。

## 1. 验收结论：**PASS**

B 口径（用户裁决：扩口径到 mount 时序）下验收标准**全部达成**：

| 验收标准 | 口径映射 | 证据 |
|---|---|---|
| S1 语义定位 | cordis 子入口 create 的 import→fiber→reload hop 链 | vendored src 研读（group.ts/entry.ts/fiber.ts/registry.ts） |
| S2 判据修正 | Finish 批次（D-168，②普通任务未排空）+ mount 顺延（D-169） | m28 2 断言 + loader-25 字节对齐 |
| S3 校验面 | loader-25 golden 字节级 + 旧 25 不回归 + m-series 全绿 | verify-diff **26/26 ALL SCENARIOS PASS** |

## 2. 阶段 4 证据（测试验证）

- **红→绿**：回退 context.rs（92c…），m28 `nested_group_finish_batches_after_plain_work` FAIL
  （`got plain@16, group@15`——gIso Pending-only 中途 finish，正是 C1 缺口）→ 确认红因缺行为。
  恢复修复 → 全绿。
- **m28**（`crates/dsh-loader/tests/m28_group_finish.rs`）：C1（G>L 聚末尾）+
  C2（父不先于子）+ 末态（p/c1 Active、b1 Pending、三组 Group fiber Active）——3 断言全绿。
- **loader-25 字节级**（`scenarios/loader-25-nested-finish.json/.golden`，21 行）：TS(cordis)
  与 Rust(dsh-diff --async) trace **逐行一致**——3 层嵌套 + gIso isolate 边界 + Pending-only 子组
  （blocked 仅 `plugin:b` 无状态迁移）+ 三组聚末尾 + `status:p:Loading:Active` 于 c/b 注册前。
- **全回归**：`cargo test --workspace` **EXIT=0 零失败**（209 个 ok 块）；`cargo clippy --workspace
  --all-targets -- -D warnings` **0**；`verify-diff.mjs` **26/26 PASS**。
- **serve 冒烟**：`dsh web target/web/cordis.yml --port 32111` → `GET /` **HTTP 200 LEN=13270**
  （与 D-164/D-166 基线一致；cwd=仓库根）。

## 3. 编码期发现与取舍（D-169，B 口径）

1. **唯一剩余偏差**（M28 后）：`status:p:Loading:Active` 落点——Rust 在 `plugin:c`/`plugin:b`
   后、cordis 在前（3 次运行稳定，结构性）。机制：cordis 组子入口 create 多 hop
   （import→fiber 构造→reload），扁平子 Active（~2 hop）抢在组兄弟孙辈注册（≥3 hop）之前；
   Rust `drive_async_loads` 在 `Apply(gInner)` 内联注册孙辈 → 提前 hop。
2. **顺延法（deferred-await）**：`AsyncTask::Await(fid)` 变体 + `Runtime.pending_awaits`
   存 `EffectOutcome::Await` future；drive `Apply` 臂遇 Await 不再内联 `fut.await`，标记
   `await_children`、存 future、入队 `Await(fid)`；新 Await 臂 yield → `fut.await`（子/孙入口
   注册在此发生）→ `pop_current` → collect → 入队 `Finish`。孙辈注册晚**一个队列 hop**，
   落在兄弟扁平子 `Finish(p)` 之后 → loader-25 字节对齐；flat（loader-10）不变。
3. **current 栈修复**（顺延引入的必要配套）：延迟窗口内多个组 apply 都留在 `current` 栈
   （push 序），Await 任务按 FIFO 执行时栈顶未必是本组 → 子入口 `parent` 误挂兄弟组
   （其 isolate 令 svc 不可见 → 消费方永久 Pending）。Await 臂执行前 `retain != fid` + `push(fid)`
   把本组抬到栈顶、运行毕 `retain != fid` 移除。
4. `should_wait` 的 `queued_plain` match 增 `Await(_) => false`（不计数——组 finish 由
   ①Loading 后裔 + ②普通 Apply/Finish 排队保住批次；经 loader-25 推演与全 golden 校验）。

## 4. 诚实边界（残余 / 记录）

- 顺延法统一给「组子入口创建」加一个 hop；**更复杂的批内形态**（如一个组内多个组兄弟互相
  依赖、或 >3 层少分支深链）与 cordis 精确微任务序是否在所有形态下逐字节一致，**仅由现有
  26 golden + m28 语义锁定**；后续遇新形态需新增 golden 再验（不背书未测形态）。
- 卸载/更新路径不受 load 顺延影响：unload 过渡、reload 快照（loader-24 golden）、HMR
  （m15/m16/m18）全绿。
- **DIV-A4-5**（disposer 并发交错）保持文档化、未触碰；unprovide 唤醒（A4/m27 T1）、父链
  walk（m27 T2）不受影响（m27_a4 全绿）。

## 5. 决策链互查（git ↔ DECISIONS ↔ 工件）

D-167（需求：用户确认起点 M27/M28，口径 A=聚焦 Finish 时序）→ D-168（设计：Finish 批次
规则 + m28 + 划界 DIV-nested-1/2）→ 编码期实测定位 DIV-nested-2 → **用户裁决 B**（扩口径到
mount 时序）→ D-169（编码：顺延法 + current 修复 + loader-25 字节 golden）→ D-170（本验收）。
工件：requirements/design/acceptance + golden + DECISIONS + git 提交（commit 哈希见 git log）。

## 6. 部署与回滚

- **部署**：改动集中在 dsh-core 异步装载驱动（`drive_async_loads`）的 Await 处理 + loader
  组路径；全量测试/clippy/verify-diff/serve 冒烟已过。无需配置迁移。
- **回滚点**：撤销 `AsyncTask::Await` 变体 + `Runtime.pending_awaits` + drive 的 Await 臂与
  current 顶抬（即复现 D-168 代码状态）。m28 C1/C2 语义**不依赖** mount hop 结构，回滚后
  m28 仍绿；`loader-25` golden 与 verify-diff 登记需随回滚移除（D-168 划界 DIV-nested-2 重新生效）。

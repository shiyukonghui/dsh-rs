# 设计：group 嵌套异步时序（M27/M28）—— 聚焦 Finish 时序

日期：2026-08-27
阶段：系统设计（瀑布流阶段 2）——本文档为阶段关卡工件。
依据：`.spec/group-nested-async/requirements.md`（D-167）+ fork 源码实证 + 双侧探针实测。

## 1. 设计目标（用户确认 target=A：聚焦 Finish 时序，不动 disposer 并发）

让 Rust 异步驱动的 **Group Finish 时序**对齐 cordis 两个可观测契约：
- **C1（聚末尾/batch）**：组的 Loading→Active 落点在装载批的末尾——即首个 Group Active 不早于
  批内最后一个普通 fiber（非组）Loading→Active（末态 G>L 不变量）。含「Pending-only 子组的组」。
- **C2（父不先于子）**：Group 不在其任何 Loading 后裔 settle 前 Active（已由 `await_children`
  保证，本轮加偏序加固）。

## 2. S1 实证（自下而上）

| 探针 | cordis（TS） | Rust（现） | 结论 |
|---|---|---|---|
| `probe-nested-active`（g1[p, gInner[c1]]） | 两组聚末尾；p Active 于 c1 注册前 | 两组聚末尾（**组位置一致**）；p Active 于 c1 注册后 | 组批次已一致；余 provider 时序 |
| `probe-isolate2`（g1[p, gIso[b1 隔离Pending]]） | 两组聚末尾 | 两组聚末尾（一致） | Pending-only 子组在**两层**下被排末尾（队列天然） |
| `probe-nested-finish`（3 组） | 三组全部末尾 | **一个 Group 提前**（在 c1 apply 后、Active 前） | 队列交错让 Pending-only 组（gIso）Finish 中途出队 |

源码事实：
- cordis `Group = async* [Service.init] { yield disposer; await this.update(children) }`——组 fiber
  的 inertia（Loading 门槛）挂在该 generator 上，`update = Promise.allSettled(children.create)`；
  父组 create 经 `await fiber.await()` 链式等待 → **偏序：父组 finish 晚于其全部后裔 settle**。
- Rust `drive_async_loads` Finish 臂（context.rs:652-678）`await_children` 延迟只查
  「Loading 后裔」；独立入队的各 Finish 任务彼此不保证偏序 → Pending-only 子组的 Finish 可中途跑。
- `associate:'loader'` 实为 traceable-proxy 属性转发（utils.ts:180-208），**非**激活门——已排除。

## 3. S2 修复设计（context.rs Finish 臂，最小改动）

扩展 `should_wait`（context.rs:656-663）为：
```
should_wait = 组 fiber(await_children=true) && (
     ① 存在 Loading 后裔（现行）；OR
     ② 队列(pending_async_loads)中存在「普通 fiber（await_children=false）的 Apply/Finish 任务」
)
```
- ② = 批内普通工作未排空 → 任何组不提前 finish → 满足 C1（G>L）；普通任务排空后组按
  Loading-后裔序 finish（C2）。
- **无死锁**：仅组延迟；组只因①Loading 后裔 或②普通任务延迟；普通任务必然排空；②消失后
  叶组（无 Loading 后裔）即刻 finish，父组经①紧随 → 树序收敛。
- 不动：notify/唤醒、disposer 逆序（DIV-A4-5）、unload 过渡（U→D 不涉 Finish 臂）、sync 路径。

## 4. 划界（诚实边界 / DIV）

- **DIV-nested-1（已解决）**：Pending-only 子组的提前 finish → 由 ② 规则纳入批次末尾（C1）。
- **DIV-nested-2（预留，B 口径）**：**mount 时序**——cordis 子入口 `create()` 顺序一件到底（p
  完整 Active 后才 start gInner），Rust 为「先批量注册（Apply 入队）→ 再驱动 apply/finish」两阶段
  → provider Active 相对兄弟注册的位置不同。此属**装载调度**、非 Finish 时序，用户口径 A 不含；
  需 byte 级嵌套 golden 时另行（顺序 create 化，B 级）。
- disposer 并发（DIV-A4-5）保持文档化、不动。

## 5. 实现顺序（TDD）

1. m28 红：`crates/dsh-loader/tests/m28_group_finish.rs`——3 层嵌套 + Pending-only 组；断言
   末态（p/c1 Active、b1 Pending、三组 fiber Active）+ **G>L 不变量**（`take_trace` 解析：
   首个 `status:Group:Loading:Active` 的索引 > 末个普通 fiber `Loading:Active` 索引）。
2. Finish 臂加 ② 规则（context.rs）。
3. m28 绿 + `cargo test --workspace` 全绿 + `verify-diff`（goldens 不得回归）+ clippy 0。
4. 尝试恢复嵌套组 golden（若 byte 对齐则入；否则留 m28 + DIV-nested-2 记录）→ 阶段 5 验收。

## 6. 验收

- C1/C2 由 m28 确定性锁定；全回归基线保持（cargo test 0 失败 / clippy 0 / verify-diff 25/25）。
- DECISIONS 记修理+回滚点；DIV-nested-1/2 文档化。

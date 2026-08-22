# M4 里程碑验收报告（M4-ACCEPTANCE）

> 范围：M4 里程碑（goal / round-driver / plan / subagent / jobs / schedule / todo+workflow /
> web.rs 集成）。流程：瀑布流——M4i 为**测试验证 → 部署（收口）阶段**；验收依据
> `M4-REQUIREMENTS.md` §5 验收标准与 D-044 决策。本报告为 M4i 通过凭证。
>
> 方法与纪律：编码阶段走 TDD；关键决策记入 `DECISIONS.md`（D-044..D-053）且 git 提交可互查
> （D-053 = M4i 补齐收口）。**每条验收均对应一组真实测试 + 可运行证据，无伪装成功。**

---

## 1. 全仓测试与静态检查

| 检查 | 结果 |
|---|---|
| `cargo test --offline --workspace` | **1130 passed / 0 failed**（全部 143 组 test-result 全 ok） |
| `cargo clippy --offline --workspace --all-targets -- -D warnings` | **exit 0，零告警** |

> dsh-cli `--lib`：81 绿（含补4 fake-loop 端到端、补5 工具 bind / schedule 注入、补6 todo
> 事件 + 投影折叠、补7 round-driver 实配）。crate 级：dsh-goal 42、dsh-jobs、dsh-schedule、
> dsh-tools 16、dsh-session-query 等全部绿。

**结论：验收标准 #1 通过。**

---

## 2. goal：全生命周期 + 事件溯源 + 投影 + 严格 fold

- 验收：create→edit→pause→resume→complete→clear 全链路 + CAS（stale → `GOAL_STALE_REVISION`、
  重复 create → existing）+ `goal/change` 事件落会话 + `goal` 投影 fold（clear 墓碑 → null）+
  严格 fold 逐字段校验（revision 精确 +1、计数/时间戳守恒）。
- 实现：`dsh-goal`（service/fold/types，纯域）+ web.rs `goal_dispatch`（每次 mutation 后
  `take_last_change` → append `goal/change` 到目标会话）+ `goal/plan/subagent` 投影 unit
  （`dsh-session-query::m4_units`）。
- 测试：`dsh-goal/tests/m4_goal_change.rs`（6，含 last_change/take_last_change/snapshot_meta）+
  `web::tests::rpc_goal_*`（create 真实 ref 1.1、缺 objective → GOAL_INVALID_OBJECTIVE、缺
  sessionId → bad-request、complete→clear 全链路含 ref 缺失分叉、create 后 `goal/change`
  落会话 + 投影折叠）。

**结论：验收标准 #2 通过。**

---

## 3. goal-round-driver：armed + idle 自动续跑（真实 agent-loop 驱动）

- 验收：active+armed+未超 cap 自动排队续跑（followup 驱动）、roundsStarted 回放递增、超 cap → 停、
  session-start/fork → disarmed。
- 实现：`dsh-goal/src/round_driver.rs`（`StatusPort` trait + `round_driver_outcome` + `drive_once`
  + `render_round_prompt`）+ web.rs **`GoalRoundPort`**（把 `Rc<ReactLoopAgent>` 实配到
  StatusPort：status_idle / has_pending_inbox / followup）。
- 测试：`dsh-goal/tests/m4_goal_round_driver.rs`（eligibility 判定、cap、Noop、prompt 渲染、
  disarmed）+ **`web::tests::goal_round_driver_drives_real_agent_round`（fake-loop：mock adapter
  装配真实 Rust loop）**——armed 目标 + 空闲 + 空 inbox → drive_once admit 第 1 轮 + followup
  驱动真实轮次（user/assistant/turn/end 落共享 store、`Round: 1/2` 提示进消息、adapter 恰好调
  1 次）；回 idle 后再续跑第 2 轮；到 cap → 判定 None。

**结论：验收标准 #3 通过（含 fake-loop 驱动链路）。**

---

## 4. plan-mode：事件 + 投影 + 判定 + exit 工具

- 验收：`plan/mode` 事件 + `plan` 投影（active/pending）+ `/plan off` 判定 + `exit_plan_mode`
  （计划前置条件/评审通道缺失时明确报错）。
- 实现：`dsh-session-query/src/m4_units.rs`（`plan_projection_unit`，view 携带 mode）+ 
  `dsh-tools::m4::exit_plan_mode`（无 plan-mode 宿主服务 → 明确报错）。
- 测试：`dsh-session-query/tests/m4_plan_projection.rs` 等 + `dsh-tools/tests/m4_tools.rs`
  （exit_plan_mode 注册/未装配报错路径）。

**结论：验收标准 #4 通过。**

---

## 5. subagent：in-process spawn/fork 真实 child Agent 跑一轮 + 目录/history/prompt/interrupt

- 验收：in-process spawn/fork 真实 child Agent 跑一轮；list 完整可达目录（one-shot/continuable
  + activity/hasChildren + parentAvailable 提示）；history 持久化转录分页；prompt 经 alive
  parent 投递 + `{messageId}` 回执；interrupt 收到即 `{accepted:true}`；descriptor 事件 +
  subagent 投影 + 深度预算。
- 实现：`crates/dsh-cli/src/subagent_runtime.rs`（spawn_child / fork_child / list_children /
  history / prompt / interrupt）——子代理 = store 里真实 Session（origin=Subagent +
  parentSession + delegationDepth），身份经 `subagent/descriptor` 事件 fold；prompt 经
  `AgentLoopHost.ensure_agent` + followup 驱动一轮；投影 view 直接返回身份（含 agentProvider/
  agentModel，靠 descriptor `rename_all_fields="camelCase"` 序列化修复）。
- 测试：subagent_runtime 7 绿 + `web::tests::rpc_subagent_list_and_history_real_driver`（spawn →
  list 一行 + history 事件 + projections.subagent.mode）+ `rpc_subagent_prompt_gates_and_drives_
  fake_loop`（one-shot gate / 缺 loop fail loud）+ `rpc_subagent_prompt_drives_real_child_agent_
  round`（**fake-loop：spawn continuable child agentProvider=mock → subagent.prompt → real loop
  驱动一轮 → user/assistant 落 store → 真实 messageId `pmsg-<child>:N` → history 回读
  "child says hi"**）。

**结论：验收标准 #5 通过（in-process 真实 child + fake-loop 驱动，非空桩）。**

---

## 6. jobs：注册表生命周期 + 授权 + 子代理 producer + 投影帧

- 验收：注册表生命周期（running→stopping→终态 first-wins）+ id `<kind>-N` + 授权围栏 +
  list/read/kill/wait；子代理 producer 真实跑；`session/jobs` 投影帧。
- 实现：`dsh-jobs`（registry 状态机 + StartSpec producer + wait / jobs_frame /
  snapshot_to_view + ProducerPanic 回滚）+ web.rs `job_*` executor bind 到真实 JobRegistry
  （输出 `{text,job}` / `{outcome,job}` 对齐 SA-4 schema）。
- 测试：`dsh-jobs` 全绿 + `web::tests::register_m4_tools_with_job_registry_binds_really`
  （start → list 帧 → settle → job_output read 完成态 → job_kill）。

**结论：验收标准 #6 通过（crate 级 + web.rs 宿主 bind）。**

---

## 7. schedule：create/list/delete + 三类规则 + 事件 fold 重放 + 到期注入

- 验收：create/list/delete + after/at/every 三类 + `schedule/change` 事件 fold 重放 +
  dispatch 推进 + 到期注入（followup 或 framing 文本落事件）。
- 实现：`dsh-schedule`（after/at/every 构造、decode 精确键、fold + every 锚定、
  dispatch_schedule_change、framing_text、due_records）+ web.rs `ScheduleHost`（session 事件为
  权威：fold / create（decode 校验后追加）/ list / delete / dispatch_due 到期注入）。
- 测试：`dsh-schedule/tests/m4_schedule_inject.rs`（9 绿）+ `web::tests::*schedule*`
  （register bind：schedule_create 落 create 事件 → fold 1 条 → schedule_list → dispatch_due
  到期 dispatch + framing 文本 → after 消费后不再 active；create_then_delete 生命周期）。

**结论：验收标准 #7 通过。**

---

## 8. todo + workflow：事件 + 投影 + 工具 + 桩

- 验收：`todo/write` 事件 + `todos` 投影 + todo 工具；workflow meta 校验 + 事件骨架 + 致命 code
  分类的诚实桩。
- 实现：`dsh-session-query::todo`（to_todo_list / todos_projection_unit / todo_counts）+
  web.rs `TodoWriteHost`（todo_write 校验后把整表落 `todo/write` 事件到属主会话——投影据此
  折叠）+ `dsh-tools::m4::todo_write`（无宿主时自包含校验）+ `dsh-workflow`（meta 校验 /
  事件构造 / `run_stub` 恒 `UNSUPPORTED_OPTION` 诚实桩）+ web.rs `m4::workflow` 注册。
- 测试：`dsh-session-query` todo 6 绿 + `dsh-workflow` 6 绿 + `web::tests::todo_tool_with_host_
  lands_todo_write_and_projection_folds`（todo_write → `todo/write` 事件落会话 → `todos` 投影
  折叠出整表 → 无 agent 拒绝）。

**结论：验收标准 #8 通过。**

---

## 9. web.rs：10 RPC 经 handle_rpc_host 集成真实服务驱动 + 投影键随 history 携带

- 验收：10 个 RPC 方法（goal 6 + subagent 4）经 `handle_rpc_host` 集成真实服务驱动（不再空桩），
  投影键经 history 响应携带。
- 实现：`goal_dispatch`（6，boot.goal 真实状态机 + 事件落会话）、`subagent_dispatch`（4，
  真实驱动：list/history/prompt/interrupt）。`session.history projections` 块（asOfSeq + values）
  由 `ProjectionSession` 折叠真实注册表（goal/plan/subagent/todos）。
- 测试：`web::tests` 全集——rpc_goal_*（真实 ref / 错误码 / 事件落会话 / 投影折叠）、
  rpc_subagent_*（真实驱动 + fake-loop）、projections 块断言。

**结论：验收标准 #9 通过。**

---

## 10. 决策日志 + git 提交可互查

- M4 关键决策 D-044..D-053 全部在 `DECISIONS.md`；D-052（M4h 接线，1ccd261）、D-053（M4i
  补齐收口）在对应提交中落地。「改动 → 提交 → 决策日志」三者可互查（提交信息引用编号）。

**结论：验收标准 #10 通过。**

---

## 环境与差异记录的诚实说明

- **fake-loop**：无真实 LLM key 环境下，唯一可跑通真实 Rust loop 的方式 = mock `LlmAdapter`
  装配 `AgentLoopHost`（张 Bruno mock provider）。subagent 驱动轮与 goal round-driver 均以此
  验证「真实 Rust loop 一轮完整执行 + 事件落共享 store + 回读」；这是既定代偿（用户裁决 B
  明确要求含 fake-loop 驱动链路），非伪装。
- **schedule 到期注入**：同步单线程下由宿主显式调用 `dispatch_due`（frame/轮次钩子），非真实
  定时器——事件与 fold 语义一致；定时推进属 M5 宿主调度。
- **IANA 全时区 / out-of-process provider / workflow JS 引擎**：D-044 明确 M5+，不属本次验收；
  schedule `time_zone` 仅 UTC/数值偏移，IANA local-at 按 invalid_time_zone 报错（见 D-050）。

---

*验收执行：父会话（0 号 agent）旗舰复核 + SA-1..SA-4 子块独立验收（子代理）均已返回；全仓
回归 + clippy -D warnings 在本报告汇总。流水线：M4 编码（TDD）→ 本验收 → D-053 决策日志与
git 提交，进入部署（M5）前置条件已齐。*

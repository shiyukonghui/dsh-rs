# 阶段 10 · panel-schedule-create（创建调度）— ✅ 通过（揪出并修复真实对接缺口 D-218）

- 功能清单：表单卡（kind/prompt/afterSeconds）+ 动作 create → 宿主
  `schedule/create` 臂；语义终点=**到期后提示真执行**（调度提醒的本质目的）。
- 首跑（修复前）浏览器实测暴露**真缺口**：创建/列表/派发全真（会话日志
  seq3 `schedule/change{operation:"dispatch", id:schedule-2}` 准点 +60s 落档），
  **但聊天卡永远看不到提示执行**——生产主循环把 `m5g_tick_once` 返回的
  framing **丢弃**（只记派发事件，从不进 agent loop）：「到期注入」名不副实，
  既有测试也只断言到派发事件（断言止步过早）。
- **修复（TDD，D-218）**：
  - 红：新测试 `schedule_dispatch_executes_prompt_as_agent_turn`——到期 framing
    须成会话轮次事件（修复前不可能通过）；
  - 绿：serve 主循环消费 `(framing, dispatched)`；新 `spawn_schedule_turns`
    = 与 chat `session/prompt` **同一执行权威**（`run_rust_loop_on_host`，
    杜绝第二套轮次语义），批内顺序单线程保序、线程隔离不阻塞 accept、
    loop 缺席诚实 eprintln；
  - 回归：dsh-cli lib 271/271。
- 浏览器实测（修复后 verify-schedule-fire.mjs，**5/5 PASS**，console 零错）：
  真填表(after/60s) → 创建 `✓ 已保存` → 调度卡行 `schedule-1 after
  E2E-FIRE-R10B-1788096162.99231` → **~60s 真触发后聊天卡出现该提示原文 +
  助手回应**（`chat-shows-firing` + `assistant-echo-after` 双断言）。
- 脚本诚实性一并修复：TIMEOUT 路径不再误报 pass（要求步骤数与关键步在场）。
- 留痕（R2）：触发轮次永久留在 default 会话日志（schedule/change create+dispatch
  + 注入轮次），不可回滚项按约留痕。

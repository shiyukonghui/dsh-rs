# PLAN-approval-rpc.md — approval RPC 里程碑（承接 D-105 L1 裁决）

> 阶段：**需求分析关闸**（round 1，无实现）。上一轮裁决记录：D-105 L1 「指令层优先，
> 执行层并入 approval RPC 里程碑」。本规划沿用瀑布流：需求结论 → 设计 → 编码（TDD）→
> 验证 → 部署/维护；每段独立提交=回滚点；DECISIONS/git 互查；fail-loud；key 永不落盘。

## 0. 范围总览（三段，各自独立提交）

- **S1 · enter/leave plan-mode 宿主入口**：`/plan` 命令执行面（`[off|message]`）→
  落 `plan/mode{active}` 事件（PlanModeHost.enter/exit 已备）+ 事件后下一 turn 的
  standing 段随折叠注入/移除（折叠源已建）。
- **S2 · 执行层联动**：plan-active 时 **mutation 工具**的 execute 经 ApprovalProvider
  强制裁决（ApprovalAsked / ApprovalDecided / ApprovalPolicy 事件 + 一次性
  AllowedOnce/Rejected 语义）。
- **S3 · per-agent plan-mode 保真（范围核算）**：多会话共享 standing 时，prompt 段 /
  审批判定各自读自己 agent→session 的折叠。

## 1. 需求分析（第一性原理 + 自上而下 / 自下而上）

### 1.1 根本目的
plan mode 的「先规划、后实施」纪律的**机器化兜底**：模型在 plan 模式不得静默改
文件/执行变更——即便它调 mutation 工具，宿主也要强制拦截：要么显式批准（一次性
放行该次）、要么拒绝（诚实原因上抛）。用户 D-105 已拍板执行层并入本里程碑。

### 1.2 自上而下：目标与成功标准
- S1：宿主 RPC 进入/离开 plan mode → 事件落 → standing 段随折叠注入/移除（下一 turn
  assemble 即生效）。
- S2：plan-active 时 mutation 工具的 execute **不再静默放行**——经 approval 通道：
  allowed-once → 放行本次；rejected → 拒绝；无通道/无决策 → **fail-closed 拒绝**并有
  诚实 message（绝不伪造批准来源，D-074 纪律）。事件流落会话
  （ApprovalAsked → ApprovalDecided + ApprovalPolicy）。
- S3：跨会话共享 standing 时，每 agent 的判定以其**自己**会话折叠为准。

### 1.3 自下而上：已核对事实（round 1）
- ApprovalProvider 缝已存在（dsh-tools runtime:90 / set_approval_provider :610），
  `Ask → provider → ApprovalOutcome` **同步单次裁决**（:825 resolve_approval），**无
  挂起队列**；**生产 web 未注册** provider；**dsh-agent-loop 不消费审批缝**
  （grep 零命中）。
- `ToolRegistry.add_pre_decision` 是现成注入缝（:582）；web_m5
  `register_pre_execute_hook`（D-088）即先例——`PreToolDecision::Allow/Deny/Ask`。
  Ask 时在 `tools.execute` 内同步解析（guard 前）。
- loop 同步执行工具（dsh-agent-loop/tool_calls.rs `run_group` → `tools.execute`），
  **无 turn 暂停/恢复**；真实异步 UI 轮询 = 需 loop 异步缝（大改，超本轮）。
- `commands/list` 已声明 `/plan`（`input.hint "[off|message]"`）但**无执行 RPC**——
  enter 宿主入口本 build 缺，S1 即补此面。
- ApprovalAsked / ApprovalDecided / ApprovalPolicy 事件词已存在（dsh-session
  EventKind）但**当前零消费者**（web_m5 沙箱注释：approved 级联留宿主接线）。
- PlanModeHost（round 29，`web::dsh_cli_host`）已备 enter/exit + agent→session 解析 +
  dsh_plan fold/前置；exit_plan_mode 执行器已走真实链路。

### 1.4 硬约束
- 单线程同步 accept；工具执行路径现为同步——**异步审批需要 loop 层新增「暂停/恢复」
  工具门**（D-a 选项 B，用户已选）；不得破坏既有同步工具执行/回归。
- 不伪造批准来源：缺 approval 通道→诚实；`ApprovalDecided` 只由真实决定写
  （host/UI 落 approval/decided 事件）。沙箱 `approved` 级联同理（既有 D-084 注释）。
- key 永不落盘；每段独立提交=回滚点；全回归 + clippy `-D warnings` 零 + live 复验。

### 1.5 决策点（**用户裁决已定 round 1**）
- **D-a 执行层落地边界**：**异步 UI 往返（本轮）**——改造 dsh-agent-loop 为异步工具
  门（turn 暂停/恢复）：plan-active mutation → `ApprovalAsked` 落事件 + 调用挂起 →
  GUI 弹窗 → `approval/decided`（allowedOnce|rejected）→ allowed-once 放行并
  resume/re-dispatch 该调用、rejected 物化拒绝原因。用户明确选 B（harness UX 对齐），
  不做同步 fail-closed 折衷。
- **D-b mutation 工具集**（已确认）：`fs write/edit`、`terminal open/send/signal`、
  `bash`/`pwsh`(bare-sh) 发送、`str_replace_editor` 变更、`run_code`、
  `schedule create/delete`、`job_kill`。**read 系不拦**（read/read_image/glob/grep/
  terminal_read/list、job_list/output、schedule_list 等）。
- **D-c 判定作用域**：plan-active = 调用 agent 所属会话的折叠（PlanModeHost
  agent→session 解析）；**S3 per-agent prompt 保真留后续段**（用户确认）。

### 1.6 非目标（本轮）
- 不改 GUI 端弹窗 UI 实现（GUI 是外部 harness GUI；宿主提供事件 + 决策面）；
  不改既有 ApprovalProvider 缝语义；不做 S3 per-agent prompt 保真（后续段）；
  不引入多线程/真并发（单线程轮询异步门）。

## 2. 验收标准（S1/S2 各自关闸）
- S1：进入 RPC → `plan/mode{active:true}` 落事件 + standing 段注入（既有折叠源链路
  单测 + RPC 级单测）；离开 → 段移除；`/plan [message]` / `/plan off` 语义齐（message
  进入时随带用户消息；off 离开）。
- S2：plan-active mutation（D-b 清单）→ `ApprovalAsked` 落事件 + 调用挂起（turn 等
  决策）；`approval/decided{allowedOnce}` → 放行该调用（resume/re-dispatch）；
  `{rejected}` → 拒绝结果物化；plan-inactive → 直通（回归既有）；非 mutation →
  直通；未勾批的挂起不阻塞其它无关调用（若设计如此）。生产装配后 live 复验
  （GUI 弹窗 → 允许/拒绝）。
- 全回归 + clippy `-D warnings` 零 + live :60165 复验；`TEST_REPORT-BC-segments.md`
  追加章（D-106 段）。

## 3. 阶段规划
1. 需求分析（本篇）→ 用户裁决 D-a（异步）/D-b（清单）/D-c（S3 留后续）→ 定稿。
2. **设计（round 1，已关闸，见 §4）**：S1 wire/RPC 面 + enter/leave 语义；S2 loop
   异步工具门（pending 机制 + ApprovalPending + 恢复、宿主 ApprovalGate + 包装 +
   decide RPC + kick 恢复）。
3. 编码（TDD）：**A loop pending 机制 → 独立提交；B ApprovalGate+RPC → 独立提交；
   C S1 RPC → 独立提交**。
4. 验证：全回归 / clippy / live；DECISIONS 各段补记；测试报告追加章（D-106）。

## 4. 设计决策（round 1 设计关闸；无实现）

### 4.0 总原则**approval 策略全在 web 宿主层（dsh-cli）**；`dsh-agent-loop` 只加通用「pending 工具
调用」机制（停步/恢复 + ApprovalPending turn 结束理由），`dsh-tools` 语义与 approve
缝不改。这样 loop 改动是纯机制性的（可独立回归），策略（mutation 清单/plan 判定/
决策折叠/事件）集中在宿主并可随时换。

### 4.1 S1 · enter/leave plan-mode 宿主入口
- 新 RPC `session.plan.mode`，payload `{"active": bool, "message"?: string}`（web.rs
  handle_rpc 新分支）。
- 入（active:true）：`PlanModeHost.enter(default)` 落 `plan/mode{active:true}`；
  message 非空 → 顺带 append UserMessage「plan:<message>」（模型拿到规划请求）；
  落 `approval/policy` 首条（`{active:true, scope:"mutation", tools:[D-b清单]}`，诚实
  宣告当前作用域）。
- 出（active:false）：`PlanModeHost.exit` **宿主 leave 无 heading 前置**（GUI 离开不
  校验计划文本；`exit_plan_mode` 模型工具保持既有 dsh_plan 三重前置）。落
  `plan/mode{active:false}`；再落 `approval/policy{active:false}`。
- 段注入：standing 折叠源已建（round 29），下一 turn 装配即随 fold 注入/移除（无新增）。
- 测试：RPC 级（enter→fold active=true + policy；leave→false；message 行为）+ 既有
  PlanModeHost 单测保持。

### 4.2 S2 · loop 异步审批门
**loop 改动（dsh-agent-loop；纯机制）**
1. `ToolExecOutcome { concluded, context }` → 增 `pending: Vec<PendingCall>`；
   `PendingCall { block: ToolCallBlock, call_event_seq: u64 }`（call 事件 seq 供恢复期
   result 定位、防重复 tool/call）。lib.rs 导出。
2. `ToolExecCtx` 增 `resume: Vec<PendingCall>`（正常空；恢复期携带上一步 pending）。
3. `execute_tool_calls`：resume 非空时对该列表**只 execute + append_tool_result**
   （用存储 seq；不重复 append_tool_call）；其余路径不变。
4. ReactLoopAgent 增 `approval_pending: RefCell<Vec<PendingCall>>`（**agent 级，非
   Phase 字段**——越过 pause→Idle 停驻存活；dispose/abort 清空）。
5. step()（line ~846 tool_exec 后）：若 `outcome.pending` 非空 →
   `approval_pending = pending` + 返回 `TurnEndReason::ApprovalPending`（新增变体）。
   step() 顶部：`approval_pending` 非空 → 取走并以 `resume` 语义重跑 tool_exec →
   结果落会话 → 清空 → 继续 build_request（模型下一请求看到 result）。
6. turn()（line ~523-527 空消息短路）增条件：`approval_pending.is_empty()`——恢复踢
   永不因「step0+空 inbox」短路（pre_step 空 inbox 仍给 Enter+assembly，已核）。
7. `AgentLoopHost` 增 `kick(id)`（bare-wake，不 append 消息；走既有 wake_driver→
   kick 路径）。followup 保持。

**宿主改动（dsh-cli；策略）**
8. `ApprovalGate`（web 内新模块）：
   - mutation_set：D-b 静态清单；
   - `plan_active(agent)`：PlanModeHost.session_id_for(agent) → 会话 fold（dsh_plan）；
   - `fold_decided(call_id)`：该 call_id 最近 `approval/decided` → AllowedOnce|Rejected|None；
   - `emit_asked(agent, call, reason)`：落 `approval/asked{tool, toolCallId, agent, reason}`。
9. tool_exec 包装（web 装配，包在 service tool_exec 外一层）：
   - resume 空（正常）：逐 call：`plan_active(agent) ∧ mutation(name)`？
     - fold_decided=AllowedOnce → 正常执行；
     - fold_decided=Rejected → **合成拒绝 result**（tool/call + tool/result error\
       「the user rejected tool ...」，不 execute）；
     - None → 落 tool/call + approval/asked + 记 pending（不 execute）→ outcome.pending。
   - resume 非空：逐 call 按 fold_decided → allowed 走 resume 执行 / rejected 合成。
10. 恢复驱动：新 RPC `session.approval.decide { toolCallId, decision:"allowedOnce"|
    "rejected" }` → 落 `approval/decided{tool, toolCallId, decision}` → `agent_loop
    kick(default)` → 新 turn → step 顶 resume 重跑 → 结果 → 模型续。多个 pending
    并行：全部 decided 后一次 kick（余下未决则 again ApprovalPending）。
11. `agent.turn`/`run_rust_loop` 返回面：drain 完若 approval_pending 留驻 →
    `{"ok":true,"value":{"accepted":true,"approvalPending":[...callIds]}}`（GUI 感知
    并弹窗）。

**事件契约**
- approved/asked: `{tool, toolCallId, agent, reason}`
- approval/decided: `{tool, toolCallId, decision: "allowedOnce"|"rejected"}`
- approval/policy: `{active, scope:"mutation", tools:[...]}`（S1 时落）
- 均落共享 store 会话（既有 EventKind，零消费者→本轮接线）。

### 4.3 不变量与回归相容论证
- turn/end(ApprovalPending) 正常走 TurnEnd 路径：step 已闭（StepEnd 已落）、open_turn
  匹配、reason 新变体——invariant 校验通过（已核 lines 84-117 只查 step 闭 + open_turn）。
- pending_calls：tool/call 已注册（pause 步），tool/result 在**恢复步**（新 turn）删除
  ——invariant tool/result 检查只要求 call ∈ pending_calls（已核 line 197），跨 turn
  成立（trace 连续）。
- 正常同步路径零改动：execute_resumed 只影响 `resume` 非空上下文；`resume` 恒空除
  审批恢复。八 crate 回归兜底。

### 4.4 分拆提交（各独立回滚点；TDD）
- **A · loop pending 机制**：ToolExecOutcome.pending + ToolExecCtx.resume +
  execute_resumed + ApprovalPending 变体 + step/turn 恢复 + agent 级 approval_pending+
  turn 短路条件 + AgentLoopHost.kick。单测：ToolExecCtx.resume 语义 + step 恢复交还模型
  （mock LLM + mock adapter 既有 harness）+ ApprovalPending turn/end。dsh-agent-loop 全套回归。
- **B · ApprovalGate + tool_exec 包装 + RPC（web）**：mutation 集 / plan_active /
  fold_decided / emit_asked / 合成拒绝 / decide RPC / kick / run_rust_loop 返回面。
  单测（mock loop 替身 + 会话事件断言）。standing/live 回归。
- **C · S1 RPC**（session.plan.mode + policy 事件）。单测 RPC 级。
- 收口：TEST_REPORT 追加章（D-106）+ DECISIONS 各段补记；live 复验（进入 plan →
  发 mutation 调用 → GUI 弹窗 → 允许/拒绝 → 模型续；:60165）。

### 4.5 风险与回滚
- 风险集中在 A（loop 状态机）→ 先做 A 独立全量回归再动宿主；`git revert` 各自点为回滚。
- 诚实边界：GUI 端弹窗由外部 harness GUI 呈现（宿主只落事件 + 返回 approvalPending）；
  若 GUI 尚无对应交互面，验收以「宿主事件 + RPC + kick 恢复」的证据为准并如实标注。

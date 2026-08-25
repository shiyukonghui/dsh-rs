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
2. **设计（当前）**：S1 wire/RPC 面 + enter/leave 语义 → 设计决策；
   S2 loop 异步工具门（暂停/恢复 + 挂起表 + 事件契约 + AllowedOnce 作用域 +
   re-dispatch 语义）→ 设计决策。
3. 编码（TDD）：S1 → 独立提交；S2 → 拆小段各独立提交。
4. 验证：全回归 / clippy / live；DECISIONS 各段补记；测试报告追加章。

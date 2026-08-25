# B+C 分段测试报告（TEST REPORT — B+C Segments）

状态：**可验收**（stage-gate 工件）。B 全段（P1–P5 + P3a–e）与 C 段（K1–K4）已交付、
全回归绿、live 复验通过。本报告对照 `PLAN-BC-presets-execution.md`、`DECISIONS.md`
（D-101..D-104 各补记）与 git 历史（逐段独立提交 = 回滚点）编写。

---

## 1. 交付范围

| 段 | 交付物 | git 提交（回滚点） | DECISIONS |
|---|---|---|---|
| B：发现/解析 | typed 组合解析 + disabled_expr 分类 + 发现商 | P1/P2 | D-101/102 |
| B：standing 挂载 + 守卫 | 行审计（bridged/disabled/guarded 诚实三态） | P2 | D-102/103 |
| B：桥面 | persona/instructions/skill 目录/fs-terminal 组/单工具 | P3a–c | D-103 |
| B：闭环 | dsh-shell 双方言、真实 bash/pwsh PTY 后端、win32-A 收口 | P3d/e | D-103 补记 |
| B：P4 直通 | loop 消费 scope、E-03 变量（`{{model}}`/`{{cwd}}`） | P4 | D-103 补记 |
| C：K1 | dsh-core agent-scope 子树原语（真实 scope 标签 + hook 一致性 + leakedServices 审计） | `5bf8958` | D-104 K1 |
| C：K2 | unusable-rows 挂载否决（inactiveRows；stuck vs 诚实降级两分类） | `e053fc2` | D-104 K2 |
| C：K3 | standing 挂载生存期/泄漏完整性归位 dsh-core（挂载记录 subtree + unmount_scope + select 泄漏拒绝） | `dae55bd` | D-104 K3 |
| C：K4 | **F-05 WASM 组合引擎**（combo-eval wasm 面 + native 兜底 + row_disabled_with 注入缝） | `281cb05` | D-104 K4 |
| C：F-06 | join 键 = ScopeKey 单键（值比不透明；无第二键空间） | K1 起 | D-104 |

## 2. 验收证据（C 段为重点）

### K1 — dsh-core 作用域树原语
- 测试：`tests/m70_preset_tree.rs` ×3（agent 子树隔离 hook + 卸载处置 / 子服务落 root 判泄漏 /
  双 agent scope 互不可见）。首次跑抓出 `alloc_scope==root` bug——独立
  `next_isolate_scope`（1_000_000 起）一处修复全绿。
- 语义：`pending_scope` FIFO（F-06 join 键）、`collect_hooks` filter（root 全局/本会话）、
  `audit_subtree`（leakedServices 守卫）、`Cordis::mount_scope/unmount_scope/current_scope/isolate`。

### K2 — unusable-rows 挂载否决
- 规则（harness `inactiveRows` 对齐）：**Stuck（桥依赖不可满足）→ 拒**；
  **Honest 降级（D-103 broken/A-03/未桥）→ 只报不拒**——否则误杀真实预设。
- 测试 ×5（含 4 真实预设×生产宿主零回归安全网 + select 端到端拒绝+不留残留）。
- select fail-loud：`agent-preset-mount-rejected`。

### K3 — 挂载本体归位 dsh-core
- `mount_scope()` + 挂载记录 fiber（isolate「preset.mount」于 agent realm）→ `unmount_scope`
  整树卸载（fiber → Disposed）；`audit_subtree` 接入 select（`agent-preset-leak-rejected`）。
- 测试 ×4（4 真实预设核心子树 Active + 审计干净 + 卸载清净 / root-leak 故障注入被捕获洁净 /
  select 端到端泄漏拒绝 + 不留残留）。
- 架构裁决（如实记录）：**整 loop 迁移 dsh-core 出作用域**（SystemPrompt/ToolRegistry
  平面不跑 dsh-core）；收敛可验证的真部分——挂载生存期 + 隔离 + 泄漏完整性归位 dsh-core。

### K4 — F-05 WASM 组合引擎（用户重申后落地，未默改）
- WASM 面 = `wasm-plugins/combo-eval/`（dsh-eval **同源编译进 wasm**，C ABI）；native 兜底
  `FallbackEval`；权威 `row_disabled_with`（fail-closed + truthy 留在 dsh-agent-presets）。
- 一致性测试 ×3（m20）：真实 preset 表达式×win32/linux 两面全等 + 门控翻转；全语法面语料
  值/错误串逐字节全等；4 真实 preset 逐行真实 facade 门控全等。standing +2（注入引擎被
  消费 / 默认 wasm 面）。
- 零新增依赖权重（dsh-cli 早依赖 dsh-wasmrt → wasmtime 已在 web 二进制）。

## 3. 全回归与静态检查

- **8 crates 全部测试 644/644 绿**（dsh-tools / dsh-agent-loop / dsh-shell / dsh-terminal /
  dsh-core / dsh-agent-presets / dsh-wasmrt / dsh-cli）。
- `cargo clippy … --all-targets -- -D warnings`：**零告警**。
- 关键子集：dsh-agent-presets 18/18；dsh-wasmrt（含 m20 ×3 + C-ABI/组件/loop 系列）全绿；
  dsh-cli lib 180/180（含 standing 20 + web select 拒绝路径 ×2 + E-03 变量注入）。

## 4. live 复验（win32 开发机，dsh web :60165，真实 LLM 环境）

逐次 K 落地后重建并复验（term-26 → term-33）：
- `standard/cordis/code/minimal` **四真实预设 select 全 OK**（含 K2/K3/K4 之后零回归）；
- standard@win32 忠实门控：bash 系禁用、pwsh 系活化已桥，模型实际调用 pwsh → PS 5.1
  真执行输出 `5.1.26100.6584 on Win32NT`；
- 模型读 skill SKILL.md、用 bash/pwsh/todo/job 工具（B 段 live 验证）；
- `{{cwd}}` 解析到 workspace root、`{{model}}` 解析到配置模型（standard persona 渲染）。

## 5. 诚实边界（未桥面，D-103 设计，非缺陷）

- `web` / `tool-cordis` / `command-compact` 保持 **broken per D-103**（guard 报告）；
- `plan-mode` / `compaction-*` / `tool-fs / fs-search / jobs / goal / subagent / workflow /
  ralph / ask-user / todo / web…` 为「no Rust bridge yet」诚实降级——**意图由宿主导线
  注册面满足**（read/write/edit、todo_write、goal_*、web_search、job_*、…），非卡住；
- 整 loop 迁移 dsh-core（K3 明示出作用域）；per-agent `{{cwd}}`（单工作区 → 保持近似
  诚实）；skill 真加载器工具（需宿主 skill service，暂无）；
- WASM 面默认启用（blob 缺失自动回落 native-only，仍正确）。

## 6. 遗留决策（呈用户）

1. **shipped preset 未桥行**：改成 `disabled: true`（harness 正路，行不再出现在 guard 报告）
   还是保持「no Rust bridge yet」guard 降级（诚实呈现收窄面）？
2. 后续：loop 级状态驱动（dsh-plan-mode 段 / compaction 诚实 guard）如需推进，单独排期。

---
附：方法学循规——瀑布流分阶段、阶段关闸（本报告即 C 段关闸工件）、TDD 红绿重构
（每 K 为先红后绿）、DECISIONS/git 互查（提交信息对应决策条目）、fail-loud（select
拒绝路径）、key 纪律（密钥仅 env 注入，从未落盘/入 git/DECISIONS/.env）。

---

# 追加章：D-105 后续段（未桥面桥接 + loop 级状态桥，round 26–29）

段目标（用户拍板 D-105）与交付：规划见 `PLAN-loop-state-bridge.md`；决策见
`DECISIONS.md` D-105 各补记。

## 7. 未桥面桥接（U1–U3，完成）

| 段 | 交付 | git | 验收 |
|---|---|---|---|
| U1 | fs/family + jobs + todo **真桥接**（组解析确认宿主工具集 / 单工具重呈现）；goal 诚实 guard（宿主 goal 是 RPC/投影面非 agent 工具，与预设注释一致） | `9aff8d0` | standing +2；八 crate 646/646 |
| U2 | 下伸面 honest 呈现：dsh-tool-workflow → 桥到 M4 桩（注册即见、fail-loud）；subagent 家 / workflow-worker-thread / ralph / ask-user → 专用诚实 guard（宿主确无模型工具，第一性原理不为快伪造桥）；**parse 保真修复**：静态 `disabled: true` 与 disabled_expr 同等判禁 | `3b77dac` | dsh-agent-presets 19/19（+1）、standing +2；649/649 |
| U3 | guard 原因收口：枚举四预设全部行 → 仍落泛化的只剩 plan-mode/compaction/presentation，全部给经过决策的专用原因；**安全网测试**：真实预设任何守卫行不得落入泛化 | `75b1d83` | standing +1；650/650 |

## 8. L1 · plan-mode C 档（slice-1 → 折叠接线 + 真实执行器，完成）

- **slice-1 状态驱动段**（`e40ce09`）：`dsh-plan-mode` 行 config.section 经
  `PromptSectionText::Fn` 在 standing scope 注册（order 55，override 工具指引带）；
  Fn 组装期按 **plan-mode 折叠源**注入/缺席。standing 25/25、八 crate 651/651、
  live 四预设 select OK。
- **折叠接线（round 29，改造 slice-1 的双源设计）**：
  - 自下而上发现 `dsh-plan` crate（`fold_plan_mode` + `exit_plan_mode_check` 三重
    前置）即 harness 的 plan-mode 权威实现 → **单一权威态 = 会话 `plan/mode` 事件
    日志**（纯重放）。standing 删 per-standing cell，改**可注入折叠源**（会话事件
    fold 的替身注入/宿主注入），slice-1 测试随迁。
- **exit_plan_mode 真实执行器（round 29，`web::dsh_cli_host::PlanModeHost`）**：
  - PlanModeHost：agent→session 归属 + 事件追加 + `dsh_plan` fold + 前置校验
    （in-plan-mode / `# 标题` / 评审通道）；`enter`/`exit` 只落事件。
  - 执行器绑定：`M4HostServices.plan_mode` 在场 → bind（前置失败 → 结构化
    `PlanModeError`、非 NOT_BOUND；通过 → `{approved:true}` + `plan/mode{active:false}`）；
    缺席保持 NOT_BOUND 诚实。
  - live 接线：装配构造 PlanModeHost（`review_channel=true`，GUI user-questions 面在
    场）；standings 重建后注入折叠源（fold `boot.plan_session` 事件；select 记录之）。
  - 测试：plan-mode 3/3（折叠/前置逐点/执行器绑定）；**八 crate 655/655**；clippy 零。
- **approval linkage 裁决（执行层待用户确认）**：预设文本即「rules override 更晚工具
  指引 / tools 保持列出不变」的**指令层**语义（harness 正路，随段注入）；执行层
  （ApprovalProvider 按 plan 模式自动拒绝 mutation）属宿主导线策略、非预设契约，并入
  approval RPC 里程碑。**呈用户确认**：是否要求 execution-layer 联动。
- caveat：single-active GUI——折叠源折叠「最后一次 select 的会话」；多会话共享某
  standing 的 per-agent plan-mode 保真留白（另段）。

## 9. L3 · compaction 档位 3（完成）

- `ToolResultPrunerSpec`（dsh-agent-presets/compaction.rs，`3be1551`）：契约定型
  （thresholdChars/headChars/tailChars 解析 + 不变量 head>0、tail>0、head+tail<threshold，
  fail-loud），**行为明确不实现**（不接 append_tool_result）；真实行 config 解析测试 +2。
  dsh-agent-presets 21/21。

## 10. 段状态总览（round 29 收口）

- 已交付提交：U1 `9aff8d0`、U2 `3b77dac`、U3 `75b1d83`、L1-slice-1 `e40ce09`、
  L3 `3be1551`、+ L1 执行器/折叠接线（round 29 提交）（各自回滚点）。
- 全回归基线：**655/655**（round 29 末）；clippy `-D warnings` 零；live（term-38，
  :60165）四真实预设 select 全 OK。
- **剩余/待用户确认**：（round 29 裁决已下）approval **execution-layer** 联动**用户选定
  指令层优先、并入 approval RPC 里程碑**——L1 approval 联动以指令层收口（harness 正路，
  随段注入）；`enter_plan_mode` 宿主入口（PlanModeHost.enter 已备）与 GUI/loop 状态源
  随 approval 里程碑一并物化；多会话共享 standing 的 per-agent plan-mode 保真另段（§8
  caveat）。
- 诚实边界笔记：U1/U2 自下而上推翻了「subagent/ralph/ask-user 可桥」的预设（宿主无
  对应模型工具）；tool-skill 保持 A-03 只读 guard；broken-D-103（web/tool-cordis/
  command-compact）全程保持报错降级未改——与用户拍板一致。

---

# 追加章：D-106 approval RPC 里程碑（round 1–2；需求关闸 → 设计关闸 → 段 A/B/C）

段目标与交付：需求结论/设计决策见 `PLAN-approval-rpc.md` + `DECISIONS.md` D-106 各补记；
用户裁决 D-a=异步 UI 往返（本轮）、D-b=mutation 清单、D-c=S3 per-agent 保真留后续。

## 11. 段 A/B/C 交付（完成）

| 段 | 交付 | git | 验收 |
|---|---|---|---|
| A | loop **pending 工具调用机制**（纯机制，无 approval 语义）：`TurnEndReason::ApprovalPending`；`PendingCall`/`ToolExecCtx.resume`/`ToolExecOutcome.pending`；agent 级 `approval_pending`（越过 Idle 停车）；step 恢复/暂停；`kick_resume`；`execute_tool_calls(+resume)` 只追 result、复用 call seq；`emit_pending_calls`/`append_pending_rejection(TOOL_REJECTED)` | `5b22bd6` | dsh-agent-loop +2 集成测试（m2e2/m2e3）+ dsh-session 变体；全 workspace 回归 191/191 |
| B | 宿主审批策略：loop 注入缝 `ToolExecFactory` + `create_loop_agent_with_tool_exec`；`web/approval.rs`（plan 非激活直通 / plan∧mutation→pending+asked / resume allowedOnce→执行 / rejected→合成拒绝 / 未决→拒绝停留，不伪造批准）；`session.approval.decide` RPC（写 decided + kick）；`run_rust_loop` 返回面含 `approvalPending` | `53e5863` | approval 单测 5/5（mutation 清单/直通/暂停/放行/拒绝）；dsh-agent-loop + dsh-cli clippy 零；全 workspace 回归 191/191 |
| C | S1 用户侧入口：`session.plan.mode {active,message?}` RPC（宿主进入/离开无前置；模型 `exit_plan_mode` 保持三重前置）；`approval/policy {active,scope:"mutation",tools:[D-b]}` 随落；standing 折叠段随事件注入/撤下 | `2bbaa68` | S1 测试 1/1（进入→折叠+policy；离开→无前置折叠 false）；dsh-cli clippy 零；全 workspace 回归 191/191 |

## 12. 验证与 live 复验

- **全 workspace 回归**：段 A/B/C 各一次独立全量——**191/191 套件、0 真失败**
  （segA/segB/segC-regression.txt；历史壳 191 = 段 A 基线 191，逐段含新增测试）。
- **clippy `-D warnings` 零**：dsh-session + dsh-agent-loop + dsh-cli，及 workspace 全量。
- **live :60165 复验（新二进制，round 2）**：serve 正常起服；`host.describe` OK；
  `session.plan.mode{active:true}` → 会话落 `plan/mode{active:true,message}` +
  `approval/policy{active:true,scope:"mutation",tools:[D-b 11 项]}`，fold 投影
  `plan:{active:true}` 即时可见；`session.approval.decide` 对未决调用 fail-loud 结构化
  错误（不伪造批准）。
- **环境阻塞（诚实呈报，非代码缺陷）**：live 真机模型回合被端点拦截——`api.deepseek.com`
  对 `deepseek-v4-flash-0731-ext` 与公开别名 `deepseek-chat` 均回 `HTTP_418`
  （turn/end error；无 tool/call 触发 → approvalPending 空是正确语义）。各换模型名自修
  均同样被拦 → 判定为网关/凭据环境问题（需要正确 `DSH_LLM_BASE_URL`/网关）。真实模型
  驱动「mutation → approval 弹窗 → decide → 续跑」的 GUI 目视留待用户端环境修复后复验；
  该链路的确定性路径已由单测覆盖（approval 5/5 + S1 1/1 + 段 A driver 恢复系列，均走
  真实 `assemble_server_loop` 装配）。

> **补（round 2 续，环境结论修订）**：用户澄清 key 属**自部署网关**
> `http://100.105.152.101:18080/v1`，model `deepseek-v4-flash-0731-ext`。按此重跑：
> 曾一度 401/`malformed`——401 系**跨 `term` 进程读空 key 的探针伪影**、malformed 系
> 旧配置变体瞬时抖动；恢复已验证 live 配方后**全闭环真机通过**：plan 激活 → 模型调
> `bash` → `approval/asked` + `tool/call` + turn `approval-pending`（**未执行**）→
> `decide allowedOnce` → 真执行 `hi-live-approval\n` 续跑；`decide rejected` → 合成
> `the user rejected tool "bash"` 不执行续跑。此前「环境阻塞」判定撤销。

## 13. 段状态总览

- 已交付提交：`5b22bd6`（A）、`53e5863`（B）、`2bbaa68`（C），各自独立回滚点；
  DECISIONS 各段实施补记与提交互查。
- 里程碑目标 ③（S3 per-agent plan-mode 保真）与 D-c 一致留后续（single-active GUI
  caveat，§8）；D-104 实施补记另章预留。
- 方法学循规：瀑布流三关闸（需求→设计→编码+测试）皆过；TDD 红→绿→重构逐段；
  决策日志/git 互查；fail-loud（无决策拒绝/停留、live 环境阻塞如实呈报）；key 纪律
  （密钥仅 live 进程 env 注入，从未落盘/入 git/DECISIONS/.env）。

# 追加章：S3（D-107）per-agent plan-mode 保真 + G（D-108）approval GUI 里程碑（round 3–4）

## 14. S3 · per-agent plan-mode 折叠（完成）

- **交付**（`5ae0aa9` S3-a、`c43aa2a` S3-b/c）：`AssembleContext.session_id` 身份
  管道；standing 折叠源改 `Fn(Option<&str>) -> bool`（`PlanModeProbe`）；plan-mode 段
  按组装会话身份折叠（多会话共享 standing 各看各的、None 回退 `plan_session`）；
  `web::plan_mode_resolver` 接线。
- **测试**：m2d 身份断言（`assemble_context_for_sets_scope_to_agent` 增
  `session_id`）、standing per-session 折叠（alice/bob/None/flip）、web resolver
  per-session（alice active/bob inactive/None 回退/flip）。全 workspace 回归绿
  （dsh-cli lib 196→现 204）；clippy `-D warnings` 零。

## 15. G · approval GUI 里程碑（D-108，round 4；Rust serve 侧完成并真机闭环）

- **交付**（`222f2a5` + `35996fb`）：`web/approval_wire.rs` 注册表（append-only 帧
  日志 + pending 表；stable rpcId；approvalId=审计 id `ap-<call_id>` 配对
  asked/decided）；`events.mux` SSE/WS 下推 requested（**pending 重放逐字同
  rpcId**）+ resolved；`POST /api/respond`（client-response echo rpcId 路由、审计
  校验、accepted/not-pending/bad-response）；`approval_tool_exec` 挂起即推 requested、
  `decide()` 结算 wire；`build_boot_manifest` 跟随 junction/symlink。
- **单测**：approval_wire 8 项全绿；dsh-cli lib **204/204**；clippy `-D warnings` 零。
- **真机 wire 闭环（自部署网关 + fork 前端 dist :60165）**：
  - ALLOW：plan enter → 真模型 bash 调用 → `approval/requested` → `respond`
    allowed-once → `{accepted:true}` → `approval/resolved(allowed-once)` → **bash
    真执行（marker 落盘）** → 迟到 respond `not-pending`。
  - REJECT：新 rpcId requested → respond rejected → resolved(rejected) → **工具未
    执行**（marker 不存在）。
  - 浏览器级目视（点「允许/拒绝」按钮）留给用户：`http://127.0.0.1:60165`。
- **fork 前端**（零改动）：`feature/approval-gui` 分支 + `pnpm install` + 基线
  `pnpm build`（全部通过）；web-root = `apps/web/dist`；`DSH_PLUGIN_ROOT` =
  junction 聚合 42 个 web 客户端包。凭据仅 live env 注入，node_modules/安装包
  （npx）零接触。

## 16. 里程碑总览（round 4 收口）

- 提交链：`5ae0aa9`（S3-a）→ `c43aa2a`（S3-b/c）→ `4ae16e6`（D-106 live 复验补）
  → `222f2a5`（D-108 G）→ `35996fb`（junction fix）。各自独立回滚点，DECISIONS
  记互查。
- 方法学：S3/G 均按瀑布流需求→设计→编码（TDD）→测试→部署/验证推进；越级问题
  （junction 漏扫）显式回退实现段修 `build_boot_manifest` 并补 DECISIONS；key 纪律
  全程守。



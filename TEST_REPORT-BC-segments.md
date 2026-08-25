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

# 追加章：D-105 后续段（未桥面桥接 + loop 级状态桥，round 26–28）

段目标（用户拍板 D-105）与交付：规划见 `PLAN-loop-state-bridge.md`；决策见
`DECISIONS.md` D-105 各补记。

## 7. 未桥面桥接（U1–U3，完成）

| 段 | 交付 | git | 验收 |
|---|---|---|---|
| U1 | fs/family + jobs + todo **真桥接**（组解析确认宿主工具集 / 单工具重呈现）；goal 诚实 guard（宿主 goal 是 RPC/投影面非 agent 工具，与预设注释一致） | `9aff8d0` | standing +2；八 crate 646/646 |
| U2 | 下伸面 honest 呈现：dsh-tool-workflow → 桥到 M4 桩（注册即见、fail-loud）；subagent 家 / workflow-worker-thread / ralph / ask-user → 专用诚实 guard（宿主确无模型工具，第一性原理不为快伪造桥）；**parse 保真修复**：静态 `disabled: true` 与 disabled_expr 同等判禁 | `3b77dac` | dsh-agent-presets 19/19（+1）、standing +2；649/649 |
| U3 | guard 原因收口：枚举四预设全部行 → 仍落泛化的只剩 plan-mode/compaction/presentation，全部给经过决策的专用原因；**安全网测试**：真实预设任何守卫行不得落入泛化 | `75b1d83` | standing +1；650/650 |

## 8. L1 · plan-mode C 档（slice-1 完成，执行器设计关闸）

- **slice-1 状态驱动段**（`e40ce09`）：`dsh-plan-mode` 行 config.section 经
  `PromptSectionText::Fn` 在 standing scope 注册（order 55，override 工具指引带）；
  Fn 组装期按 standing 的 **per-agent plan_mode cell** 注入/缺席；
  `StandingRegistry::set_plan_mode(id, bool)` / `plan_mode(id)`。standing 25/25、
  八 crate 651/651、live 四预设 select OK。
- **设计结论（执行器 + approval 联动，未实现、诚实 NOT_BOUND）**：
  - wy 线已核对：`ToolExecutionInput.agent` 携带调用方 agent；`host.join_standing`
    只存 binding 不记 preset；live boot 于 bundle 装配后重设 `boot.standings`
    （web.rs:269）→ 执行器闭包无法在装配期捕获最终 standings。
  - 接线方案（下一轮实施）：select 处理记录 agent→active-preset；
    serve 期（boot.standings 设定后）把 `exit_plan_mode` 绑定为闭包——按
    `call.agent` → active-preset → `standings.set_plan_mode(preset, false)` + 追加
    会话事件（dsh-session 事件 schema 下轮核，缺则新增 plan/mode 面，不臆造）。
  - approval 联动裁决：预设文本即「rules override 更晚工具指引 / tools 保持列出不
    变」的**指令层**语义（harness 正路，slice-1 已注入）；执行层
    （ApprovalProvider 按 plan 模式自动拒绝 mutation）属宿主导线策略、非预设契约，
    并入 approval RPC 里程碑。**呈用户确认**：C 档 execution-layer 联动是否本轮跟进。

## 9. L3 · compaction 档位 3（完成）

- `ToolResultPrunerSpec`（dsh-agent-presets/compaction.rs，`3be1551`）：契约定型
  （thresholdChars/headChars/tailChars 解析 + 不变量 head>0、tail>0、head+tail<threshold，
  fail-loud），**行为明确不实现**（不接 append_tool_result）；真实行 config 解析测试 +2。
  dsh-agent-presets 21/21。

## 10. 段状态总览（本轮收口）

- 已交付提交：U1 `9aff8d0`、U2 `3b77dac`、U3 `75b1d83`、L1-slice-1 `e40ce09`、
  L3 `3be1551`（各自回滚点）。
- 全回归基线：**651/651**（round 28 末）；clippy `-D warnings` 零；live（term-37，
  :60165）四真实预设 select 全 OK。
- **剩余**：L1 执行器（下一轮按 §8 设计接线）+ approval execution-layer（待用户
  确认）；`enter_plan_mode` 宿主入口（GUI/loop 状态源）随执行器一并定。
- 诚实边界笔记：U1/U2 自下而上推翻了「subagent/ralph/ask-user 可桥」的预设（宿主无
  对应模型工具）；tool-skill 保持 A-03 只读 guard；broken-D-103（web/tool-cordis/
  command-compact）全程保持报错降级未改——与用户拍板一致。


# PLAN — loop 级状态驱动桥 + 未桥面桥接（定稿，依 D-105 用户拍板）

阶段：**需求分析**（关闸工件；→ 系统设计 → TDD → 测试 → 部署）。依 `DECISIONS.md` D-105。

---

## 〇、范围总览（D-105 四答）

1. **A · 未桥面桥接（U 段）**：host 全局基已满足类 + plan-mode/compaction（即将桥）→
   **规划并实现桥接**，不纠结标注、不 bulk 改 disabled。
2. **B · plan-mode → C 档**：完整 harness 语义（状态驱动段 + `exit_plan_mode` 真实执行器
   + **approval 联动**）。
3. **C · compaction → 档位 3**：仅守卫段 + 接口预留（本轮不做真实压缩/摘要）。
4. **broken-D-103 类（web / tool-cordis / command-compact）→ 报错降级**：保持 guard
   （原因可见、fail-loud、不拒绝），**不改**。

---

## 一、A · 未桥面桥接（U 段）

### 目标
把「intent 已被宿主全局基满足、但尚未按 preset 专属面呈现」的行真正桥接——在 standing
挂载时**按其存在的声誉于 standing scope 呈现 + report 标 bridged**（组解析确认 /
单工具行重呈现模式，复用 fs-local/terminal 组的既有机制），模型视图如实获得该行能力提示。
**不重复注册**（宿主全局已有实现）。

### 待逐行盘点（需求分析子项；以桥表逐行落 U 段，每行一个可验收子交付）
| 行 | 宿主全局基（satisfier） | 桥接方式 | 备注 |
|---|---|---|---|
| `@deepseek-ai/dsh-tool-fs` / `dsh-tool-fs-search` | read/write/edit/read_image/glob/grep | 组解析确认（同 fs-local） | 搜需确认宿主搜索工具 |
| `@deepseek-ai/dsh-tool-jobs` | job_*（job_kill/list/output） | 单工具组重呈现 | |
| `@deepseek-ai/dsh-tool-goal` | goal_create/get_goal/update_goal | 组重呈现 | |
| `@deepseek-ai/dsh-tool-todo` | todo_write | 单工具重呈现 | |
| `@deepseek-ai/dsh-tool-subagent` 家 / `subagent-control` | subagent / send_message / interrupt / list_agents | 组重呈现 | |
| `@deepseek-ai/dsh-workflow-worker-thread` / `dsh-tool-workflow` | workflow | 组重呈现 | |
| `@deepseek-ai/dsh-tool-ralph` | ralph | 单工具重呈现 | |
| `@deepseek-ai/dsh-tool-ask-user` | ask_user_question | 单工具重呈现 | |
| `@deepseek-ai/dsh-tool-skill` | （无；skill 加载器需宿主 skill service） | **暂不能桥 → 保持 guard** | tool-skill≠skill 目录桥 |
| `@deepseek-ai/dsh-tool-web` | web_search / web | **broken-D-103 → 报错降级（不改）** | |

> 每行桥接前做自下而上核对：宿主注册面该工具确实在册（`register_m5_tools_with_host` /
> M4 注册表）才桥；不在册→保持 guard（诚实）。

### 验收
- 对每个已桥行：桥表项 + standing report `bridged`（name + host toolset 呈现）+ 测试断言；
- 真实 preset（standard/code/cordis）挂载后 report 的 guarded 集显著收窄、bridged 集扩充；
- K2/K3 逻辑不回归（桥后行不再「no Rust bridge yet」≠ 变 stuck）。

## 二、B · plan-mode C 档（L1）

### 目标（D-105 档位 C）
- **状态驱动段**：组合 `@deepseek-ai/dsh-plan-mode` 行的 `config.section`（已有完整文本）
  → 会话处于 plan 模式时注入 SystemPrompt（standing scoped section；模式退出即移除）。
- **`exit_plan_mode` 真实执行器**：从 NOT_BOUND 变真实应答（提交计划 / 确认模式切换）。
- **approval 联动**：plan 模式下 mutation/执行类工具的行为与 approval 联动。

### 需求阶段待证（不臆造，先自下而上核对）
- 会话「plan 模式」状态载体：现有 EventKind `plan/mode`（wire）+ 状态字段是否已在
  dsh-session/dsh-agent-loop 存在（本轮查证未确认 → 需求子项）。
- 组装入口如何知情：`AssembleContext { scope }` 无 mode → 条件化 section / 上下文增字段。
- harness `dsh-plan-mode` 与 approval 的联动语义（源证据）；本 host `approval` 现状
  （ApprovalAsked/Decided 事件 + web 侧策略）。
- 事件对齐：进入/退出发 `plan/mode` 事件，谁消费。

### 验收
- 模式注入/移除单测；`exit_plan_mode` 真实 ok/拒绝语义（非 NOT_BOUND）；approval 联动按
  需求结论；全回归 + clippy + live 复验；逐子段 DECISIONS/git。

## 三、C · compaction 档位 3（L3）

- **守卫段**：`compaction-basic` / `compaction-*` 行保持诚实 guard（report-only）；
  行已进入挂载审计信息面。
- **接口预留**：定义 compaction 接口形状（如结果变换钩子签名，前瞻
  `tool-result-pruner` 的 `thresholdChars/headChars/tailChars` 语义），**不实现行为**、
  不接 append_tool_result。文档落 DECISIONS。
- **验收**：接口形状 + 守卫测试（不动行为、零回归）。

## 四、分段实施计划（K 风格；逐段独立提交 = 回滚点）

| 段 | 内容 | 关闸验收 | 风险/回滚 |
|---|---|---|---|
| U1 | 桥表首批发（fs 体系 / jobs / goal / todo） | report bridged 断言 + 测试 | 低 |
| U2 | 桥表次批发（subagent 家 / workflow / ralph / ask-user） | 同上 | 低 |
| U3 | 盘点收口：tool-skill 等保持 guard 的逐行理由 + 安全网测试 | 文档 + 测试 | 低 |
| L1 | plan-mode C 档（状态驱动段 → exit_plan_mode → approval 联动） | 每子步单测 + live | 中-高（approval 面） |
| L3 | compaction 守卫段 + 接口预留 | 接口 + 零回归 | 低 |

每段流程：需求结论 → TDD 红→绿 → 全回归（644 基线 + 增量）→ clippy `-D warnings` 零 →
live `:60165` 复验 → DECISIONS 补记 → git commit（回滚点）。

## 五、验收形态与报告
- 真实 preset report：guarded 收窄到「真正不满足/刻意 broken」；bridged 面覆盖宿主实际
  能力；plan-mode 段随模式注入/退出；compaction 守卫可见 + 接口可延展。
- 最终并入 `TEST_REPORT-BC-segments.md` 追加章；DECISIONS/git 互查完备。

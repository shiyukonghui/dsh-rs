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

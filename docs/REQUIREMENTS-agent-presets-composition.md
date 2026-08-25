# REQUIREMENTS — Rust per-session agent preset 插件组合（路径 B + C）

> 需求分析阶段结论文档（瀑布流阶段关闸工件）。所有技术可行性经 spike-1..6、8 与
> 源码核证（见 `PLAN-BC-presets-execution.md` §2/§6）；决策记录待 D-103。
> 本文档 = 目标 / 非目标 / 假设 / 约束 / 边界 / 验收标准，供用户逐项确认。

## 0. 背景与目标

**顶层目标（第一性原理）**：让 Rust 侧 agent 会话像 deepseek-harness 一样，能按
「预设（preset）」组合插件——会话选择 preset 后，其**模型可见行为真实改变**（工具集、
system-prompt、私有服务实例、生命周期），且**组合权威归位 dsh-core/loader**（不旁路核心）。

**成功标准**：
- S1 组合真实改变会话行为（直通 P4）：select preset ≠ 空转，工具/提示词/服务生效。
- S2 权威归位：装载/解析/守卫/代/隔离走 dsh-core/loader 原位（路径 B→C 收敛）。
- S3 隔离正确：两会话不同预设互不串（工具/提示词/私有服务实例）。
- S4 忠实而诚实：shipped 预设可装载；能力差异（broken 集）显式而非伪装。
- S5 工程质量：TDD 红绿重构、基线回归（149+）、clippy -D warnings、DECISIONS ↔ git 互查。

## 1. 范围与边界

### 1.1 范围内（In scope）
- **FR1 预设自持与发现**：4 个 shipped 预设（minimal/standard/code/cordis）已复制自持于
  `resources/agent-presets/`（D-A，已落地）+ 用户根 `.agent-presets` 发现（B-04 推荐照 TS
  约定）+ `scanRoot/discoverPresets`（DIR 名=preset id、broken=组合缺失/不可装载、order-id
  排序、每 id 首根胜出）。
- **FR2 standing 挂载**：每 preset 每进程一次 standing 组合（`preset-id` 单飞队）；路径 B =
  每 standing 一个 Cordis + dsh-loader 装载 + `disabled_expr`/`__jsExpr` 正确求值（process
  门面）＋守卫（泄漏服务/inactive 行审计→broken）。
- **FR3 join / recompose**：会话 select → 会话 scope parent 绑到 standing（`bind_scope_parent`/
  `rebind`，dsh-scope 已具备）；select 仅 blank 会话（有 turn/start 则 `agent-preset-locked`）；
  同 id 幂等、换 preset 走 rebind（TS recompose 语义）。
- **FR4 loop 消费（P4，必经关）**：ReactLoopAgent 从 agent scope 组装 tools+prompt
  （`agent.rs:664`/`host.rs:185-187` 已按 agent scope 决议——P4 是机械接线，非新机制）。
- **FR5 RPC/settings**：`agentPreset.list/read/select/copy/remove/openDocument` 真实语义
  （agent-presets 设置 namespace `{default}`；选定持久化 + `agent-preset/selected` 事件已有线头）。
- **FR6 换代**：standing 文件/配置变化 → 原地换代（loader create/update/remove/sync，无需重启）。

### 1.2 非目标（Out of scope，本期不做）
- NG1 `dsh-skill`（skill 内容装载工具）/`dsh-web`（网页检索）→ 预设中相关行先 **broken**（有待定）。
- NG2 逐会话独立模型选择（`session.selectModel` 真实语义）——单独立项。
- NG3 C 阶段之前的循环整体迁入 dsh-core（载荷/水位语义）——留给独立架构里程碑 C。
- NG4 HMR 文件监听自动重载（手动/事件触发 regeneration 即可）。
- NG5 copy/remove/openDocument 作者流并入 P5（发现+只读先行，authorable 即真）。

### 1.3 边界 / 不变量（不可破）
- **key 纪律**：任何模型密钥不落盘 / 不入 git / 不进 DECISIONS；`.env` 禁用。
- **fail-loud**：未知 preset、broken 组合、二次 provide、inactive 挂载 → 显式错误，
  绝不静默跳过或降级。
- **架构不偏离**：组合权威归位 dsh-core/loader（路径 B 的窄服务桥只映射 loop 已消费的
  工具/提示词作用域层；服务实例经桥映射 loop 可消费句柄）。
- **诚实差异**：能力缺口（无 pwsh、无 skill/web、`!!js` 求值边界、win32 门控）如实记录，
  不装作能 1:1 挂载 TS 组合。

## 2. 假设（自下而上已核证的既定条件）
- H1 dsh-core `Cordis`=一个 Runtime（spike-1）→ 每 standing 一 Cordis 可行。
- H2 dsh-scope `bind_scope_parent`/`rebind`/`scope_chain_of` 已具备（spike-2）→ join/recompose 无需新 API。
- H3 SystemPrompt 全作用域化（sections/complete/suppress/vars；spike-3）→ persona/指令/变量 1:1。
- H4 loop 已按 agent scope 决议 tools/prompt（spike-5）→ P4 机械。
- H5 dsh-eval 子集（spike-4/6）：`process.platform/env` 可注入门面；`process.cwd()` 需 +1
  `eval_call` 白名单项；`new URL(...)` 超子集 → 静态解析。
- H6 bash 在 win32 可用（resolve.rs Git Bash，spike-8）；**无 pwsh 工具**（spike-7 待立项）。
- H7 基线程：lib 149 测试全绿（E-02 已实测）；开发机 win32、服务 60165 live。

## 3. 约束（硬性，方法论四）
- C1 瀑布流阶段关闸：需求(本文档)→设计(D-103)→编码(TDD 红绿重构)→测试→部署；阶段工件可验收。
- C2 关键决策落 DECISIONS + git 互查（改动→提交→日志）。
- C3 依赖引入先调查评估（成熟库直接用，不重复造轮子）；自实现的唯一理由是核心/缺口。
- C4 每阶段交付可运行代码+测试+测试报告；不得长期红。
- C5 关键决策后 git 提交（未初始化/不支持 git 先说明）。

## 5. 结构性风险（已降险）
- R1 平台门控全部误禁（eval_scope 无 process + fail-closed）→ P1 修复 + 回归测试（spike-4/6）。
- R2 minimal `cwd`/cordis `skills` 超出 dsh-eval 子集 → P1 门面/白名单项 + 静态 baseUrl 解析。
- R3 win32 空 shell（无 pwsh）→ 方向 B（自持门控改写，零新增）先行，A（pwsh P3）随上。
- R4 服务行双键空间投影（scope 层 vs dsh-core isolate）→ B 阶段最小投影面 + C 全迁收敛。

## 6. 验收标准（AC，对应 §0 成功标准）- AC1（S1/P4）：`agentPreset.select(s, 'standard')` 后，s 会话下一轮 prompt 的工具集/提示词
  与 default 会话不同且含 standard 组合产物（集成测试断言模型面差异）。
- AC2（S2）：standing 装载/守卫/换代/隔离全部经 dsh-core/loader 路径（loader 单元 + 收敛测试）。
- AC3（S3）：两会话分别 select minimal/standard → 工具/persona/服务实例互不可见（隔离断言）。
- AC4（S4）：shipped 4 preset 均可装载（12 个 `!!js` 节点精确求值；process 门面正确）；
  broken 集（skill/web/tool-cordis/command-compact…按拍板）显式报 broken 而非伪装。
- AC5（S1 win32 验证）：win32 上 standard 有可用 shell（A 或 B 二选一，E-03 live 验收）。
- AC6（S5）：`cargo test --lib -p dsh-cli` 149+ 全绿（相对 E-02 基线只增不减）、clippy `-D warnings`、
  DECISIONS D-103 与代码提交互查、无密钥落盘/入 git。
- AC7（S3/S4 边界）：未知预设 fail-loud；已有 turn 的会话 select → `agent-preset-locked`。

## 7. 待用户决策项（★，均带推荐）
见 `PLAN-BC-presets-execution.md` §5：A-01（每 standing 一 Cordis）、A-03/B-11（broken 集与
技能面大小）、win32 A/B（B 先行/A 随 P3）、A-05（换代为主路径）、B-04（dshHome 约定照抄）、
C-04（default 不 join）、F-05/F-06（C 阶段后置）。
用户逐项确认/改动后 → **DECISIONS D-103**（记每项选项/理由/回滚）→ git → TDD 分段实现（P1–P5 → C）。

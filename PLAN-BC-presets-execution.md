# PLAN：Rust per-session agent preset 插件组合 —— B+C 执行的问题清单与阶段规划草案

> **状态**：草案，**等待用户深入分析后定稿**（用户明示：先把所有需决策/改动的点通通写入本文档，经深入分析后再规划分阶段解决）。
> **依据**：用户已确认——执行路径 **B（组合权威归位 dsh-core/loader + 窄服务桥）→ C（循环开进 dsh-core 收敛）**；
> preset 决策 **A（内置复制自持 + 支持读取自定义 agent）、B（直通 P4：组合真实改变会话行为）、C（`dsh-eval` 现成子集）、D（按推荐）**。
> **前置调研**：`DESIGN-agent-presets-composition.md`（TS 权威语义 standing mount/join/realm/settings 全带 file:line 引用 + Rust 基建盘点 + **架构偏离逐缝表 §3.0** + **收敛路径 §5.1**）。
> **需求结论**：`REQUIREMENTS-agent-presets-composition.md`（瀑布流需求分析阶段关闸工件：目标/非目标/假设/约束/边界/验收标准）。
> **本文档职责**：把 B+C 落地所需的**全部问题/改动点/风险/技术验证**逐条编号列出（每条注明类型、影响面、选项、建议、依赖、阻塞），末尾给**阶段规划草案**供用户修订后成为正式执行计划。**不写实现代码。**

---

## 0. 已确认决策（约束，不再变）

| # | 决策 | 内容 |
|---|---|---|
| D-A | 内置 root 载体 | vendored `apps/cli/config/agent-presets/{minimal,standard,code,cordis}` **复制进 Rust 项目自持**（build/资源目录下发；运行时绝不依赖 vendored 参考树） |
| D-A2 | 自定义 agent 读取 | 与 deepseek-harness 一致，支持**用户自定义根**（`<home|cwd>/.agent-presets/*`）发现+读取（discovery 首根赢、trust:system/user、broken 健康检查） |
| D-B | 推进粒度 | **直通 P4**：协议不逐段停，P1–P4 连续推进，组合最终**真实改变会话行为**（工具面/提示词/persona 按会话作用域生效） |
| D-C | `!!js` 求值 | 用 `dsh-eval` 现成子集 + 对 vendored 预置做**平台静态预排除**兜底（win32 行 disabled） |
| D-D | 作者体验 | 按推荐：`authorable`=用户根存在即真（`.agent-presets/` 发现前置交付），`copy/remove` 作者流并入 P5 |
| D-X | 执行路径 | **B 先行**（组合权威归位 dsh-core/loader + 窄服务桥，不固化架构缺口），**C 随后**（独立架构收敛里程碑，P5 之后） |

---

## 1. 问题与改动点清单

> 类型：`决策`（需用户定夺）/ `改动`（明确实现项）/ `验证`（需技术核实或 spike）/ `风险`（语义/回归）/ `缺口`（Rust 侧不存在的能力）。
> 建议阶段：P1 解析·发现·根 · P2 组合挂载 · P3 插件行·服务桥 · P4 loop 消费 scope · P5 RPC·作者流 · C 收敛。
> 状态：`待用户分析` / `待验证` / `已建议`。

### 1.1 架构与组合编排（P2 主体 + C 收敛）

| # | 类型 | 问题 | 影响面 | 选项 / 建议 | 依赖 |
|---|---|---|---|---|---|
| A-01 | 决策 | **standing 组合挂在哪**：每个 preset 一个独立 `Cordis::new()`，还是共享 1 个 boot Cordis 上按作用域挂子树？TS=host 树内 agentCtx.plugin 挂子树（共享一树）；Rust `dsh-loader::Loader::new(&Cordis)` 需 `&Cordis` | dsh-core/loader | 选项①每 preset 独立 Cordis（隔离彻底、但服务 store 不再全局、与 boot Cordis 的服务两次组装）；②共享 boot Cordis + 每 standing 一个 loader/entry 集 + ScopeId 隔离（最像 TS，但需验证 Cordis 内多 standing 共存）；**建议先 spike ②** | spike-1 |
| A-02 | 决策 | **组合权威与 loop 消费的桥形态**（双键空间）：dsh-core `ScopeId`（组合/服务侧）↔ dsh-agent `ScopeKey`（loop/工具/提示词侧）怎么接？ | dsh-core/dsh-agent/dsh-tools | ①双轨：dsh-core 管组合+isolate 服务实例，**投影**成 ScopeKey 层给 loop（快，保留第二织物）；②全迁 ScopeKey→ScopeId（彻底收敛，归 C）；**建议 P2 用①，C 用②** | A-01 |
| A-03 | 决策 | **窄服务桥的服务子集**：shipped 预设 service 行 = planMode / compaction+toolResultPruner / workflowEngine / terminals / fs-shadow 等；哪些桥接、哪些先 `broken` | 桥接层 | 需先**盘点 shipped 四预设全部 service 行 → Rust 侧现有句柄**（见 1.3）；建议先桥接 loop 真要用到的（plan-mode、compaction、fs-shadow、terminals），其余 `broken` 诚实展示 | B-11 |
| A-04 | 验证 | **dsh-scope `ScopeKey` 父链是否支持运行时断链/重绑**（会话 join 撤销/`recompose`=父链重载）？不支持则以「会话层置空+不解析」兜底 | dsh-scope | spike-2：bind/rebind/断链能力确认，无则补最小 API | A-02 |
| A-05 | 决策 | **代（generation）与 HMR**：组合文件变化→新代（TS）是否本期做？还是进程级 standing + 版本靠重启 | dsh-loader hmr | **建议 P2/此后（round-6 已核实）**：`Loader::create/update/remove/sync`（loader.rs:416-606）都是公开流程 → standing 用自己的 Loader（spike-1 单 Cordis）即可**原地换代**（新代事务 + 旧 fiber teardown），不必重启；HMR 文件监听可后置（先手动/事件触发 regeneration）。进程级 standing + 重启只是兜底开关，不作主路径 | A-01 |
| A-06 | 决策 | **挂载守卫在 Rust 的判据**：inactiveRows（行未激活/缺注入）与 leakedServices（行注册未带 scope→root 泄漏）如何建模 | 组合层 | dsh-loader 已有事务+回滚；泄漏判据=「该行注册未带 scope」；归纳到 §0 D-C 的 fail-loud | A-02 |

### 1.2 解析 / 发现 / 元数据（P1）

| # | 类型 | 问题 | 影响面 | 选项 / 建议 | 依赖 |
|---|---|---|---|---|---|
| B-01 | 改动 | **`agent.cordis.yml` 解析复用**：`dsh-loader` Include/EntryOptions 已吃顶层数组（验证 include.rs:308-355），preset 组合与其同形——确认直接复用 + `cordis:group`/`isolate` 键解析 | dsh-loader | 复用；补 `presetEntryList` 方言校验（对齐 TS `entryListSchema` 的健康检查，B-03） | — |
| B-02 | 改动 | **`!!js` 标签进 Rust**：vendored preset 文件是字面 `!!js` YAML 标签；Rust `serde_yaml` 不认 → 需（①构建期复制时转译成 `__jsExpr` 对象（对齐 include.rs:6 既有差异）；②运行期自定义 YAML tag handler） | 复制/解析层 | **建议①**（复制脚本一次性转译；文档 `D-C` 平台静态预排除顺带在此做） | B-01 |
| B-03 | 改动 | **preset 元数据 `preset.yml`**（name/description/order）解析 + **健康检查**（组合缺失/不可加载→broken；用 loader 方言校验而非自造 schema） | 发现层 | 对齐 discovery.ts；`first root wins`、trust 来自 root、`authorable`=存在 user 根 | B-01 |
| B-04 | 决策 | **自定义根位置**：`.agent-presets` 放 harness home 还是 cwd？ | 发现层/CLI | **round-7 已核实 TS 权威约定**：`resolveDshHome()` = 显式配置路径 → `$DSH_HOME`（空/纯空白视为未设）→ `~/.dsh`（`DSH_HOME_DIR_NAME='.dsh'`，util/home-paths/src/index.ts:12,18,62,88）；用户根= `dshHomePath('.agent-presets')` trust:`'user'`（index.ts:134 追加到配置 roots）；系统根=SHIPPED `config/agent-presets` trust:`'system'`；`discoverPresets` 按 roots 顺序**每 id 首根胜出**（用户可覆盖内置）。→ **Rust 建议照抄**：P1 加 `dsh_home()`（`$DSH_HOME`→空白忽略→`home_dir()/.dsh`）＋用户根 `<dsh_home>/.agent-presets`（`authorable`=其存在即真，D 决策）＋系统根 `resources/agent-presets` ＋ roots 数组 + `includeUserRoot:false` 开关（hermetic 测试用） | B-03, D |
| B-05 | 改动 | **内置根下发形态**：4 个预设拷进 Rust 项目，作为资源随二进制下发（`include_bytes!`/装箱资源）还是原样目录 + 数据路径？版本同步策略（与 vendored 参考树的差异标注） | 构建/资源 | 建议资源目录 + 复制脚本（含 `!!js`→`__jsExpr` 转译 + win32 预排除，与 B-02 合并）；内置与自定义同形同逻辑 | B-02 |

### 1.3 插件行实现 / 服务桥（P3）

| # | 类型 | 问题 | 影响面 | 选项 / 建议 | 依赖 |
|---|---|---|---|---|---|
| B-11 | 盘点 | **shipped 四预设行→Rust 实现映射全表**：minimal/standard/code/cordis 每行（persona/agent-instructions/tool-bash/pwsh/fs/fs-search/jobs/goal/plan-mode/subagent*/workflow/ralph/ask-user/todo/web/skill-filesystem/tool-skill/presentation…）+ `cordis:group` 嵌套——逐一映射到现有 crate hook 或待实现 | 插件行层 | 生成完整映射表（工具行→`register(Some(standing_key))` 包装既有 `dsh-*` 工具；persona→scope 化 section；service 行→服务桥 A-03） | A-03 |
| B-12 | 改动 | **persona 行**：`systemPrompt.section('deployment:persona', order 0)` scope-only，`{{model}}`/`{{cwd}}` 渲染 | dsh-system-prompt | 映射 VariableProvider + scoped sections（值集 B-13） | A-02 |
| B-13 | 验证 | **`{{model}}`/`{{cwd}}` 变量**：TS=system-prompt 变量（loop 每回合注册，cwd 来自 session.header.cwd）；Rust `VariableProvider` 语义对齐 + 严格插值（未知→装配错） | dsh-system-prompt | 验证 `assemble(&AssembleContext{scope, vars})` 现成支持 | B-12 |
| B-14 | 改动 | **`disabled: !!js` 平台门控**：`process.platform==='win32'` 等；`dsh-eval` scope `{config,ctx,env}` 是否含 `process`？无则补最小 platform 常量 | dsh-eval/loader | 谱：`disabled_expr` 机制已存在（loader.rs:83-119）；补 platform 若缺 | B-02 |
| B-15 | 改动 | **`tools.presentAs('code')`（code 预设）**：dsh-tools `presentAs(scope)` 现成；presentation 服务依赖 codeRuntime —— code 行是否先 bridge（codeRuntime 句柄） | dsh-tools/code | 建议：code 预设 presentation 行本期 bridge codeRuntime（不破坏现 run_code） | A-03 |

### 1.4 loop 消费 scope —— P4 接缝（组合真正生效的必经关）

| # | 类型 | 问题 | 影响面 | 选项 / 建议 | 依赖 |
|---|---|---|---|---|---|
| C-01 | 改动 | **每 agent 从 scope 链组装 tools/prompt**：`build_loop_deps` 现在把共享 `SystemPrompt`/`ToolRegistry` 全量注入（service.rs:41-128）；需改为按 agent 的 `ScopeKey`（join 的 standing 链）决议 `tools.schemas(scope)`/`prompt.assemble(&AssembleContext{scope})` | dsh-agent-loop/service.rs | 每 agent 建独立组装闭包；`AssembleContext{scope}`/`schemas(scope)` 已存在——主要改数据来源与生命周期 | A-02, B-11 |
| C-02 | 验证 | **SystemPrompt scoped sections/persona API 是否完备**：`tools(scope)` 有（lib.rs:447）；`sections(scope)`/persona 覆盖/`assemble(scope)` 沿祖先链合并（lib.rs:499-523 已有 merge）——确认全链可用 | dsh-system-prompt | spike-3：一个最小「两 scope 互不可见 + persona 覆盖」验证 | C-01 |
| C-03 | 风险 | **回归红线**：`request/header`（build_request 快照）、`tool.exec` 语义、tool/call 事件、M4/M5 全工具默认面（默认会话=全局层）——改 scope 决议不得破坏 `default` 会话既有行为（全局=standing 之上的基） | dsh-agent-loop/web | 基线测试先固化再改；217+ lib 全绿为准 | — |
| C-04 | 决策 | **默认会话与预设的关系**：全局层（无 preset）仍=现在的全量工具/prompt？还是 default 会话也 join 某 standing（如 standard）？ | 组合层 | 建议：无选择时**不 join**（等价现状，全局层），选择后 join standing——行为可回退 | C-01 |

### 1.5 RPC / settings / 会话语义（P5）

| # | 类型 | 问题 | 影响面 | 选项 / 建议 | 依赖 |
|---|---|---|---|---|---|
| D-01 | 改动 | **`agentPreset.list/read` 真义**：现在 echo stub（web.rs:2768-2794）；需 roster（发现 §1.2）+ isDefault（settings 解析）+ authorable/hasDocument | web.rs | 对齐 apiproxy list 契约 | B-03, D-03 |
| D-02 | 改动 | **`session.create` 带 preset / resume 从日志 resolve**：header `agentPreset` 持久化（dsh-session 线头已埋）、`resolveSessionPreset` 最新事件>header、resume `assertPresetUnchanged` | web.rs/dsh-session | 对齐 session.ts:48-54 + api-proxy:1590-1602 | D-06 |
| D-03 | 改动 | **`agent-presets` settings namespace**：base=部署默认 + user 层覆盖 + hot reload（align index.ts:141-152）；删除预设后 unset default 回落 | dsh-settings/cli | D-095 样式照抄（lib.rs:296-372 有既有槽） | — |
| D-04 | 改动 | **`agentPreset.select` + 锁定**：仅 blank（无 turn/start）可 recompose（父链重载），否则 `agent-preset-locked`；成功 append `agent-preset/selected` 事件 | web.rs/dsh-session | 对齐 api-proxy:2987-3015 | C-01, D-02 |
| D-05 | 决策 | **`copy/remove/openDocument`（作者流）**：TS=仅 user 根可写；Rust 侧 openDocument=打开 preset.md（IOnlyFileDialog?）/copy 到 `.agent-presets` | web.rs/fs | **D 决策**：作者流并入 P5；openDocument 复用 D-098 原生对话框缝 | — |
| D-06 | 风险 | **会话生命周期与 standing 的接缝**：会话结束/删除→撤销 join（父链断链）+ 回收会话 disposer 桶（现在 teardown 仅 host 全局 host.rs:151,338-344） | dsh-agent-loop | 需 add_session_disposer 类 API | A-04 |

### 1.6 测试 / 部署 / 横切

| # | 类型 | 问题 | 影响面 | 选项 / 建议 | 依赖 |
|---|---|---|---|---|---|
| E-01 | 测试 | TDD 要点：组合解析单测（含 `!!js` 转译）、per-session 工具/提示词投影**两会话互不可见**、standing 守卫（inactive/leaked→broken）、select 锁定、RPC surface、resume 还原 | 全层 | 每 P 阶段红→绿→重构，全程 `cargo test --lib` + clippy `-D warnings` | — |
| E-02 | 验证 | **基线固化 ✅（2026 实测）**：改动前先固化 default 会话既有基线——`cargo test --lib -p dsh-cli` = **149 passed / 0 failed**（REAL_EXIT=0，~15s；首跑 exit1 系 PowerShell 管道伪码）。P4 回归以此为对照 | 测试 | 已锁，无需再跑（改动后重跑比对） | — |
| E-03 | 部署 | 60165（term-22，D-101 二进制）→ 阶段交付后**停进程→build→重部署→live 验收**（`session.create`→`session.prompt` 真回合 + 预设生效观察） | 部署 | 循 D-101 live 验收模式（`\\` 转义、`--data-binary @file`、dsh.exe 释放） | — |

### 1.7 C 阶段（收敛）全量项

| # | 类型 | 问题 | 影响面 | 选项 / 建议 | 依赖 |
|---|---|---|---|---|---|
| F-01 | 改动 | loop 服务访问从 LoopDeps Rc 线程 → `Cordis::get`（agent 作用域 ctx） | dsh-agent-loop | C 主体；align `agent.ctx` 服务解析 | A-02 |
| F-02 | 改动 | `ReactLoopAgent.ctx = dsh-core ctx.extend({agent})`；组合行 `ctx.on/provide/get` 直达 loop | dsh-agent-loop/dsh-core | 对齐 TS `runtime-types.ts:76` | F-01 |
| F-03 | 改动 | **AgentBus 折入 dsh-core 事件**（agent/request、tools/pre-execute 等水岭同源）——消除第二织物 | dsh-agent/dsh-core | 迁移清单：bus 用例→Cordis 事件命名空间 | F-02 |
| F-04 | 改动 | **每 agent 一 fiber + entry tree**；isolate realm 服务实例原生可达（真 mount.ts 对偶） | dsh-core/loader/loop | C 的终态 | F-02, A-01 |
| F-05 | 决策 | 收敛后 WASM loop（`run_turn`）与 native loop 关系：`boot.agent_loop` 分叉是否保留 | cli | 建议保留双驱动（native 生产），WASM 仅兜底 | F-04 |
| F-06 | 决策 | 收敛后 dsh-tools/dsh-system-prompt 是否迁到 `ScopeId` 键空间（彻底单键）还是保留 ScopeKey 双轨 | dsh-tools/dsh-system-prompt | 若 C 全迁则 B 阶段投影（A-02①）成为临时层，投入需权衡 | A-02 |

---

## 2. 技术预研（spike）清单 —— 编码 P2 前必须验证

| # | 验证问题 | 通过判据 | 关联 |
|---|---|---|---|
| spike-1 ✅ | Cordis/loader 多 standing 共存（A-01） | **已核实结构**：`Cordis`=一个 `Runtime`（独立 store/fiber/scope，库中无进程级共享态）→ 路径 B 建议**每 standing 一个 Cordis**（独立组合引擎 + isolate 私有服务实例；无跨 standing 泄漏可能；投影桥=共享面）；共享单树留给 C 收敛（agent 纤维 parent 进 standing 子树） | A-01 |
| spike-2 ✅ | dsh-scope `ScopeKey` 父链绑定/断链/重绑能力（A-04） | **已核实 src**：`bind_scope_parent`（lib.rs:140，仅一次+环检测）/`ScopeParentBinding::rebind`（lib.rs:134，**运行时父链重载=recompose**）/`scope_parent_of`/`scope_chain_of`（近者优先，lib.rs:159）/每 scope disposer（`ScopeContext::dispose/on_dispose`，lib.rs:276）——**无需新 API，只需接线** | A-04 |
| spike-3 ✅ | SystemPrompt scoped sections/persona/变量/完整链 assemble（C-02） | **已核实 src**：`SystemPrompt.layers: ScopedLayers<PromptLayer>`（每层 scoped `sections/contexts/runtime_context_suppressors/tool_providers/variables`）；`assemble(&AssembleContext{scope})` = `layers.merge(scope)` **全局基+远→近覆盖（最近胜）** + 变量同规则 + `suppress_runtime_context`→contexts 清空 + **单 `complete` section 整体替换提示词**；`section(scope)/context(scope)/tools(scope)/variable(scope)/suppress_runtime_context(scope)` 全现成。**minimal 的 `complete:true`+`includeRuntimeContext:false` 直接映射。** | C-02 |
| spike-4 ✅ | `!!js`→`__jsExpr` 复制转译 4 个 vendored 预设 + dsh-loader 装载 + win32 门控生效 | **已核实**；见下「spike-4 结论」——转译机械可做，但 dsh-eval 作用域有缺口 | B-02, B-05 |
| spike-5 ✅（预核） | 基线与 C-01 最小改动验证：`agent.rs:664` 已用 `assemble_context_for(&agent)` 装配 + host.rs:185-187 `tools.schemas(ctx.scope)` 已按 scope 决议 → **P4 是机械的**（填 standing 层即被 loop 拾取；未 join 的 default 保持全局=安全基线） | default 全绿 + s2 join standing 后可见 standing 工具 | C-03, C-01 |
| spike-6 ✅（已核实就位） | 装配 `process` 门面进 dsh-eval 作用域（platform/env/cwd）+ 静态预排除效果 | **已核实**：`process.platform`/`process.env.X`=member access，JSON 门面即可；`process.cwd()` 是 Call，需 `eval_call` 增一条 `process.cwd` 白名单项（lib.rs:314-341）；`entry_disabled` 用同一 eval_scope（loader.rs:84-117）。P1 TDD 最小切面 = 门面注入 + 1 白名单项 + 回归测试 | B-14, B-02 |
| spike-7 | 基带回归：`pwsh` 工具面在 win32 的最小可执行路径（§6.1-2 方向 A） | standard 在 win32 有 shell（A）或 bash 启用（B） | C-03 |
| spike-8 ✅ | bash on win32 可用性（§6.1-2 前提） | **已核实**：dsh-shell/resolve.rs:100-105 在 win32 解析 Git Bash（`C:\Program Files\Git\bin\bash.exe` 等）→ Rust bash 工具 win32 可用 | §6.1-2 |

### spike-4 结论（已核实，2026 复核 src）

- **转译机械**：4 预设共 12 处 `!!js`，仅 4 种模式——`process.platform === win32` / `!== win32`
  （disabled，10 处）、`process.env.DSH_CWD ?? process.cwd()`（minimal cwd，1 处）、
  `process.getBuiltinModule('node:url').fileURLToPath(new URL('skills/', baseUrl))`
  （cordis customSkillDirs，1 处）。`!!js` → `{"__jsExpr": "<expr>"}` 可直接在复制脚本做。
- **⚠️ dsh-eval 作用域缺口（真 bug 倾向）**：`dsh-loader::eval_scope`（loader.rs:120-125）只有
  `{config, ctx, env}`，**无 `process`**；`disabled` 求值 fail-closed（loader.rs:102-104
  `.unwrap_or(true)`）→ **当前所有平台门控行在任意平台上都被判定 disabled（win32 的
  `===` 与 !win32 的 `!==` 求值全失败 → 全禁用）**——不是偶发，是结构性错。修复 = eval_scope
  注入 `process` 门面（`std::env::consts::OS` → `"win32"` 对齐 JS）+ 断言级回归测试。
- **`env` 为空对象**（loader.rs:124）→ `process.env.DSH_CWD` 读不到真环境；最小改 = 真实 env 注入
  （白名单核对）。
- **调用白名单是硬门（spike-6 实测核到）**：`dsh-eval::eval_call`（lib.rs:314-341）只放行
  标识符 `String/Number/Boolean` 与成员 `Array.isArray/Object.keys`；scope 值为纯 JSON（无可调用
  值）→ `process.cwd()` 是**Call**，靠 scope 注入不够，需给 `eval_call` 增**一条白名单项
  `process.cwd`**（返回注入的 cwd 字符串），`process.env.DSH_CWD` 才是 member access（JSON
  门面即可）。二者合起来 `cwd` 表达式可精确求值；这是 P1 TDD 的最小切面。
- **两处超出 dsh-eval 子集**：`process.cwd()`（上一条的白名单扩展解决）与 cordis `new URL(...)` +
  `baseUrl`（`new` 不在文法，tokenizer 视其为标识符，必 fail）。处理：`customSkillDirs` 复制期按
  `baseUrl`=预设目录静态解析为 `<preset_dir>/skills` 字面路径（诚实差异，见 D-C + §5 遗留
  F-06）。

---

## 3. 阶段规划（**已定稿，2026，用户确认全部 ★ 推荐**）

> 定稿决议（round-9 用户拍板）：**采纳 §5 全部推荐** → 进入 TDD 实现；
> **win32 shell = B 先直通 P4、A(pwsh) 随 P3**；**broken 集 = skill 最小只读 + web/
> tool-cordis/command-compact 显式 broken**。对应 **DECISIONS D-103**。

```
P0 需求/设计收口 ✅（PLAN+DESIGN+REQUIREMENTS 定稿 → D-102/D-103 + git；D-A 已落地）
 │
P1 解析·发现·根（小/低险）          B-01..B-05, D-03
 ├─ agent.cordis.yml 解析复用 + !!js 转译 + preset.yml + 健康检查 + 自定义根发现
 ├─ agent-presets settings namespace（默认持久化）
 ├─ 通过：4 内置 + 1 自定义发现的 roster 单测绿；agentPreset.list/read（D-01 前半）可用
 │
P2 组合挂载 + 守卫（中/中险）        A-01..A-06, spike-1/2/4
 ├─ standing mount（挂钩 dsh-core/loader，每 standing 一 Cordis）+ join（父链）+ 守卫（inactive/leaked→broken）
 ├─ 通过：两 standing 隔离单测；守卫拒绝泄漏行；join 后视图正确
 │
P3 插件行实现 + 服务桥（中/中险）    B-11..B-15, spike-3
 ├─ persona/instructions/工具行/disabled 求值(process 门面)/presentAs(code)/skill 最小只读
 ├─ 窄服务桥 subset（plan-mode/compaction/fs-shadow/terminals…，web/tool-cordis/command-compact broken 诚实）
 ├─ pwsh（方向 A）**并行立项**（P3 内；未落地前 win32 走 B 门控=bash）
 ├─ 通过：四 shipped 预设全行映射表落地；每行单测；未桥接行 broken 有据
 │
P4 loop 消费 scope（中/高险）        C-01..C-04, spike-3/5
 ├─ 每 agent 按 scope 链组装 tools/prompt（default 基线不动）
 ├─ 通过：default 全绿回归 + s2 join standing 后模型真实看到 standing 工具/persona
 │     （直通 P4 = 组合真正改变会话行为，D-B）
 │
P5 RPC 全语义 + 作者流（小/低险）    D-01,D-02,D-04,D-05,D-06,E-01..E-03
 ├─ select/锁定/resume 还原/copy/remove/openDocument；per-session disposer 桶
 ├─ live 验收（E-03）+ 阶段小结（DECISIONS + 测试报告 + 部署说明）
 │
C 收敛（独立架构里程碑，P5 之后）    F-01..F-06
 ├─ 循环开进 dsh-core（Cordis::get、ctx.extend({agent})、AgentBus 折入、每 agent 一 fiber）
 ├─ 通过：组合行 ctx.on/provide/get 直达 loop；isolate 服务原生可达；dsh-core 为核心运行时成立
```

---

## 4. 风险与回滚

- **P4 语义回归**（最高）：default 会话基线被 scope 决议改动破坏 → 先 spike-5/基线固化，改不动基线即回滚该提交（安全回滚点=每阶段独立提交）。
- **双键空间投影（A-02①）若 C 全迁②则成为临时层**：投入沉没风险 → 在 D-103 记录预期，B 阶段最小投影面（只投影 loop 真消费的）。
- **`!!js` 转译正确性**：转译产物若偏离 vendored 原义 → spike-4 健康检查 + 复制脚本测试覆盖。
- **standing 内存/生命周期**：进程级 standing + 每会话 join，若文件频繁变（现无代支持）→ 文档记录重启归位（A-05）。
- **key 纪律**：任何阶段 key 只经 env 注入运行进程，不落盘/不入 git/DECISIONS；.env 禁用。

---

## 5. 待您深入分析后定夺的集中点（★）—— **2026 全部已决，见 DECISIONS D-103**

> 用户 round-9 拍板：**全部采纳本节省推荐**（含 win32 B→A、broken 集=skill 最小只读 +
> web/tool-cordis/command-compact broken）。下方各项保留为「决议依据」存档。

1. **A-01** standing 组合挂载形态——✅ 采纳 spike-1 方向：每 standing 一个 Cordis。
2. **A-03 + B-11** 窄服务桥**子集**：全表已在 §6；§6.1 把 shipped 行分为**必须桥**
   （planMode/compaction+pruner/terminals/fs）、**必须先决 win32 shell**（§6.1-2 方向 A 新增
   pwsh vs 方向 B 自持改写、**E-03 前必选**）、**先 broken**（skill/web/tool-cordis/command-compact）
   ——请拍板 broken 集与技术面大小。
2b. **win32 shell 方向**（§6.1-2）：**A** 立即新增 pwsh（round-6 已核实尺寸：PTY 侧已参数化只需
   注册 "pwsh" 后端，dsh-shell 需平行 pwsh executor，量=中，P3）｜**B** 无 pwsh 前按 Rust 能力改写
   门控（win32 用 bash，诚实差异、零新增）——推荐 **B 作直通 P4 的先行 + A 随 P3**。
3. **A-05** 组合文件变动：**round-6 已核实** `Loader::create/update/remove/sync` 均为公开流程 →
   推荐**原地换代（generation-based，无需重启）**为 P2 起主路径；HMR 文件监听后置；进程级重启仅兜底。
4. **B-04** 自定义根位置——**round-7 已核实 TS 权威约定**（`$DSH_HOME`→空白忽略→`~/.dsh`；
   用户根 `<dsh_home>/.agent-presets` trust=user、系统根 `resources/agent-presets` trust=system、
   每 id 首根胜出）→ 推荐 Rust 照抄，P1 加 `dsh_home()` 解析 + roots 数组 + `includeUserRoot`。待确认采纳。
5. **C-04** 默认会话与预设关系——**建议（TS 对齐）**：default 会话**不隐式 join**——保持「部署
   默认组合」语义（E-02 安全基线、向后兼容）；`agent-presets.default` 设置（TS `SETTINGS_NAMESPACE`
   的 `{default: z.string()}` 字段，apiproxy-config.spec.ts:464）只决定**新会话的初始预设选择**
   （仍走同一 standing+join 流程），不改变未选择预设的既有行为。
6. **F-05/F-06** 收敛后 WASM/native 双驱动与 ScopeId/ScopeKey 键空间去留（C 阶段决策，可后置）。

## 6. shipped 预设行 → Rust 实现映射盘点（B-11 预研结果，2026 实测四 yml + Rust 工具面）

> 结论：**standard/code/cordis 的模型可见行绝大部分在 Rust 已有对应工具**
> （bash/fs 家族/terminal 六件套/jobs/schedule/todo/subagent/workflow/goal/plan/ask-user/
> run_code/str_replace_editor），所需工作 =「按 scope 注册的包装」+「service-realm 桥」。
> **真缺口集中在 4 类（§6.1）**。

| preset | 行 id | TS 插件 | Rust 映射（现有） | 状态 |
|---|---|---|---|---|
| std/code/cordis | persona（text；minimal 加 complete/suppressRT） | dsh-persona | dsh-system-prompt scoped section order0 + `{{model}}/{{cwd}}` Var | 组装+新小节；minimal 语义点（R2） |
| std/code/cordis | agent-instructions（maxBytes） | dsh-agent-instructions | **AGENTS.md 自动装配待核实** | 需核实/新实现（§6.1-4） |
| std/code/cordis | tool-bash（disable win32） | dsh-tool-bash | dsh-shell `bash`（web_m5.rs:631、dsh-shell/tool_bash.rs） | 覆盖·作用域包装 |
| std/code/cordis | tool-pwsh（disable 非 win32） | dsh-tool-pwsh | **Rust 无 pwsh** | 新实现（§6.1-2，win32 必需） |
| std/code/cordis | tool-fs | dsh-tool-fs | dsh-fs read/write/edit（web_m5.rs:641-668） | 覆盖·作用域包装 |
| std/code/cordis | tool-fs-search | sampleOverCapGlobResults | dsh-fs glob/grep | 近似覆盖 |
| std/code/cordis | tool-jobs | dsh-tool-jobs（registry host 平面） | dsh-jobs 控件 | 覆盖·作用域包装 |
| std/code/cordis | skill-filesystem / tool-skill | dsh-skill-* | **Rust 无 dsh-skill** | broken 或最小（§6.1-3） |
| std/code/cordis | tool-goal | dsh-tool-goal | dsh-goal | 覆盖·作用域包装 |
| std/code/cordis | planning 组 {planMode:true} | dsh-plan-mode（section） | dsh-plan + scoped section | 覆盖+桥（realm 实例） |
| std/code/cordis | compaction 组 {compaction,toolResultPruner} | compaction-basic/command-compact/pruner | dsh-compaction + 桥 | 覆盖+桥；command-compact 待核 |
| std/code/cordis | delegation 组 {workflowEngine:true} | subagent-control/list/subagent(spawn/fork)/worker/workflow/ralph | dsh-subagent + dsh-workflow | 覆盖·作用域包装；codex/claude 已 disabled |
| std/code/cordis | tool-ask-user | dsh-tool-ask-user | dsh-tools ask_user（D-098 面） | 覆盖 |
| std/code/cordis | tool-todo | allowParallelInProgress | dsh-jobs todo_write | 覆盖 |
| std/code/cordis | tool-web | fetch:false, searchTimeoutMs | **Rust 无 web 工具** | broken 或最小（§6.1-3） |
| code 仅 | tool-presentation | {mode:code} | dsh-tools `presentAs('code')` + dsh-code-runtime run_code（D-068/073 既有面） | 覆盖+桥 |
| cordis 仅 | skill-filesystem | customSkillDirs（`!!js` URL） | §6.1-3 + spike-4 静态解析 | broken 或最小 |
| cordis 仅 | tool-cordis | dsh-tool-cordis | **无对应**（动态自改自身组合超出静态语义） | broken（诚实） |
| minimal | persistent-shell 组 {terminals:true} | pty/terminal-bash/persistent-bash/pwsh 对 | dsh-terminal 六件套（web_m5.rs:599-626）+ **pwsh 持久缺** | 部分覆盖+新增 pwsh |
| minimal | filesystem 组 {fs:true} | fs-local（cwd !!js）/str-replace-editor | dsh-fs（Rust fs 即本地，无沙箱 provider 遮蔽概念）+ **str_replace_editor 已有**（web_m5.rs:663） | 覆盖；realm 语义弱化如实记 |
| minimal | persona | complete:true, includeRuntimeContext:false | dsh-system-prompt complete section + **RuntimeContextProjection 按会话关** | 新语义点（R2） |

### 6.1 新暴露的缺口（直接影响 A-03「桥接子集 vs broken」决策）

1. **`persona complete:true` + `includeRuntimeContext:false`（minimal）**：完整提示词替换 +
   抑制运行时上下文 → 需 C-01 的 RuntimeContextProjection/`project_context` 有**按会话开关**
   （现全局）。低成本，属语义点（R2）。
2. **`pwsh` 工具面缺失（win32 上 standard/code/cordis 的 shell 会空）**：Rust 现有 `bash`
   工具在 win32 也**可用**（dsh-shell/resolve.rs:100-105 解析到 Git Bash `C:\Program Files\Git\bin\
   bash.exe`，spike-8 ✅），但**无 pwsh**。TS 在 win32 上 bash 禁、pwsh 开；照搬该门控 →
   win32 空 shell。**两个互斥方向（请拍板）**：
   - **A · 立即新增 pwsh 工具**（round-6 **已核实尺寸**）：`PtyBackend::new(label, program)`
     已参数化（dsh-terminal/backend.rs:77，types.rs:140 已注「未来可扩 pwsh」）→ PTY 侧只需
     注册 `"pwsh"` 后端类型 = powershell.exe；`dsh-shell` 为 bash 形（BashConfig/`bash -c`）→
     需平行 `pwsh` executor（one-shot + start）或把 shell 参数化（后者回归面大，不推荐）+
     m5 注册 tool-pwsh。**量=中（P3 立项）**。
   - **B · 自持预设的 win32 门控按 Rust 能力改写（诚实差异，零新增）**：无 pwsh 前，win32 的
     standard/code/cordis 把 bash 行改为**启用**（复制期把 `process.platform === 'win32'` 门控
     按 Rust 能力矩阵重写，并注释差异）——绝不空 shell、零新增；待 pwsh 落地后再切回忠实门控。
   > E-03 直通 P4 live 验收在 win32 开发机跑——**A 或 B 必选其一**，否则 standard 在 win32 空 shell。
3. **skills（dsh-skill）与 web（dsh-tool-web）无 Rust 侧**（crates 无 dsh-skill/dsh-web）。
   选项：①先 broken（诚实）；②最小 skill 目录（`.agents/` 发现+catalog/loader 只读面）；
   ③最小 web（需联网后端，成本高）。建议 skills=broken 或最小只读、web=broken。**待用户拍板。**
4. **`agent-instructions`（AGENTS.md 自动装配 + fs 触达重读）**：Rust 是否已存在 AGENTS.md
   指令装配待核实；若无 → 新实现（读工作区 AGENTS.md 注入 prompt section + fs 命中重读钩子）。
5. **`command-compact`（`/compact` 会话命令）**：属 dsh-compaction 面；Rust 有 `/compact`
   命令则映射，否则 broken/后续 CLI 命令立项（非 P-path）。
6. **`tool-cordis`（自省/动态改自身组合）**：超出 preset 静态语义 → 本期 broken（诚实），
   与 C 收敛后的只读行分家——**不引入非建模 JSON 面**。

> 由此 A-03 桥接子集建议演化为：**必须桥**= planMode / compaction+pruner / terminals / fs
> （静态语义可表达）；**必须新增**= pwsh（win32 必需）；**先 broken**= skill / web /
> tool-cordis / command-compact（待用户拍板）。

---

> 定稿流程：您对本节 ★ 点 + §3 阶段草稿给出取舍 → 我落 DECISIONS D-103（含选项/理由/回滚）→ git 提交 → 进入 TDD 分段实现（红→绿→重构，全程基线+clippy）。关键决策 doc ⊆ DECISIONS ⊆ git 三者互查；**D-A 复制自持已落（D-102）**（与 vendored 断开，不依赖参考树运行）。

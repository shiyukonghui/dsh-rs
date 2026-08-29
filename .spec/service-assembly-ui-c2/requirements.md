# 需求结论：桌布 C2 —— 宿主实时清单端点（uiManifest/list + rev + 坏包语义）

日期：2026-09-04
阶段：需求分析（瀑布流阶段 1）——本文档为阶段关卡工件，**待用户确认后**方进入设计。
上游：`docs/SERVICE-ASSEMBLY-UI-HANDOFF.md`（接手文档）+ `.spec/service-assembly-ui-canvas/design.md`
（桌布契约 §6 发现面，D-181 锁定）+ D-182（C1 已落地）。
范围决策：本次接手交付 **C2**（用户确认）；C3 桌布壳为下一阶段、另过关。

---

## 1. 目标（第一性原理拆解）

**根本目的**（承 D-178/D-179/D-181）：插件 UI 生态要能**不依赖宿主发版**长出新界面。
桌布（C3）要显示卡片，必须先回答「现在到底有哪些卡」——这就是 C2：**宿主侧聚合清单端点**。

**C2 要不可再分地做到**：
1. **实时**：清单每次请求从实时状态计算（`boot.packages` + loader 生效状态 + 各包
   `web/ui.json` 实文件），**禁止启动期快照缓存**——否则热插新插件不出卡片，D-175/D-177
   热更语义直接断裂。热插拔是**第一等要求**，不是优化项。
2. **可缓存协商**：`rev` = 清单**内容**的代数。内容不变 → rev 不变（**重启后仍稳定**，
   故用内容哈希而非进程内单调计数）；内容变 → rev 变。客户端可携 `rev` 问「变了吗」。
3. **诚实失败面**：装了但坏了的声明**必须可见**（error 条目），没装 UI 的包**正常跳过**。
   两者语义必须区分——坏卡画 fail-loud 元数据卡，无卡不该出现任何东西。
4. **宿主是单一权威**：size 封顶裁剪（`w≤4`/`h≤8`）与 `type` 未知→`misc` 归一在**清单层**
   完成；渲染器（C3）只信清单，不再各自解释原始声明。

**非目标**（明确不做，越界即违规）：
- 桌布壳/侧栏/网格渲染（C3）；`status`/`list` 渲染器（C4）；SSE `ui-manifest-changed`
  推送（C5）；`view` 内容下发（清单只含**元数据**，内容按分类选中才经
  `/plugins/<n>/ui.json` 拉取——这是 D-181 三条通道分工的设计本意）；
  真实 LLM 调用 / loader entry 化 / SSR（承试点边界）。

## 2. 自上而下：成功标准分解

| # | 子目标 | 验收判据（草案，设计阶段细化到测试名） |
|---|---|---|
| S1 | 聚合正确 | 多个带 `web/ui.json` 的包 → `cards[]` 各一条：`pluginName/cardId/type/title/size/declPath`；**无坐标字段** |
| S2 | 实时性 | 请求→请求之间装卸包（改 `boot.packages` 或改 ui.json 文件）→ 清单条目与 rev 随之变；**无缓存层** |
| S3 | rev 语义 | 同内容 → 同 rev（跨「进程」稳定）；增/删/改卡 → rev 变；卡增删顺序变化 → rev 变 |
| S4 | 坏包不静默 | `ui.json` 非 JSON / `$schema` 非 v2 / 顶层 `kind≠"card"` / `cardId` 缺失 → 该包以 **error 条目**出现（带 `pluginName/declPath/error{code,message}`），其余卡照常返回 |
| S5 | 无 UI 区分 | 无 `web/` 或无 `web/ui.json` 的包 → 完全跳过，不是 error 条目 |
| S6 | 归一在清单层 | 未知 `type` → `misc`（保留 `declaredType` 原值可诊断）；`size` 越上限 → 裁剪值 + 记录（降级不是失败） |
| S7 | disabled 交叉 | loader 中存在同名 entry 且 `disabled=true` → 该包**不出卡**；无同名 entry → 出卡（试点现状） |
| S8 | 不回归 | 本机基线 230 通过/0 失败不变（见 A6 实测）；m32 8/8；clippy 0；verify-diff 26/26 |

## 3. 自下而上：现有代码实证（已读源码核对，非推测）

| 事实 | 证据 | 对 C2 的含义 |
|---|---|---|
| `/api` 路由：`path.trim_start_matches("/api/")` → `method` | `web.rs:971-972` | 裸 `/api/ui-manifest` 会得到无 `/` 的 method → `dispatch_wasm_remote` 判 `not-implemented`（除非开特判例外） |
| 原生 RPC 臂同型先例 | `web.rs:4399 "commands/list"`、`4202 "settings.describe"` 等（`dispatch()` = `web.rs:3459` 的 `match method`） | 新臂 `uiManifest/list` 以原生臂加入 `dispatch()`，与既有 wire 同形，复用 trust fence（`/api` 仅 loopback，`web.rs:906`）与 RPC 信封 |
| `dispatch_wasm_remote` 要求 `namespace/method`，无 `/` → not-implemented | `web.rs:4452-4459` | 证实选项 (b) 必须为单端点开路由例外——**为个案破例是架构债** |
| `boot.packages: Vec<PluginPackage>`，`PluginPackage.web: Option<PathBuf>` | `lib.rs:146`、`plugin_pkg.rs:40-53` | 清单 = 遍历 packages，`web/ui.json` 存在才读；**每请求实读文件**即天然实时 |
| `boot.loader: Option<Loader>`，`loader.entries() -> Vec<EntrySnapshot{id,name,disabled,group,fiber}>` | `lib.rs:143`、`loader.rs:714/73-79` | disabled 交叉取数确认：按 `entry.name == package.name` 匹配；**「待查」已闭环** |
| 试点 llm-deepseek 非 loader entry（serve 装配直接 push） | `web.rs:290-296` | 「disabled 不出卡」与「试点未 entry 化」并存的唯一一致语义：**无同名 entry 视为生效** |
| loader 投影已有 `disabled` 字段先例 | `remote_host.rs:152-169` | 语义对齐 harness 既有 loader 面 |
| `sha2`/`sha1` 已在 Cargo.lock | `Cargo.lock:2404/2415` | 内容哈希可离线引入成熟依赖，无需手写哈希 |
| v2 声明实样 + m32 契约断言 | `web/ui.json`、`m32_llm_deepseek.rs:113-158`（cardId 非空/type 闭集/size 封顶且无坐标） | 清单层校验规则与 wasm 侧契约**同源**，错误码复用 §7 fail-loud 表 |

**双视角相遇**：自上而下要求「实时 + 协商 + 诚实」；自下而上证明
「boot.packages 每请求遍历 + ui.json 实读」零新机制即可满足，且 wire 形状已有
`pluginInventory/list` 同型先例——**现有条件完全允许契约形态落地，无需破例**。

## 4. 假设（用户未明说、但默认成立——已列出待确认）

- A1：本次接手只做 **C2**，桌布壳（C3）下一阶段另过关（接手文档 §7 顺序）。
- A2：端点形状采 **(a) `/api/uiManifest/list`** 原生臂（接手文档作者倾向；与
  `pluginInventory/list`/`settings/describe` 同形）。选定后**必须回写**
  `design.md §6.1`（现文本 `/api/ui-manifest?rev=` 与 wire 不一致，属契约文档纠偏，
  不是改契约——语义不变，只改 URL 形状表述）。
- A3：请求体走既有 RPC 信封 `{args:{rev?}}`；`args.rev` 等于当前 rev →
  `{unchanged:true, rev}` 短路（省带宽；C5 的 SSE 落地前，轮询是过渡消费方式）。
- A4：disabled 语义 = 「同名 loader entry 存在且 disabled → 排除；无同名 entry → 生效」
  （S7，试点 entry 化是后续装配侧工作，本阶段不强行 entry 化试点）。
- A5：清单条目字段 = 设计 §6.1 的六元组 + 归一记录（`declaredType`/`size-clamped` 仅异常时出现）；
  error 条目**无** type/size（无从归一），但必须带 `pluginName/declPath/error`。
- A6：测试基线由接手实测对齐。**接手实测（2026-09-04，git `44f9618` 干净树）**：
  dsh-cli **230 通过 / 0 失败**（接手文档记 225/5——那 5 个 M5 环境性失败在本机全部跑绿，
  本机基线**更严**，直接以 230/0 为回归底线）；m32 **8/8**；dsh-wasmrt 14 测试目标全绿；
  clippy `-D warnings` **0**；verify-diff **26/26**。构建/测试姿势按接手文档 §4 执行有效
  （清 RUSTC_WRAPPER + `CARGO_NET_OFFLINE=true` 不带 `--offline`；PowerShell 假失败以
  Finished 为准）。

## 5. 约束（硬性与软性）

**硬性**（契约/不变式，违反 = 推翻 D-181）：
- 清单**只含元数据**，不含 `view` 内容。
- 卡身份 = `(pluginName, cardId)`；坐标永不出现在清单/声明里。
- 坏声明**不静默丢弃**；无 UI 与坏 UI **语义不同**。
- `type`/`view.kind` 双枚举不合并；清单归一不发明新 type 值（未知只落 `misc`）。
- 不做启动快照缓存（热插拔第一等）。

**软性**（工程纪律）：
- 无外网：构建用 `$env:CARGO_NET_OFFLINE="true"` + 不带 `--offline`；先清 RUSTC_WRAPPER。
- 含中文文件只用编辑器工具写；提交信息中文走 `git commit -F`。
- 新断言必须做**红验证**（证明非恒真）。

## 6. 边界与最易犯错误（主动纠偏）

**本类工作最常见的错误**：
1. **为省事先缓存清单**（Boot 里存一份，装卸时手动失效）——看似等价，实则一旦漏掉某个
   变更源（ui.json 直接改文件、loader 运行时 create/remove）就静默说谎；契约明令禁止，
   实时计算才是唯一正确形。
2. **把端点做成裸路由例外 (b)**——为单个端点破坏 `/api` 网关约定，前端 rpc 通道/trust
   fence 复用全部打折扣，后续每个新端点都有样学样，网关名存实亡。
3. **把「坏包」和「无包」混成同一路径**（都跳过，或都报错）——前者必须可见（用户装了
   东西坏了），后者必须安静；混了就不是诚实系统。

## 7. 验收标准（阶段关卡）

1. 新增测试全绿且每条**经过红验证**：聚合 / rev 稳定与变化 / 实时（装卸包）/ 坏包 error
   条目 / 无 UI 跳过 / disabled 排除 / size 裁剪与 type 归一在清单层生效 / `unchanged` 短路。
2. `cargo test -p dsh-cli -p dsh-wasmrt`：新增失败 0；m32 8/8；clippy `-D warnings` 0；
   verify-diff 26/26；基线数字与 §5（接手文档）一致。
3. 工件齐：`.spec/service-assembly-ui-c2/{requirements,design,acceptance}.md` +
   `docs/DECISIONS.md` D-183 条目 + git 提交互查 + **回写** canvas design §6.1。
4. 代码里清单聚合是**纯函数**（输入 = packages + loader 快照，输出 = cards/rev），
   HTTP 层只做取数与序列化——C3/C5 复用同一核心，不再复制规则。

## 8. 决策回执（用户确认，2026-09-04 —— 需求闸已过）

| # | 问点 | 结论 |
|---|---|---|
| Q1 | 本次接手范围 | **仅 C2**（清单端点）；过关验收后再进 C3 桌布壳 |
| Q2 | 端点形状 | **(a) `/api/uiManifest/list`**：原生 `dispatch()` match 臂，与 `pluginInventory/list`/`settings/describe` 同形；选定后**回写** canvas design §6.1 + §1 架构图 |
| Q3 | disabled 语义 | 包名匹配 loader entry：**存在同名 entry 且全部禁用 → 排除**；无同名 entry → 生效（试点现状兼容；entry 化属装配引擎侧后续） |

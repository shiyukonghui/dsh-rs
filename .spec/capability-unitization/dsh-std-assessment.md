# 评估：dsh-std 元协议概念 × 本项目 wasm 能力（第 69 轮，能力单元化任务支线）

## 0. dsh-std 是什么（实勘）
嵌套 git 仓库（dsh-rs 内未追踪的 TS/npm pnpm monorepo，18 包）。核心主张：
- **元协议**：`@dsh-std/core` 零业务概念，只有三个原语（protocol/negotiation/version）；
- **身份语法**：`apiVersion = group/vN[(alpha|beta)rev]` + PascalCase `kind`，严格文法校验；
- **声明**：`ProtocolDeclaration { participant, requires[], supports[] }`（可带 spec 附属数据）；
- **协商**：每协议定义**自持** `validateRequirement/validateSupport/negotiate`；
  版本兼容**不推断**（显式 `accepts` 集）；纯函数产出 `NegotiationReport {compatible,
  protocols[], issues[{code,severity,path,message}]}`；
- **Profiles/一致性套件**：产品形态准入规范 + 「宣称符合必须过套件」；
- **adapter=单点减震**：上游重构的破坏全被 adapter 吸收；
- **自我宣言**：采纳自愿、符合实现**无需依赖这些 npm 包**。

## 1. 与我们的既有同构（为什么能对上）
| dsh-std 概念 | 我们已有/已拍板 |
|---|---|
| 声明式单元+宿主解耦 | 13 张声明卡+ui.json 契约 `dsh/plugin-ui/v2`（=apiVersion 方言雏形） |
| adapter 单点减震 | **host-remote 单载体**（D-185/D-212 的 remote 回落架构） |
| 一致性套件 | e2e-audit T0–T15 + verify-diff（事实上的 conformance 在跑） |
| 不推断兼容/显式 accepts | 我们的世界 kinds 编译期固化（WIT=更强形式） |
| 诚实报告 philosophy | 全项目的诚实错误面（bad card/reason） |

## 2. 真缺口（dsh-std 照出来的镜子）
1. **无 requires/supports 声明面**：单元不能声明「我需要哪些宿主服务/哪个版本」——
   薄服务族试点（用户已定首刀）进 service world 时**必然需要**这个，否则接缝消费关系
   只存在于代码考古里；
2. **挂载前零协商**：mount-sync 现状=扫到即挂，坏了对账失败才出 bad card（一长串
   error 字符串）；dsh-std 模式=挂载**前**纯函数协商 → 结构化 reason codes，
   「为什么挂不上」机器可读、UI 可展示（panel-plugin-inventory 是天然展示位）；
3. **版本接受面靠字符串硬比对**：`"dsh/plugin-ui/v2"` 全等比较，无 accepts 矩阵、
   无 alpha/beta 通道；
4. **契约 validator 无目录化**：validate_declaration（canvas-shell schema.rs）事实上
   就是 ui 契约的 definition-owned validator——但各契约各自散落，没有 Catalog。

## 3. 硬冲突（不可照搬处，用 §4a 判据复核过）
1. **范畴论拦截「协议即插件」的接口面**：WIT 接口是编译期强类型根，运行时加载
   不可信单元「自定义接口」=把类型根变成住户，禁止。可落地的只有**数据级**可插拔
   （accepts 集、每契约注册 validator 进 host Catalog）——即 dsh-std 的 Catalog 面
   可学，「核心永不变+协议无限插」的**接口**半句不采。
2. **双账本风险**：JSON 元协议账本（requires/supports）+ WIT 账本并行手写=必漂移。
   规矩：**manifest 从 WIT/构建期生成，不是手写第二契约源**（JSON 是视图，WIT 是真身）。
3. **范围错位**：dsh-std 标准化的是**产品级互操作**（宿主↔TUI/Web/远程 UI/第三方
   插件生态）；我们 wasm 单元接缝是运行时内部。内部全量外化=过度标准化税（§5 常见
   错误）。**但**用户驱动已选「扩展生态」——外部第三方 wasm 单元要的恰恰是这套。

## 4. 结论（推荐路线：采概念、不采依赖）
1. **三原语 Rust 纯函数移植**（~200-300 LOC，零依赖，TDD 田）：apiVersion 文法
   解析器 + 声明校验器 + Catalog/协商器出结构化报告。落点：dsh-wasmrt（或新
   `dsh-contract` 小 crate）——自身成为地基新成员（范畴层，天然不可单元化，自洽）；
2. **plugin.json 升级 = dsh-plugin.json 方言**：`participant + requires/supports`，
   **从 WIT/构建产物生成**；ui 契约保持 `dsh/plugin-ui/v2` 但纳入文法（方言↔标准
   语法映射表，留互操作之门）；
3. **mount-sync 加协商关**：compatible→挂；不兼容→结构化 issues 进
   panel-plugin-inventory 展示（不静默、不半挂）；
4. **与首刀合流**：薄服务族试点的 service world 契约形状**第一天就带
   requires/supports**——试点即新标准的第一个消费者（标准不空转）；
5. **一致性面**：每 profile（canvas 面板单元/服务单元/loop 单元）列 conformance
   清单，挂进现有 audit/verify-diff，不新造轮子。

## 5. 待用户确认
1. 路线「采概念、不采依赖、Rust 移植+manifest 生成自 WIT」是否定案？
2. apiVersion 语法：直接采用 dsh-std 文法（`dsh.std/ui/v2` 式，换生态兼容）还是
   保留 `dsh/plugin-ui/v2` 方言+映射表（零迁移）？
3. 协商关是否纳入薄服务族首刀设计稿（推荐：纳入，试点即首个消费者）？
4. 不兼容报告的 UI 落点：panel-plugin-inventory 加区块（推荐）还是新单元卡？

> **第 69 轮用户答复**：路线=采概念不采依赖（定案）；其余三问要求展开做法 → 见 §6。

## 6. 三案详解（用户令「进一步分析并说明打算如何做」）

### 6a. 版本语法：为什么「标准文法 + 方言映射」而不是全面换文法
**事实盘点**：现存契约标识有三层——① ui.json 契约字符串 `dsh/plugin-ui/v2`
（wire 不变式+校验器+13 张卡+审计全在引用）；② WIT world 名（编译期）；
③ cargo 包命名空间 `dsh:xxx`。注意 `dsh/plugin-ui/v2` 含两个 `/`，**本身不合法**
于 dsh-std 文法（`group/vN[stabN]`，group 无斜杠）。
**方案**：新元数据（requires/supports/accepts/协商报告）一律用**纯标准文法**
canonical id（如 `dsh.panel-ui/v2`、`dsh.service/v1alpha1`）；旧字符串作为**方言
字面量保留**，由 `contract-registry` 静态映射表关联（一张表+一组测试）；未来契约
换 major 时**出生即用标准文法**，旧方言永久兼容不回填。
**为什么不全面换**：换文法=动 13 张单元卡的 ui.json（曾拍板单元层零改动）+
canvas-shell 校验器 + wire 不变式 + 审计断言，纯迁移税换不到任何新能力；映射表
方案里协商器只见 canonical id，方言在门口翻译一次——生态兼容门与零迁移兼得。

### 6b. 首刀合流：协商层怎么长进薄服务族试点（四步）
1. **契约定义先行**：`dsh-contract` 新小 crate（地基新成员，自洽于 §4a 范畴层）：
   apiVersion 文法解析器 + ProtocolDeclaration 校验器 + Catalog/协商器，
   纯函数零依赖 ~300 LOC，纯 TDD 田；
2. **单元侧声明生成**：service world 单元的 plugin.json 增 `"requires":
   [{apiVersion,kind,optional?}]`——**由构建期生成/校验**（单元 crate 内声明表
   + CI 校验「WIT imports ⊆ 声明 requires」，杜绝手写漂移；即 §3-2 双账本规矩）；
3. **宿主协商关**：mount-sync 在 kind 检测之后、实例化之前跑一次纯函数协商——
   compatible→挂；不兼容→**不挂**并把 issues 记入 `pending_incompatible`
   （新 RPC 臂 `contract/negotiationReport` 可查）；
4. **验收即测标准**：同一单元对 v1/v2 两种 host catalog 构建，协商判定进单测；
   故意不兼容件挂载 → 审计断言「未挂载 + reason codes 可见」。
**成本对账**：现在加=元数据字段+一次纯函数调用；以后补=给已发布单元重开契约形状
+ manifest 迁移——首刀合流是便宜的时候把地基打好，不是加戏。

### 6c. 报告 UI：为什么落 inventory 而不是新卡
**展示链**：mount-sync `pending_incompatible` → 新 RPC `contract/negotiationReport`
→ **panel-plugin-inventory 单元**把自己的 dataRpc 结果与报告合并渲染：不兼容行
`state="incompatible"`、issue codes 进消息列。
**为什么零壳改动**：inventory 是声明卡，list 视图天然支持行+列——「不兼容」只是
行的一种状态（与现有 bad/ok 同型）；canvas-shell/桌布**一行不改**，改动全落在
单元层（ui.json+数据映射）+一个 RPC 臂。新专卡=多一张卡+新视图语义，收益相同
成本高一级；只进 RPC=违背全项目「诚实可见」philosophy，排障要靠人开 curl。
**验收**：审计 T16——放一个 manifest 版本号不存在的假单元 → 断言桌面卡数不变
+ inventory 行含 reason code + 移除后报告清空。

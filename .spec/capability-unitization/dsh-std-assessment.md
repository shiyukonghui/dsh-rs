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

# 设计稿：契约标准化（dsh-std 概念落地）+ 薄服务族试点（2026-09-05，第 70 轮）

依据：requirements.md（P2 命题+四根判据+边界）与 dsh-std-assessment.md §6；
用户拍板：路线=采概念不采依赖；**6a=全部纯标准文法、不做兼容设计**（方言映射否决）；
b/c 授权按阐明方案细化。

## 0. 全局分期与关卡（每相一个可提交工件，过关才下一相）
| 相 | 工件 | 回滚点 | 状态 |
|---|---|---|---|
| P0 契约文法硬切 | 「dsh/plugin-ui/v2」→「dsh.panel-ui/v2」全量硬替换 | 单提交 revert | ✅ a69d1e9 |
| P1 dsh-contract crate | 三原语纯函数（TDD） | 新 crate，删除即回滚 | ✅ bf0a444 |
| P2 协商关+报告面 | mount-sync 协商 + RPC 臂 + inventory 展示 + T16 | 单提交 revert | ✅ 9eb6ca8 |
| P3 薄服务族试点 | **修正**：不立新 world（复用 host-remote，目录=ns）；首单元 plan 读+判定面（写面=v2，见 D-217）+ `--service-units` + m42 对拍 + T17 | 单元删除+挂载回退原生 | ✅ D-217 |

## 1. P0 文法硬切（6a 落地，无兼容）
- **canonical id**：`dsh.panel-ui/v2`（文法合规：group=`dsh.panel-ui`，major=2，stable）。
  改标识不改契约内容 → major 保持 2（重命名≠契约破坏）。
- **改动清单（实勘钉死）**：13×`web/ui.json` "$schema" + 13×单元 `src/lib.rs`
  内联声明副本 + `canvas-shell/src/model.rs` SCHEMA 常量 + `dsh-cli/src/ui_manifest.rs`
  硬比对（含报错文案）+ 宿主测试 ~12 处断言（ui_manifest/web.rs/m32~m41）+
  `docs/SERVICE-ASSEMBLY-UI-HANDOFF.md` 一处。
- **零兼容语义**：旧字符串「dsh/plugin-ui/v2」此后与 v1 同罪（`schema-version-unsupported`）；
  新增测试钉死此拒收。
- **验收**：全工作区测试绿 + 真路由审计 T0–T15 全绿（行为不变，仅身份变）+
  grep「plugin-ui」在 src/wasm-plugins 命中数=0。

## 2. P1 dsh-contract crate（三原语 Rust 移植，零依赖纯函数）
- **模块**：`version.rs`（`ApiVersion{group,major,stability∈{Stable,Alpha,Beta},revision}`
  parse/format，严格文法，非法即 Err）；`declaration.rs`（`ApiReference{api_version,kind}`、
  `Requirement{optional,spec}`、`Support{spec}`、`Declaration{participant,requires,supports}`
  + 校验器）；`catalog.rs`（`ProtocolDefinition{accepts,validate_requirement,
  validate_support,negotiate}` 以 trait 表达 + `Catalog::register/find`）。
- **移植纪律**：核心**不推断版本兼容**（只认显式 accepts 集，同 dsh-std）；
  报告形状对齐：`NegotiationReport{api_version:"dsh.core/negotiation-report/v1alpha1",
  evaluator,compatible,protocols[],issues[{code,severity,participant,path,message}]}`。
- 文法正则逐字对齐 dsh-std（`^[a-z][a-z0-9.-]*/v[1-9][0-9]*((alpha|beta)[1-9][0-9]*)?$`，
  kind=大驼峰）——这是「采概念」的实证：同一文法，双实现互认。
- **验收**：单测含 dsh-std README/源码里出现过的字面量样本对拍（含 alpha/beta/非法文法）。

## 3. P2 协商关 + 报告面（6b 第 3 步 + 6c）
- **host catalog 种子**：宿主服务身份表（session/log、settings、llm、schedule、
  jobs、approvals、workspace-files、runtime-status… 各 `dsh.<svc>/v1` stable），
  常量表生成，与 dispatch 面 CI 对账。
- **单元声明**：`plugin.json` 增可选 `"requires": [{apiVersion,kind,optional?}]`；
  **生成规矩**：单元 crate 内声明常量 + 构建脚本/CI 校验「声明 ⊇ 实际消费的 RPC 面」
  （本期以 rpc 名单对账代替 WIT imports 解析——我们的面板单元经 host-remote 消费 RPC，
  imports 恒同形，声明面按 RPC 消费计更贴真身）。
- **挂载序**：mount-sync 现序「kind 检测→实例化」之间插协商步：无 requires→直接挂
  （老单元零声明零扰，P0 语义）；有 requires→Catalog 协商，兼容→挂，
  不兼容→**不实例化**，issue 列表入 `NegotiationStore`（serve 态，随扫描刷新）。
- **RPC 臂**：`contract/negotiationReport/list`（POST，走既有 envelope）返回报告 JSON。
- **inventory 展示**：panel-plugin-inventory 单元 dataRpc 合并报告：不兼容行
  `state:"incompatible"`，issues.codes 串进消息列（声明卡 list 视图零壳改动）。
- **验收**：P1 单测全绿；审计 **T16**=投放假版本单元（requires 指向不存在服务）→
  断言桌面卡数不变 + report RPC 含该 participant 与 reason code + inventory 行可见
  + 移除后报告清空。

## 4. P3 薄服务族试点（首个 service world 单元）
- **首刀单元**=`plan`（dsh-plan 314 LOC，最薄、语义独立、消费面窄：settings+session/log）。
- **WIT world** `dsh:service`：export `handle(cordis: string, payload: string) -> string`
  （对齐 host-remote 既有惯例，零新机制）；宿主新增 kind=Detection::Service 挂载，
  把该组件注册为 `plan/*` RPC 的承载体（dispatch 现路「未装配→host-remote 兜底」旁
  新增「service 单元承载」一级）。
- **requires 声明**：`[{apiVersion:"dsh.session-log/v1",kind:"SessionLog"},
  {apiVersion:"dsh.settings/v1",kind:"Settings"}]`（P2 协商关自然生效=标准第一消费者）。
- **回退**：cordis 开关 `service_units: on|off`（默认 off=纯原生行为；试点在冒烟场景开）；
  原生 dsh-plan crate 保留不动（等价性对拍：同 RPC 序列 新旧两路响应一致=核心验收）。
- **验收**：等价性对拍测试 + 审计回归（plan 卡面若存在则行为一致；本期先 RPC 面）。

## 5. 风险与对策
- **P0 遗漏点**：以「grep 命中=0」为硬闸（含 docs/audit），漏网即测试红，不会静默。
- **P2 声明与真身漂移**：CI 对账（声明⊇消费）；只紧不松——多声明 harmless，漏声明=拒挂。
- **P3 范围蔓延**：本期只做 plan 一族一个单元；goal/subagent/workflow/presets 立为
  同模版的后续复制，设计稿不为它们预开接口。
- **双账本**：JSON 声明永远是视图（生成/校验），WIT+RPC 消费面是真身——CI 是两账本的装订机。

## 6. 决策待落章（过关后 DECISIONS 追加 D-216）
文法硬切（无兼容）/ dsh-contract 落点 / 协商关挂载序 / 试点=plan+对拍回退模式。

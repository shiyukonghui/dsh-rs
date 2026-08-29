# 需求结论：面板改写 #2 —— 运行时状态卡（panel-runtime-status）

日期：2026-09-05 | 阶段：需求分析 | 关卡：自主过闸 | 决策记录 D-187。
上游：C4 改写型（面板 = 服务装配单元：UI 声明 + wasm 逻辑，scan 自动挂载）；
C4-A 点亮的 status 渲染器（§4.1 契约）；宿主投影器现成数据面（remote_host.rs 实证）。

## 1. 目标与选型（第一性）

「前端全部由服务单元组成」按面板逐块迁移。#2 选 **运行时状态卡**：
1. harness 侧栏/概览的本质之一是「后端现在什么状态」——loader 条目数、active/disabled、
   动态包数、装配单元卡数；
2. **status 渲染器至今无真实卡**（C4 只有纯函数测）——本卡让 §4.1 status 档端到端落地；
3. 只读、零新宿主后端（`loader`/`dynamicPlugins` 投影现成）、失败面简单——最低风险高样板。

## 2. 决策回执（自主过闸，可回退）

| # | 开放点 | 默认值 |
|---|---|---|
| 1 | 数据面 | `host_services.get("loader")` + `get("dynamicPlugins")`，单元内聚合（跨服务组合是单元逻辑，宿主零改动） |
| 2 | 条目 | `{label:"loader 条目",…}`、`{label:"fiber 活跃",…,tone:ok/idle}`、`{label:"禁用",…,tone: disabled>0 ? warn : ok}`、`{label:"动态包",…}` |
| 3 | 任一服务失败 | 整体 fail-loud（`ok:false` 透传）——**不部分伪造**（缺一条腿的状态卡比诚实报错危险） |
| 4 | type/尺寸 | `runtime`；status 契约默认 2×2（不写 size） |
| 5 | 命名/位置 | `wasm-plugins/panel-runtime-status/`，cardId `panel-runtime-status.status`，namespace 同名 |

## 3. 验收判据

| # | 判据 |
|---|---|
| S1 | v2 status 卡契约：`view.kind:"status"`、`dataRpc` 显式、无 items 硬编码（数据面驱动，静态兜底可缺省） |
| S2 | `static ui.json == describeUI`（一份契约） |
| S3 | status 聚合正确：条目/active/disabled/动态包计数来自两服务；tone 规则（disabled>0→warn） |
| S4 | 任一服务失败 → `ok:false` 透传，value 不夹带伪造 items |
| S5 | 未知端点 fail-loud；scan 自动挂载 + 清单出第三卡（宿主零改动） |
| S6 | 回归：全套 0 新增失败、clippy 0、node 16/16 不变 |

## 4. 边界
不新增投影器服务 · 不做图表/时间序列（chart 契约预留未点亮）· 不改渲染器 ·
不推翻 harness 前端（本卡与旧前端并存是迁移期常态）。

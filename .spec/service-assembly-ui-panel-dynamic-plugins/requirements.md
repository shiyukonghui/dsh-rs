# 需求结论：面板改写 #3 —— 动态插件清单卡（panel-dynamic-plugins）

日期：2026-09-05 | 阶段：需求分析 | 关卡：自主过闸 | 决策记录 D-188。
改写型第三次复制（C4/D-185 定型，#2/D-187 验证可批量）；台账 `.spec/service-assembly-ui-panels/progress.md`。

## 1. 目标与选型（第一性）

harness「动态插件面板」= dynamicCordisRunner 定义/运行态的列表视图。宿主投影器
`dynamicPlugins` 服务现成（remote_host.rs 实证：`{ok, plugins:[{pluginId, packages[],
currentPackageId, activeRun?/latestRun?}]}`）→ **list 卡、只读、零新宿主后端**。
与 #1（loader 静态装配）互补：#1 看"装配了什么"，#3 看"动态定义/在跑什么"。

## 2. 决策回执（自主过闸，可回退）

| # | 开放点 | 默认值 |
|---|---|---|
| 1 | 行投影 | `{pluginId, name(=currentPackageId 对应包名), state}`；state = activeRun 存在→`running` 否则 `defined`（行语义归单元，双权威禁令） |
| 2 | 行动作 | v1 **只读**——stop/undefine 属宿主 `set` 面（dynamicStop/dynamicUndefine 服务存在，但写动作卡需先定"卡内确认"形态，留后续面板） |
| 3 | type/尺寸 | `runtime`；4×4（list 默认档） |
| 4 | 失败面 | 服务失败透传 `{ok:false}`，不伪造空表（承 m33/m34 纪律） |

## 3. 验收判据

S1 v2 list 卡契约（rowsPath 显式/dataRpc 显式/columns 齐/size 无坐标/type 闭集）；
S2 一份契约（静态==describeUI）；S3 行投影（running/defined、name 取当前包）；
S4 服务失败 fail-loud 无 items；S5 未知端点 fail-loud；S6 scan 挂载 + 清单第四卡；
S7 回归 0 劣化 + clippy 0。

## 4. 边界
不做写动作（stop/undefine/define）——需要"卡内动作确认"的渲染形态先行走契约/设计；
不动 dynamicCordisRunner 既有 RPC 面；不做 SSE 之外的增量拉取。

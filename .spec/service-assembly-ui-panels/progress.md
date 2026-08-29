# 面板改写进度台账（目标：前端全部由服务单元组成，不再使用 deepseek 前端）

模式（C4/D-185 定型，D-186 闭环热插拔）：每个面板 = 一个服务装配单元包
（wit 复用 host-remote 接口身份 + `describeUI` 与静态 ui.json 一份契约 + 数据面
端点自持逻辑）→ `scan_remote_units` 发现挂载 → 清单/桌布零改动自动出卡。
宿主数据面优先复用 `remote_host.rs` 投影器现成服务（loader / dynamicPlugins /
kv / sessionMessages / workspaceFiles / time / …），避免宿主扩张。

| # | 面板 | 包 | 视图 | 数据面 | 状态 |
|---|---|---|---|---|---|
| 0 | Provider 设置（试点） | `wasm-plugins/llm-deepseek` | form | kv 读写 + 模型目录 | ✅ D-180/D-182 |
| 1 | 插件清单 | `wasm-plugins/panel-plugin-inventory` | list | loader 投影行 | ✅ D-185（m33） |
| 2 | 运行时状态 | `wasm-plugins/panel-runtime-status` | status | loader+dynamicPlugins 聚合 | ✅ D-187（m34） |
| 3 | 动态插件 | `wasm-plugins/panel-dynamic-plugins` | list + **rowActions(stop/undefine, confirm)** | dynamicPlugins 投影行（running/defined）；**首张写能力卡（C6/D-189）** | ✅ D-188/D-189（m35 10 测） |
| 4 | 工作区文件 | `wasm-plugins/panel-workspace-files` | list | 两段式：agentWorkspace 解析→workspaceFiles 列举（失败零枚举，不猜目录）；**resource 分类首卡** | ✅ D-190（m36） |
| 5 | 会话清单 | `wasm-plugins/panel-sessions` | list | sessionCandidates 行零加工（epoch 原样）；**session 分类首卡**，发现端先行（打开/切换与 chat 同题留契约演进） | ✅ D-191（m37） |
| — | 会话打开/切换 | — | chat（预留） | 需「卡级选择/跳转」交互形态 + chat 渲染器点亮（远景关键路径，独立契约流程） | ⬜ 契约演进 |
| 6 | 设置概览 | `wasm-plugins/panel-settings` | list | 宿主新投影 arm `settingsDescribe`（与原生 describe 同形状共用 namespace_view，redact 在源头）；行拍平 {ns,field,value}；**config 分类首卡** | ✅ D-192（m38 + 宿主 2 测） |
| C8 | 聊天 | `wasm-plugins/panel-chat`（声明单元） | chat（**全链路落地**） | 三 RPC 面复用宿主既表面；折叠同源；**SSE 直订已接（C8-3b，仅订 events.mux）** | ✅ D-193 C8-1..4+3b（m39 3/3，清单第八卡） |
| S | 设置编辑 | `wasm-plugins/panel-settings-edit`（声明单元） | form + `fieldsFrom` 动态投影 + **nsSelect 下拉** | describe/update 经 canonical 别名复用宿主既表面；expectedRevision 乐观锁；secrets 仅存在性；嵌套只读；**D-201 一卡通用全部 ns** | ✅ D-194 S1..S4 + **D-201**（m40 3/3） |
| 10 | 调度任务 | `wasm-plugins/panel-schedule`（声明单元） | list | 宿主薄臂 `schedule/list`（Boot 挂载与 M4 工具同一 ScheduleHost，fold 事件日志权威；缺宿主诚实报错）；写端另立切片 | ✅ D-195（m41 3/3 + 宿主测） |
| 11 | 创建调度 | `wasm-plugins/panel-schedule-create`（声明单元） | form | 保存走 `schedule/create` 臂（D-196 wire 形钉死）；rowActions 删除带 confirm | ✅ D-197（m42 3/3，调度建/看/删闭环） |
| 12 | 待审批 | `wasm-plugins/panel-approval`（声明单元） | list + rowActions | `approval/pending` 臂（wire.pending_requests 单权威）；允许/拒绝同臂 args.decision 区分（D-198 扩展），拒绝 confirm | ✅ D-199（m43 3/3，**技术队列清零**） |
| 13 | 设置编辑 · locale | `wasm-plugins/panel-locale-edit`（声明单元） | form + fieldsFrom | panel-settings-edit 逐字节同构仅换 pick=locale（「机械复制」实证）；define/activate 取证后**重分级非机械**（vendored 面无源码） | ✅ D-200（m44 3/3） |
| … | 审批… | — | 契约演进：审批交互形态 | — | ⬜ 契约演进后迁移 |

## 迁移完成的判定（远景）
「前端全部由服务单元组成」= harness 面板逐块迁移到桌布卡片直至旧前端可下线；
其中聊天（chat 视图）依赖契约预留渲染器的点亮——届时按同流程先做契约设计再实现。
每块迁移走独立的需求→设计→TDD→验收闭环；本台账只记进度与选型理由。

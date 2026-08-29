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
| 3 | 动态插件 | `wasm-plugins/panel-dynamic-plugins` | list | dynamicPlugins 投影行（running/defined）；写动作（stop/undefine）待"卡内确认"渲染形态 | ✅ D-188（m35） |
| 4 | 会话概览 | `panel-sessions` | list | sessionMessages/sessionIdentity（payload 需 sessionId，需评估卡级选择形态） | ⬜ 候选 |
| 5 | 工作区文件 | `panel-workspace-files` | list | workspaceFiles | ⬜ 候选 |
| … | 设置/调度/任务/聊天… | — | form/list/chat（chat 渲染器属契约预留，点亮前无法迁移聊天面板） | — | ⬜ 路线图 |

## 迁移完成的判定（远景）
「前端全部由服务单元组成」= harness 面板逐块迁移到桌布卡片直至旧前端可下线；
其中聊天（chat 视图）依赖契约预留渲染器的点亮——届时按同流程先做契约设计再实现。
每块迁移走独立的需求→设计→TDD→验收闭环；本台账只记进度与选型理由。

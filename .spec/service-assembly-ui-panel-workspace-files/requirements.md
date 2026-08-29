# 需求结论：面板改写 #4 —— 工作区文件清单卡（panel-workspace-files）

日期：2026-09-05 | 阶段：需求分析 | 关卡：自主过闸 | 决策记录 D-190。
改写型第四次复制；宿主服务实证：`agentWorkspace → {ok,cwd}`、`workspaceFiles
→ {cwd,query} → {ok,paths}`（remote_host.rs:344-368）。

## 1. 目标与选型（第一性）

harness 的 workspace/文件面板对应 D-181 分类表 `resource`（fs、凭据、工作区）——本卡
是该分类的**首卡**（此前 model/runtime 已有，侧栏缺 resource 组）。数据面现成、只读、
零新宿主后端。

## 2. 决策回执（自主过闸，可回退）

| # | 开放点 | 默认值 |
|---|---|---|
| 1 | 数据面 | 两段式：`agentWorkspace` 解析默认工作区 → `workspaceFiles{cwd,query:""}` 列举 |
| 2 | 诚实失败 | **解析失败/空 cwd → fail-loud 且不得触达枚举服务**（绝不猜目录）；枚举失败透传 |
| 3 | 行投影 | `{path}` 单列（全路径直出，不发明 basename 语义——展示加工是渲染器/未来的事） |
| 4 | type/尺寸 | `resource`（分类表语义：fs/工作区）；4×4 |
| 5 | 空态 | 「工作区没有文件」（与「不可读=错误态」严格分开） |

## 3. 验收判据

S1 v2 list 契约 + type resource；S2 一份契约；S3 行投影 + **调用序 = 探测序**
（agentWorkspace→workspaceFiles）；S4 解析失败 fail-loud 且**零枚举调用**（调用记录断言）；
S5 枚举失败透传；S6 未知端点 fail-loud；S7 scan 挂载 + 清单第五卡；S8 回归 0 劣化 + clippy 0。

## 4. 边界
只读（文件操作/预览属未来 rowActions+详情形态）· 不做递归树（顶层列举）· 不做搜索框
（query 契约在宿主，卡级输入控件未定形——留渲染契约演进）。

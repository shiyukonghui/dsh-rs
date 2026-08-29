# 需求结论：面板改写 #5 —— 会话清单卡（panel-sessions）

日期：2026-09-05 | 阶段：需求分析 | 关卡：自主过闸 | 决策记录 D-191。
改写型第五次复制；宿主服务实证：`sessionCandidates → {ok, candidates:[{sessionId,
label, createdAt}]}`（remote_host.rs:330-342，零 payload，sessionReferenceResolver 同源）。

## 1. 目标与选型（第一性）

harness 会话面板的**发现端**（有哪些会话）投影现成且零 payload——此前因「sessionMessages
需 sessionId」被搁置的会话面板，其发现半边其实可先行落地为只读卡（D-188 记录的
「卡级选择形态」欠账只挡打开/切换，不挡列举）。`session` 语义位（D-181 分类表）至此
首卡落地，侧栏四分类（model/runtime/resource/session）齐。

## 2. 决策回执（自主过闸，可回退）

| # | 开放点 | 默认值 |
|---|---|---|
| 1 | 行投影 | **零加工直传** `{sessionId,label,createdAt}`（epoch ms 不格式化——时间展示属渲染器演进，双权威禁令） |
| 2 | 展示列 | 两列（会话 / 创建 epoch ms）；label 现与 id 重复，列先不出（数据保留在行里） |
| 3 | 打开/切换 | **不做**（需「卡级选择/跳转」交互形态，留渲染契约演进；与 chat 渲染器同题） |
| 4 | type/尺寸 | `session`；4×4 |

## 3. 验收判据
S1 v2 list 契约 + type session；S2 一份契约；S3 行零加工 + 单服务探测（调用计数）；
S4 服务失败 fail-loud 无 items；S5 未知端点 fail-loud；S6 scan 挂载 + 清单第六卡；
S7 回归 0 劣化 + clippy 0。

## 4. 边界
不做打开/切换/删除会话 · 不做消息预览（sessionMessages 需 sessionId 逐会话拉取，
属未来详情形态）· 不做标题推断（label=id 是宿主投影事实，单元不改写）。

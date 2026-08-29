# 需求结论（种子）：调度/任务面板（panel-schedule）——旧前端下线清单 §2 最大缺口

日期：2026-09-05 | 阶段：需求分析（种子；带侦查项）| 决策记录待实现轮立 **D-195**。
E2E 清单指认的最大未迁移项。已知既有面（历史轮源码旁证，**下轮开工先逐项取证**）：
原生 RPC 家族 schedule create/list（kind=after|at|every，键集强制：after={id,kind,prompt,afterSeconds,scheduledAt}…）；
事件 `schedule/change`（SessionHost append）；投影器**无** schedule arm（需照 D-192 受测扩展型新加）。

## 决策回执（默认值，可回退）
| # | 开放点 | 默认 |
|---|---|---|
| 1 | 读端 | RemoteHost 新投影 arm `scheduleList`（照 D-192 settingsDescribe 受测模式：形状一致 + 缺依赖诚实报错 + 伪造空表探针）→ `panel-schedule` list 卡（type runtime）：{id, kind, prompt(截), 计划时间} |
| 2 | 写端 v1 | **只读**（create/delete 表单与 rowActions 确认后作 S 系列后的独立切片；先立读端） |
| 3 | 实时 | 复用 events.mux `schedule/change` 帧触发列表重拉（chat 归一型接线，帧形状先取证再订） |

## 待取证项（开工第一件事，一轮 grep/读即清）
1. schedule 状态宿主存储位置（Boot 字段/注册表）与 list 的既有点号 RPC 名（"schedule.list"? 行形状字段名）。
2. `schedule/change` 帧 payload（mux 帧是否含 sessionId 维度）。
3. 旧前端 schedule 面板实际消费的 RPC 面（对齐用，防遮蔽教训重演——先 grep `"schedule.` 全表面）。

## 验收判据
照 D-192 型四件套：宿主 arm 2 测 + 探针；mNN list 契约 + 失败透传；清单第十卡；回归 0 劣化。
完成后更新 e2e-offline-checklist §2（划掉"调度未迁移"）。

## 边界
v1 只读 · 不做 cron 表达式（宿主仅 after/at/every）· 不做任务输出流（属 jobs 详情域，另题）。

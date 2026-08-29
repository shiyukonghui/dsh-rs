# 需求结论（种子）：审批交互卡（panel-approval）——技术队列最后一项

日期：2026-09-05 | 阶段：需求分析（种子；带侦查项）| 决策记录待实现轮立 **D-198**。
桌面 11 卡生态中唯一未迁移的 harness 交互域。已有旁证（历史轮源码旁证，**开工先逐项取证**）：
长 RPC 名单含 `session.approval.decide`；events.mux 上有 **approval wire 增量帧**
（requested/resolved，append-only 游标，仅 mux——R19 取证注释）；`ApprovalWireRef`
挂 `stream_sse_events`；plan-mode/approval policy 是会话事件（`approval/asked`、
`approval/decided`、`approval/policy` EventKind 规范串在 dsh-session types 实证过）。

## 决策回执（默认值，可回退）
| # | 开放点 | 默认 |
|---|---|---|
| 1 | 形态 | **status 型实时卡**（panel-runtime-status 型）：当前待决审批列表（pending 项 + 会话 + 时间）；批准/拒绝 = rowActions（confirm:true）指 `session.approval.decide` 薄臂（若已有点号面则复用别名，**遮蔽教训先行 grep `"session.approval` 全表面**） |
| 2 | 数据面 | 新投影/薄臂 `approvalPending`：从 approval wire（append-only log）折叠 requested−resolved（与 fold_schedule 同构，单一权威=事件/wire 日志） |
| 3 | 实时 | events.mux approval 帧触发重拉（chat 归一型接线，帧形状先取证） |
| 4 | 归属 | 若 decide 是长 RPC（需 &mut Boot）→ 原生薄臂（session/prompt 先例）；单元只拥有声明（D-193-B 第九次复制） |

## 待取证项（开工第一步）
1. `session.approval.decide` 现签名与 payload 形（long-RPC 分支内）；批准语义的 id 键名。
2. approval wire 帧 payload 形状（requested/resolved 帧的字段）与 pending 折叠可行性
   （wire 是否保留 resolved 对应 id）。
3. 旧前端审批面板消费面全表面（防遮蔽重演）。

## 验收判据
照 D-195 型：宿主臂 2 测（含缺依赖诚实）+ roundtrip（ask→pending 可见→decide→resolved）；
m43 声明单元三测；清单第十二卡；回归 0 劣化。完成后 E2E 清单 §2 划账。

## 边界
v1 只处理 ask/decide 二元（不渲染 reason 富文本）· 不做批量批准 · 不做策略编辑
（approval/policy 属设置域，可后补 select 字段）。

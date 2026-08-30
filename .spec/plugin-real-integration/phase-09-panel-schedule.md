# 阶段 9 · panel-schedule（调度任务·列表+删除）— ✅ 通过

- 功能清单：列表卡；dataRpc `schedule/list`（宿主特判臂）+ rowAction 删除
  （confirm，scope=row → `schedule/delete` 臂，行身份=row.id）。
- 种子（RPC，非浏览器面）：`schedule/create` afterSeconds=600 →
  `{id:"schedule-1", prompt:"E2E-DEL-1788095565.26747"}`（远端时刻防测试期间触发）。
- 浏览器实测（verify-action-delete.mjs，**2/2 PASS**，console 零错）：
  1. 行真实渲染：`schedule-1 | after | E2E-DEL-1788095565.26747 |
     2026-08-30T05:22:45.386Z | 删除`（id/类型/提示/触发时刻全列）；
  2. 点「删除」→ **confirm 弹窗真实出现并应答**（dialog 日志 `确认「删除」？`）
     → 重载行消失 + 卡空态「没有调度记录」→ RPC 面复验 items=[]。
- 判定：调度清单的查看与撤销（删除）在浏览器真实发挥作用。
- 基建：verify-action-delete.mjs（--title/--marker/--btn 参数化，confirm 自动应答）。

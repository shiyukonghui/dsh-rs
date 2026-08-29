# 验收结论：桌布 C6 —— rowActions 渲染 + `confirm` 契约收敛（首张写能力卡）

日期：2026-09-05 | 关卡：自主过闸 | 决策记录 **D-189** | git：本提交（上游 `ffd7e20`）。

## 逐条验收

| # | 判据 | 证据 | 结果 |
|---|---|---|---|
| S1 | 线形状 `{row}` 完整原样；`needsConfirm` 只认严格 true | node `rowActionBody wraps the full row untouched` + `needsConfirm only strict true`（**反向桩探针被抓**：伪造 `{}` / 恒 true 均红） | ✅ |
| S2 | rowActions 非法形态 → view-malformed | node `validateDeclaration rejects malformed rowActions`（**禁用校验块探针被抓红**后还原） | ✅ |
| S3 | stop/undefine：坏 body fail-loud 且**不触达宿主服务**；成功/失败透传 | m35 `stop_requires_row_plugin_id_fail_loud`（五类坏 body + set 调用数 = 0）、`stop_passthrough_success`（payload 只带 pluginId）、`stop_service_failure_passthrough`、`undefine_passthrough_and_validation` | ✅ |
| S4 | 一份契约（ui.json 含 rowActions == describeUI） | m35 `static_ui_json_matches_describe_ui`（既有）+ `declaration_carries_confirm_row_actions`（先红：无端点时代 FAILED） | ✅ |
| S5 | 渲染：操作列 + confirm 未确认不发 RPC + 成功刷新 | app.js `act()`（DOM 层，见诚实台账 2）；纯函数面 S1 钉死 | ✅(代码走查) |
| S6 | 回归 | dsh-cli **251/0**、dsh-wasmrt 全绿（m35 **10/10**）、clippy **0**、node **19/19** | ✅ |

## TDD / 红验证记录
- m35 新 5 测：先对无端点单元红（4 FAILED + 声明测红）→ 实现转绿。
- node C6 三测的红被并行编辑污染（竞速读到已实现版本）→ **反向桩探针补正**：
  `rowActionBody→{}`、`needsConfirm→恒 true` 被抓红；`if(false)` 禁用 validate 块被抓红；全部还原后 19/19。

## 诚实台账
1. `window.confirm` 是 v1 最小确认形态（阻断式）；富确认 UI 属渲染演进。
2. 操作列渲染/confirm 拦截/成功后刷新属 DOM 粘合层（无自动化基建），纯函数契约（S1）已钉死其输入面；浏览器端到端手测未执行。
3. 动作幂等/并发（连点）未防护——宿主 dynamicStop 对未运行返回错误即诚实可见；卡级节流留后续。
4. `confirm` 字段是**新增可选**：旧声明零影响（既有 form 卡动作行为逐位不变）。

## 意义
写能力卡闭环：声明（rowActions+confirm）→ 渲染（确认+发出）→ 线形状（{row}）→
单元自校验 → 宿主 set 面透传 → 状态反馈+刷新。管理型面板（任务/调度/会话操作）自此有型。

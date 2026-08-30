# 阶段 12 · panel-approval（待审批）— ✅ 通过（R4 最真实形态：真 LLM 真工具调用）

- 功能清单：list 卡；dataRpc `approval/pending`（wire 未决投影）+ rowActions
  允许(allowedOnce)/拒绝(rejected+confirm) → `session.approval/decide`（worker 长 RPC）。
- 真 pending 注入源（R4）：**真 LLM**（用户凭据，adapter=deepseek，
  `DSH_LLM_EFFORT=low` 兼容端点）+ plan 激活 + 提示「调用 bash 执行 echo <marker>」
  → mutation 门拦下 → wire.requested（asked 事件 + pending 条目 + reason 全文）。
- 浏览器全链（verify-approval.mjs，**5/5 PASS**，console 零错）：
  plan-on → 提示→pending→**重载后行真实渲染（bash+reason）**→点「允许」→
  `approval/decided(allowedOnce)` + **`tool/result` 含 E2E-APPR-ALLOW marker
  （bash 真执行铁证）**→ 卡「没有待审批项」空态 → 二轮提示→重载→行在→点「拒绝」
  →**confirm 弹窗真实出现并应答**→ `approval/decided(rejected)` → plan 复原 off。
- **揪出并修复真缺陷（D-220）**：worker 长 RPC 臂（dispatch_long_rpc 的
  session.approval.decide 拷贝）**只认平铺 `{toolCallId}`，不吃画布行形
  `{args:{row,decision}}`**——卡面 allow/reject 全部 invalid-args（D-198 只修了
  accept 短臂，worker 化迁移时行形支持没跟上）。修复=与短臂同形解包+row 回退；
  回归测试 `worker_decide_arm_accepts_canvas_row_shape`（272/272）。
- 附带（D-219）：`DSH_LLM_EFFORT=off|low|high|max` env 旋钮（仿 key env-only 先例），
  解决非原厂端点拒收默认 effort=high 的 HTTP 400（实测 low 通过）。
- 过程诚实记录：v1 脚本两处误报（marker 撞用户消息散文、"rejected" 撞模型散文）
  + 卡列表=打开时快照不自刷新需重载取行——v2 断言只认类型化事件
  （approval/decided、tool/result）后证据净。

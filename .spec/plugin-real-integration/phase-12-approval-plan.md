# 阶段 12 · panel-approval 勘察结论（**已执行完毕，见 phase-12-panel-approval.md**）

结果速览：蓝图全部落地，全链 5/5 PASS（真 LLM 真工具调用）。执行中揪出并修复
一个真缺陷 **D-220**：worker 长 RPC 臂 `session.approval.decide` 只认平铺
`toolCallId`，不吃画布 rowAction 行形 `{args:{row:{toolCallId},decision}}`
（D-198 的 row 形支持只加在 accept 短臂上，worker 化那份拷贝漏了）——
即**卡面 allow/reject 此前必失败**（invalid-args），已补齐并加回归测试
`worker_decide_arm_accepts_canvas_row_shape`。另立 **D-219**：`DSH_LLM_EFFORT`
启动期 env 旋钮（缺省原行为），因非原厂兼容端点拒绝适配器缺省的 `effort=high`。

## 真 pending 的唯一生产链（已核实）
1. **plan mode 激活**：RPC `session.plan.mode`（web.rs:927 → set_plan_mode_on）
   或 /plan 命令 → 会话落 `plan/mode{active:true}`；
2. **LLM 发出 mutation 工具调用**：`web/approval.rs` `mutation_tool_set()`
   （bash/write/edit/job_kill… 列表在其 30-46 行）命中 → 审批门
   （is_mutation 且 plan 折叠 active 才拦，approval.rs:52-55 注释钉死）；
3. **挂起+投影**：`ApprovalAsked` 审计事件 + `wire.push_requested`
   （approval.rs:126，生产唯一入口）→ `approval/pending` RPC 出条目 +
   mux 下推 `approval/requested` 帧；
4. **卡面决策**：待审批卡 actions allow/reject → `session.approval/decide`
   （canonical→session.approval.decide 臂）→ `approval_respond` →
   allowedOnce 执行工具 / rejected 合成拒绝 + `approval/decided` 事件。

## 唯一前置缺口（与阶段 13 共用解法）——**解法已查实**
echo provider 不发 tool call → 必须真 LLM 起 serve。装配链：
`server_llm_runtime(base_url, model)`（m6_llm.rs:104）注册 **`deepseek` adapter**，
**key 只从环境变量 `DEEPSEEK_API_KEY` 读取**（`DEEPSEEK_API_KEY_ENV`）。
故 serve 新配方（凭据见 `target/verify-secrets.env`）：
```
$env:DEEPSEEK_API_KEY = "<verify-secrets.env 里的 key>"
target\debug\dsh.exe web scenarios\web-smoke.cordis.yml --port 60890 `
  --agent-loop --service-units --dynamic-plugins-dir target\web\dynamic-plugins `
  --llm-base-url http://100.105.152.101:18080/v1 --llm-model qwen3.8-flash-next
```
注意：provider 名固定 `deepseek`（adapter 注册 `["deepseek"]`），故 settings 的
`llm.provider` 需为 deepseek 才走真外呼（当前冒烟=dsh/echo）——先经
`settings/update` 臂切 provider（阶段 6 已验证该臂真写），或直接确认
serve 的 --llm-* 覆盖优先于 settings（下轮实测为准）。

## 浏览器验收脚本设计（verify-approval.mjs）
1. RPC `session.plan.mode`{active:true} 开 plan（或 /plan 命令走 UI）；
2. session/prompt 发「用 bash 执行 echo E2E-APPROVE-<ts>」；
3. 轮询待审批卡：出现 `bash` 行（callId/reason 上屏）；
4. 点「允许」→ 断言卡行消失 + 会话日志 `approval/decided`+allowed +
   bash 真执行结果进轮次（聊天可见 E2E-APPROVE）；
5. 再发一条 mutation 提示 → 点「拒绝」→ 断言 decided+rejected 合成拒绝可见；
6. 复原 plan off。console 零错误。

## 风险备案
- 若真 LLM 端点不支持 tool schema（qwen3.8-flash-next 未必走 OpenAI tools
  协议）→ 降级：如实记录「pending 链在宿主侧全真（单元测 5921/10413 +
  wire/decide 臂在场）但端到端触发受 LLM 能力限」，并向用户确认替代触发源。
- plan mode 开启会改会话投影（聊天卡出现折叠段）——验证后必须关回（回滚纪律）。

# 阶段 11 · panel-chat（聊天）— ✅ 通过（深化验证）

- 功能清单：kind=chat 专用卡；historyRpc `session/history` + sendRpc
  `session/prompt`（长 RPC worker）+ cancelRpc `session/cancel`；宿主特判臂三件套。
- 浏览器实测（verify-chat.mjs，**4/4 PASS**，console 零错）：
  1. **历史回放跨阶段互证**：阶段 10 真触发轮次
     （`E2E-FIRE-R10B-1788096162.99231`，D-218 注入）在聊天卡历史中真实重现；
  2. 发送→**乐观泡即时可见**（120ms 内 M2 上屏，点发送真走 worker）；
  3. **echo 回应活折**：`助手:` + `✓ 已发送` 状态流转在流中出现；
  4. **cancel 状态机实证**：紧随发送的 1.5s 观察窗内**捕获到「停止」按钮
     并点击**（轮次执行中态真实存在）→ 卡存活、输入面完好、M3 仍在流、
     console 零错误（取消路径不炸卡）。
- 如实记录（非本目标范围）：历史里触发轮次显示 framing 原文
  （`reminder_prompt_json: "…"`）——framing 包裹格式直出聊天文本，属未来
  文案美化（新能力不做），功能语义（提示真执行+可见）成立。
- 与审计 T6/T11 关系：T6/T11=系统级冒烟，本阶段=插件级四端点全覆盖。

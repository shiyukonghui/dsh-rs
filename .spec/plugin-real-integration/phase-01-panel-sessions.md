# 阶段 1 · panel-sessions（会话清单）— ✅ 通过

- 功能清单：只读卡；dataRpc `panel-sessions/list` → 自有载体 → host service
  `sessionCandidates`（RemoteHost 投影臂，real 会话枚举）。
- 静态链路：四层全在场（矩阵 §逐单元；无缺口，无需对接改动）。
- 浏览器实测（verify-plugin.mjs，新鲜 profile 新页）：
  `{"cards":13,"found":true,"expectHits":[{"x":"default","hit":true}],"rows":6,
  "textSlice":"…会话\t创建 (epoch ms)\ndefault\t1788063573185","consoleErrs":[],"pass":true}`
- 判定：真实会话态（default + 真实 epoch）在浏览器渲染 = **功能真实发挥作用**。
- 动作面：无（只读卡）。提交：本阶段=基建（校验器）+ 首插件验证。

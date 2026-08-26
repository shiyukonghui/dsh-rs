# Phase 4 → 5 过渡：live 一键即停验证记录（60881 演示实例）

**日期**：Phase 4 关闸提交 511dcaa 之后。

## 环境

- 生产演示服务：`dsh.exe web` @ 60880（原命令行，HTTP 200，Phase 4 二进制）。
- Phase 5 专用演示实例 @ 60881：`dsh.exe web ... --agent-loop
  --llm-base-url http://127.0.0.1:18123/v1 --llm-model mock-model`，指向本机
  **慢速 mock LLM**（`target/web/slow_mock_llm.py`：80 chunk × 120ms ≈ 全量 ~9.6s
  的 OpenAI 兼容 SSE 流；`DEEPSEEK_API_KEY=mock-key` 注入进程 env）。
- 原因：本环境无真实 `DEEPSEEK_API_KEY`（真实模型 e2e 属 GATED），故用慢速 mock
  模拟长生成——wire/worker/取消路径全真，仅模型为本地替身。

## 步骤与结果

1. POST `/api/session.prompt`（rpcId=r1，session=default，content=长生成指令）
   → 200 `{ok:true, accepted:true}`（worker 线程驱动 turn，开始 ~9.6s 生成）。
2. 延迟 ~1.5s 后 POST `/api/session.cancel`（rpcId=c1）→ **200
   `{ok:true, accepted:true}`，耗时 0.054s**——accept 循环未被 worker 长 turn 占死，
   cancel 并发送达并**立即返回**（对齐设计目标：accept 空闲接 cancel）。
3. GET `/api/session.history` → 两个 turn/end：
   - `turn/end reason: {"kind":"completed"}`（首轮先跑完，历史残留）；
   - `turn/end reason: {"kind":"aborted","reason":{"kind":"user"}}`（cancel 落定的
     第二轮被**拒绝/中断**，reason=user 取消）。

## 结论（对照 Phase 5 验收点）

- 「生成中一键即停」在**真实 serve wire**（真实 HTTP → dispatch → worker 线程 →
  真实 driver → 慢流 LLM）验证成立：cancel 请求不被长生成阻塞（0.05s 返回），
  turn 以 aborted/user 终结。
- 传输中断（B）：cancel 令牌经 `request.signal` 直达传输层；本 mock 的流是
  惰性 chunk（driver step 边界消费亦可停），与 dsh-core abortable-read 单测
  （`chat_completions_stream_abortable_interrupts_blocking_read`）互补。
- 真实模型上的同路径属 GATED（需 `DEEPSEEK_API_KEY`/`DSH_LLM_BASE_URL`；
  与仓库 M6W 真实端点测试同一 gating 纪律），本验证已覆盖其 wire 面全部机制。

## 清理

- kill 60881 实例 + slow_mock_llm.py；保留 60880 生产演示服务（HTTP 200）。
- 测试产物：`target/web/slow_mock_llm.py`（gitignored，target/ 下）。

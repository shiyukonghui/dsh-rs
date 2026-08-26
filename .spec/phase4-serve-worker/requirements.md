# D-115 Phase 4（serve worker 化）需求结论

**日期**：本会话（Phase 3 已关闸提交 c17849c）

## 1. 目标

在已完成 Phase 1-3（请求面全部 `Arc`/`Mutex`/`Atomic` + `Send + Sync`）之上，把
serve 的长 RPC 移到 **worker 线程**执行，使 accept 循环不再被长 turn 占死：

- 长 RPC：`session.prompt`、`agent.run|loop|turn`、`commands/execute` 的
  `/plan <msg>` steer、审批 `decide` kick 的恢复回合（D-115 Phase 0 设计 §3 所列）。
- 效果：长生成中 accept 线程空闲，可并发送达 `session.cancel` → **真一键即停**
  （driver 在 step 边界轮询共享取消令牌）。每 driver 仍单 turn（相位机连续）；
  跨会话 turn 可并行（扩展点，不在本阶段强制）。
- HTTP 同步契约不变：客户端仍等待结果（Result 经 channel 回填 accept 线程后
  响应），只是长 turn 不再阻塞 accept 循环。

## 2. 非目标

- 不引入 full async（tokio）——D-004/D-006 维持既判否决。
- 不拆细锁 / 读写锁——Phase 1-3 粗锁决定不变（worker 化只要 Send+Sync）。
- 不改 M1 WASM loop 路径（`run_turn`/SessionLog adopt）——只动 Rust AgentLoopHost
  装配的路径。
- 不做每会话多 worker / 任务队列框架；最小 = 长 RPC 上独立 worker 线程。

## 3. 假设与约束（自上而下）

- 请求面三库（dsh-session / dsh-agent / dsh-agent-loop / dsh-llm / dsh-tools /
  dsh-system-prompt / dsh-scope / dsh-llm-deepseek）**已全部 Send**（Phase 1-3 完成）。
- `ReactLoopAgent` / `AgentLoopHost` / `LoopDeps` / `ToolRegistry` / `LlmRuntime` /
  `SystemPrompt` 句柄均为 `Arc`，可安全送进 worker 线程。
- `run_rust_loop(boot, sid, content)` 当前同步驱动整轮 turn——worker 化后仍同步
  语义（Result 经 channel），只是执行线程换到 worker。
- SEP 观察者/下链（EventSink `Arc<Mutex<Vec>>`、host_events、SSE/WS 线程）已
  Send，与 worker 写入不冲突（锁序 `session.data → 叶子` 无环，Phase 0 审计结论
  延续）。

## 4. 自下而上发现（本阶段关键约束，改变原库存）

D-115 Phase 0 库存把 M5 宿主值视为「已并发面不改」（tick schedule/bash_jobs 用
Arc）。但 **Phase 3 落地时**为三个非 Send 底物引入了 `ThreadCell<T>` thread-local
桥（`dsh-cli/src/web/web_m5.rs`）：

- `dsh_jobs::registry::JobRegistry`（内含 `Box<dyn Fn>`：`now`/`on_cancel`/
  `read_output`/`producer`）—— !Send；
- `dsh_shell::types::ShellProcess`（内部 `Rc<RefCell<ShellProcessInner>>`，
  持 `SubprocessHandle` / JoinHandle）—— !Send；
- `dsh_terminal::registry::TerminalSessionService`（`Box<dyn TerminalBackend>` /
  `BackendProvider` / `OwnerLiveness = Box<dyn Fn>`）—— !Send。

`ThreadCell` 设计 = 状态放**创建线程**的 thread_local 池，`.with()` 跨线程调用即
panic。**这与本阶段目标直接冲突**：worker 线程驱动的 turn 会执行 `tool_exec` →
M5 工具执行器（terminal/bash/fs/jobs 等）→ 对 `ThreadCell` 宿主 `.with()` → 在
worker 线程上必然 panic。

**结论（第一性原理）**：worker 线程要能安全驱动整轮 turn（含工具执行），M5 底物
必须真正 `Send` —— `ThreadCell` 桥无法在 worker 下存活。因此 **Phase 4 前置工作 =
`dsh-jobs` / `dsh-shell` / `dsh-terminal` 三 crate 整体 Send 化**（与 Phase 3 把
dsh-system-prompt 逼进 Phase 3 同型：自下而上推翻「预算外」注记），随后移除
`ThreadCell` 桥、改用真实 `Arc+Mutex` 共享。

（激进替代「工具执行仍回主线程、仅 LLM 上 worker」被否决：把 turn 的 LLM 与工具
拆到两个线程 = 半协作泵（方案 I 变体），重入/同步点复杂化，且 driver 相位机要求
整轮连续——正是方案 II 要避免的。）

## 5. 边界（哪些路径 worker 化）

- **worker 化**（长 RPC）：`session.prompt`（agent-loop 装配时）、`agent.turn|
  run|loop|turn`、`commands/execute` 中 `/plan <message>` 的非空 steer、审批
  `decide` 的恢复 kick。
- **保持 accept 同步**（短操作）：`session.cancel`（写取消令牌，必须快速可达——
  这是 worker 化的目的）、`session.history`/投影、审批 `respond`/`requested`、
  settings/credentials/workspaces/presets/standings 读、tick 推进
  （`m5g_tick_once` 仍在主线程，schedule/bash_jobs 已是 Arc）。
- `session.prompt` 在「未装配 agent-loop」（M1 WASM 路径）时**仍同步**（短路径，
  adopt 语义不变）。

## 6. 验收标准（本阶段关闸）

1. `cargo test --workspace` 全绿 EXIT=0（在 Phase 3 191 套之上，本阶段新增
   dsh-jobs / dsh-shell / dsh-terminal Send 化后的既有测试全绿 + dsh-cli web 的
   worker 化测试新增/调整全绿）。
2. `cargo clippy --workspace --all-targets` EXIT=0 零告警。
3. 60880 演示服务以原命令行重启 → HTTP 200。
4. 生产方式验证点：长生成中 `session.cancel` **并发送达生效**（driver 在 step
   边界消费）——以 dsh-cli web 测试表达（worker + cancel 并发：长 mock 流 turn
   中注入 cancel，断言 turn 中止且 accept 未被占死）。
5. 决策日志追加「D-115（实施·Phase 4）」条目，git 提交（改动→提交→决策日志互查）。

## 7. 复盘：假设清单与待确认结论

- 假设 A（用户默认成立）：「生成中一键即停」的验收以「cancel 到达 turn 内生效 +
   服务不卡死」为准，而非「瞬时物理杀线程」——worker 化是**合作式取消**（step 边界
   轮询），长 LLM 阻塞读取期间仍可能延迟到流返回。结论：符合既有合作式设计
   （D-032），不承诺中断性抢占。
- 假设 B：M5 工具（terminal/bash/jobs）被 worker 线程调用是常态路径（turn 响应对
  话必走工具）。结论：确认真实（`register_m5_tools_with_host` 把执行器闭包捕获宿主）。
- 假设 C：`dsh-subprocess` 已 Send（其 `SubprocessHandle` 内部已是 JoinHandle/
   Arc<Mutex>；`win_job::Job` 已有 `unsafe impl Send + Sync`）。结论：已验证，无需
   改造。
- 常见错误提醒（不只对自己）：「把 LLM 单独上线程、工具留主线程」看似最小改动，
  实则是方案 I 半泵复发——相位机要求整轮连续，拆线程会引入回同步点与重入，违背
  D-115 选型理由。正确路径 = M5 底物整体 Send 化（一次成本），让整轮 turn 完整地在
  worker 里跑。

## 8. 已解决 / 待解决

- 已解决：三底物 Send 化清单（见 §4）；worker 化路径清单（见 §5）。
- 待解决（编码阶段 TDD 消化）：`ThreadCell` 移除后的 `Arc+Mutex` 接法、`ShellProcess`
  `Rc<RefCell>` → `Arc<Mutex>` 的具体锁点、`run_rust_loop` 的 worker 化签名、
  channel 结果回填与超时策略（等待期间主线程继续 recv？——见设计文档）。

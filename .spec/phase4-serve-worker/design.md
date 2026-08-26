# D-115 Phase 4（serve worker 化）设计决策

**日期**：本会话（Phase 3 关闸后，round 4）

## 1. 触发问题

serve 主循环（`serve()` 中 `recv_timeout` → `dispatch_request` 内联）把 `session.
prompt`/`agent.run` 同步驱动整轮 turn：turn 排空期间 accept 循环被占死，
`session.cancel`（以及任何其它 RPC / SSE）无法并发送达 → 「生成中一键即停」不可达
（HANDOFF 0.3 明示这是设计使然，等 D-115 完成）。Phase 1-3 已完成请求面 Send 化，
解锁 worker 线程执行长 RPC。

## 2. 设计目标（对齐 D-115 §3）

- 长 RPC（`session.prompt`[agent-loop 装配时]、`agent.run|loop|turn`、
  `commands/execute` 的 `/plan <msg>` steer、审批 `decide` kick 恢复回合）在
  **worker 线程**执行；`Result` 回填 accept 线程，**HTTP 同步契约不变**（客户端仍
  等待结果）。
- accept 循环空闲 → 可并发送达 `session.cancel`（短操作，写共享取消令牌）→ turn
  在 step 边界消费 → 真一键即停。
- **取消语义（用户拍板 B）**：要求**传输级中断**——研究结论（用户要求研究 TS 参考后
  再决策）：
  - TS 参考（`packages/llm/llm-deepseek/src/adapter.ts`）：`GenerateOptions.signal:
    AbortSignal` 贯穿 `prepareCall`/`resolveModel`/`stream`；`streamWithConnection`
    用 `AbortSignal.any([options.signal, consumer.signal])` 直插 `fetch(url,
    {signal})`——abort **主动 tear down 在途 HTTP 请求**，SSE 迭代随即抛
    `ABORTED`；并有 idle watchdog（`streamIdleTimeoutMs`）。
  - Rust 现状：`GenerateOptions` **无 signal 字段**；`LlmAdapter::stream` 返回同步
    `Box<dyn Iterator>`；`dsh_core::llm_http::chat_completions_stream` 是**整段
    阻塞读**（读到 Content-Length/close 才返回），`parse_sse` 一次性解析整缓冲。
    **→ 长生成中 cancel 即使 worker 化也只在整段读完后的 step 边界生效，不是真·即停。**
  - 结论：为对齐 TS，须把 Rust 传输改成**增量读 + 可中断**——cancel 信号传入
    `dsh-core llm_http` 读取循环（短读超时轮询 cancel 谓词），abort 主动断开阻塞读、
    适配器以 `Aborted` finish 归一。这是本阶段最大的范围扩张（由用户第二次提问确认）。
- 每 driver 单 turn（相位机连续）；跨会话并行是扩展点（本阶段只保证不破坏单 turn
  语义，不强做并行池）。

## 3. 关键约束（自下而上，见 requirements.md §4）

M5 底物 `dsh-jobs`/`dsh-shell`/`dsh-terminal` 非 Send，Phase 3 以 `ThreadCell`
thread-local 桥承载——`.with()` 仅创建线程可调用。worker 线程驱动 turn 时
`tool_exec` → M5 执行器会对 `ThreadCell` `.with()` → panic。

**结论**：三 crate 整体 Send 化 + 移除 `ThreadCell` 桥（改真实 `Arc+Mutex` 共享）。

## 4. 分项设计

### 4.1 `dsh-jobs` Send 化
- `JobRegistryConfig.now` / `ProducerHooks.on_cancel` / `read_output` /
  `StartSpec.producer` / `JobRecord.on_cancel`：`Box<dyn Fn>` → `Box<dyn Fn + Send>`
  （producer 是 `FnMut + Send`；now 是 `Fn + Send`——worker 线程会 invoke）。
- `JobRegistry` 由此自动 Send；连带 `JobRegistryConfig`/`ProducerHooks`/`StartSpec`
  Send。
- 若个别闭包供应商（web_m5 BashJobsBridge）捕获 `Rc<ShellProcess>`，则关涉
  `ShellProcess` 需先 Send（见 4.3）——跨 crate 依赖序：先 shell 后 jobs 或同批。
- 测试连锁：dsh-jobs 自身测试的 `Box::new(...)` 构造处若闭包可 Send 则无需改；
  若捕获 Rc 则改。

### 4.2 `dsh-terminal` Send 化
- `BackendProvider = Box<dyn Fn(TerminalConfig) -> Box<dyn TerminalBackend>>` →
  `Box<dyn Fn(TerminalConfig) -> Box<dyn TerminalBackend + Send> + Send>`；
- `OwnerLiveness = Box<dyn Fn(&str) -> bool + Send>`；
- `TerminalSession.backend: Box<dyn TerminalBackend>` →
  `Box<dyn TerminalBackend + Send>`；
- `trait TerminalBackend` 不需要 `Send` supertrait（避免把所有 impl 都逼 Send）——
  改为在 Box 处加 `+ Send`；但 `PtyBackend` 含 `Arc<Mutex<CollectedOutput>>` 与
  `Box<dyn Child + Send + Sync>`（已 Send），装配处满足即可。
- dsh-cli web_m5 `TerminalSessionService`（在 ThreadCell 内）届时改 `Arc<Mutex<..>>`。

### 4.3 `dsh-shell` Send 化
- `ShellProcess { inner: Rc<RefCell<ShellProcessInner>> }` →
  `Arc<Mutex<ShellProcessInner>>`；`.borrow()` → `.lock().unwrap()`、
  `.borrow_mut()` → `.lock().unwrap()`。
- `SubprocessHandle` 已 Send（JoinHandle / Arc<Mutex> / std::process::Child / win_job
  `unsafe impl Send`）→ `ShellProcessInner` 含 `Option<SubprocessHandle>` 自然 Send。
- `LocalShellExecutor` / 相关句柄连锁（若有 Rc/RefCell）一并 Send。
- 测试连锁：dsh-shell 测试中 `Rc<ShellProcess>` 捕获进闭包处 → `Arc`；`Rc::clone`
  → `Arc::clone`；若测试只用同线程则 Rc 可保留但容器线程化后建议统一 Arc。

### 4.4 dsh-cli：移除 `ThreadCell`，改真实共享
- `web_m5.rs`：`ThreadCell<JobRegistry>` → `Arc<Mutex<JobRegistry>>`；
  `ThreadCell<TerminalSessionService>` → `Arc<Mutex<TerminalSessionService>>`；
  `BashJobsBridge.state: ThreadCell<BashJobsState>` → `Arc<Mutex<BashJobsState>>`
  （BashJobsState 含 `Vec<Rc<ShellProcess>>` → `Vec<Arc<ShellProcess>>`——
  `BashJobsBridge::start_shell_job(process: Rc<ShellProcess>)` → `Arc<ShellProcess>`，
  `kill_all`/`pump` 相应改 lock；
  `M5HostServices.shutdown` 等 `.with(|s| ...)` → `lock().unwrap()`。
- `web.rs`：`M4HostServices.jobs: ThreadCell<JobRegistry>` → `Arc<Mutex<JobRegistry>>`
  （同步 `assemble_server_runtime_with_llm` 的 `ThreadCell::new` 与
  `register_m4_tools_with_host`/job 工具执行器绑定点）。
- `web_m5.rs` ThreadCell 类型定义与 thread_local 池可**删除**（无使用者）——保持
  仓库卫生；若仍有 BMP（如 `ThreadCell<TerminalSessionService>` 在别处）则保留到全清。

### 4.5 serve worker 化：长 RPC 上 worker
- 在 `dispatch_request` 中，对「装配了 agent-loop 且为长 RPC」的 method
  （`session.prompt`、`agent-loop`/`agent.turn`/`agent.run`、`commands/execute`
  的 `/plan <message>` 分支、审批 decide kick 恢复回合）**spawn 一个 worker 线程**：
  - worker 持有该请求所需的 Send 事实：`Arc<AgentLoopHost>`（含共享 store/tools/
    prompt）、`Arc<SessionHost>`（adopt 用）、`sid`/`text`/`line`/`decision` 等
    值拷贝；
  - worker 内跑原本同步的驱动（`run_rust_loop`/`run_rust_loop_with_...` 的
    worker 变体），经 `mpsc::channel` 把 `Result<(u16, serde_json::Value)>
    回传 accept；
  - **accept 线程如何在不阻塞下收结果**：`serve` 循环改用「结果 pending 表
    + 每次循环轮询 try_recv」——由于 `dispatch_request` 与 rust-loop 驱动在不同
    线程，accept 循环在 `recv_timeout` 间隙会处理（SSE/静态/cancel 等其它请求）。
    但 HTTP 同步契约要求「客户端等到结果」：分两种收束方式——
    a) **worker 线程直接持有 Request 并 `request.respond()`**（pickDirectory 已有
       先例，见 line 595-612：spawn 线程 + `let _ = request.respond(resp)`）：
       最简单、不动 accept 主循环、天然满足同步契约；`dispatch_request` 只做
       路由判断后把 `tiny_http::Request` move 进 worker；
    b) Result 经 channel 回填 accept（D-115 原文措辞）后再由 accept 线程 respond
       ——与 (a) 语义等价的另一种布置，但需要 accept 维护 pending Request 表，
       复杂度高且无收益。
  - **选定 (a)**：worker 持有 Request 并直接 respond；这其实就是「Result 回填」的
    实现（响应在 worker 完成时发出），accept 循环完全不被占用。D-115 的「Result 经
    channel」是布置细节，以「accept 空闲 + HTTP 同步契约不变」为准绳；pickDirectory
    已在同文件确立该先例。
- **每 driver 单 turn 保证**：同一会话/agent 的并发 prompt 必须序列化。驱动在
  `ReactLoopAgent.followup`（`send` → 若非 Idle → `wakeRequested` latch）已有
  单 turn 语义：worker 并发提交时，第二个 worker 的 `followup` 会把消息追加进
  inbox 并在当前 turn 后回放——**不破坏**相位机。故无需额外会话锁（Phase 2 已保证
  inbox/phase 线程安全）。跨会话并发天然成立。
- **cancel 路径不变**：`session.cancel` 仍在 accept 线程同步执行（短）：写共享
  token；worker 的 driver 在 step 边界消费 → turn/end reason=aborted。断然不把
  cancel worker 化。

### 4.6 worker 线程的 Send 事实（自下而上精化）

worker 无法取 `&Boot`（含 Rc/RefCell 非 Send 字段）。四个长 RPC 路径所需的
Send 事实：**全部是 `Arc` 句柄**（Phase 3 已 Send+Sync）：

| 长 RPC | 所需 Send 事实 |
|---|---|
| `session.prompt` [agent-loop 装配时] | `Arc<AgentLoopHost>`（`run_rust_loop_on_host`）、`Arc<SessionHost>`（无——adopt 仅 M1 WASM 路径，该路径不同步 workerize） |
| `agent.run\|turn\|loop` | 同上：`Arc<AgentLoopHost>` |
| `commands/execute` 的 `/plan <msg>` steer | `Arc<AgentLoopHost>`（`set_plan_mode_on_host` + `run_rust_loop_on_host`）、`plan_session: Option<Arc<Mutex<String>>>` |
| `session.approval.decide` / `respond` kick | `Arc<AgentLoopHost>`（`decide_on_host`）、`Option<ApprovalWireRef>` |

已落位 worker 变体：`run_rust_loop_on_host` / `ensure_session_agent_on_host`
（lib.rs）、`decide_on_host` / `set_plan_mode_on_host`（web/approval.rs）。全部摆脱
`&Boot`，只收 `Arc` 句柄。

**dispatch 布置（选型 (a)）**：`dispatch_request` 的通用 Post arm 内，对长方法名
（`session.prompt`、`agent-loop`/`agent.turn`/`agent.run`、`commands/execute` 的
`/plan <非空>` 分支、`session.approval.decide`）→ 把 `tiny_http::Request` **move
进 worker 线程**（pickDirectory 先例，web.rs:595）：worker 读 body → 调上文
worker 变体 → 直接 `request.respond(resp)`。accept 循环完全不被占用；HTTP 同步
契约不变（响应仍在 worker 完成时发出）。
- `session.cancel` / 短 RPC / SSE / 静态文件 → 维持 accept 同步（不 workerize）。
- `respond`（审批答复，D-108/G）：body 是 client-response（非 unary），kick 恢复
  回合此时可能在 worker 里跑/停——decide 本身（写 decided + kick）很快，且要
  让前端尽量快拿 receipt，**保持 accept 同步**（kick 是裸踢，恢复回合由 driver
  kick 内部的 turn() 在调用线程同步驱动——若 kick 内含长恢复回合，则它天然在
  accept 线程阻塞；D-115 边界认定为可接受：审批恢复是多轮确认后的用户动作，
  不像自由生成那样需要途中 cancel）。若实测阻塞明显，后续把 kick 的恢复回合
  也移 worker（扩展点，非本阶段强制）。

### 4.7 传输中断化（用户拍板 B；对齐 TS fetch-AbortSignal）

**目标**：长生成中 `session.cancel` 不只 step 边界生效，而是**主动中断在途 LLM 阻塞读**。

**当前现状（已研究确认）**：
- `GenerateOptions`（dsh-llm types）无 signal/cancel 字段；
- `dsh_core::llm_http::chat_completions_stream` + `tcp_exchange`：整段阻塞读
  （30s 读超时、读到 Content-Length/close 为止），`parse_sse` 一次性解析整个缓冲；
- `dsh-llm-deepseek::resolve_payloads`（m6_llm thunk）= 上述整段读 + 解析；
- `LlmAdapter::stream` 返回同步 `Box<dyn Iterator>`（无取消句柄）。

**改造（分步，TDD）**：
1. `dsh_llm::types::GenerateOptions` 增加可选的取消谓词/信号字段（Send+Sync，
   例如 `signal: Option<Arc<dyn Fn() -> bool + Send + Sync>>` 或共享 Abort
   句柄），缺省 None（既有调用不变）。
2. `dsh_core::llm_http`：新增**可中断读**路径——`chat_completions_stream_abortable`
   （或给 `tcp_exchange` 加 `cancel: &dyn Fn() -> bool`）：读取循环短读超时
   （如 200ms 轮询）逐段读，每段前检查 cancel 谓词；cancel → 关闭 socket、
   返回已读部分并标记 aborted（不返回 Error——对齐 TS：abort 是正常终止语义）。
   保留原 `chat_completions_stream`（存量调用不动），新增变体供 m6_llm 使用。
3. `dsh-llm-deepseek`：`DeepSeekAdapter::stream` 在读取前/中检查取消谓词——
   cancel 后不产出更多 text chunk，以 `FinishReason::Aborted`（对齐 TS `ABORTED`）
   结束流；`http_error_code` 等不变。
4. `dsh-cli/src/m6_llm.rs`：`resolve_payloads` thunk 改用可中断读并传入
   `GenerateOptions.signal`。
5. `dsh-agent-loop`：driver 的 `abort_reason()`（轮询共享取消令牌）以 Send+Sync
   谓词注入 `GenerateOptions.signal`——cancel 时 LLM 阻塞读被打断，
   `FinishReason::Aborted` → driver 现有 branch 归 `Halt::Aborted` →
   turn/end reason=aborted。
6. **取消令牌与 LLM 流的一致性**：driver step 内 `check_loop_request`（invariant）
   按同一令牌；`abort_reason` 同时服务（a）step 边界轮询、（b）LLM 流中断谓词。

**测试**：`dsh-core llm_http` 可中断读——本地慢速 SSE 服务端 + cancel 谓词在流中
置位 → 读在下次轮询窗口内返回 aborted（不等 30s/完整体）；`dsh-llm-deepseek`/
`dsh-cli m6_llm` 以 mock 断言 Aborted finish 短路不再产出 text；`dsh-agent-loop`
长 mock 流 + cancel → turn/end reason=aborted 且流中断（不再 collect 全量）。

### 4.8 worker 化后的测试策略（TDD 红→绿）
- 红：现有全绿 + 新增两类测试先失败：
  a) **worker 可执行含 M5 工具的 turn**：把「装配完整 bundle（真实 M4/M5 宿主 mock
     深 seek/shell）+ run_rust_loop 驱动一轮含工具调用」在 `std::thread::spawn` 的
     worker 线程执行（模拟 serve worker）→ 断言不 panic、事件落共享 store。当前
     会因 ThreadCell 跨线程 panic → 红。
  b) **长生成中 cancel 并发有效**：worker 线程跑一个长 mock 流 turn（脚本给多段
     text chunk + sleep），测试线程同时 `session.cancel` → 断言 turn 中止
     （turn/end reason=aborted）且 accept（测试线程）未被占死。Phase 3 后先绿
     （cancel_token 已 Send）；需确认在 worker 里驱动 run_rust_loop 不 panic →
     (a) 先红。
- 绿：三 crate Send 化 + ThreadCell 移除 + worker 化后上述绿。
- 重构：锁点收敛（每 M5 宿主一个 Mutex，无嵌套锁序问题——tool_exec 不进 session 锁
  反向路径，Phase 0 死锁审计仍成立：worker 顺序 `session.data → 叶子` 与主线程
  一致）。

## 5. 被否决的方案

- **只把 LLM 流上 worker、工具留主线程**：半协作泵（方案 I 变体），driver 相位机
  要求整轮连续，拆线程引入回同步点与重入，违背 D-115 选型理由。
- **worker 不持 Request、经 channel 由 accept respond**（布置 b）：无收益的复杂度
  （accept 要维护 pending Request 表 + 与 recv 竞争），一票否决。
- **引入每会话常驻 worker / 线程池**：跨会话并行是扩展点，本阶段不强制；池化管理
  属后续优化，避免提前引入调度复杂度。
- **给 `TerminalBackend` trait 加 `Send` supertrait**：把所有 backend impl 逼 Send
  是破坏性扩面；只需要 Box 处 `+ Send`（PtyBackend 已 Send），非 Pty 测试替身若为
  单线程闭包仍可用 `Box<dyn TerminalBackend>` 变体或简单内存替身。

## 6. 预期影响与回滚点

- 三 crate 公开类型 Box<dyn Fn>/Rc<RefCell> → Send/Arc<Mutex>：破坏性换型，编译期
  连锁 dsh-cli（web_m5/web）消费者；dsh-shell `ShellProcess` 从 Rc 变 Arc 后凡是
  克隆进闭包处一致化。
- `ThreadCell` 桥删除 → dsh-cli 副作用面无遗留 thread_local 池（若全清）。
- worker 化后 `session.prompt`/`agent.run` 的 HTTP 响应时点不变（仍是 turn 完成
  后）；cancel 可在 turn 中并发送达。
- 回滚点：Phase 4 单次提交可回（三 crate + dsh-cli 一处提交）；Phase 4 在
  Phase 1-3 之上，回滚不影响已关闸的 1-3。

## 7. 验证

- 关闸：`cargo test --workspace` 全绿 EXIT=0；`cargo clippy --workspace
  --all-targets` EXIT=0；60880 原命令行重启 → HTTP 200；新增并发测试验证
  (a) worker 内整轮（含 M5 工具）可执行、(b) 长生成中 cancel 并发送达生效。
- live 补验（Phase 5）：「生成中一键即停」wire 打点（真机长生成 + 途中 cancel）。

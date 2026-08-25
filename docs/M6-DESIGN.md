# M6 系统设计

> **阶段二（系统设计）** 产出：对 M6-REQUIREMENTS 各子步的缝/实现设计 + TDD 计划 + 顺序依赖 +
> DIV/让步清单。**编码前定设计与契约；不在此阶段改需求**（需求已由 M6-REQUIREMENTS + P1-P4
> 裁定锁定）。自下而上证据（本阶段实测）见各缝小节。

## 0. 总览

M6 主轴 = 服务器执行闭环：把 `AgentLoopHost`（真实 LLM + M4/M5 工具注册表 + 共享 SessionStore）
装配进 `dsh web` 的 serve()。全部已备积木组合（无新引擎）：装配工厂 / 生命周期 / tick 注入 /
sandbox·policy 投影 / LLM 诚实装配 / 前端最小闭环；穿插篮（settings/.env → provider caps →
hooks/skill → ts-host diff/SQLite）按子步排后。

依赖方向：`dsh-cli`（web/serve + main）→ `dsh-agent-loop`（AgentLoopHost）→ `dsh-llm` →
`dsh-llm-deepseek`（DeepSeekAdapter 缝）+ `dsh-core`（llm_http stream 桥）→ `dsh-tools`（M4/M5
register_*_with_host）+ `web/web_m5`（M5Host）。不新增 crate。

## 1. step1 服务器装配工厂（验收 #2）

### 缝（自下而上已实测）
- 既有骨架 = web.rs 测试 `rpc_prompt_routes_to_rust_agent_loop_shared_store`：`LlmRuntime::new`
  + `register_adapter(&["provider"], Rc<LlmAdapter>)` → `ToolRegistry::new(Native)` →
  `AgentLoopHost::with_store(config, llm, tools, session_host.store.clone())` →
  `boot.agent_loop = Some(host)` → `session.prompt` 走 `run_rust_loop`，事件落共享 store。
- 真实装配点 = `dsh web` main.rs `web::serve(&boot, cfg)`；`serve` 内已有 `SessionHost`
  （`host.store: Rc<SessionStore>` 公开）。

### 设计
```rust
// web.rs（M6）：构造服务器 LoopHost（在 serve() 内、seed "default" 之后）。
fn assemble_server_loop(
    session_store: Rc<dsh_session::SessionStore>,   // 与 SessionHost 共享
    workspace_root: PathBuf,                         // WebConfig 指定 | CWD（P2）
    llm_endpoint: LlmEndpoint,                       // {base_url, model} + key@env
) -> Result<Rc<dsh_agent_loop::AgentLoopHost>, String> {
    let llm = real_llm_runtime(&llm_endpoint)?;      // step5：deepseek adapter + llm_http stream
    let tools = Rc::new(dsh_tools::ToolRegistry::new(ToolExecutionMode::Native));
    let m5 = M5Host::assemble(workspace_root)?;      // D-074 生产工厂
    register_m4_tools_with_host(&tools, Some(&m4host));   // todo/jobs/schedule 宿主 bind（M4 既有）
    register_m5_tools_with_host(&tools, Some(&m5.services)); // fs/terminal/shell/bash/code（D-068..073）
    let config = AgentLoopConfig {
        max_parallel_tool_calls: None,
        agents: vec![ConfiguredAgent {
            id: "default".into(),
            provider: Some("deepseek".into()),
            model: Some(llm_endpoint.model.clone()),
            session_id: Some("default".into()),
            max_tokens: None, cwd: Some(workspace_root.clone().to_str().unwrap_or("").into()),
            resume_session_id: None,
        }],
    };
    AgentLoopHost::with_store(config, llm, tools, session_store)  // .map(Rc::new 已由 with_store)
        .map(|h| { h.add_disposer(Rc::new(move || { m5.shutdown(); })); h })
}
```
- `serve()` 改造：装配后 `boot.agent_loop = Some(host)`（借 `&mut` 或经 `Boot` 字段的可变通道；
  `Boot.agent_loop` 现为 `Option<Rc<AgentLoopHost>>`，字段语义允许在 serve 内赋值——按 struct
  字段可变性确认后以内部 `RefCell` 或构造期预置；**编码期以红测定夺**）。
- 真实注册表 = M4 + M5 全工具；`agent.turn/agent.run/session.prompt` 不经 WASM adopt，直接
  `run_rust_loop`。
- **TDD**：web.rs 新装配单测（红）——`assemble_server_loop(store, root, fake_endpoint)` 后
  `boot.agent_loop` 就位；stub LLM（沿用 mock adapter 脚本）驱动一轮：`bash echo`（门控）/
  `todo_write`（M4）真实执行；事件落共享 store；退出 disposer 被调（用可观察哨兵）。

## 2. step2 宿主生命周期 + 清理（验收 #3）

### 缝
- `M5Host`（web/web_m5.rs）现仅 `assemble/register`；无 shutdown。`M5gTick` 持 `AtomicBool stop`；
  `BashJobsBridge` 可对 running 进程 kill（`processes` 表）；`TerminalSessionService` 关全会话需
  确认 dispose 方法（PTY worker 线程随 handle drop 结算，D-064）。

### 设计
```rust
impl M5Host {
    pub fn shutdown(&self) {            // 幂等：停 tick → 杀 live bash → 关 terminal
        if let Some(t) = self.tick()    { t.stop() };
        if let Some(b) = &self.services.bash_jobs { b.kill_all(); } // 遍历 processes → kill + settle Killed
        if let Some(t) = &self.services.terminal { t.close_all(); } // 逐会话 dispose/close
    }
}
```
- serve 装配时 `host.add_disposer(host shutdown)`；`WebServer` Drop（或 serve 请求循环退出）触发
  disposers 按注册序执行（复用 `AgentLoopHost.add_disposer` 通道）。
- `WebConfig.workspace_root: Option<PathBuf>`；缺省 = `std::env::current_dir().canonicalize()`（P2）。
- **TDD**：shutdown 后（a）tick 线程 `m5g_tick` 停（原子位断言/join 超时）、（b）live bash bg 进程
  被 kill 且 settle 为 Killed（真实 Git Bash 门控）、（c）terminal 会话关闭、无孤儿（子进程 PID
  try_wait 非 0 或在极短时限内退出）。

## 3. step3 M5g tick 注入 serve（验收 #4）

### 缝
- serve 主循环现 `for request in server.incoming_requests()`（阻塞）。tiny_http 提供 `Server::
  recv_timeout(d)` → 可改轮询 + tick。
- `m5g_tick_once(sched, Some(bridge), now)`（web_m5，D-072）主线程执行调度派发 + jobs settle。
- `ScheduleHost::new(shared_session)`（web.rs dsh_cli_host）——需要与 loop 同 store 或独立
  session；设计取 `SessionHost` 共享 store 的 session（schedule/schedule+change 事件落同一读模型）。

### 设计
```rust
// serve 请求循环改造（保持 Rpc/静态的 &Boot 非 Send 留在主线程）：
let tick_interval = Duration::from_millis(M5G_INTERVAL_MS);   // 如 250
loop {
    match server.recv_timeout(tick_interval) {
        Ok(Some(req)) => dispatch_request(req, &root, &manifest, boot, &host, &sink),
        Ok(None) => {}                       // 超时：无请求，纯 tick
        Err(e) => { /* 记录；continue（不因瞬时错误断服务） */ }
    }
    let now = m5g_epoch_now_ms();
    if let (Some(sched), Some(bridge)) = (&sched_host, &loop_bridge) {
        let _ = m5g_tick_once(sched, Some(bridge), now);   // 调度到期 + jobs settle（真推进）
    }
}
```
- `M5gTick` 服务线程 + mpsc 不再需要进入 serve（主循环自带节拍）；**决定：serve 用
  recv_timeout 自驱节拍**（同一 M5g 语义，主线程执行调度/jobs——`tick_once` 唯一推进点）。若
  recv_timeout 在该 tiny_http 版本不可用 → 升级/改用带超时接收（红测先定夺）。
- **TDD**：serve 层集成——schedule after(1s) create → 仅 serve 主循环自驱 → `schedule/change`
  事件自动落 store（非手工 dispatch_due）；bash bg `sleep; echo` → 自动 settle completed + 全文。

## 4. step4 sandbox/mode 投影进循环（验收 #5）

### 缝
- `resolve_sandbox_mode(approved, events)`（D-075）→ `EffectiveSandbox{mode, source}`；
  `sandbox_policy_segment(mode, workspace_root)`（D-074，order 110 词表已对齐）。
- SystemPrompt = `SystemPrompt::new(&PromptConfig::default(), ...)`（AgentLoopHost 内部构造）。
  **缺注入缝**：需确认 SystemPrompt 是否支持追加具名段（harness/sandbox-policy、order 110）；
  不支持则补（dsh-system-prompt 加 segment 源，TDD）。

### 设计
- loop host 构造后，把动态 policy 段纳入 prompt：`SystemPrompt` 支持注入 →
  `sandbox:policy` 段（order 110：`effective mode {mode}\nwritable roots: {roots}`）随构建放进
  注入列表；approved 由调用方（审批缝）传入 `resolve_sandbox_mode`，缺省 None。
- escalation 工具面（bash/fs）：执行时 `sandbox_permissions`/`justification` 先过
  `dsh_sandbox::validate_escalation_args`（fail-closed）；审批通道缺省无 → **deny + escalation
  hint marker**（复用 D-070 已有 UNSUPPORTED，改为结构化墨迹：拒绝 + 提升提示）。
- **TDD**：policy 段内容快照（含 mode/roots）；srota 事件 fold 后有效模式进 prompt；escalation
  校验用例（缺 justification / 空句 / 合法同现缺审批 → deny + hint）。

## 5. step5 LLM 装配 + 诚实无 key（验收 #6；P4 端点）

### 缝
- `LlmRuntime::register_adapter(&["deepseek"], Rc<DeepSeekAdapter>)`；`DeepSeekAdapterOptions{
  resolve_connection, resolve_payloads }`（dsh-llm-deepseek 缝保持无 IO）。
- `DeepSeekConnection{ base_url(+"/chat/completions"), defaults, max_tokens,
  default_context_window, models(catalog≥deepseek-v4-flash-0731-ext), retry_policy }`。
- **真实 IO 桥缺失**：现 `dsh-core::llm_http::chat_completions` 返回 final `Value`（非流式）；
  `PayloadsResolver = Vec<String>`（SSE `data:` payloads）需**流式变体**。

### 设计
1. **`dsh-core::llm_http::chat_completions_stream(base, api_key, model, messages, tools)
   -> Result<Vec<String>, LlmError-ish>`（新，TDD）**：复用既有 HTTP/1.1 POST 构造 + Bearer +
   SSE 行解析（llm_http.rs 已具备），把 `data: ` payload 行累积为 `Vec<String>` 返回（不再坍缩成
   final JSON）；错误行 `data: {"error": {...}}` → 结构化错误；连接错误 → 明确码。这是
   `PayloadsResolver` 的 transport thunk。
2. **M6 deepseek thunk（新，dsh-cli 或 dsh-core 薄层）**：`resolve_connection` 从
   `DSH_LLM_BASE_URL`（缺省 `http://100.105.152.101:18080/v1` 为**配置读写**而非硬编码——端点在
   WebConfig/环境；需求 P4 端点作为缺省值记录，非提交常量）取 base_url + `DEEPSEEK_API_KEY` 取
   key（**仅环境变量，key 不入库**）；`resolve_payloads = |conn, wire, opts| ->
   chat_completions_stream(base, key, model, wire→messages/tools)`。
3. **无 key fail-loud**（P3）：key 缺失 → thunk 返回 `LlmError::new("missing DEEPSEEK_API_KEY:
   set it to enable agent turns", AUTH/*明确码*)`——`agent.turn` 在首轮即失败并回传可读
   code/message；工具注册、`llm.models/discoverModels`（列录 catalog）照常——诚实、不降级。
4. 装配 `LlmRuntime`：`register_adapter(&["deepseek"], Rc::new(DeepSeekAdapter::new(opts)))`。
- **TDD**：单测 stub thunk 编辑 wire→流式 payload 断言；`chat_completions_stream` 对本地可回环
  端点（可注入的 test server）红绿；无 key → fail-loud 错误消息断言；真实端点冒烟**门控**（端点
  可达才跑，不可达 → 记录 skipped，acceptance #6 允许）。

## 6. step6 前端最小闭环（验收 #6 冒烟面）

### 设计
- 复用既有 `session/event` downlink（EventSink）+ `session.history`：loop 事件写共享 store →
  前端下链读同一读模型（M4h 已证）。M6 闭环验证 = RPC 层集成测试（真实循环一轮后 history 含
  turn/工具事件）+（门控）真实端点一轮 `agent.turn`（stub LLM 逐句驱动工具 vs 真实 LLM 冒烟）。
- 不做新前端 UI（边界）。

## 7. 穿插篮（P1 纳入，step7-10 概要）

| 子步 | 设计要点（编码期细化） |
|---|---|
| step7 settings/.env | settings YAML 注释保真 leaf-diff + `.env` 解析（键注入 server 装配：LLM key 上游可选来源，但 key 仍以环境变量/不落盘为准） |
| step8 provider caps 做实 | provider/models RPC 从真实 `DeepSeekConnection.models` catalog 列录（含容量/重试/模式） |
| step9 hooks/skill | hooks=pre/post-execute 宿主钩子（dsh-tools 既有 pre-decision 缝延伸）；skill=system-prompt 段注册 |
| step10 ts-host diff / SQLite | dsh-diff 差分对 ts-host；SQLite 落盘/回读（持久化面） |
| step11 M6-ACCEPTANCE | 全量 test + clippy + DECISIONS 互查 + git + 冒烟报告 |

## 8. 顺序依赖与 DIV/让步清单

- **顺序**：step1→2→3→4→5→6（主轴，线性，step5 是 step1 的 LLM 前置硬块）；step7-10 在主轴
  绿后按 7→8→9→10 穿插（互不阻塞）；step11 收口。
- **DIV / 让步（记录为 D 条目，不静默）**：
  - IV-1 真实 HTTP/SSE stream 桥加在 dsh-core llm_http（新变体），不另起 crate。
  - IV-2 serve 主循环改 `recv_timeout` 自驱节拍（替代 M5gTick 线程——同一 M5g 语义，推进点
    唯一收敛到主线程 `tick_once`）；若 tiny_http 无 recv_timeout → 升级或小适配（红测定夺）。
  - IV-3 key 来源仅 `DEEPSEEK_API_KEY`（+settings 只读可选别名，写入仍禁止）。
  - IV-4 冒烟门控：真实端点不可达 → skipped（记录），不阻塞 M6-ACCEPTANCE。
  - IV-5 `M5Host::shutdown` 为新增清理面（terminal close_all / bash kill_all / tick stop）；关
    闭顺序依依赖逆序（tick→jobs→terminal）。
- **诚实边界（沿用）**：run_code 嵌套 tools 派发、read_image 解码仍渐进（D-069/D-073）；
  mcp/acp 仅能力登记缝出口。

## 9. 阶段关卡验收（进入编码前）

本设计文档经 M6-REQUIREMENTS 锚定：每个主轴子步有「缝（已实测）+ 设计 + TDD 计划」，穿插篮子步
有概要；DIV/让步显式记录。通过后进入阶段三（编码实现，TDD 红→绿，逐子步提交 + D 互查）。

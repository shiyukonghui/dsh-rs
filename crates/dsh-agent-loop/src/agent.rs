//! ReactLoopAgent 同步驱动（对齐 `agent.ts` 逐行语义；sync 差值见 D-032）。
//!
//! 驱动把一个 `Agent`（含 inbox/session/scope）粘合 `llm`/system-prompt/工具执行为
//! turn/step 状态机。Rust 面为**同步 inline**：send 唤起 driver 时整个 drain 同步完成；
//! reentrant 唤醒按同一 latch 语义（running 期间 send → `wakeRequested`，轮次边界回放）。
//! 取消是**合作式**（轮次/步骤边界检查），无中流抢占（D-032）。

// LoopDeps 的语义闭包类型显式化是设计（Rc<dyn Fn> 作为 driver 泥合 seam）；Halt 粘
// LlmFailure 事实（结构化错误）也是设计 —— 两者按 build_request 先例模块级收窄。
#![allow(clippy::type_complexity)]
#![allow(clippy::result_large_err)]

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use dsh_agent::{
    agent_carrier, Agent, AgentEventDispatch, AgentRegistry, AgentStatus, CancelOptions,
    InboxTarget, NextFn, Inbox,
};
use dsh_llm::{
    BlockAssembler, CallConfig, ContentBlock, FinishReason, GenerateOptions, LlmError,
    LlmFailure, Message, MessageId, MessageSource, ModelMessageSource, PreparedLlmCall,
    Role, StreamChunk, ToolCallBlock,
};
use dsh_session::{
    AgentCancelCause, EventKind, SessionId, SurfaceIntent, SurfaceOp, TurnEndCancelCause,
    TurnEndReason,
};
use dsh_system_prompt::{render_prompt, AssembleContext, PromptAssembly};
use serde_json::{json, Value};

use crate::build_request::build_request;

// ---------------------------------------------------------------------------
// 类型
// ---------------------------------------------------------------------------

/// 驱动相位（对齐 `Phase`）。`abort_reason` = 取消标记（sync 合作式；对应 TS AbortSignal）。
#[derive(Debug, Clone)]
enum Phase {
    Idle { last_turn: u64 },
    Maintenance {
        last_turn: u64,
        wake_requested: bool,
        abort_reason: Option<AgentCancelCause>,
    },
    Running {
        turn: u64,
        step: u64,
        wake_requested: bool,
        abort_reason: Option<AgentCancelCause>,
    },
}

impl Phase {
    fn turn(&self) -> u64 {
        match self {
            Phase::Running { turn, .. } => *turn,
            Phase::Idle { last_turn } | Phase::Maintenance { last_turn, .. } => *last_turn,
        }
    }
    fn step(&self) -> u64 {
        match self {
            Phase::Running { step, .. } => *step,
            _ => 0,
        }
    }
    fn abort_reason(&self) -> Option<AgentCancelCause> {
        match self {
            Phase::Running { abort_reason, .. } | Phase::Maintenance { abort_reason, .. } => {
                abort_reason.clone()
            }
            Phase::Idle { .. } => None,
        }
    }
    fn set_turn(&mut self, turn: u64) {
        if let Phase::Running { turn: t, .. } = self {
            *t = turn;
        }
    }
    fn set_step(&mut self, step: u64) {
        if let Phase::Running { step: s, .. } = self {
            *s = step;
        }
    }
}

/// 一次冻结失败的事实（`{message, code:'UNKNOWN'}` 归一化；TS `errorChain` 版）。
fn unknown_failure(message: String) -> LlmFailure {
    LlmFailure {
        message,
        code: "UNKNOWN".into(),
        status: None,
        provider_retry_after_ms: None,
        request_id: None,
    }
}

/// 内部停机值：取消或失败。失败在 turn catch 处结构化+上报（emit agent/error）。
#[derive(Debug, Clone)]
enum Halt {
    Aborted(AgentCancelCause),
    Failed(LlmFailure),
}

/// 工具执行上下文（M2e-2 由注入钩子消费；M2e-3 接真实 scheduler）。
pub struct ToolExecCtx<'a> {
    pub turn: u64,
    pub step: u64,
    pub tool_calls: &'a [ToolCallBlock],
}

/// 工具执行结果（模型顺序结果 + 延后到全部 result 之后的 context）。
pub struct ToolExecOutcome {
    pub concluded: bool,
    pub context: Vec<Message>,
}

/// driver 依赖哑合（全部 Rc，构造即注入；M2e-3 由 AgentLoop 服务装配真实实现）。
pub struct LoopDeps {
    /// `systemPrompt.assemble(context)`：装配 system sections/contexts/tools。
    pub assemble: Rc<dyn Fn(&AssembleContext) -> Result<PromptAssembly, String>>,
    /// `llm.prepareCall(config)`：路由解析/默认值。
    pub prepare_call: Rc<dyn Fn(CallConfig) -> Result<PreparedLlmCall, LlmError>>,
    /// `llm.stream(request)`：拖出原始 chunk（sync；finish error/aborted 在 chunk 内表达）。
    pub stream: Rc<dyn Fn(&GenerateOptions) -> Result<Vec<StreamChunk>, LlmError>>,
    /// runtime-context 投影（M2e-3 接 RuntimeContextProjection；缺省不写）。
    pub project_context: Rc<dyn Fn(&PromptAssembly) -> Option<Message>>,
    /// 工具调用执行（M2e-3 接 executeToolCalls）。
    pub tool_exec: Rc<dyn Fn(&ToolExecCtx) -> ToolExecOutcome>,
}

/// pre-step 决策（对齐 `PreparedStep`；assembly 在 driver 本地附着）。
#[derive(Debug, Clone)]
pub enum PreStepDecision {
    Reject,
    Enter { messages: Vec<Message>, assembly: PromptAssembly },
}

impl PreStepDecision {
    fn from_value(v: &Value, assembly: &PromptAssembly) -> Result<Self, Halt> {
        let kind = v
            .get("kind")
            .and_then(Value::as_str)
            .ok_or_else(|| Halt::Failed(unknown_failure("agent/pre-step decision missing kind".into())))?;
        match kind {
            "reject" => Ok(PreStepDecision::Reject),
            "enter" => {
                let messages = v
                    .get("messages")
                    .cloned()
                    .ok_or_else(|| Halt::Failed(unknown_failure("agent/pre-step enter decision missing messages".into())))?;
                let messages: Vec<Message> =
                    serde_json::from_value(messages).map_err(|e| Halt::Failed(unknown_failure(e.to_string())))?;
                Ok(PreStepDecision::Enter { messages, assembly: assembly.clone() })
            }
            other => Err(Halt::Failed(unknown_failure(format!(
                "agent/pre-step decision unknown kind {other:?}"
            )))),
        }
    }
}

// ---------------------------------------------------------------------------
// ReactLoopAgent
// ---------------------------------------------------------------------------

pub struct ReactLoopAgent {
    pub agent: Rc<Agent>,
    pub registry: Rc<AgentRegistry>,
    pub deps: LoopDeps,
    dispatch: AgentEventDispatch,
    propose: Rc<dyn Fn(CallConfig, u64, u64) -> Result<CallConfig, String>>,
    phase: RefCell<Phase>,
    request_header_logged: Cell<bool>,
}

impl ReactLoopAgent {
    pub fn new(agent: Rc<Agent>, registry: Rc<AgentRegistry>, deps: LoopDeps) -> Rc<Self> {
        let dispatch = AgentEventDispatch::new(agent.ctx.bus().clone(), agent_carrier(&agent));
        let propose: Rc<dyn Fn(CallConfig, u64, u64) -> Result<CallConfig, String>> = {
            let dispatch = dispatch.clone();
            let agent = agent.clone();
            Rc::new(move |seed: CallConfig, turn: u64, step: u64| -> Result<CallConfig, String> {
                let seed_json = serde_json::to_value(&seed).map_err(|e| e.to_string())?;
                let innermost: NextFn = Rc::new(move |_p| seed_json.clone());
                let d = dispatch.clone();
                let a = agent.clone();
                let name = "agent/request";
                let payload = json!({ "turn": turn, "step": step });
                let v = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    d.waterfall(&a, name, payload, innermost)
                }))
                .unwrap_or_else(|e| json!({ "__panic__": panic_message(&e) }));
                if let Some(p) = v.get("__panic__") {
                    return Err(p.as_str().unwrap_or("listener panic").to_string());
                }
                serde_json::from_value(v).map_err(|e| e.to_string())
            })
        };
        // live agent/inbox/* 通知 → 作用域事件
        {
            let d = dispatch.clone();
            let notify_agent = agent.clone();
            let notify = Rc::new(move |n: &dsh_agent::InboxNotification| {
                let (name, payload) = match n {
                    dsh_agent::InboxNotification::Inserted { message } => {
                        ("agent/inbox/inserted", json!({ "message": message }))
                    }
                    dsh_agent::InboxNotification::Discarded { message } => {
                        ("agent/inbox/discarded", json!({ "message": message }))
                    }
                    dsh_agent::InboxNotification::Claimed { message, turn } => {
                        ("agent/inbox/claimed", json!({ "message": message, "turn": turn }))
                    }
                };
                let mut warns = Vec::new();
                d.emit(&notify_agent, name, payload, &mut warns);
            });
            agent.set_inbox_notify(notify);
        }
        let last_turn = last_turn_of(&agent.session);
        Rc::new(ReactLoopAgent {
            agent,
            registry,
            deps,
            dispatch,
            propose,
            phase: RefCell::new(Phase::Idle { last_turn }),
            request_header_logged: Cell::new(false),
        })
    }

    pub fn id(&self) -> &SessionId {
        &self.agent.id
    }

    pub fn session(&self) -> &Rc<dsh_session::Session> {
        &self.agent.session
    }

    pub fn status(&self) -> AgentStatus {
        match &*self.phase.borrow() {
            Phase::Idle { .. } | Phase::Maintenance { .. } => AgentStatus::Idle,
            Phase::Running { .. } => AgentStatus::Running,
        }
    }

    pub fn inbox(&self) -> &Inbox {
        &self.agent.inbox
    }

    fn set_phase(&self, next: Phase) {
        let prev = self.status();
        let now = match &next {
            Phase::Idle { .. } | Phase::Maintenance { .. } => AgentStatus::Idle,
            Phase::Running { .. } => AgentStatus::Running,
        };
        *self.phase.borrow_mut() = next;
        self.agent.status.set(now);
        if now != prev {
            let mut warns = Vec::new();
            self.dispatch
                .emit(&self.agent, "agent/status", json!({ "status": now.wire_str() }), &mut warns);
        }
    }

    // ---- sleep 原语 ----

    pub fn send(&self, message: Message, target: InboxTarget, wakeup: bool) -> Result<(), String> {
        let waking_after_abort = wakeup
            && !matches!(*self.phase.borrow(), Phase::Idle { .. })
            && self.phase.borrow().abort_reason().is_some();
        let resolved = if waking_after_abort {
            InboxTarget::NextTurn
        } else {
            target
        };
        let start = match resolved {
            InboxTarget::NextTurn => self.agent.inbox.next_turn().len() as f64,
            InboxTarget::NextStep => self.agent.inbox.next_step().len() as f64,
        };
        self.agent.inbox.splice(resolved, start, 0.0, vec![message])?;
        if wakeup {
            self.wake_driver(waking_after_abort);
        }
        Ok(())
    }

    pub fn followup(&self, input: Message) -> Result<(), String> {
        self.send(input, InboxTarget::NextTurn, true)
    }

    pub fn steer(&self, input: Message) -> Result<(), String> {
        self.send(input, InboxTarget::NextStep, true)
    }

    pub fn inject(&self, input: Message) -> Result<(), String> {
        self.send(input, InboxTarget::NextStep, false)
    }

    pub fn cancel(&self, cause: AgentCancelCause, options: &CancelOptions) {
        if !options.keep_inbox.unwrap_or(false) {
            let _ = self.agent.inbox.clear();
            if !matches!(*self.phase.borrow(), Phase::Idle { .. }) {
                let mut ph = self.phase.borrow_mut();
                match &mut *ph {
                    Phase::Running { wake_requested, .. }
                    | Phase::Maintenance { wake_requested, .. } => {
                        *wake_requested = false;
                    }
                    _ => {}
                }
            }
        }
        if !matches!(*self.phase.borrow(), Phase::Idle { .. }) {
            let mut ph = self.phase.borrow_mut();
            match &mut *ph {
                Phase::Running { abort_reason, .. }
                | Phase::Maintenance { abort_reason, .. } => {
                    *abort_reason = Some(cause);
                }
                _ => {}
            }
        }
    }

    /// sync：send/kick 返回前整个 driver 已排空（D-032；无并发等待）。
    pub fn when_idle(&self) {
        let _ = &*self.phase.borrow();
    }

    pub fn run_maintenance<T>(&self, job: impl FnOnce() -> T) -> Result<T, String> {
        if !matches!(*self.phase.borrow(), Phase::Idle { .. }) {
            return Err(format!("agent \"{}\" already has active work", self.agent.id));
        }
        let last_turn = self.phase.borrow().turn();
        self.set_phase(Phase::Maintenance {
            last_turn,
            wake_requested: false,
            abort_reason: None,
        });
        let result = job();
        let (idle_turn, rewake) = {
            let mut ph = self.phase.borrow_mut();
            match &mut *ph {
                Phase::Maintenance {
                    last_turn,
                    wake_requested,
                    ..
                } => {
                    let r = *wake_requested && self.agent.inbox.has_pending();
                    (*last_turn, r)
                }
                _ => (0, false),
            }
        };
        self.set_phase(Phase::Idle { last_turn: idle_turn });
        if rewake {
            self.wake_driver(false);
        }
        Ok(result)
    }

    fn wake_driver(&self, wake_after_abort: bool) {
        {
            let mut ph = self.phase.borrow_mut();
            match &mut *ph {
                Phase::Running {
                    wake_requested,
                    abort_reason,
                    ..
                } => {
                    let latches = abort_reason.as_ref().is_none_or(|r| *r != AgentCancelCause::Disposed)
                        && wake_after_abort;
                    if latches {
                        *wake_requested = true;
                    }
                    return;
                }
                Phase::Maintenance {
                    wake_requested,
                    abort_reason,
                    ..
                } => {
                    // maintenance 期间任何 wake（非 disposed）都 latch，无视 wake_after_abort
                    let latches = abort_reason.as_ref().is_none_or(|r| *r != AgentCancelCause::Disposed);
                    if latches {
                        *wake_requested = true;
                    }
                    return;
                }
                Phase::Idle { .. } => {}
            }
        }
        let last_turn = match &*self.phase.borrow() {
            Phase::Idle { last_turn } => *last_turn,
            _ => unreachable!("non-idle handled above"),
        };
        self.set_phase(Phase::Running {
            turn: last_turn,
            step: 0,
            wake_requested: false,
            abort_reason: None,
        });
        match self.registry.with_initiator(&self.agent, || self.kick()) {
            Ok(()) => {}
            Err(_) => {
                // initiator 作用域已 disposed：关闭无主 running 相位（dispose 时 wake 不拉 latch）
                let t = self.phase.borrow().turn();
                self.set_phase(Phase::Idle { last_turn: t });
            }
        }
    }

    fn abort_reason(&self) -> Option<AgentCancelCause> {
        self.phase.borrow().abort_reason()
    }

    fn kick(&self) {
        loop {
            while let Ok(true) = self.turn() {}
            let (idle_turn, rewake) = {
                let mut ph = self.phase.borrow_mut();
                match &mut *ph {
                    Phase::Running { turn, wake_requested, .. } => {
                        let r = *wake_requested && self.agent.inbox.has_pending();
                        (*turn, r)
                    }
                    _ => (0, false),
                }
            };
            self.set_phase(Phase::Idle { last_turn: idle_turn });
            if !rewake {
                break;
            }
            self.set_phase(Phase::Running {
                turn: idle_turn,
                step: 0,
                wake_requested: false,
                abort_reason: None,
            });
        }
    }

    // ---- 报告与派发 ----

    fn emit_agent_error(&self, failure: LlmFailure) {
        let (turn, step) = {
            let ph = self.phase.borrow();
            (ph.turn(), ph.step())
        };
        let mut warns = Vec::new();
        self.dispatch
            .emit(&self.agent, "agent/error", json!({ "turn": turn, "step": step, "error": failure }), &mut warns);
    }

    fn waterfall_safe(&self, name: &str, payload: Value, innermost: NextFn) -> Result<Value, Halt> {
        let d = self.dispatch.clone();
        let agent = self.agent.clone();
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            d.waterfall(&agent, name, payload, innermost)
        }))
        .map_err(|e| Halt::Failed(unknown_failure(format!("{name} listener threw: {}", panic_message(&e)))))
    }

    fn serial_safe(&self, name: &str, payload: Value, innermost: NextFn) -> Result<Value, Halt> {
        let d = self.dispatch.clone();
        let agent = self.agent.clone();
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            d.serial(&agent, name, payload, innermost)
        }))
        .map_err(|e| Halt::Failed(unknown_failure(format!("{name} listener threw: {}", panic_message(&e)))))
    }

    // ---- turn / step 状态机 ----

    /// 打开一个 turn 并驱动其步骤。Ok(true) = 换新相位继续下一 turn。
    fn turn(&self) -> Result<bool, Halt> {
        let running = matches!(*self.phase.borrow(), Phase::Running { .. });
        if !running {
            let f = unknown_failure(format!(
                "agent \"{}\": turn without driver reservation",
                self.agent.id
            ));
            self.emit_agent_error(f.clone());
            return Err(Halt::Failed(f));
        }
        // pre-turn 取消（try 外）：不写任何 turn 记录（TS `signal.throwIfAborted()` @ line 252）
        if let Some(c) = self.abort_reason() {
            return Err(Halt::Aborted(c));
        }
        let phase_turn = self.phase.borrow().turn();
        let turn = phase_turn + 1;
        if let Err(e) = self
            .agent
            .session
            .append(EventKind::TurnStart, json!({ "turn": turn }), None)
        {
            let f = unknown_failure(e.0);
            self.emit_agent_error(f.clone());
            return Err(Halt::Failed(f));
        }
        self.phase.borrow_mut().set_turn(turn);
        let mut turn_ends: Option<TurnEndReason> = None;
        let mut target = InboxTarget::NextTurn;

        // 主循环（对应 try 块）
        let main: Result<bool, Halt> = (|| {
            loop {
                if let Some(c) = self.abort_reason() {
                    return Err(Halt::Aborted(c));
                }
                let step = self.phase.borrow().step() + 1;
                let decision = self.pre_step(target, turn, step)?;
                match decision {
                    PreStepDecision::Reject => {
                        turn_ends = Some(TurnEndReason::Blocked);
                        return Ok(false);
                    }
                    PreStepDecision::Enter { messages, assembly } => {
                        if turn_ends.is_some() && messages.is_empty() {
                            break;
                        }
                        if self.phase.borrow().step() == 0 && messages.is_empty() {
                            turn_ends = Some(TurnEndReason::Completed);
                            return Ok(false);
                        }
                        if let Some(c) = self.abort_reason() {
                            return Err(Halt::Aborted(c));
                        }
                        self.agent
                            .session
                            .append(EventKind::StepStart, json!({ "turn": turn, "step": step }), None)
                            .map_err(|e| Halt::Failed(unknown_failure(e.0)))?;
                        self.phase.borrow_mut().set_step(step);
                        // step 内部 try/finally：finally 总是写 step/end
                        let step_result: Result<Option<TurnEndReason>, Halt> = (|| {
                            for m in &messages {
                                self.append_user_msg(m)?;
                            }
                            self.step(turn, step, &assembly)
                        })();
                        let _ = self
                            .agent
                            .session
                            .append(EventKind::StepEnd, json!({ "turn": turn, "step": step }), None);
                        let step_end = match step_result {
                            Ok(se) => se,
                            Err(h) => return Err(h),
                        };
                        // max-tokens 粘性：已完成步骤不得降级
                        if !matches!(turn_ends, Some(TurnEndReason::MaxTokens)) {
                            turn_ends = step_end;
                        }
                        if let Some(c) = self.abort_reason() {
                            return Err(Halt::Aborted(c));
                        }
                        if turn_ends.is_some() && self.agent.inbox.next_step().is_empty() {
                            self.dispatch_turn_stopping(turn)?;
                            if let Some(c) = self.abort_reason() {
                                return Err(Halt::Aborted(c));
                            }
                        }
                        if turn_ends.is_some() && self.agent.inbox.next_step().is_empty() {
                            break;
                        }
                        target = InboxTarget::NextStep;
                    }
                }
            }
            Ok(self.agent.inbox.has_pending())
        })();

        // catch 等价：结构化收尾
        let exit_halt = match &main {
            Ok(_) => None,
            Err(h) => {
                turn_ends = Some(match h {
                    Halt::Aborted(c) => TurnEndReason::Aborted {
                        reason: cancel_to_turn(c),
                    },
                    Halt::Failed(f) => {
                        let f = f.clone();
                        self.emit_agent_error(f.clone());
                        TurnEndReason::Error { error: f }
                    }
                });
                Some(h.clone())
            }
        };

        // finally：turn/end（含 reason）
        if let Some(reason) = &turn_ends {
            let reason_v = serde_json::to_value(reason).unwrap_or(Value::Null);
            if let Err(e) = self.agent.session.append(
                EventKind::TurnEnd,
                json!({ "turn": turn, "reason": reason_v }),
                None,
            ) {
                let f = unknown_failure(e.0);
                self.emit_agent_error(f.clone());
                return Err(Halt::Failed(f));
            }
        }

        match exit_halt {
            Some(h) => Err(h),
            None => {
                let has_pending = self.agent.inbox.has_pending();
                if !has_pending {
                    return Ok(false);
                }
                // 换新相位（fresh controller）：清取消与 latch，step 归零
                if let Phase::Running {
                    abort_reason,
                    wake_requested,
                    step,
                    ..
                } = &mut *self.phase.borrow_mut()
                {
                    *abort_reason = None;
                    *wake_requested = false;
                    *step = 0;
                }
                Ok(true)
            }
        }
    }

    fn append_user_msg(&self, m: &Message) -> Result<(), Halt> {
        let v = serde_json::to_value(m).map_err(|e| Halt::Failed(unknown_failure(e.to_string())))?;
        self.agent
            .session
            .append(
                EventKind::UserMessage,
                v,
                Some(&SurfaceIntent {
                    surface_op: SurfaceOp::Append,
                    source_event_seqs: None,
                }),
            )
            .map_err(|e| Halt::Failed(unknown_failure(e.0)))?;
        Ok(())
    }

    /// 提出一个步骤：claim + 装配 + runtime-context + pre-step 决策水岭。
    fn pre_step(&self, target: InboxTarget, turn: u64, step: u64) -> Result<PreStepDecision, Halt> {
        if !matches!(*self.phase.borrow(), Phase::Running { .. }) {
            return Err(Halt::Failed(unknown_failure(format!(
                "agent \"{}\": pre-step outside running phase",
                self.agent.id
            ))));
        }
        let claimed = self
            .agent
            .inbox
            .claim(target, turn)
            .map_err(|e| Halt::Failed(unknown_failure(e)))?;
        let mut claimed_msgs: Vec<Message> = claimed.next_steps().to_vec();
        if let Some(front) = claimed.next_turn_front() {
            claimed_msgs.push(front);
        }
        let assembly = (self.deps.assemble)(&dsh_agent::assemble_context_for(&self.agent))
            .map_err(|e| Halt::Failed(unknown_failure(e)))?;
        if let Some(c) = self.abort_reason() {
            return Err(Halt::Aborted(c));
        }
        let context = (self.deps.project_context)(&assembly);
        let claimed_json: Vec<Value> = claimed_msgs
            .iter()
            .map(|m| serde_json::to_value(m).unwrap_or(Value::Null))
            .collect();
        let context_json = context.as_ref().map(|m| serde_json::to_value(m).unwrap_or(Value::Null));
        let default_messages = match &context_json {
            None => claimed_json.clone(),
            Some(c) => {
                let mut v = claimed_json.clone();
                v.push(c.clone());
                v
            }
        };
        let innermost: NextFn = Rc::new(move |_p| json!({ "kind": "enter", "messages": default_messages.clone() }));
        let payload = json!({ "messages": claimed_json, "turn": turn, "step": step });
        let decision_v = self.waterfall_safe("agent/pre-step", payload, innermost)?;
        if let Some(c) = self.abort_reason() {
            return Err(Halt::Aborted(c));
        }
        PreStepDecision::from_value(&decision_v, &assembly)
    }

    fn dispatch_turn_stopping(&self, turn: u64) -> Result<(), Halt> {
        let innermost: NextFn = Rc::new(|p| p);
        let _ = self.serial_safe("agent/turn-stopping", json!({ "turn": turn }), innermost)?;
        Ok(())
    }

    fn dispatch_request_error(
        &self,
        turn: u64,
        step: u64,
        provider: &str,
        failure: &LlmFailure,
        _has_prepared: bool,
    ) -> Result<bool, Halt> {
        // retryPolicy：M2e-2 未序列化（D-032 声明；M2e-3 接 prepared.retryPolicy）
        let payload = json!({ "turn": turn, "step": step, "provider": provider, "failure": failure });
        let innermost: NextFn = Rc::new(|_p| Value::Null);
        let v = self.waterfall_safe("agent/request-error", payload, innermost)?;
        if let Some(c) = self.abort_reason() {
            return Err(Halt::Aborted(c));
        }
        Ok(v.get("kind").and_then(Value::as_str) == Some("retry"))
    }

    /// 执行一个步骤：守恒 buildRequest → 流式 → 组装 → 消息落盘 → 工具。
    /// 返回 `None` = 继续（tool 续步）；`Some(reason)` = 本 step 关闭。
    fn step(&self, turn: u64, step: u64, assembly: &PromptAssembly) -> Result<Option<TurnEndReason>, Halt> {
        if !matches!(*self.phase.borrow(), Phase::Running { .. }) {
            return Err(Halt::Failed(unknown_failure(format!(
                "agent \"{}\": step outside running phase",
                self.agent.id
            ))));
        }
        if let Some(c) = self.abort_reason() {
            return Err(Halt::Aborted(c));
        }
        let system = render_prompt(assembly).map_err(|e| Halt::Failed(unknown_failure(e)))?;
        loop {
            let boundary = self
                .agent
                .session
                .derive_messages()
                .map_err(|e| Halt::Failed(unknown_failure(e.0)))?;
            let mut built = build_request(
                &self.agent.session,
                &self.agent.options,
                self.request_header_logged.get(),
                &assembly.tools,
                &system,
                boundary,
                turn,
                step,
                &*self.propose,
                &*self.deps.prepare_call,
            )
            .map_err(|e| Halt::Failed(unknown_failure(e)))?;
            self.request_header_logged.set(built.request_header_logged);
            let request = built.request.options().clone();
            if let Some(c) = self.abort_reason() {
                return Err(Halt::Aborted(c));
            }
            let mut assembler = BlockAssembler::new();
            let mut chunk_seqs: Vec<u64> = Vec::new();
            // `preparedCall?.stream(request) ?? llm.stream(request)`：prepared 派发优先。
            let stream_result = match built.prepared_call.as_mut().and_then(|p| p.stream.take()) {
                Some(mut ps) => ps(request.clone()).map(|it| it.collect()),
                None => (self.deps.stream)(&request),
            };
            let chunks = match stream_result {
                Ok(chunks) => chunks,
                Err(e) => {
                    if let Some(c) = self.abort_reason() {
                        self.finalize_interrupted(turn, step, &request, &mut assembler, &chunk_seqs);
                        return Err(Halt::Aborted(c));
                    }
                    return Err(Halt::Failed(e.failure));
                }
            };
            for chunk in chunks {
                if let Some(c) = self.abort_reason() {
                    self.finalize_interrupted(turn, step, &request, &mut assembler, &chunk_seqs);
                    return Err(Halt::Aborted(c));
                }
                let seq = self
                    .agent
                    .session
                    .append(
                        EventKind::AssistantChunk,
                        json!({ "turn": turn, "step": step, "chunk": chunk.clone() }),
                        None,
                    )
                    .map_err(|e| Halt::Failed(unknown_failure(e.0)))?
                    .seq;
                chunk_seqs.push(seq);
                assembler.push(chunk);
            }
            if let Some(c) = self.abort_reason() {
                self.finalize_interrupted(turn, step, &request, &mut assembler, &chunk_seqs);
                return Err(Halt::Aborted(c));
            }
            let finish = assembler.finish();
            if let FinishReason::Error { failure } | FinishReason::Aborted { failure } = &finish {
                let failure = failure.clone();
                let retry =
                    self.dispatch_request_error(turn, step, &request.provider, &failure, built.prepared_call.is_some())?;
                if retry {
                    continue; // 丢弃失败 attempt 的 chunks（已登录但无 assistant/message 关闭）
                }
                return Err(Halt::Failed(failure));
            }
            let blocks = assembler.blocks();
            let replay = assembler
                .replay_state()
                .map(|r| serde_json::to_value(r).unwrap_or(Value::Null));
            let usage = assembler.usage().cloned();
            let message = Message {
                id: MessageId::from_raw(format!("assistant-{}-{}", turn, step)),
                role: Role::Assistant,
                content: blocks,
                source: MessageSource::Model(ModelMessageSource {
                    provider: request.provider.clone(),
                    model: request.model.clone(),
                    replay_state: replay,
                }),
            };
            let mut payload = json!({ "turn": turn, "step": step, "message": message });
            if let Some(u) = &usage {
                payload["usage"] = serde_json::to_value(u).unwrap_or(Value::Null);
            }
            self.agent
                .session
                .append(
                    EventKind::AssistantMessage,
                    payload,
                    Some(&SurfaceIntent {
                        surface_op: SurfaceOp::Append,
                        source_event_seqs: Some(chunk_seqs.clone()),
                    }),
                )
                .map_err(|e| Halt::Failed(unknown_failure(e.0)))?;
            if matches!(finish, FinishReason::MaxTokens) {
                return Ok(Some(TurnEndReason::MaxTokens));
            }
            let tool_calls: Vec<ToolCallBlock> = message
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolCall(t) => Some(t.clone()),
                    _ => None,
                })
                .collect();
            if tool_calls.is_empty() {
                return Ok(Some(TurnEndReason::Completed));
            }
            let outcome = (self.deps.tool_exec)(&ToolExecCtx {
                turn,
                step,
                tool_calls: &tool_calls,
            });
            for context_msg in outcome.context {
                let len = self.agent.inbox.next_step().len();
                self.agent
                    .inbox
                    .splice(InboxTarget::NextStep, len as f64, 0.0, vec![context_msg])
                    .map_err(|e| Halt::Failed(unknown_failure(e)))?;
            }
            if let Some(c) = self.abort_reason() {
                return Err(Halt::Aborted(c));
            }
            return Ok(if outcome.concluded {
                Some(TurnEndReason::Completed)
            } else {
                None
            });
        }
    }

    /// 中断前缀落盘（`interrupted:true` 的 assistant/message，仅引用自己的 chunk seqs）。
    fn finalize_interrupted(
        &self,
        turn: u64,
        step: u64,
        request: &GenerateOptions,
        assembler: &mut BlockAssembler,
        chunk_seqs: &[u64],
    ) {
        let content = assembler.interrupted_blocks();
        if content.is_empty() {
            return;
        }
        let message = Message {
            id: MessageId::from_raw(format!("assistant-{}-{}", turn, step)),
            role: Role::Assistant,
            content,
            source: MessageSource::Model(ModelMessageSource {
                provider: request.provider.clone(),
                model: request.model.clone(),
                replay_state: None,
            }),
        };
        let mut payload = json!({ "turn": turn, "step": step, "message": message, "interrupted": true });
        if let Some(u) = assembler.usage() {
            payload["usage"] = serde_json::to_value(u).unwrap_or(Value::Null);
        }
        let _ = self.agent.session.append(
            EventKind::AssistantMessage,
            payload,
            Some(&SurfaceIntent {
                surface_op: SurfaceOp::Append,
                source_event_seqs: Some(chunk_seqs.to_vec()),
            }),
        );
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn last_turn_of(session: &Rc<dsh_session::Session>) -> u64 {
    session
        .events()
        .into_iter()
        .rev()
        .find(|e| e.kind == EventKind::TurnStart)
        .and_then(|e| e.data.get("turn").and_then(Value::as_u64))
        .unwrap_or(0)
}

fn cancel_to_turn(c: &AgentCancelCause) -> TurnEndCancelCause {
    match c {
        AgentCancelCause::User => TurnEndCancelCause::User,
        AgentCancelCause::Parent => TurnEndCancelCause::Parent,
        AgentCancelCause::Hook { reason } => TurnEndCancelCause::Hook { reason: reason.clone() },
        AgentCancelCause::Disposed => TurnEndCancelCause::Disposed,
    }
}

fn panic_message(e: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = e.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = e.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_string()
    }
}

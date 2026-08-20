//! 会话事件日志的关系不变量校验（对齐
//! `deepseek-harness/packages/core/session/src/invariant.ts` 的 `validateEvent`/`apply`）。
//!
//! 校验 turn/step 序号单调性与 tool call 配对；纯函数、不持状态。
//! `SessionTrace` 是每会话账户，`validate_event` 原子的（不突变 committed trace），
//! `apply_transition` 在提交后落账。

use std::collections::HashSet;

use dsh_brand::CallId;

use crate::types::{EventKind, SessionEvent};

/// 每会话关系账户（供外部迭代式校验）。
#[derive(Debug, Clone)]
pub struct SessionTrace {
    /// 最后接受的 seq（TS 起于 -1 以允许首个事件 seq=0）。
    pub last_seq: i64,
    pub open_turn: Option<u64>,
    pub open_step: Option<u64>,
    pub next_turn: u64,
    pub next_step: u64,
    pub pending_calls: HashSet<CallId>,
}

impl Default for SessionTrace {
    fn default() -> Self {
        SessionTrace {
            last_seq: -1,
            open_turn: None,
            open_step: None,
            next_turn: 0,
            next_step: 0,
            pending_calls: HashSet::new(),
        }
    }
}

/// 一个已接受事件的提交后 deferred 迁移。
#[derive(Debug, Clone, Default)]
pub struct SessionTraceTransition {
    pub last_seq: i64,
    pub open_turn: Option<u64>,
    pub open_step: Option<u64>,
    pub next_turn: u64,
    pub next_step: u64,
    pub pending_calls: PendingCallsChange,
}

/// pendingCalls 的 deferred 变更。
#[derive(Debug, Clone, Default)]
pub enum PendingCallsChange {
    #[default]
    None,
    Add(CallId),
    Delete(CallId),
    Clear,
}

impl SessionTrace {
    pub fn new() -> Self {
        SessionTrace::default()
    }
}

/// 校验一个候选事件（不突变）。返回违规消息或可提交的迁移。
pub fn validate_event(
    trace: &SessionTrace,
    event: &SessionEvent,
) -> Result<SessionTraceTransition, String> {
    if event.seq as i64 <= trace.last_seq {
        return Err(format!(
            "seq must strictly increase: saw {} after {}",
            event.seq, trace.last_seq
        ));
    }
    let mut open_turn = trace.open_turn;
    let mut open_step = trace.open_step;
    let mut next_turn = trace.next_turn;
    let mut next_step = trace.next_step;
    let mut pending_calls: PendingCallsChange = PendingCallsChange::None;

    match event.kind {
        EventKind::TurnStart => {
            if let Some(open) = trace.open_turn {
                return Err(format!(
                    "turn/start {} while turn {open} is still open",
                    turn_of(event)
                ));
            }
            let turn = turn_of(event);
            if Some(turn) != Some(trace.next_turn) {
                return Err(format!(
                    "turn/start expected turn {}, got {}",
                    trace.next_turn,
                    turn_of(event)
                ));
            }
            open_turn = Some(turn);
            next_step = 1;
        }
        EventKind::TurnEnd => {
            let turn = turn_of(event);
            if Some(turn) != trace.open_turn {
                return Err(format!(
                    "turn/end {} does not match open turn {}",
                    turn,
                    trace.open_turn.map(|t| t.to_string()).unwrap_or_else(|| "none".into())
                ));
            }
            if let Some(open) = trace.open_step {
                return Err(format!(
                    "turn/end {} while step {open} is still open",
                    turn
                ));
            }
            open_turn = None;
            next_turn += 1;
        }
        EventKind::StepStart => {
            if Some(turn_of(event)) != trace.open_turn {
                return Err(format!(
                    "step/start in turn {} but open turn is {}",
                    turn_of(event),
                    trace.open_turn.map(|t| t.to_string()).unwrap_or_else(|| "none".into())
                ));
            }
            if let Some(open) = trace.open_step {
                return Err(format!(
                    "step/start {} while step {open} is still open",
                    step_of(event)
                ));
            }
            if step_of(event) != trace.next_step {
                return Err(format!(
                    "step/start expected step {} in turn {}, got {}",
                    trace.next_step,
                    turn_of(event),
                    step_of(event)
                ));
            }
            open_step = Some(step_of(event));
            next_step += 1;
        }
        EventKind::StepEnd => {
            if Some(turn_of(event)) != trace.open_turn {
                return Err(format!(
                    "step/end in turn {} but open turn is {}",
                    turn_of(event),
                    trace.open_turn.map(|t| t.to_string()).unwrap_or_else(|| "none".into())
                ));
            }
            if step_of(event) != trace.open_step.unwrap_or(u64::MAX) {
                return Err(format!(
                    "step/end {} does not match open step {}",
                    step_of(event),
                    trace.open_step.map(|s| s.to_string()).unwrap_or_else(|| "none".into())
                ));
            }
            pending_calls = PendingCallsChange::Clear;
            open_step = None;
        }
        EventKind::AssistantMessage => {
            if Some(turn_of(event)) != trace.open_turn {
                return Err(format!(
                    "assistant/message in turn {} but open turn is {}",
                    turn_of(event),
                    trace.open_turn.map(|t| t.to_string()).unwrap_or_else(|| "none".into())
                ));
            }
            if step_of(event) != trace.open_step.unwrap_or(u64::MAX) {
                return Err(format!(
                    "assistant/message in step {} but open step is {}",
                    step_of(event),
                    trace.open_step.map(|s| s.to_string()).unwrap_or_else(|| "none".into())
                ));
            }
            // assistant 消息携带 tool-call blocks；每个 pending 直到对应 tool/result
            // （M1：不在此记录 pending——repair 决定；invariant 只校验 step 归属）
        }
        EventKind::ToolResult => {
            if Some(turn_of(event)) != trace.open_turn {
                return Err(format!(
                    "tool/result in turn {} but open turn is {}",
                    turn_of(event),
                    trace.open_turn.map(|t| t.to_string()).unwrap_or_else(|| "none".into())
                ));
            }
            if step_of(event) != trace.open_step.unwrap_or(u64::MAX) {
                return Err(format!(
                    "tool/result in step {} but open step is {}",
                    step_of(event),
                    trace.open_step.map(|s| s.to_string()).unwrap_or_else(|| "none".into())
                ));
            }
            if let Some(call_id) = tool_result_call_id(event) {
                if !trace.pending_calls.contains(&call_id) {
                    return Err(format!("tool/result references unknown call {}", call_id));
                }
                pending_calls = PendingCallsChange::Delete(call_id);
            }
        }
        _ => {}
    }

    Ok(SessionTraceTransition {
        last_seq: event.seq as i64,
        open_turn,
        open_step,
        next_turn,
        next_step,
        pending_calls,
    })
}

/// 把一个已校验迁移提交到 committed trace（在事件被接受后）。
pub fn apply_transition(trace: &mut SessionTrace, transition: SessionTraceTransition) {
    trace.last_seq = transition.last_seq;
    trace.open_turn = transition.open_turn;
    trace.open_step = transition.open_step;
    trace.next_turn = transition.next_turn;
    trace.next_step = transition.next_step;
    match transition.pending_calls {
        PendingCallsChange::None => {}
        PendingCallsChange::Add(id) => {
            trace.pending_calls.insert(id);
        }
        PendingCallsChange::Delete(id) => {
            trace.pending_calls.remove(&id);
        }
        PendingCallsChange::Clear => {
            trace.pending_calls.clear();
        }
    }
}

/// 便捷：校验并提交一条事件（迭代器式用法）。
pub fn accept(trace: &mut SessionTrace, event: &SessionEvent) -> Result<(), String> {
    let transition = validate_event(trace, event)?;
    apply_transition(trace, transition);
    Ok(())
}

fn turn_of(event: &SessionEvent) -> u64 {
    event.data.get("turn").and_then(serde_json::Value::as_u64).unwrap_or(u64::MAX)
}

fn step_of(event: &SessionEvent) -> u64 {
    event.data.get("step").and_then(serde_json::Value::as_u64).unwrap_or(u64::MAX)
}

fn tool_result_call_id(event: &SessionEvent) -> Option<CallId> {
    event
        .data
        .get("message")
        .and_then(|m| m.get("source"))
        .and_then(|s| s.get("callId"))
        .and_then(serde_json::Value::as_str)
        .map(CallId::from_raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(seq: u64, kind: EventKind, data: serde_json::Value) -> SessionEvent {
        SessionEvent::new(seq, 1000, kind, data)
    }

    fn turn_start(seq: u64, turn: u64) -> SessionEvent {
        ev(seq, EventKind::TurnStart, json!({"turn": turn}))
    }
    fn turn_end(seq: u64, turn: u64) -> SessionEvent {
        ev(seq, EventKind::TurnEnd, json!({"turn": turn, "reason": {"kind": "complete"}}))
    }
    fn step_start(seq: u64, turn: u64, step: u64) -> SessionEvent {
        ev(seq, EventKind::StepStart, json!({"turn": turn, "step": step}))
    }
    fn step_end(seq: u64, turn: u64, step: u64) -> SessionEvent {
        ev(seq, EventKind::StepEnd, json!({"turn": turn, "step": step}))
    }

    #[test]
    fn balanced_turn_sequence_passes() {
        let mut trace = SessionTrace::new();
        let events = vec![
            turn_start(0, 0),
            step_start(1, 0, 1),
            ev(2, EventKind::UserMessage, json!({
                "turn": 0, "step": 1,
                "message": {"id": "u", "role": "user", "content": [], "source": {"kind": "user"}},
            })),
            step_end(3, 0, 1),
            turn_end(4, 0),
        ];
        for e in &events {
            accept(&mut trace, e).expect("balanced log must pass");
        }
        assert_eq!(trace.next_turn, 1);
    }

    #[test]
    fn non_monotonic_seq_rejected() {
        let mut trace = SessionTrace::new();
        accept(&mut trace, &turn_start(0, 0)).unwrap();
        accept(&mut trace, &turn_end(1, 0)).unwrap();
        // seq 回退
        let err = validate_event(&trace, &turn_start(1, 1)).unwrap_err();
        assert!(err.contains("strictly increase"));
    }

    #[test]
    fn turn_start_out_of_order_rejected() {
        let trace = SessionTrace::new();
        // 跳号（1 而不是 0）
        let err = validate_event(&trace, &turn_start(0, 1)).unwrap_err();
        assert!(err.contains("expected turn 0"));
    }

    #[test]
    fn turn_start_while_open_rejected() {
        let mut trace = SessionTrace::new();
        accept(&mut trace, &turn_start(0, 0)).unwrap();
        let err = validate_event(&trace, &turn_start(1, 1)).unwrap_err();
        assert!(err.contains("still open"));
    }

    #[test]
    fn turn_end_with_open_step_rejected() {
        let mut trace = SessionTrace::new();
        accept(&mut trace, &turn_start(0, 0)).unwrap();
        accept(&mut trace, &step_start(1, 0, 1)).unwrap();
        let err = validate_event(&trace, &turn_end(2, 0)).unwrap_err();
        assert!(err.contains("still open"));
    }

    #[test]
    fn step_out_of_order_rejected() {
        let mut trace = SessionTrace::new();
        accept(&mut trace, &turn_start(0, 0)).unwrap();
        // step 从 2 开始而非 1
        let err = validate_event(&trace, &step_start(1, 0, 2)).unwrap_err();
        assert!(err.contains("expected step 1"));
    }

    #[test]
    fn tool_result_unknown_call_rejected_and_completed_fine() {
        let mut trace = SessionTrace::new();
        accept(&mut trace, &turn_start(0, 0)).unwrap();
        accept(&mut trace, &step_start(1, 0, 1)).unwrap();
        // tool/result 引用未知 call → reject
        let result = ev(2, EventKind::ToolResult, json!({
            "turn": 0, "step": 1,
            "message": {"id": "t", "role": "user", "content": [], "source": {"kind": "tool", "callId": "c1"}},
        }));
        match validate_event(&trace, &result) {
            Err(msg) => assert!(msg.contains("unknown call")),
            Ok(_) => panic!("expected unknown-call rejection"),
        }
        // 注册 pending call 后通过
        trace.pending_calls.insert(CallId::from_raw("c1"));
        accept(&mut trace, &result).expect("registered call must pass");
        assert!(trace.pending_calls.is_empty());
    }
}

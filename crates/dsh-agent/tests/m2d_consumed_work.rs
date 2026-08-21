//! foldConsumedWork 行为测试（移植 consumed-work.spec.ts 全部场景）。

use dsh_agent::{fold_consumed_work, ConsumedWork};
use dsh_llm::LlmFailure;
use dsh_session::{
    EventKind, SessionEvent, StepStartPayload, TurnEndCancelCause, TurnEndPayload, TurnEndReason,
    TurnStartPayload,
};
use serde_json::{json, Value};

fn ev(seq: u64, kind: EventKind, data: Value) -> SessionEvent {
    SessionEvent::new(seq, 0, kind, data)
}

fn turn_start(seq: u64, turn: u64) -> SessionEvent {
    ev(seq, EventKind::TurnStart, serde_json::to_value(TurnStartPayload { turn }).unwrap())
}
fn step_start(seq: u64, turn: u64, step: u64) -> SessionEvent {
    ev(
        seq,
        EventKind::StepStart,
        serde_json::to_value(StepStartPayload { turn, step }).unwrap(),
    )
}
fn turn_end(seq: u64, turn: u64, reason: TurnEndReason) -> SessionEvent {
    ev(
        seq,
        EventKind::TurnEnd,
        serde_json::to_value(TurnEndPayload { turn, reason }).unwrap(),
    )
}
fn splice(seq: u64, removed_count: Option<u64>, outcome: Option<&str>, inserted: usize) -> SessionEvent {
    let mut data = json!({
        "target": "next-turn",
        "start": 0,
        "inserted": vec![json!({"id": format!("m{seq}"), "role": "user", "content": [], "source": {"kind": "user"}}); inserted],
    });
    if let Some(rc) = removed_count {
        data["removedCount"] = json!(rc);
    }
    if let Some(o) = outcome {
        data["outcome"] = json!(o);
    }
    ev(seq, EventKind::AgentInboxSpliced, data)
}

fn failure() -> LlmFailure {
    LlmFailure {
        message: "boom".into(),
        code: "UNKNOWN".into(),
        status: None,
        provider_retry_after_ms: None,
        request_id: None,
    }
}

fn aborted() -> TurnEndReason {
    TurnEndReason::Aborted {
        reason: TurnEndCancelCause::User,
    }
}

fn assert_end_kind(w: &ConsumedWork, kind: &str) {
    let end = w.end.as_ref().expect("expected accounting turn/end");
    assert_eq!(end.data["reason"]["kind"], json!(kind), "unexpected end: {}", end.data);
}

#[test]
fn accepts_without_claim_or_cancel_leaves_account_empty() {
    // 无任何 turn 结构：只有 accept（本测试以空账表示）
    let w = fold_consumed_work(&[]);
    assert!(w.end.is_none());
    assert!(!w.dropped_unrun);
}

#[test]
fn recent_stepped_turn_wins_over_earlier_completed() {
    // t1 stepped → completed；t2 stepped → max-tokens → end 取 t2 的 turn/end
    let events = vec![
        turn_start(0, 1),
        step_start(1, 1, 1),
        turn_end(2, 1, TurnEndReason::Completed),
        turn_start(3, 2),
        step_start(4, 2, 1),
        turn_end(5, 2, TurnEndReason::MaxTokens),
    ];
    let w = fold_consumed_work(&events);
    assert_end_kind(&w, "max-tokens");
    assert!(!w.dropped_unrun);
}

#[test]
fn claim_then_fail_before_any_step_accounts_the_turn() {
    // claim（无 outcome）+ turn/end error → 记账
    let events = vec![
        turn_start(0, 1),
        splice(1, Some(1), None, 0),
        turn_end(2, 1, TurnEndReason::Error { error: failure() }),
    ];
    let w = fold_consumed_work(&events);
    assert_end_kind(&w, "error");
    assert!(!w.dropped_unrun);
}

#[test]
fn claim_then_stopped_before_any_step_accounts_the_turn() {
    let events = vec![
        turn_start(0, 1),
        splice(1, Some(1), None, 0),
        turn_end(2, 1, aborted()),
    ];
    let w = fold_consumed_work(&events);
    assert_end_kind(&w, "aborted");
}

#[test]
fn unclaimed_stopped_or_failed_turn_is_ignored() {
    // 无 step 无 claim 的 stopped（aborted）/failed（error）/rejected（blocked）turn → 忽略
    for (reason, kind) in [
        (aborted(), "aborted"),
        (TurnEndReason::Error { error: failure() }, "error"),
        (TurnEndReason::Blocked, "blocked"),
    ] {
        let w = fold_consumed_work(&[turn_start(0, 1), turn_end(1, 1, reason)]);
        assert!(w.end.is_none(), "{kind} turn must be ignored without step/claim");
        assert!(!w.dropped_unrun, "{kind} turn must not report dropped");
    }
}

#[test]
fn claim_then_pre_step_rejection_blocked_accounts_the_turn() {
    let events = vec![
        turn_start(0, 1),
        splice(1, Some(1), None, 0),
        turn_end(2, 1, TurnEndReason::Blocked),
    ];
    let w = fold_consumed_work(&events);
    assert_end_kind(&w, "blocked");
}

#[test]
fn claim_then_turn_emptied_completed_is_ignored() {
    // claim 后 completed → 账不成立
    let events = vec![
        turn_start(0, 1),
        splice(1, Some(1), None, 0),
        turn_end(2, 1, TurnEndReason::Completed),
    ];
    let w = fold_consumed_work(&events);
    assert!(w.end.is_none(), "completed after claim must not account");
}

#[test]
fn claim_without_open_turn_accounts_nothing() {
    // owned suffix 中段开始：claim 时 open == undefined → 不记给任何 turn
    let events = vec![
        splice(0, Some(1), None, 0),
        turn_start(1, 1),
        turn_end(2, 1, TurnEndReason::Error { error: failure() }),
    ];
    let w = fold_consumed_work(&events);
    assert!(w.end.is_none(), "claim outside open turn must not account");
}

#[test]
fn cancel_pending_after_accounted_turn_sets_dropped_unrun_keeping_end() {
    let events = vec![
        turn_start(0, 1),
        step_start(1, 1, 1),
        turn_end(2, 1, TurnEndReason::Completed),
        splice(3, Some(1), Some("canceled"), 0),
    ];
    let w = fold_consumed_work(&events);
    assert_end_kind(&w, "completed");
    assert!(w.dropped_unrun);
}

#[test]
fn replacement_cancel_keeps_dropped_unrun_false() {
    // canceled 但 inserted 非空（替换）→ droppedUnrun=false
    let events = vec![
        turn_start(0, 1),
        step_start(1, 1, 1),
        turn_end(2, 1, TurnEndReason::Completed),
        splice(3, Some(1), Some("canceled"), 2),
    ];
    let w = fold_consumed_work(&events);
    assert_end_kind(&w, "completed");
    assert!(!w.dropped_unrun, "replacement keeps pending, not dropped");
}

#[test]
fn later_accounting_turn_absorbs_earlier_drop() {
    // 早期 canceled drop → 之后 claim + aborted 的 turn 成为最新 end 并复位 droppedUnrun
    let events = vec![
        splice(0, Some(1), Some("canceled"), 0),
        turn_start(1, 1),
        splice(2, Some(1), None, 0),
        turn_end(3, 1, aborted()),
    ];
    let w = fold_consumed_work(&events);
    assert_end_kind(&w, "aborted");
    assert!(!w.dropped_unrun, "later accounting turn absorbs the drop");
}

#[test]
fn removed_count_omitted_is_ignored_not_a_claim() {
    // removedCount 缺省 → 忽略（纯 accept，不记 claim）
    let events = vec![
        turn_start(0, 1),
        splice(1, None, None, 2),
        turn_end(2, 1, aborted()),
    ];
    let w = fold_consumed_work(&events);
    assert!(w.end.is_none(), "accept-only splice must not appear as claim");
}

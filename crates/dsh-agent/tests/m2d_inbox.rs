//! Inbox 行为测试（移植 agent.spec.ts 的 Inbox 部分，消息/事件/形状逐字）。

use std::rc::Rc;
use std::sync::{Arc, Mutex};

use dsh_agent::{
    inbox_splice, Inbox, InboxNotification, InboxSpliceOutcome, InboxSpliceRecord, InboxTarget,
};
use dsh_llm::{Message, MessageId};
use dsh_session::{
    store::SessionStore, CreateSessionMeta, CreateSessionOptions, EventKind, Session, SessionEvent,
    SessionId,
};
use serde_json::json;

fn store() -> Arc<SessionStore> {
    Arc::new(SessionStore::new())
}

fn msg(id: &str) -> Message {
    Message::user(MessageId(id.to_string()), vec![])
}

fn session(store: &Arc<SessionStore>, id: &str) -> Arc<Session> {
    session_with(store, id, None, None)
}

fn session_with(
    store: &Arc<SessionStore>,
    id: &str,
    seed: Option<Vec<SessionEvent>>,
    seed_length: Option<u64>,
) -> Arc<Session> {
    store
        .create(
            Some(SessionId(id.to_string())),
            &CreateSessionOptions {
                seed,
                meta: Some(CreateSessionMeta {
                    seed_length,
                    ..Default::default()
                }),
            },
        )
        .unwrap()
}

fn inbox_with_log(s: Arc<Session>) -> (Inbox, Arc<Mutex<Vec<InboxNotification>>>) {
    let log = Arc::new(Mutex::new(Vec::new()));
    let log2 = log.clone();
    let inbox = Inbox::with_notify(s, Rc::new(move |n| log2.lock().unwrap().push(n.clone()))).unwrap();
    (inbox, log)
}

fn spliced(
    seq: u64,
    target: &str,
    start: u64,
    removed_count: u64,
    inserted: Vec<Message>,
    outcome: Option<&str>,
) -> SessionEvent {
    let mut data = json!({
        "target": target,
        "start": start,
        "removedCount": removed_count,
        "inserted": inserted,
    });
    if let Some(o) = outcome {
        data["outcome"] = json!(o);
    }
    SessionEvent::new(seq, 0, EventKind::AgentInboxSpliced, data)
}

fn ids(messages: &[Message]) -> Vec<String> {
    messages.iter().map(|m| m.id.0.clone()).collect()
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum NKind {
    Claimed,
    Discarded,
    Inserted,
}

/// 从通知日志提取某一类的消息 id（返回 owned String，避免借用临时锁）。
fn notif_ids(log: &Mutex<Vec<InboxNotification>>, kind: NKind) -> Vec<String> {
    let guard = log.lock().unwrap();
    guard
        .iter()
        .filter_map(|n| match (kind, n) {
            (NKind::Claimed, InboxNotification::Claimed { message, .. }) => Some(message.id.0.clone()),
            (NKind::Discarded, InboxNotification::Discarded { message }) => Some(message.id.0.clone()),
            (NKind::Inserted, InboxNotification::Inserted { message }) => Some(message.id.0.clone()),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 构造 / 持久化重放
// ---------------------------------------------------------------------------

#[test]
fn reconstructs_from_persisted_splices() {
    // seed 中的 splice 事件以 seed_length=0（无持久边界）重放
    let s = session_with(
        &store(),
        "r",
        Some(vec![
            spliced(0, "next-turn", 0, 0, vec![msg("m1")], None),
            spliced(1, "next-step", 0, 0, vec![msg("m2"), msg("m3")], None),
        ]),
        Some(0),
    );
    let inbox = Inbox::new(s).unwrap();
    assert_eq!(inbox.next_turn(), vec![msg("m1")]);
    assert_eq!(inbox.next_step(), vec![msg("m2"), msg("m3")]);
    assert!(inbox.has_pending());
}

#[test]
fn rejects_invalid_persisted_splice() {
    // start 超出当前长度 → 重建包装为逐字消息（seed_length=0 重放 seq 0）
    let s = session_with(
        &store(),
        "bad",
        Some(vec![spliced(0, "next-turn", 5, 0, vec![msg("m1")], None)]),
        Some(0),
    );
    let err = Inbox::new(s).err().unwrap();
    assert_eq!(err, "invalid persisted inbox splice at session seq 0");
}

#[test]
fn empty_inbox_has_no_pending() {
    let inbox = Inbox::new(session(&store(), "e")).unwrap();
    assert!(!inbox.has_pending());
    assert!(inbox.next_turn().is_empty());
    assert!(inbox.next_step().is_empty());
}

// ---------------------------------------------------------------------------
// splice 坐标标准化（JS 算术）
// ---------------------------------------------------------------------------

#[test]
fn splice_normalizes_coordinates_like_js() {
    let s = session(&store(), "n");
    let (inbox, _log) = inbox_with_log(s.clone());
    // 空队列 append（start = len）
    inbox.append_msg(InboxTarget::NextTurn, msg("a")).unwrap();
    inbox.append_msg(InboxTarget::NextTurn, msg("b")).unwrap();
    inbox.append_msg(InboxTarget::NextTurn, msg("c")).unwrap();
    assert_eq!(inbox.next_turn().len(), 3);
    // 负 start：-1 删尾部 1 条
    inbox.splice(InboxTarget::NextTurn, -1.0, 1.0, vec![]).unwrap();
    assert_eq!(ids(&inbox.next_turn()), vec!["a", "b"]);
    // 越界 start 被上界截断（不删）
    inbox.splice(InboxTarget::NextTurn, 999.0, 1.0, vec![]).unwrap();
    assert_eq!(inbox.next_turn().len(), 2);
    // NaN start → 0；在队首插入
    inbox.splice(InboxTarget::NextTurn, f64::NAN, 0.0, vec![msg("x")]).unwrap();
    assert_eq!(inbox.next_turn()[0].id.0.as_str(), "x");
    // remove 缺失 id → false（不写事件）
    let before = s.seq();
    assert!(!inbox.remove(&MessageId("missing".into())).unwrap());
    assert_eq!(s.seq(), before, "missing remove must not write");
    // append 重复身份（已在另一队列）→ 抛
    inbox.append_msg(InboxTarget::NextStep, msg("dup")).unwrap();
    let err = inbox.append_msg(InboxTarget::NextTurn, msg("dup")).unwrap_err();
    assert_eq!(err, "message \"dup\" is already pending");
}

#[test]
fn splice_zero_zero_writes_no_event() {
    let s = session(&store(), "z");
    let inbox = Inbox::new(s.clone()).unwrap();
    let before = s.seq();
    inbox.splice(InboxTarget::NextTurn, 0.0, 0.0, vec![]).unwrap();
    assert_eq!(s.seq(), before, "0-delete 0-insert must not append");
}

// ---------------------------------------------------------------------------
// replace / remove 跨双队列身份
// ---------------------------------------------------------------------------

#[test]
fn replace_by_identity_across_queues() {
    let s = session(&store(), "x");
    let (inbox, log) = inbox_with_log(s.clone());
    let new_b = msg("b");
    // 在 next-step 队列放置 a
    inbox.append_msg(InboxTarget::NextStep, msg("a")).unwrap();
    // missing → false
    assert!(!inbox.replace(&MessageId("nope".into()), new_b.clone()).unwrap());
    // 找到并替换：discarded=[a]（next-step），inserted=[b]
    log.lock().unwrap().clear();
    assert!(inbox.replace(&MessageId("a".into()), new_b.clone()).unwrap());
    assert_eq!(ids(&inbox.next_step()), vec!["b"]);
    assert_eq!(notif_ids(&log, NKind::Discarded), vec!["a"]);
    assert_eq!(notif_ids(&log, NKind::Inserted), vec!["b"]);
    // 替换成 repeat 身份（另一队列已有 b）→ 抛
    inbox.append_msg(InboxTarget::NextStep, msg("a2")).unwrap();
    let err = inbox.replace(&MessageId("a2".into()), new_b.clone()).unwrap_err();
    assert_eq!(err, "message \"b\" is already pending");
}

#[test]
fn remove_by_identity() {
    let s = session(&store(), "rm");
    let (inbox, log) = inbox_with_log(s.clone());
    inbox.append_msg(InboxTarget::NextTurn, msg("a")).unwrap();
    assert!(!inbox.remove(&MessageId("missing".into())).unwrap());
    log.lock().unwrap().clear();
    assert!(inbox.remove(&MessageId("a".into())).unwrap());
    assert!(inbox.next_turn().is_empty());
    assert_eq!(notif_ids(&log, NKind::Discarded), vec!["a"]);
}

// ---------------------------------------------------------------------------
// clear / claim 顺序与持久事件
// ---------------------------------------------------------------------------

#[test]
fn clear_discards_next_step_then_next_turn_and_writes_canceled_events() {
    let s = session(&store(), "c");
    let (inbox, log) = inbox_with_log(s.clone());
    inbox.prepend_msg(InboxTarget::NextStep, msg("s1")).unwrap();
    inbox.append_msg(InboxTarget::NextTurn, msg("t1")).unwrap();
    inbox.append_msg(InboxTarget::NextTurn, msg("t2")).unwrap();
    log.lock().unwrap().clear();
    let before = s.seq();
    inbox.clear().unwrap();
    assert!(!inbox.has_pending());
    // 持久事件：next-step 在前
    let events = s.events();
    let splices: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e.seq >= before && matches!(e.kind, EventKind::AgentInboxSpliced))
        .map(|e| &e.data)
        .collect();
    assert_eq!(splices.len(), 2, "one event per queue");
    assert_eq!(
        *splices[0],
        json!({"target": "next-step", "start": 0, "removedCount": 1, "inserted": [], "outcome": "canceled"})
    );
    assert_eq!(
        *splices[1],
        json!({"target": "next-turn", "start": 0, "removedCount": 2, "inserted": [], "outcome": "canceled"})
    );
    // 丢弃通知顺序：next-step 先，next-turn 后
    assert_eq!(notif_ids(&log, NKind::Discarded), vec!["s1", "t1", "t2"]);
    // 空后再 clear 不新增事件（0 删 0 插 → no-op）
    let after_first_clear = s.seq();
    inbox.clear().unwrap();
    assert_eq!(s.seq(), after_first_clear, "second clear on empty inbox must not append");
}

#[test]
fn claim_takes_all_next_step_then_front_turn() {
    let s = session(&store(), "cl");
    let (inbox, log) = inbox_with_log(s.clone());
    inbox.append_msg(InboxTarget::NextStep, msg("s1")).unwrap();
    inbox.append_msg(InboxTarget::NextStep, msg("s2")).unwrap();
    inbox.append_msg(InboxTarget::NextTurn, msg("t1")).unwrap();
    inbox.append_msg(InboxTarget::NextTurn, msg("t2")).unwrap();
    log.lock().unwrap().clear();
    let result = inbox.claim(InboxTarget::NextTurn, 7).unwrap();
    // 返回：next-step 全取 + 队首 1 条 turn
    assert_eq!(ids(result.next_steps()), vec!["s1", "s2"]);
    assert_eq!(result.next_turn_front().unwrap().id.0.as_str(), "t1");
    // 投影：next-step 空；next-turn 只剩 t2
    assert!(inbox.next_step().is_empty());
    assert_eq!(ids(&inbox.next_turn()), vec!["t2"]);
    // 通知：全 claimed（s1,s2,t1）
    assert_eq!(notif_ids(&log, NKind::Claimed), vec!["s1", "s2", "t1"]);
    // 持久事件无 outcome（claim 不取消）、removedCount=1
    let splice_events = s.events();
    let splices: Vec<&SessionEvent> = splice_events
        .iter()
        .filter(|e| matches!(e.kind, EventKind::AgentInboxSpliced))
        .collect();
    let last = splices.last().unwrap();
    assert!(last.data["outcome"].is_null());
    assert_eq!(last.data["removedCount"], json!(1));
}

// ---------------------------------------------------------------------------
// wire 形状
// ---------------------------------------------------------------------------

#[test]
fn wire_shapes_omit_optional_fields() {
    // 默认 serialize：无 removedCount/outcome 时字段缺省即省略
    let rec = InboxSpliceRecord {
        target: InboxTarget::NextTurn,
        start: 0,
        removed_count: None,
        inserted: vec![msg("a")],
        outcome: None,
    };
    let json = serde_json::to_value(&rec).unwrap();
    assert_eq!(
        json,
        json!({"target": "next-turn", "start": 0, "inserted": [{"id": "a", "role": "user", "content": [], "source": {"kind": "user"}}]})
    );
}

#[test]
fn inbox_splice_helper_builds_record() {
    let rec = inbox_splice(InboxTarget::NextStep, 1, 0, vec![msg("x")], Some(InboxSpliceOutcome::Canceled));
    let json = serde_json::to_value(&rec).unwrap();
    // 0 删 → removedCount 缺省（wire 不出现，fold 视为纯插入）
    assert!(json.get("removedCount").is_none());
    assert_eq!(json["outcome"], json!("canceled"));
    assert_eq!(json["start"], json!(1));
    let deser: InboxSpliceRecord = serde_json::from_value(json).unwrap();
    assert_eq!(deser.target, InboxTarget::NextStep);
}

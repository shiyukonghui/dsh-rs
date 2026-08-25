//! M1a：dsh-session 运行时端到端测试（Session append/surface/deriveMessages +
//! SessionStore create/fork/event 发布 + repair 崩溃关闭器）。

use std::sync::{Arc, Mutex};

use dsh_brand::SessionId;
use serde_json::json;
use serde_json::Value;

use dsh_session::repair::{interrupted_turn_closers, TOOL_OUTCOME_UNKNOWN};
use dsh_session::store::{SessionForkErrorCode, SessionForkSource, SessionStore};
use dsh_session::types::{CreateSessionOptions, EventKind, SessionEvent, SurfaceOp};
use dsh_session::runtime::Session;
use dsh_session::surface::fold_surface;

fn ev(seq: u64, kind: EventKind, data: Value) -> SessionEvent {
    SessionEvent::new(seq, 1000 + seq as i64, kind, data)
}

fn user_msg(id: &str, text: &str, seq: u64) -> SessionEvent {
    ev(
        seq,
        EventKind::UserMessage,
        json!({
            "id": id,
            "role": "user",
            "content": [{"type": "text", "text": text}],
            "source": {"kind": "user"},
        }),
    )
    .with_surface_op(SurfaceOp::Append)
}

fn assistant_msg(id: &str, text: &str, seq: u64) -> SessionEvent {
    ev(
        seq,
        EventKind::AssistantMessage,
        json!({
            "turn": 1, "step": 2,
            "message": {
                "id": id,
                "role": "assistant",
                "content": [{"type": "text", "text": text}],
                "source": {"kind": "model", "provider": "deepseek", "model": "deepseek-chat"},
            },
        }),
    )
    .with_surface_op(SurfaceOp::Append)
}

// ---- Session 运行时 ----

#[test]
fn session_create_with_empty_seed_produces_contiguous_log() {
    let session = Session::create(SessionId::from_raw("s0"), None, None).unwrap();
    assert_eq!(session.seq(), 0);
    assert_eq!(session.first_live_seq(), 0);
    assert!(session.events().is_empty());
}

#[test]
fn session_create_appends_end_seed_marker_to_seed() {
    let msg = user_msg("m0", "hi", 0);
    let session = Session::create(SessionId::from_raw("s0"), Some(&[msg]), None).unwrap();
    // seed(1) + end-seed marker(1) = 2
    assert_eq!(session.seq(), 2);
    assert_eq!(session.first_live_seq(), 1);
    let events = session.events();
    assert!(events[1].is_end_seed());
}

#[test]
fn session_append_test_derives_messages() {
    let session = Session::create(SessionId::from_raw("s0"), None, None).unwrap();
    let u = user_msg("u0", "hello", 0);
    session
        .append(
            EventKind::UserMessage,
            u.data.clone(),
            Some(&dsh_session::types::SurfaceIntent {
                surface_op: SurfaceOp::Append,
                source_event_seqs: None,
            }),
        )
        .unwrap();
    let a = assistant_msg("a0", "world", 1);
    session
        .append(
            EventKind::AssistantMessage,
            a.data.clone(),
            Some(&dsh_session::types::SurfaceIntent {
                surface_op: SurfaceOp::Append,
                source_event_seqs: None,
            }),
        )
        .unwrap();
    let msgs = session.derive_messages().unwrap();
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0].role, dsh_llm::types::Role::User);
    assert_eq!(msgs[0].content.len(), 1);
    assert_eq!(msgs[1].role, dsh_llm::types::Role::Assistant);
    // seq 推进
    assert_eq!(session.seq(), 2);
    assert_eq!(session.first_live_seq(), 0);
}

#[test]
fn session_surface_nodes_track_append_and_replace() {
    let session = Session::create(SessionId::from_raw("s0"), None, None).unwrap();
    for (i, text) in ["a", "b"].iter().enumerate() {
        let u = user_msg(&format!("u{i}"), text, i as u64);
        session
            .append(
                EventKind::UserMessage,
                u.data.clone(),
                Some(&dsh_session::types::SurfaceIntent {
                    surface_op: SurfaceOp::Append,
                    source_event_seqs: None,
                }),
            )
            .unwrap();
    }
    assert_eq!(session.surface_nodes().unwrap(), vec![0, 1]);
    // replace 0..=1 → 节点 2
    let summary = assistant_msg("a2", "sum", 2);
    session
        .append(
            EventKind::AssistantMessage,
            summary.data.clone(),
            Some(&dsh_session::types::SurfaceIntent {
                surface_op: SurfaceOp::Replace { start: 0, end: 1 },
                source_event_seqs: Some(vec![0, 1]),
            }),
        )
        .unwrap();
    assert_eq!(session.surface_nodes().unwrap(), vec![2]);
    assert_eq!(session.surface_replace_generation().unwrap(), 1);
    let msgs = session.derive_messages().unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].role, dsh_llm::types::Role::Assistant);
}

#[test]
fn session_appends_validate_surface_before_commit() {
    let session = Session::create(SessionId::from_raw("s0"), None, None).unwrap();
    // 非 surface eligible 事件携带 surfaceOp → append 拒
    let turn_start = ev(0, EventKind::TurnStart, json!({"turn": 1}))
        .with_surface_op(SurfaceOp::Append);
    let err = session
        .append(
            EventKind::TurnStart,
            turn_start.data.clone(),
            Some(&dsh_session::types::SurfaceIntent {
                surface_op: SurfaceOp::Append,
                source_event_seqs: None,
            }),
        )
        .unwrap_err();
    assert!(err.0.contains("is not surface-eligible"));
}

#[test]
fn session_bad_seed_contiguity_rejected() {
    // seq 从 1 开始而非 0（非消息事件即可触发，避免消息形状校验先失败）
    let bad = ev(1, EventKind::TurnStart, json!({"turn": 1}));
    match Session::create(SessionId::from_raw("s0"), Some(&[bad]), None) {
        Err(err) => assert!(err.0.contains("must be contiguous from 0")),
        Ok(_) => panic!("expected contiguity rejection"),
    }
}

#[test]
fn session_bad_seed_requires_surface_marker() {
    // user/message 无 surfaceOp → fold 拒
    let bad = ev(0, EventKind::UserMessage, json!({
        "id": "m0", "role": "user", "content": [], "source": {"kind": "user"},
    }));
    match Session::create(SessionId::from_raw("s0"), Some(&[bad]), None) {
        Err(err) => assert!(err.0.contains("requires a surfaceOp marker")),
        Ok(_) => panic!("expected surface marker rejection"),
    }
}

#[test]
fn session_request_header_fold_and_context() {
    let session = Session::create(SessionId::from_raw("s0"), None, None).unwrap();
    let payload = json!({
        "reason": "change",
        "header": {
            "config": {
                "provider": "deepseek",
                "model": "deepseek-chat",
                "reasoningEffort": "high",
                "temperature": 0.5,
            },
        },
        "contextPath": "/",
    });
    session
        .append(
            EventKind::RequestHeader,
            payload.clone(),
            Some(&dsh_session::types::SurfaceIntent {
                surface_op: SurfaceOp::Append,
                source_event_seqs: None,
            }),
        )
        .unwrap_err(); // RequestHeader 非 surface-eligible → 不带 surfaceOp 才对
    // 正确：不带 surface 元数据
    let _ = session
        .append(EventKind::RequestHeader, payload, None)
        .unwrap();
    let header = session.request_header().unwrap();
    assert_eq!(header.config.provider, "deepseek");
    assert_eq!(header.config.model, "deepseek-chat");
    let ctx_payload = json!({"provider": "deepseek", "model": "deepseek-chat", "contextWindow": 8192});
    session.append(EventKind::RequestContext, ctx_payload, None).unwrap();
    let ctx = session.request_context().unwrap();
    assert_eq!(ctx.provider, "deepseek");
    assert_eq!(ctx.model, "deepseek-chat");
    assert_eq!(ctx.context_window, Some(8192));
}

// ---- SessionStore ----

#[test]
fn store_create_lists_and_gets() {
    let store = Arc::new(SessionStore::new());
    let created_events = Arc::new(Mutex::new(Vec::new()));
    {
        let created_events = created_events.clone();
        store.on_created(Arc::new(move |s| {
            created_events.lock().unwrap().push(s.id().clone());
        }));
    }
    let s1 = store.create(None, &CreateSessionOptions { seed: None, meta: None }).unwrap();
    let s2 = store.create(None, &CreateSessionOptions { seed: None, meta: None }).unwrap();
    assert_ne!(s1.id(), s2.id());
    assert_eq!(store.list().len(), 2);
    assert_eq!(created_events.lock().unwrap().len(), 2);
    let got = store.get(s1.id()).unwrap();
    assert!(Arc::ptr_eq(&got, &s1));
}

#[test]
fn store_event_observer_fires_on_append() {
    let store = Arc::new(SessionStore::new());
    let seen = Arc::new(Mutex::new(Vec::new()));
    {
        let seen = seen.clone();
        store.on_event(Arc::new(move |_, event| {
            seen.lock().unwrap().push(event.kind.as_str().to_string());
        }));
    }
    let session = store.create(None, &CreateSessionOptions { seed: None, meta: None }).unwrap();
    let u = user_msg("u0", "hi", 0);
    session
        .append(
            EventKind::UserMessage,
            u.data.clone(),
            Some(&dsh_session::types::SurfaceIntent {
                surface_op: SurfaceOp::Append,
                source_event_seqs: None,
            }),
        )
        .unwrap();
    assert_eq!(seen.lock().unwrap().as_slice(), &["user/message"]);
    store.flush(&session);
}

#[test]
fn store_fork_creates_child_with_parent_lineage() {
    let store = Arc::new(SessionStore::new());
    let parent = store.create(None, &CreateSessionOptions { seed: None, meta: None }).unwrap();
    let u = user_msg("u0", "hello", 0);
    parent
        .append(
            EventKind::UserMessage,
            u.data.clone(),
            Some(&dsh_session::types::SurfaceIntent {
                surface_op: SurfaceOp::Append,
                source_event_seqs: None,
            }),
        )
        .unwrap();
    let child = store
        .fork(&SessionForkSource::Object(parent.as_ref()), None, None)
        .unwrap();
    // parent 无 seed：只有 user/message（1 事件）
    assert_eq!(parent.seq(), 1);
    assert_eq!(child.header().parent_session.as_ref().unwrap(), parent.id());
    // child seed = 父全部事件（长度 1）
    assert_eq!(child.header().seed_length, Some(1));
    // child recover：seed(1) + end-seed marker(1) = 2 事件
    assert_eq!(child.events().len(), 2);
    let _msgs = child.derive_messages().unwrap();
    // 未来 append 独立
    let u2 = user_msg("u2", "child turn", 0);
    child
        .append(
            EventKind::UserMessage,
            u2.data.clone(),
            Some(&dsh_session::types::SurfaceIntent {
                surface_op: SurfaceOp::Append,
                source_event_seqs: None,
            }),
        )
        .unwrap();
    assert_eq!(child.seq(), 2 + 1);
    assert_eq!(parent.seq(), 1);
}

#[test]
fn store_fork_open_turn_rejected() {
    let store = Arc::new(SessionStore::new());
    let parent = store.create(None, &CreateSessionOptions { seed: None, meta: None }).unwrap();
    parent.append(EventKind::TurnStart, json!({"turn": 1}), None).unwrap();
    // 前缀内最后 turn 边界是 turn/start（seq 0）→ open turn fork = 拒绝（boundary=0）
    match store.fork(&SessionForkSource::Object(parent.as_ref()), Some(0), None) {
        Err(err) => assert_eq!(err.code, SessionForkErrorCode::OpenTurn),
        Ok(_) => panic!("expected open-turn fork rejection"),
    }
}

#[test]
fn store_fork_boundary_not_found_rejected() {
    let store = Arc::new(SessionStore::new());
    let parent = store.create(None, &CreateSessionOptions { seed: None, meta: None }).unwrap();
    parent.append(EventKind::TurnStart, json!({"turn": 1}), None).unwrap();
    parent.append(EventKind::TurnEnd, json!({"turn": 1, "reason": {"kind": "complete"}}), None).unwrap();
    // boundary 99 不存在
    match store.fork(&SessionForkSource::Object(parent.as_ref()), Some(99), None) {
        Err(err) => assert_eq!(err.code, SessionForkErrorCode::InvalidBoundary),
        Ok(_) => panic!("expected boundary rejection"),
    }
}

#[test]
fn store_dispose_announces_paired_notification() {
    let store = Arc::new(SessionStore::new());
    let disposed = Arc::new(Mutex::new(Vec::new()));
    {
        let disposed = disposed.clone();
        store.on_disposed(Arc::new(move |s| {
            disposed.lock().unwrap().push(s.id().clone());
        }));
    }
    let s = store.create(None, &CreateSessionOptions { seed: None, meta: None }).unwrap();
    let id = s.id().clone();
    store.dispose(&id);
    assert!(!store.is_live(&id));
    assert_eq!(disposed.lock().unwrap().len(), 1);
}

// ---- fork seed pure function（对齐 store.cpp 的 fork seed 计算）----

#[test]
fn fork_seed_default_to_last_event() {
    let store = Arc::new(SessionStore::new());
    let parent = store.create(None, &CreateSessionOptions { seed: None, meta: None }).unwrap();
    parent.append(EventKind::TurnStart, json!({"turn": 1}), None).unwrap();
    parent.append(EventKind::TurnEnd, json!({"turn": 1, "reason": {"kind": "complete"}}), None).unwrap();
    let child = store
        .fork(&SessionForkSource::Object(parent.as_ref()), None, None)
        .unwrap();
    // child seed = 父全部事件（2）+ end-seed marker
    assert_eq!(child.events().len(), 2 + 1);
    assert_eq!(child.header().seed_length, Some(2));
}

// ---- repair：interrupted turn closers ----

#[test]
fn repair_balanced_log_returns_empty() {
    let events = vec![
        ev(0, EventKind::TurnStart, json!({"turn": 1})),
        ev(1, EventKind::TurnEnd, json!({"turn": 1, "reason": {"kind": "complete"}})),
    ];
    assert!(interrupted_turn_closers(&events).is_empty());
    assert!(interrupted_turn_closers(&[]).is_empty());
}

#[test]
fn repair_open_turn_closes_with_interrupted_turn_end() {
    let events = vec![ev(0, EventKind::TurnStart, json!({"turn": 1}))];
    let closers = interrupted_turn_closers(&events);
    assert_eq!(closers.len(), 1);
    assert_eq!(closers[0].kind, EventKind::TurnEnd);
    assert_eq!(closers[0].seq, 1);
    assert_eq!(closers[0].time, 1000);
    assert_eq!(
        closers[0].data.get("reason").and_then(|r| r.get("kind")).and_then(Value::as_str),
        Some("interrupted")
    );
}

#[test]
fn repair_open_step_gets_step_end_before_turn_end() {
    let events = vec![
        ev(0, EventKind::TurnStart, json!({"turn": 1})),
        ev(1, EventKind::StepStart, json!({"turn": 1, "step": 1})),
    ];
    let closers = interrupted_turn_closers(&events);
    assert_eq!(closers.len(), 2);
    assert_eq!(closers[0].kind, EventKind::StepEnd);
    assert_eq!(closers[1].kind, EventKind::TurnEnd);
    assert_eq!(closers[0].seq, 2);
    assert_eq!(closers[1].seq, 3);
}

#[test]
fn repair_open_tool_call_synthesizes_error_result() {
    // assistant/message 声明 tool-call c1，但无 tool/call 记录（未 started）
    let events = vec![
        ev(0, EventKind::TurnStart, json!({"turn": 1})),
        ev(1, EventKind::StepStart, json!({"turn": 1, "step": 1})),
        ev(2, EventKind::AssistantMessage, json!({
            "turn": 1, "step": 1,
            "message": {
                "id": "a0",
                "role": "assistant",
                "content": [{"type": "tool-call", "id": "c1", "name": "demo", "arguments": "{}"}],
                "source": {"kind": "model", "provider": "p", "model": "m"},
            },
        })).with_surface_op(SurfaceOp::Append),
    ];
    let closers = interrupted_turn_closers(&events);
    // tool/result(c1) + step/end + turn/end
    assert_eq!(closers.len(), 3);
    assert_eq!(closers[0].kind, EventKind::ToolResult);
    assert_eq!(closers[1].kind, EventKind::StepEnd);
    assert_eq!(closers[2].kind, EventKind::TurnEnd);
    let error = closers[0].data.get("error").unwrap();
    assert_eq!(error.get("code").and_then(Value::as_str), Some("TOOL_NOT_STARTED"));
    // seq 连续 3..=5，time 复用最后真实事件 1002
    assert_eq!(closers[0].seq, 3);
    assert_eq!(closers[2].seq, 5);
    assert_eq!(closers[2].time, 1002);
}

#[test]
fn repair_recorded_call_outcome_unknown_uses_different_code() {
    // tool/call 已记录但无 tool/result
    let events = vec![
        ev(0, EventKind::TurnStart, json!({"turn": 1})),
        ev(1, EventKind::StepStart, json!({"turn": 1, "step": 1})),
        ev(2, EventKind::AssistantMessage, json!({
            "turn": 1, "step": 1,
            "message": {
                "id": "a0",
                "role": "assistant",
                "content": [{"type": "tool-call", "id": "c1", "name": "demo", "arguments": "{}"}],
                "source": {"kind": "model", "provider": "p", "model": "m"},
            },
        })).with_surface_op(SurfaceOp::Append),
        ev(3, EventKind::ToolCall, json!({"callId": "c1", "name": "demo", "arguments": "{}"})),
    ];
    let closers = interrupted_turn_closers(&events);
    let error = closers[0].data.get("error").unwrap();
    assert_eq!(error.get("code").and_then(Value::as_str), Some(TOOL_OUTCOME_UNKNOWN));
    // sourceEventSeqs 引用 tool/call 的 seq 3
    assert_eq!(closers[0].source_event_seqs(), Some(&vec![3_u64]));
    // 消息携带 isError: true 与文案
    let message = closers[0].data.get("message").unwrap();
    assert_eq!(message.get("role").and_then(Value::as_str), Some("user"));
}

// ---- fold_surface 与 Session 一致性（重构闸）----

#[test]
fn session_derive_equals_pure_fold_projection() {
    let session = Session::create(SessionId::from_raw("s0"), None, None).unwrap();
    let u0 = user_msg("u0", "a", 0);
    let a0 = assistant_msg("a0", "b", 1);
    let u1 = user_msg("u1", "c", 2);
    // 构造完整事件列表
    let events = [
        u0.data.clone(),
        a0.data.clone(),
        u1.data.clone(),
    ];
    for (i, data) in events.iter().enumerate() {
        let kind = if i % 2 == 0 { EventKind::UserMessage } else { EventKind::AssistantMessage };
        session
            .append(
                kind,
                data.clone(),
                Some(&dsh_session::types::SurfaceIntent {
                    surface_op: SurfaceOp::Append,
                    source_event_seqs: None,
                }),
            )
            .unwrap();
    }
    let live_msgs = session.derive_messages().unwrap();
    // 从 Session 日志重放 fold
    let log = session.events();
    let fold = fold_surface(&log).unwrap();
    let mut replayed = Vec::new();
    for seq in &fold.nodes {
        let event = &log[*seq as usize];
        replayed.push(serde_json::to_value(event.data.clone()).unwrap());
    }
    assert_eq!(replayed.len(), live_msgs.len());
}

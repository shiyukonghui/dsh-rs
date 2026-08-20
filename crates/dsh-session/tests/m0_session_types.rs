//! dsh-session types 契约测试。
//!
//! 权威参考：`deepseek-harness/packages/core/session/src/{types,known-event-types}.ts`。
//! 断言：① 48 词表与 TS `KNOWN_SESSION_EVENT_TYPES` 完全一致；② SessionEvent 信封
//! （strict envelope + wide data）JSON 往返与 TS 序列化一致；③ SurfaceOp/TurnEndReason/
//! SessionHeader 等 serde 形状；④ 未知必需事件 refuse / 未知可忽略 skip 读取语义。

use dsh_brand::{CallId, MessageId, SessionId};
use dsh_session::types::*;
use dsh_session::types::{
    EventKind, SessionEvent, SessionHeader, SurfaceOp, TurnEndCancelCause, TurnEndReason,
};

#[test]
fn event_type_vocabulary_matches_ts_known_set() {
    // 逐项对照 known-event-types.ts 的 KNOWN_SESSION_EVENT_TYPES（48 项，含 compaction+hook 合并扩展）
    let expected: &[&str] = &[
        "agent-preset/selected",
        "agent/inbox/spliced",
        "approval/asked",
        "approval/decided",
        "approval/policy",
        "assistant/chunk",
        "assistant/message",
        "command/done",
        "command/run",
        "compaction/end",
        "compaction/prune",
        "compaction/start",
        "compaction/summary",
        "feedback/record",
        "goal/change",
        "hook/invoked",
        "hook/result",
        "llm/retry",
        "llm/retry-started",
        "permission/preset",
        "plan/mode",
        "request/context",
        "request/header",
        "sandbox/mode",
        "schedule/change",
        "session/end-seed",
        "session/title",
        "session/title-llm-request",
        "step/end",
        "step/start",
        "subagent/descriptor",
        "team/member",
        "team/message/delivered",
        "team/message/queued",
        "team/task",
        "todo/write",
        "tool-workflow/agent-end",
        "tool-workflow/agent-start",
        "tool-workflow/run-end",
        "tool-workflow/run-start",
        "tool/call",
        "tool/code-dispatch",
        "tool/code-dispatch-start",
        "tool/result",
        "turn/end",
        "turn/start",
        "user/message",
        "web/deepseek-search-llm-request",
    ];
    assert_eq!(expected.len(), KNOWN_EVENT_TYPES.len());
    for (i, &name) in expected.iter().enumerate() {
        assert_eq!(KNOWN_EVENT_TYPES[i], name, "entry {i} mismatches");
    }
    // 每种已知类型都能解析为 EventKind 且不回落到 Unknown
    for &name in expected {
        let kind = EventKind::from_str(name);
        assert!(!matches!(kind, EventKind::Unknown(_)), "{name} must be a known variant");
        assert_eq!(kind.as_str(), name);
    }
}

#[test]
fn event_kind_unknown_extension_preserves_string() {
    let kind = EventKind::from_str("plugin/custom-event");
    assert!(matches!(kind, EventKind::Unknown(_)));
    assert_eq!(kind.as_str(), "plugin/custom-event");
}

#[test]
fn session_event_envelope_roundtrips_with_ts_shape() {
    // strict envelope + wide data（对齐 sessionEventSchema：type/seq/time/data 严格，data 宽）
    let raw = serde_json::json!({
        "type": "turn/start",
        "seq": 0,
        "time": 1723456789012_i64,
        "data": { "turn": 1 }
    });
    let event: SessionEvent = serde_json::from_value(raw.clone()).unwrap();
    assert_eq!(event.kind, EventKind::TurnStart);
    assert_eq!(event.seq, 0);
    assert_eq!(event.time, 1723456789012_i64);
    assert_eq!(
        event.as_turn_start().unwrap().unwrap().turn,
        1
    );
    let back = serde_json::to_value(&event).unwrap();
    assert_eq!(back, raw, "envelope must round-trip byte-identically");
}

#[test]
fn surface_eligible_envelope_carries_source_seqs_and_surface_op() {
    let event = SessionEvent::new(
        5,
        1000,
        EventKind::AssistantMessage,
        serde_json::json!({
            "turn": 1, "step": 0,
            "message": {
                "id": "m1", "role": "assistant", "content": [],
                "source": { "kind": "model", "provider": "p", "model": "m" }
            }
        }),
    )
    .with_surface_op(SurfaceOp::Append)
    .with_source_event_seqs(vec![2, 3]);
    let v = serde_json::to_value(&event).unwrap();
    assert_eq!(v["type"], serde_json::json!("assistant/message"));
    assert_eq!(v["surfaceOp"], serde_json::json!("append"));
    assert_eq!(v["sourceEventSeqs"], serde_json::json!([2, 3]));
    // 往返保留
    let back: SessionEvent = serde_json::from_value(v).unwrap();
    assert_eq!(back.surface_op(), Some(&SurfaceOp::Append));
    assert_eq!(back.source_event_seqs(), Some(&vec![2, 3]));
}

#[test]
fn surface_op_variants_roundtrip() {
    let append: SurfaceOp = serde_json::from_str(r#""append""#).unwrap();
    assert_eq!(append, SurfaceOp::Append);
    assert_eq!(serde_json::to_string(&append).unwrap(), r#""append""#);

    let replace: SurfaceOp = serde_json::from_str(r#"{"op":"replace","start":0,"end":2}"#).unwrap();
    assert_eq!(replace, SurfaceOp::Replace { start: 0, end: 2 });
    // 键序无关的语义等价（D-014 规范序）
    assert_eq!(
        serde_json::to_value(replace).unwrap(),
        serde_json::from_str::<serde_json::Value>(r#"{"op":"replace","start":0,"end":2}"#).unwrap()
    );
}

#[test]
fn turn_end_reason_variants_roundtrip() {
    let completed: TurnEndReason = serde_json::from_str(r#"{"kind":"completed"}"#).unwrap();
    assert_eq!(completed, TurnEndReason::Completed);
    assert_eq!(serde_json::to_string(&completed).unwrap(), r#"{"kind":"completed"}"#);

    let aborted: TurnEndReason =
        serde_json::from_str(r#"{"kind":"aborted","reason":{"kind":"user"}}"#).unwrap();
    assert_eq!(
        aborted,
        TurnEndReason::Aborted { reason: TurnEndCancelCause::User }
    );
    // legacy 取消原因（旧导入无 cause 记录）
    let legacy: TurnEndReason =
        serde_json::from_str(r#"{"kind":"aborted","reason":{"kind":"legacy"}}"#).unwrap();
    assert!(matches!(
        legacy,
        TurnEndReason::Aborted { reason: TurnEndCancelCause::Legacy }
    ));

    let err: TurnEndReason = serde_json::from_str(
        r#"{"kind":"error","error":{"message":"boom","code":"E"}}"#,
    )
    .unwrap();
    let TurnEndReason::Error { error } = &err else { panic!("expected error") };
    assert_eq!(error.code, "E");
    // 往返保形状（键序无关；D-014 规范序）
    assert_eq!(
        serde_json::to_value(&err).unwrap(),
        serde_json::from_str::<serde_json::Value>(
            r#"{"kind":"error","error":{"message":"boom","code":"E"}}"#
        )
        .unwrap()
    );

    let interrupted: TurnEndReason = serde_json::from_str(r#"{"kind":"interrupted"}"#).unwrap();
    assert_eq!(interrupted, TurnEndReason::Interrupted);
}

#[test]
fn request_header_folding_types_roundtrip() {
    // EpochHeader：config(provider/model) + adapterDefaults + system + tools
    let raw = serde_json::json!({
        "config": { "provider": "deepseek", "model": "deepseek-chat", "maxTokens": 4096 },
        "adapterDefaults": { "maxTokens": true },
        "system": "be helpful",
        "tools": [{ "name": "fs_read", "description": "read", "parameters": { "type": "object" } }]
    });
    let header: EpochHeader = serde_json::from_value(raw.clone()).unwrap();
    assert_eq!(header.config.provider, "deepseek");
    assert_eq!(header.tools.as_ref().unwrap().len(), 1);
    let back = serde_json::to_value(&header).unwrap();
    assert_eq!(back, raw);
}

#[test]
fn core_event_payloads_parse_from_wide_data() {
    let raw = serde_json::json!({
        "type": "tool/result",
        "seq": 7,
        "time": 1,
        "data": {
            "turn": 1, "step": 0, "callId": "c1",
            "message": {
                "id": "m9", "role": "user",
                "content": [{ "type": "tool-result", "toolCallId": "c1", "content": [{"type":"text","text":"ok"}] }],
                "source": { "kind": "tool", "callId": "c1" }
            },
            "meta": { "diff": "..." }
        }
    });
    let event: SessionEvent = serde_json::from_value(raw).unwrap();
    let tr = event.as_tool_result().unwrap().unwrap();
    assert_eq!(tr.turn, 1);
    assert_eq!(tr.message.source.as_tool().unwrap().call_id.raw(), "c1");
    assert_eq!(tr.meta.as_ref().unwrap()["diff"], serde_json::json!("..."));
}

#[test]
fn user_message_data_is_lossless_message() {
    let raw = serde_json::json!({
        "type": "user/message",
        "seq": 1,
        "time": 2,
        "data": {
            "id": "u1", "role": "user",
            "content": [{ "type": "text", "text": "hi" }],
            "source": { "kind": "user" }
        },
        "surfaceOp": "append"
    });
    let event: SessionEvent = serde_json::from_value(raw.clone()).unwrap();
    let msg = event.as_user_message().unwrap().unwrap();
    assert_eq!(msg.id.raw(), "u1");
    assert_eq!(msg.content[0].as_text().unwrap().text(), "hi");
    let back = serde_json::to_value(&event).unwrap();
    assert_eq!(back, raw);
}

#[test]
fn unknown_required_event_refuses_and_ignorable_skips() {
    // 未知类型且非 ignorable → 读取必须 refuse（对齐 coordinator.assertEventsSupported）
    let unknown = SessionEvent::new(0, 0, EventKind::from_str("future/required"), serde_json::json!({}));
    let result = validate_readable(&[unknown]);
    assert!(result.is_err(), "unknown required event must refuse reconstruction");

    // 未知类型但 ignorable:true → 放行（skip）
    let skippable = SessionEvent::new(0, 0, EventKind::from_str("future/optional"), serde_json::json!({}))
        .with_ignorable(true);
    assert!(validate_readable(&[skippable]).is_ok());

    // 已知类型 → 放行
    let known = SessionEvent::new(0, 0, EventKind::TurnEnd, serde_json::json!({"turn":1,"reason":{"kind":"completed"}}));
    assert!(validate_readable(&[known]).is_ok());
}

#[test]
fn surface_event_type_constants_match_ts() {
    assert_eq!(SURFACE_EVENT_TYPES, ["user/message", "assistant/message", "tool/result"]);
    assert!(is_surface_event_type("user/message"));
    assert!(!is_surface_event_type("turn/start"));
}

#[test]
fn session_header_roundtrips_with_optional_lineage() {
    let raw = serde_json::json!({
        "version": 0,
        "id": "sess-1",
        "createdAt": 1000,
        "cwd": "/home/u",
        "parentSession": "sess-0",
        "seedLength": 4,
        "origin": "subagent",
        "delegationDepth": 1,
        "agentPreset": "default"
    });
    let header: SessionHeader = serde_json::from_value(raw.clone()).unwrap();
    assert_eq!(header.version, 0);
    assert_eq!(header.id.raw(), "sess-1");
    assert_eq!(header.parent_session.as_ref().map(SessionId::raw), Some("sess-0"));
    assert_eq!(header.origin, Some(Origin::Subagent));
    assert_eq!(header.delegation_depth, Some(1));
    let back = serde_json::to_value(&header).unwrap();
    assert_eq!(back, raw);
}

#[test]
fn end_seed_event_is_empty_object() {
    let event = SessionEvent::end_seed(3, 99);
    assert!(event.is_end_seed());
    let v = serde_json::to_value(&event).unwrap();
    assert_eq!(v["type"], serde_json::json!("session/end-seed"));
    assert_eq!(v["data"], serde_json::json!({}));
}

#[test]
fn create_and_prepare_options_carry_header_meta() {
    let opts = CreateSessionOptions {
        seed: None,
        meta: Some(CreateSessionMeta {
            cwd: Some("/w".into()),
            parent_session: Some(SessionId::from_raw("p")),
            created_at: Some(5),
            seed_length: Some(2),
            origin: None,
            delegation_depth: None,
            agent_preset: None,
        }),
    };
    assert_eq!(opts.meta.as_ref().unwrap().cwd.as_deref(), Some("/w"));
    assert_eq!(opts.meta.as_ref().unwrap().seed_length, Some(2));
    // PrepareSessionOptions 可作饿（CreateSessionOptions 无 seedSource）或恢复（RestoredSessionOptions）
    let _restored: PrepareSessionOptions = PrepareSessionOptions::Restored(RestoredSessionOptions {
        seed: vec![SessionEvent::end_seed(0, 1)],
        meta: SessionHeader {
            version: 0,
            id: SessionId::from_raw("s"),
            created_at: 1,
            cwd: None,
            parent_session: None,
            seed_length: None,
            origin: None,
            delegation_depth: None,
            agent_preset: None,
        },
        seed_source: SeedSource,
    });
}

#[test]
fn todo_write_and_request_context_payloads() {
    let raw = serde_json::json!({
        "type": "todo/write",
        "seq": 2, "time": 3,
        "data": { "todos": [{ "content": "fix bug", "status": "in_progress" }] }
    });
    let event: SessionEvent = serde_json::from_value(raw).unwrap();
    let todos = event.as_todo_write().unwrap().unwrap();
    assert_eq!(todos.todos[0].content, "fix bug");
    assert_eq!(todos.todos[0].status, TodoStatus::InProgress);

    let rc: RequestContext = serde_json::from_str(
        r#"{"provider":"deepseek","model":"deepseek-chat","contextWindow":64000}"#,
    )
    .unwrap();
    assert_eq!(rc.context_window, Some(64000));
}

#[test]
fn session_id_and_message_id_are_distinct_brands() {
    let s = SessionId::from_raw("x");
    let m = MessageId::from_raw("x");
    assert_eq!(s.raw(), m.raw()); // 名义类型：字符串相同但类型身份不同
    let call = CallId::from_raw("c1");
    assert_eq!(call.raw(), "c1");
}

//! M1c：checkpoint 来源 + absorb 事件 wire 形状 / 落盘顺序。

mod common;
use common::*;

use dsh_brand::CompactionId;
use dsh_compaction::{
    checkpoint_message_source, commit_compaction_body, compact_checkpoint_source,
    compaction_start_payload, is_compact_checkpoint_source,
};
use dsh_llm::types::{ContentBlock, MessageId, MessageSource};
use dsh_session::types::EventKind;
use serde_json::{json, Value};

#[test]
fn checkpoint_source_shapes_wire() {
    let cid = CompactionId::from_raw("cid-123");
    let cps = compact_checkpoint_source(cid.clone(), Some("cmd-9".into()));
    let source = checkpoint_message_source(&cps);
    match &source {
        MessageSource::Plugin(p) => {
            assert_eq!(p.plugin(), "compact");
            assert_eq!(p.extra("compactionId"), Some(&json!("cid-123")));
            assert_eq!(p.extra("sourceCommandId"), Some(&json!("cmd-9")));
        }
        other => panic!("expected plugin source, got {other:?}"),
    }
    assert!(is_compact_checkpoint_source(&source));
    // 序列化 round-trip：插件 extra 无损
    let json_value = serde_json::to_value(&source).unwrap();
    let back: MessageSource = serde_json::from_value(json_value).unwrap();
    match &back {
        MessageSource::Plugin(p) => {
            assert_eq!(p.extra("compactionId"), Some(&json!("cid-123")));
        }
        other => panic!("round-trip failed: {other:?}"),
    }
}

#[test]
fn checkpoint_source_without_command_id() {
    let cid = CompactionId::from_raw("cid-1");
    let cps = compact_checkpoint_source(cid, None);
    let source = checkpoint_message_source(&cps);
    match &source {
        MessageSource::Plugin(p) => {
            assert!(p.extra("sourceCommandId").is_none());
            assert_eq!(p.extra("compactionId"), Some(&json!("cid-1")));
        }
        other => panic!("expected plugin source, got {other:?}"),
    }
}

#[test]
fn non_checkpoint_plugin_not_identified() {
    let source = MessageSource::Plugin(
        dsh_llm::types::PluginMessageSource::new("other-plugin"),
    );
    assert!(!is_compact_checkpoint_source(&source));
}

// ---- absorb payload wire 形状 ----

#[test]
fn start_payload_shapes() {
    let cid = CompactionId::from_raw("cid-a");
    let v = compaction_start_payload(&cid, Some("src"), Some(3));
    assert_eq!(v.get("compactionId").and_then(|x| x.as_str()), Some("cid-a"));
    assert_eq!(v.get("sourceCommandId").and_then(|x| x.as_str()), Some("src"));
    assert_eq!(v.get("turn").and_then(|x| x.as_u64()), Some(3));

    let v2 = compaction_start_payload(&cid, None, None);
    assert!(v2.get("sourceCommandId").is_none());
    assert_eq!(v2.get("turn"), Some(&Value::Null));
}

#[test]
fn summary_payload_shapes() {
    let cid = CompactionId::from_raw("cid-s");
    let summary = vec![ContentBlock::text("body")];
    let v = dsh_compaction::compaction_summary_payload(
        &cid,
        None,
        &summary,
        Some(&summary),
        false,
        dsh_compaction::ShadowedRange { start: 0, end: 4 },
        &[0, 1, 2, 3, 4],
        1234,
        "deepseek",
        "deepseek-chat",
        Some(8192),
        None,
    );
    assert_eq!(v.get("compactionId").and_then(|x| x.as_str()), Some("cid-s"));
    assert_eq!(v.get("shadowedRange").and_then(|x| x.get("start")).and_then(|x| x.as_u64()), Some(0));
    assert_eq!(v.get("shadowedRange").and_then(|x| x.get("end")).and_then(|x| x.as_u64()), Some(4));
    assert_eq!(v.get("shadowedSeqs").and_then(|x| x.as_array()).map(|a| a.len()), Some(5));
    assert_eq!(v.get("shadowedTokenCount").and_then(|x| x.as_u64()), Some(1234));
    assert_eq!(v.get("provider").and_then(|x| x.as_str()), Some("deepseek"));
    assert_eq!(v.get("model").and_then(|x| x.as_str()), Some("deepseek-chat"));
    assert_eq!(v.get("maxTokens").and_then(|x| x.as_u64()), Some(8192));
    assert_eq!(v.get("llmStreamCall").and_then(|x| x.as_bool()), Some(false));
    assert!(v.get("rawOutput").is_some());
}

#[test]
fn prune_payload_shapes() {
    let v = dsh_compaction::compaction_prune_payload(
        dsh_compaction::ShadowedRange { start: 2, end: 2 },
        &[2],
        999,
    );
    assert_eq!(v.get("shadowedRange").and_then(|x| x.get("start")).and_then(|x| x.as_u64()), Some(2));
    assert_eq!(v.get("shadowedSeqs").and_then(|x| x.as_array()).map(|a| a.len()), Some(1));
    assert_eq!(v.get("shadowedTokenCount").and_then(|x| x.as_u64()), Some(999));
}

// ---- commit_compaction_body：落盘顺序 + sourceEventSeqs ----

#[test]
fn commit_body_order_and_source_seq() {
    let session = new_session("s");
    // 表面 [0,1]：两个 user 消息
    append_surface(&session, EventKind::UserMessage, user_message_json("u0", "aaaa"));
    append_surface(&session, EventKind::UserMessage, user_message_json("u1", "bbbb"));
    // 先落 compaction/start（占锁，seq 2）
    let cid = CompactionId::from_raw("cid-c");
    let start_event = append_log_only(
        &session,
        EventKind::CompactionStart,
        compaction_start_payload(&cid, None, None),
    );
    let (summary_event, replacement) = commit_compaction_body(
        &session,
        start_event.seq,
        &cid,
        None,
        &[ContentBlock::text("summary")],
        vec![ContentBlock::text("this is a checkpoint sum")],
        Some(&[ContentBlock::text("summary")]),
        false,
        dsh_compaction::ShadowedRange { start: 0, end: 0 },
        &[0],
        500,
        "deepseek",
        "deepseek-chat",
        Some(8192),
        None,
        MessageId::from_raw("cp"),
    )
    .unwrap();
    // summary 在 replacement 之前
    assert!(summary_event.seq < replacement.seq);
    // replacement 的 sourceEventSeqs = start, summary, shadowed(0)
    assert_eq!(
        replacement.source_event_seqs(),
        Some(&vec![start_event.seq, summary_event.seq, 0])
    );
    // replacement 是一条 user/message，经 derive 是 checkpoint 来源（plugin='compact'）
    let derived = dsh_session::derive_event_message(&replacement)
        .unwrap()
        .unwrap();
    assert_eq!(derived.role, dsh_llm::types::Role::User);
    assert!(is_compact_checkpoint_source(&derived.source));
    // 替换节点出现在 surface 位置 0
    let nodes = session.surface_nodes().unwrap();
    assert_eq!(nodes, vec![replacement.seq, 1]);
    // 被替换的旧节点 seq 0 已遮蔽
    assert!(!nodes.contains(&0));
}

#[test]
fn end_payload_shapes() {
    let cid = CompactionId::from_raw("cid-e");
    let v = dsh_compaction::compaction_end_payload(&cid, Some("s"), Some(1), Some("boom"));
    assert_eq!(v.get("compactionId").and_then(|x| x.as_str()), Some("cid-e"));
    assert_eq!(v.get("sourceCommandId").and_then(|x| x.as_str()), Some("s"));
    assert_eq!(v.get("turn").and_then(|x| x.as_u64()), Some(1));
    assert_eq!(v.get("error").and_then(|x| x.as_str()), Some("boom"));

    let v2 = dsh_compaction::compaction_end_payload(&cid, None, None, None);
    assert!(v2.get("error").is_none());
    assert_eq!(v2.get("turn"), Some(&Value::Null));
}

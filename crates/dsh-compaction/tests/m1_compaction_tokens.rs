//! M1c：token 估算/测量/折叠（对齐 token-meter estimate.ts + surface-fold.ts + index.ts）。
//!
//! 固定启发式（与 TS 对齐）：CHARS_PER_TOKEN=4、ROLE_OVERHEAD=4、BLOCK_OVERHEAD=4。

mod common;
use common::*;

use dsh_compaction::{
    estimate_content, estimate_header, estimate_message, estimate_system_tokens,
    estimate_tools_tokens, frame_summary, measure,
};
use dsh_llm::types::ContentBlock;
use dsh_session::types::EventKind;
use serde_json::json;

fn text_len_chars(s: &str) -> u64 {
    s.encode_utf16().count() as u64
}

fn text_block_tokens(text: &str) -> u64 {
    text_len_chars(text).div_ceil(4) + 4
}

// ---- estimate_content / estimate_message ----

#[test]
fn estimate_text_block_rounds_utf16_to_tokens() {
    let blocks = vec![ContentBlock::text("hello")]; // 5 utf16
    assert_eq!(estimate_content(&blocks), 2 + 4); // ceil(5/4)=2 + overhead 4
    let msg = dsh_llm::types::Message::user(dsh_brand::MessageId::from_raw("m"), blocks);
    assert_eq!(estimate_message(&msg), 2 + 4 + 4); // + role overhead 4
}

#[test]
fn estimate_content_adds_block_overhead_per_block() {
    let blocks = vec![
        ContentBlock::text("abcd"),   // 4 utf16 → 1 + 4
        ContentBlock::text("abcdef"), // 6 utf16 → 2 + 4
    ];
    assert_eq!(estimate_content(&blocks), (1 + 4) + (2 + 4));
}

#[test]
fn estimate_empty_content_is_zero() {
    assert_eq!(estimate_content(&[]), 0);
    let msg = dsh_llm::types::Message::user(dsh_brand::MessageId::from_raw("m"), vec![]);
    assert_eq!(estimate_message(&msg), 4); // role overhead
}

#[test]
fn estimate_utf16_counts_surrogate_pairs_once() {
    // "😀" 是 2 个 utf16 code units（1 codepoint）→ ceil(2/4)=1 + 4
    let blocks = vec![ContentBlock::text("😀")];
    assert_eq!(estimate_content(&blocks), 1 + 4);
}

// ---- estimate_system / tools / header（无 header 时为零）----

#[test]
fn estimate_header_none_is_zero() {
    assert_eq!(estimate_system_tokens(None), 0);
    assert_eq!(estimate_tools_tokens(None), 0);
    assert_eq!(estimate_header(None), 0);
}

// ---- frame_summary ----

#[test]
fn frame_summary_wraps_checkpoint_tags() {
    let framed = frame_summary(&[ContentBlock::text("body")]);
    assert_eq!(framed.len(), 3);
    match &framed[0] {
        ContentBlock::Text(t) => {
            assert!(t.text.starts_with("This is an automatically generated checkpoint"))
        }
        _ => panic!("first block should be preamble text"),
    }
    match &framed[1] {
        ContentBlock::Text(t) => assert_eq!(t.text, "body"),
        _ => panic!("second block should be summary body"),
    }
    match &framed[2] {
        ContentBlock::Text(t) => assert_eq!(t.text, "</compacted-summary>"),
        _ => panic!("third block should be close tag"),
    }
    let msg = dsh_llm::types::Message::user(dsh_brand::MessageId::from_raw("m"), framed);
    assert!(estimate_message(&msg) > 0);
}

// ---- measure()（surface fold + usage-anchor）----

#[test]
fn measure_empty_session_is_none_baseline() {
    let session = new_session("s");
    let m = measure(&session.events()).unwrap();
    assert_eq!(m.total_tokens, 0);
    assert_eq!(m.surface_tokens, 0);
    assert!(m.nodes.is_empty());
}

#[test]
fn measure_counts_user_surface_tokens() {
    let session = new_session("s");
    append_surface(&session, EventKind::UserMessage, user_message_json("u0", "abcd"));
    let m = measure(&session.events()).unwrap();
    assert_eq!(m.nodes.len(), 1);
    assert_eq!(m.nodes[0].seq, 0);
    // 估算 message token = content + role overhead
    assert_eq!(m.nodes[0].tokens, text_block_tokens("abcd") + 4);
    // 无 anchor，非空 → estimated baseline = estimate_header(None) + surface_tokens
    assert_eq!(m.total_tokens, m.surface_tokens);
}

#[test]
fn measure_usage_anchor_pins_baseline() {
    let session = new_session("s");
    append_request_header(&session);
    open_turn_step(&session, 1, 1);
    append_surface(
        &session,
        EventKind::AssistantMessage,
        assistant_message_json(
            "a0",
            "hello world",
            1,
            1,
            Some(&dsh_llm::types::TokenUsage {
                input_tokens: 100,
                output_tokens: 10,
                cache_read_tokens: None,
                cache_write_tokens: None,
                reasoning_tokens: None,
            }),
        ),
    );
    let m = measure(&session.events()).unwrap();
    // provider usage (110) 大于 header+surface 估算 → 用 usage 锚
    match &m.baseline {
        dsh_compaction::TokenMeasurementBaseline::Usage { tokens, .. } => {
            assert_eq!(*tokens, 110)
        }
        other => panic!("expected usage baseline, got {other:?}"),
    }
    // surface delta 相对 usage-anchor 为 0 → total = baseline
    assert_eq!(m.total_tokens, 110);
}

#[test]
fn measure_replace_updates_surface_delta() {
    let session = new_session("s");
    append_surface(&session, EventKind::UserMessage, user_message_json("u0", "a".repeat(100)));
    append_surface(&session, EventKind::UserMessage, user_message_json("u1", "b".repeat(100)));
    let before = measure(&session.events()).unwrap();
    assert_eq!(before.nodes.len(), 2);
    let rep = json!({
        "id": "c0",
        "role": "user",
        "content": [{"type": "text", "text": "compact"}],
        "source": {"kind": "user"},
    });
    session
        .append(
            EventKind::UserMessage,
            rep,
            Some(&dsh_session::types::SurfaceIntent {
                surface_op: dsh_session::types::SurfaceOp::Replace { start: 0, end: 0 },
                source_event_seqs: Some(vec![0]),
            }),
        )
        .unwrap();
    let after = measure(&session.events()).unwrap();
    assert_eq!(after.nodes.len(), 2);
    assert_eq!(after.nodes[0].seq, 2);
    assert!(after.total_tokens < before.total_tokens);
}

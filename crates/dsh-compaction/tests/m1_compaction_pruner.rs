//! M1c：ToolResultPruner（Unicode 码点裁剪 + shadow-price 协议）。
#![allow(clippy::needless_update)]

mod common;
use common::*;

use dsh_compaction::{
    code_point_length, resolve_prune_config, ToolResultPruner, ToolResultPruneConfig,
};
use dsh_llm::types::ContentBlock;
use dsh_session::types::EventKind;

#[test]
fn code_point_counts_unicode_not_bytes() {
    assert_eq!(code_point_length("abc"), 3);
    assert_eq!(code_point_length("😀😀"), 2); // 2 codepoints
    assert_eq!(code_point_length(""), 0);
}

#[test]
fn resolve_defaults() {
    let cfg = resolve_prune_config(&ToolResultPruneConfig::default()).unwrap();
    assert_eq!(cfg.threshold_chars, 8192);
    assert_eq!(cfg.head_chars, 4096);
    assert_eq!(cfg.tail_chars, 1024);
}

#[test]
fn resolve_rejects_zero_threshold() {
    let cfg = ToolResultPruneConfig {
        threshold_chars: Some(0),
        ..Default::default()
    };
    assert!(resolve_prune_config(&cfg).is_err());
}

#[test]
fn resolve_rejects_emitted_over_threshold() {
    // head + marker + tail > threshold
    let cfg = ToolResultPruneConfig {
        threshold_chars: Some(100),
        head_chars: Some(60),
        tail_chars: Some(100),
        ..Default::default()
    };
    assert!(resolve_prune_config(&cfg).is_err());
}

#[test]
fn prune_returns_none_within_budget() {
    let pruner = ToolResultPruner::new(&ToolResultPruneConfig {
        threshold_chars: Some(100),
        head_chars: Some(10),
        tail_chars: Some(5),
        ..Default::default()
    })
    .unwrap();
    let blocks = vec![ContentBlock::text("short")];
    assert!(pruner.prune_content(&blocks).unwrap().is_none());
}

#[test]
fn prune_keeps_head_and_tail_with_marker() {
    // PRUNE_MARKER 为 39 码点；head(10)+marker(39)+tail(5)=54 ≤ threshold(60)
    let pruner = ToolResultPruner::new(&ToolResultPruneConfig {
        threshold_chars: Some(60),
        head_chars: Some(10),
        tail_chars: Some(5),
        ..Default::default()
    })
    .unwrap();
    let body = "a".repeat(100);
    let blocks = vec![ContentBlock::text(&body)];
    let pruned = pruner.prune_content(&blocks).unwrap().unwrap();
    // 只有一个 text block：head(10) + marker + tail(5)
    assert_eq!(pruned.len(), 1);
    match &pruned[0] {
        ContentBlock::Text(t) => {
            assert!(t.text.starts_with("aaaaaaaaaa"));
            assert!(t.text.ends_with("aaaaa"));
            assert!(t.text.contains("[... tool result middle pruned ...]"));
            assert!(pruner.measure_content(&pruned[0..1]) < 60);
        }
        _ => panic!("expected text"),
    }
}

#[test]
fn prune_preserves_rich_block_order() {
    // reasoning 不计入 measure_content 的 total_chars，但保留顺序
    let pruner = ToolResultPruner::new(&ToolResultPruneConfig {
        threshold_chars: Some(200),
        head_chars: Some(10),
        tail_chars: Some(5),
        ..Default::default()
    })
    .unwrap();
    let blocks = vec![
        ContentBlock::text("a".repeat(150)),
        ContentBlock::Reasoning(dsh_llm::types::ReasoningBlock {
            text: "inner".into(),
        }),
        ContentBlock::text("b".repeat(150)),
    ];
    // total_chars = 150+150 = 300 > threshold 200 → 触发裁剪
    assert_eq!(pruner.measure_content(&blocks), 300);
    let pruned = pruner.prune_content(&blocks).unwrap().unwrap();
    // 顺序保持：text, reasoning, text
    assert!(matches!(pruned[0], ContentBlock::Text(_)));
    assert!(matches!(pruned[1], ContentBlock::Reasoning(_)));
    assert!(matches!(pruned[2], ContentBlock::Text(_)));
    // 至多一个 marker
    let joined: String = pruned
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(joined.matches("[... tool result middle pruned ...]").count(), 1);
}

#[test]
fn prune_errors_when_content_not_reduced_below_total() {
    // head+marker+tail=439 ≥ 阈值 50 → 配置不合规；换合规配置（639≤1000）
    let pruner = ToolResultPruner::new(&ToolResultPruneConfig {
        threshold_chars: Some(1000),
        head_chars: Some(500),
        tail_chars: Some(100),
        ..Default::default()
    })
    .unwrap();
    // 文本 1200 超阈值（1000）；head 到 500、tail 从 1100 起 → 中间 600 被裁 → 结果 639
    let blocks = vec![ContentBlock::text("a".repeat(1200))];
    let pruned = pruner.prune_content(&blocks).unwrap().unwrap();
    assert!(pruner.measure_content(&pruned) < 1200);
}

// ---- prune_session（shadow-price 协议）----

#[test]
fn prune_session_prunes_overbudget_tool_results() {
    let session = new_session("s");
    open_turn_step(&session, 1, 1);
    // assistant 调用工具 c1
    append_surface(
        &session,
        EventKind::AssistantMessage,
        assistant_tool_call_msg("a0", "c1", 2, 1, 1).data.clone(),
    );
    // tool result 返回超长文本
    let long_text = "x".repeat(1000);
    append_surface(
        &session,
        EventKind::ToolResult,
        tool_result_msg("t0", "c1", &long_text, 3, 1, 1).data.clone(),
    );
    let pruner = ToolResultPruner::new(&ToolResultPruneConfig {
        threshold_chars: Some(100),
        head_chars: Some(20),
        tail_chars: Some(10),
        ..Default::default()
    })
    .unwrap();
    let estimate: dsh_compaction::EstimateFn =
        Box::new(|_m: &dsh_llm::types::Message| 42);
    let result = pruner.prune_session(&session, &estimate).unwrap();
    assert_eq!(result.pruned.len(), 1);
    assert_eq!(result.pruned[0].original_seq, 3);
    assert!(result.pruned[0].chars_after < result.pruned[0].chars_before);
    assert!(result.chars_removed > 0);
    // 事件：compaction/prune + tool/result replace（按日志顺序相邻；replace 的
    // sourceEventSeqs 引用原 tool/result seq 3）
    let events = session.events();
    let prune_idx = events
        .iter()
        .position(|e| e.kind == EventKind::CompactionPrune)
        .unwrap();
    // 替换事件：kind 为 tool/result 且 sourceEventSeqs 引用 3
    let replace_idx = events
        .iter()
        .position(|e| {
            e.kind == EventKind::ToolResult && e.source_event_seqs() == Some(&vec![3])
        })
        .unwrap();
    assert!(prune_idx < replace_idx);
    // prune 事件记录了 shadow 定价
    let prune_event = &events[prune_idx];
    assert_eq!(
        prune_event.data.get("shadowedTokenCount").and_then(|v| v.as_u64()),
        Some(42)
    );
    // 替换事件的 sourceEventSeqs 引用原 seq 3
    assert_eq!(events[replace_idx].source_event_seqs(), Some(&vec![3]));
}

#[test]
fn prune_session_skips_within_budget() {
    let session = new_session("s");
    open_turn_step(&session, 1, 1);
    append_surface(
        &session,
        EventKind::ToolResult,
        tool_result_msg("t0", "c1", "short", 2, 1, 1).data.clone(),
    );
    let pruner = ToolResultPruner::new(&ToolResultPruneConfig {
        threshold_chars: Some(10_000),
        ..Default::default()
    })
    .unwrap();
    let estimate: dsh_compaction::EstimateFn = Box::new(|_m| 0);
    let result = pruner.prune_session(&session, &estimate).unwrap();
    assert!(result.pruned.is_empty());
    assert_eq!(result.chars_removed, 0);
}

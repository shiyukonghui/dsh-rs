//! M1c：tool-call/result 配对平衡折叠（对齐 tool-pairing.ts）。

mod common;
use common::*;

use dsh_compaction::{tool_pairing_balanced_after, tool_pairing_balanced_before, BalanceCache};
use dsh_session::types::EventKind;

/// 从 session 日志构建 surface_nodes + generation。
fn surface_of(session: &dsh_session::runtime::Session) -> (Vec<u64>, u64) {
    (
        session.surface_nodes().unwrap(),
        session.surface_replace_generation().unwrap(),
    )
}

fn balanced_before(
    session: &dsh_session::runtime::Session,
    cache: &mut Option<BalanceCache>,
    seq: u64,
) -> bool {
    let (nodes, gen) = surface_of(session);
    tool_pairing_balanced_before(&session.events(), &nodes, gen, cache, seq).unwrap()
}

fn balanced_after(
    session: &dsh_session::runtime::Session,
    cache: &mut Option<BalanceCache>,
    seq: u64,
) -> bool {
    let (nodes, gen) = surface_of(session);
    tool_pairing_balanced_after(&session.events(), &nodes, gen, cache, seq).unwrap()
}

#[test]
fn empty_session_balanced_before_first() {
    let session = new_session("s");
    let mut cache = None;
    // no surface nodes → 空 surface：查询任何 seq 都会失败，但我们只验证空 surface 缓存
    let (nodes, gen) = surface_of(&session);
    assert!(nodes.is_empty());
    let c = dsh_compaction::balance_cache(
        &session.events(),
        &nodes,
        gen,
        &mut cache,
    )
    .unwrap();
    assert!(c.cut_balanced.is_empty() || c.cut_balanced == vec![true]);
}

#[test]
fn plain_user_assistant_boundaries_are_balanced() {
    let session = new_session("s");
    append_surface(&session, EventKind::UserMessage, user_message_json("u0", "hi"));
    append_surface(&session, EventKind::UserMessage, user_message_json("u1", "there"));
    let mut cache = None;
    // 每个 user 节点正前方/正后方切割都平衡（无进行中 tool call）
    assert!(balanced_before(&session, &mut cache, 0));
    assert!(balanced_after(&session, &mut cache, 0));
    assert!(balanced_before(&session, &mut cache, 1));
    assert!(balanced_after(&session, &mut cache, 1));
}

#[test]
fn split_tool_call_and_result_is_unbalanced_inside() {
    let session = new_session("s");
    open_turn_step(&session, 1, 1);
    // assistant 请求工具（tool-call 计 +1）
    append_surface(
        &session,
        EventKind::AssistantMessage,
        assistant_tool_call_msg("a0", "c1", 2, 1, 1).data.clone(),
    );
    // 工具结果（配对 -1）
    append_surface(
        &session,
        EventKind::ToolResult,
        tool_result_msg("t0", "c1", "ok", 3, 1, 1).data.clone(),
    );
    let mut cache = None;
    // tool-call 节点正前方平衡（没拆任何对）
    assert!(balanced_before(&session, &mut cache, 2));
    // tool-call 与其 result 之间：正后方切割不平衡（会把 call 与 result 拆开）
    assert!(!balanced_after(&session, &mut cache, 2));
    assert!(!balanced_before(&session, &mut cache, 3));
    // result 正后方平衡（对已闭合）
    assert!(balanced_after(&session, &mut cache, 3));
}

#[test]
fn generation_rebuild_keeps_balance() {
    let session = new_session("s");
    append_surface(&session, EventKind::UserMessage, user_message_json("u0", "a"));
    let mut cache = None;
    assert!(balanced_before(&session, &mut cache, 0));
    // replace 0..=0 → generation+1，缓存应重建
    session
        .append(
            EventKind::UserMessage,
            user_message_json("c0", "compacted"),
            Some(&dsh_session::types::SurfaceIntent {
                surface_op: dsh_session::types::SurfaceOp::Replace { start: 0, end: 0 },
                source_event_seqs: Some(vec![0]),
            }),
        )
        .unwrap();
    let (nodes, gen) = surface_of(&session);
    assert!(gen > 0);
    assert_eq!(nodes, vec![1]); // 替换事件是 seq 1
    // 缓存重建后，新节点正前方平衡
    assert!(balanced_before(&session, &mut cache, 1));
}

#[test]
fn orphan_tool_result_is_rejected() {
    let session = new_session("s");
    open_turn_step(&session, 1, 1);
    // 孤立的 tool/result（无前置 assistant tool-call）→ 负平衡 → 报错
    append_surface(
        &session,
        EventKind::ToolResult,
        tool_result_msg("t0", "c9", "orphan", 2, 1, 1).data.clone(),
    );
    let mut cache = None;
    let (nodes, gen) = surface_of(&session);
    let result = tool_pairing_balanced_before(&session.events(), &nodes, gen, &mut cache, 2);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("no matching tool-call"));
}

#[test]
fn cache_is_incremental_across_events() {
    let session = new_session("s");
    append_surface(&session, EventKind::UserMessage, user_message_json("u0", "a"));
    append_surface(&session, EventKind::UserMessage, user_message_json("u1", "b"));
    let (nodes, gen) = surface_of(&session);
    let mut cache = None;
    // 第一次查询折叠全部
    let c = dsh_compaction::balance_cache(&session.events(), &nodes, gen, &mut cache).unwrap();
    assert_eq!(c.cut_balanced.len() - 1, nodes.len());
}

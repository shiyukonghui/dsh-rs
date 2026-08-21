//! M1c：区域选择/校验 + 压缩事务（compact_surface_region 全生命周期）。

mod common;
use common::*;

use serde_json::json;

use dsh_compaction::{
    compact_surface_region, inspect_compaction_entry_state, measure, select_compactable_range,
    validate_surface_region, CompactionEntryState, CompactionTransactionOptions, Owner,
    RegionDependencies, StabilityRule,
};
use dsh_session::runtime::Session;
use dsh_session::types::EventKind;

fn transaction_options() -> CompactionTransactionOptions {
    CompactionTransactionOptions {
        owner: Owner::CurrentTurn,
        stability: StabilityRule::WholeSurface,
        source_command_id: None,
    }
}

// ---- select_compactable_range ----

#[test]
fn select_range_keeps_recent_tail() {
    let session = new_session("s");
    // 3 个 user 消息，每个 ~10 tokens（content "aaaaaaaa"=6 + role 4）
    append_surface(&session, EventKind::UserMessage, user_message_json("u0", "aaaaaaaa"));
    append_surface(&session, EventKind::UserMessage, user_message_json("u1", "bbbbbbbb"));
    append_surface(&session, EventKind::UserMessage, user_message_json("u2", "cccccccc"));
    let events = session.events();
    let nodes = session.surface_nodes().unwrap();
    let generation = session.surface_replace_generation().unwrap();
    let measurement = measure(&events).unwrap();
    let mut cache = None;
    // retain=6 → 只保留尾节点（10 ≥ 6）→ keep_from_idx=2 → 压 [0,1]
    let range = select_compactable_range(
        &events,
        &nodes,
        generation,
        &mut cache,
        &measurement,
        6,
    )
    .unwrap();
    assert_eq!(range, Some((0, 1)));
}

#[test]
fn select_range_retain_all_nodes_none() {
    let session = new_session("s");
    append_surface(&session, EventKind::UserMessage, user_message_json("u0", "aaaaaaaa"));
    let events = session.events();
    let nodes = session.surface_nodes().unwrap();
    let generation = session.surface_replace_generation().unwrap();
    let measurement = measure(&events).unwrap();
    let mut cache = None;
    // retain=1000 → 单个节点无法清空 → keep_from_idx=0 → None
    let range = select_compactable_range(
        &events, &nodes, generation, &mut cache, &measurement, 1000,
    )
    .unwrap();
    assert_eq!(range, None);
}

#[test]
fn select_range_none_on_empty_surface() {
    let session = new_session("s");
    let events = session.events();
    let nodes = session.surface_nodes().unwrap();
    let generation = session.surface_replace_generation().unwrap();
    let measurement = measure(&events).unwrap();
    let mut cache = None;
    let range = select_compactable_range(
        &events, &nodes, generation, &mut cache, &measurement, 6,
    )
    .unwrap();
    assert_eq!(range, None);
}

#[test]
fn select_range_stops_at_balanced_boundary() {
    let session = new_session("s");
    // 先构造表面：[assistant tool-call, tool/result, user"c"]
    //（turn 打开但 step 已关，避免孤儿 tool/result 复发）
    append_log_only(&session, EventKind::TurnStart, json!({"turn": 1}));
    append_log_only(&session, EventKind::StepStart, json!({"turn": 1, "step": 1}));
    append_surface(
        &session,
        EventKind::AssistantMessage,
        assistant_tool_call_msg("a0", "c1", 2, 1, 1).data.clone(),
    );
    append_surface(
        &session,
        EventKind::ToolResult,
        tool_result_msg("t0", "c1", "res", 3, 1, 1).data.clone(),
    );
    append_log_only(&session, EventKind::StepEnd, json!({"turn": 1, "step": 1}));
    append_surface(&session, EventKind::UserMessage, user_message_json("u0", "c"));
    // 表面 [2,3,5]；retain 得把 tool/result 保留 → 切割必须在配对外
    let events = session.events();
    let nodes = session.surface_nodes().unwrap();
    let generation = session.surface_replace_generation().unwrap();
    let measurement = measure(&events).unwrap();
    let mut cache = None;
    let range = select_compactable_range(
        &events, &nodes, generation, &mut cache, &measurement, 2,
    )
    .unwrap();
    assert!(range.is_some());
    let (first, cutoff) = range.unwrap();
    // 配对节点 seq 2（call）+3（result）必须整体落入被压范围（或整体保留），
    // 绝不允许只压其中一半。
    let includes_2 = first <= 2 && cutoff >= 2;
    let includes_3 = first <= 3 && cutoff >= 3;
    assert_eq!(includes_2, includes_3, "range {first}-{cutoff} splits the pair");
    assert_eq!(first, nodes[0]);
}

// ---- validate_surface_region ----

#[test]
fn validate_region_accepts_plain_span() {
    let session = new_session("s");
    append_surface(&session, EventKind::UserMessage, user_message_json("u0", "a"));
    append_surface(&session, EventKind::UserMessage, user_message_json("u1", "b"));
    append_surface(&session, EventKind::UserMessage, user_message_json("u2", "c"));
    let events = session.events();
    let nodes = session.surface_nodes().unwrap();
    let generation = session.surface_replace_generation().unwrap();
    let mut cache = None;
    let sel = validate_surface_region(&events, &nodes, generation, &mut cache, 0, 1).unwrap();
    assert_eq!(sel.start, 0);
    assert_eq!(sel.end, 1);
    assert_eq!(sel.shadowed_seqs, vec![0, 1]);
}

#[test]
fn validate_region_rejects_end_before_start() {
    let session = new_session("s");
    append_surface(&session, EventKind::UserMessage, user_message_json("u0", "a"));
    append_surface(&session, EventKind::UserMessage, user_message_json("u1", "b"));
    let events = session.events();
    let nodes = session.surface_nodes().unwrap();
    let generation = session.surface_replace_generation().unwrap();
    let mut cache = None;
    let err = validate_surface_region(&events, &nodes, generation, &mut cache, 1, 0).unwrap_err();
    assert!(err.contains("after end seq"));
}

#[test]
fn validate_region_rejects_missing_start() {
    let session = new_session("s");
    append_surface(&session, EventKind::UserMessage, user_message_json("u0", "a"));
    let events = session.events();
    let nodes = session.surface_nodes().unwrap();
    let generation = session.surface_replace_generation().unwrap();
    let mut cache = None;
    let err = validate_surface_region(&events, &nodes, generation, &mut cache, 99, 0).unwrap_err();
    assert!(err.contains("start seq 99 not found in surface"));
}

#[test]
fn validate_region_rejects_split_tool_pair() {
    let session = new_session("s");
    open_turn_step(&session, 1, 1);
    // 表面 [2,3,4]：assistant tool-call, tool/result, user
    append_surface(
        &session,
        EventKind::AssistantMessage,
        assistant_tool_call_msg("a0", "c1", 2, 1, 1).data.clone(),
    );
    append_surface(
        &session,
        EventKind::ToolResult,
        tool_result_msg("t0", "c1", "res", 3, 1, 1).data.clone(),
    );
    append_surface(&session, EventKind::UserMessage, user_message_json("u0", "x"));
    let events = session.events();
    let nodes = session.surface_nodes().unwrap();
    let generation = session.surface_replace_generation().unwrap();
    let mut cache = None;
    // 以 tool/result（seq 3）为起点会拆分其与 tool-call 的配对 → 不平衡
    let err = validate_surface_region(&events, &nodes, generation, &mut cache, 3, 3).unwrap_err();
    assert!(err.contains("not a balanced boundary") || err.contains("would split"));
}

// ---- inspect/assert busy ----

#[test]
fn entry_state_empty_session() {
    let session = new_session("s");
    let state = inspect_compaction_entry_state(&session.events());
    assert_eq!(
        state,
        CompactionEntryState {
            open_turn: None,
            unmatched_start_seq: None,
            latest_end_seed_seq: None,
        }
    );
}

#[test]
fn entry_state_detects_unmatched_start() {
    let session = new_session("s");
    open_turn_step(&session, 7, 1);
    append_log_only(
        &session,
        EventKind::CompactionStart,
        json!({"compactionId": "cid", "sourceCommandId": null, "turn": 7}),
    );
    let state = inspect_compaction_entry_state(&session.events());
    assert_eq!(state.open_turn, Some(7));
    assert!(state.unmatched_start_seq.is_some());
}

#[test]
fn entry_state_matched_pair_is_clear() {
    let session = new_session("s");
    open_turn_step(&session, 7, 1);
    append_log_only(
        &session,
        EventKind::CompactionStart,
        json!({"compactionId": "cid", "turn": 7}),
    );
    append_log_only(
        &session,
        EventKind::CompactionEnd,
        json!({"compactionId": "cid", "turn": 7}),
    );
    // 尾部扫描：compaction/end 把 start 遮蔽 → unmatched=None
    let state = inspect_compaction_entry_state(&session.events());
    assert_eq!(state.unmatched_start_seq, None);
    assert_eq!(state.open_turn, Some(7));
}

// ---- compact_surface_region（完整事务）----

/// 构造：先表面节点（seq 0..=n-1），再打开 turn（seq n,n+1）→ 无间空隙。
fn session_with_surface_and_open_turn(texts: &[&str]) -> Session {
    let session = new_session("s");
    for (i, t) in texts.iter().enumerate() {
        append_surface(
            &session,
            EventKind::UserMessage,
            user_message_json(&format!("u{i}"), t.to_string()),
        );
    }
    append_log_only(&session, EventKind::TurnStart, json!({"turn": 1}));
    append_log_only(&session, EventKind::StepStart, json!({"turn": 1, "step": 1}));
    session
}

#[test]
fn compact_transaction_commits_full_lifecycle() {
    // shadowed 内容必须显著大于 checkpoint 框架（~106 tokens）→ 用大文本
    let session = session_with_surface_and_open_turn(&[
        std::str::from_utf8(&[b'x'; 4000]).unwrap(),
        "bbbb",
        "cccc",
    ]);
    let deps = RegionDependencies { summarize: stub_summarizer("compacted") };
    let result = compact_surface_region(&deps, &session, 0, 0, &transaction_options()).unwrap();
    assert_eq!(result.shadowed_range.start, 0);
    assert_eq!(result.shadowed_range.end, 0);
    assert_eq!(result.shadowed_seqs, vec![0]);
    // 生命周期事件：start < summary < end < 新的 user 替换
    let events = session.events();
    let kinds: Vec<&str> = events.iter().map(|e| e.kind.as_str()).collect();
    let s = kinds.iter().position(|k| *k == "compaction/start").unwrap();
    let m = kinds.iter().position(|k| *k == "compaction/summary").unwrap();
    let e2 = kinds.iter().position(|k| *k == "compaction/end").unwrap();
    assert!(s < m && m < e2);
    // 新 user 替换节点带 checkpoint source（在 start 之后、end 之前提交）
    let rep = events[s..].iter().find(|ev| ev.kind == EventKind::UserMessage).unwrap();
    assert!(rep.source_event_seqs().is_some());
    // surface 现在：替换节点 + "bbbb" + "cccc" = 3 节点
    let nodes = session.surface_nodes().unwrap();
    assert_eq!(nodes.len(), 3);
    assert!(nodes[0] > nodes[1]);
}

#[test]
fn compact_transaction_requires_open_turn_for_current_turn_owner() {
    let session = new_session("s");
    append_surface(&session, EventKind::UserMessage, user_message_json("u0", "aaaa"));
    let deps = RegionDependencies { summarize: stub_summarizer("x") };
    let err = compact_surface_region(&deps, &session, 0, 0, &transaction_options()).unwrap_err();
    assert!(err.to_string().contains("no open turn"));
}

#[test]
fn compact_transaction_rejects_already_busy() {
    let session = session_with_surface_and_open_turn(&["aaaa", "bbbb"]);
    // 先放一个未匹配 compaction/start 占锁（需与 surface 无冲突：log-only）
    append_log_only(
        &session,
        EventKind::CompactionStart,
        json!({"compactionId": "busy-cid", "turn": 1}),
    );
    let deps = RegionDependencies { summarize: stub_summarizer("x") };
    let err = compact_surface_region(&deps, &session, 0, 0, &transaction_options()).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("already in progress") || msg.contains("busy"), "got: {msg}");
    // 锁仍占用：事件流保持原样（没有任何新 compaction/start）
    let events = session.events();
    assert_eq!(
        events
            .iter()
            .filter(|e| e.kind == EventKind::CompactionStart)
            .count(),
        1
    );
}

#[test]
fn compact_transaction_error_path_ends_with_error() {
    let session = session_with_surface_and_open_turn(&["aaaa", "bbbb", "cccc"]);
    // 摘要替身返回 Err → 事务失败 → compaction/end 携带 error（一次尝试）
    let failing: dsh_compaction::Summarizer =
        std::rc::Rc::new(|_| Err("summarize blew up".to_string()));
    let deps = RegionDependencies { summarize: failing };
    let err = compact_surface_region(&deps, &session, 0, 1, &transaction_options()).unwrap_err();
    assert!(err.to_string().contains("summarize blew up"));
    let events = session.events();
    let end = events.iter().rev().find(|e| e.kind == EventKind::CompactionEnd).unwrap();
    assert_eq!(
        end.data.get("error").and_then(|v| v.as_str()),
        Some("summarize blew up")
    );
}

#[test]
fn compact_transaction_summary_must_shrink() {
    let session = session_with_surface_and_open_turn(&[std::str::from_utf8(&[b'z'; 2000]).unwrap(), "bbbb"]);
    let deps = RegionDependencies { summarize: stub_summarizer("short") };
    let result = compact_surface_region(&deps, &session, 0, 0, &transaction_options()).unwrap();
    assert!(result.shadowed_token_count > 0);
}

#[test]
fn manual_owner_rejects_open_turn() {
    let session = session_with_surface_and_open_turn(&["aaaa", "bbbb"]);
    let deps = RegionDependencies { summarize: stub_summarizer("x") };
    let opts = CompactionTransactionOptions {
        owner: Owner::Manual,
        stability: StabilityRule::SelectedSpan,
        source_command_id: Some("cmd-1".into()),
    };
    let err = compact_surface_region(&deps, &session, 0, 0, &opts).unwrap_err();
    assert!(err.to_string().contains("already has an open turn"));
}

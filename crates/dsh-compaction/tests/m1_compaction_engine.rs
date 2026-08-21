//! M1c：BasicCompactionEngine（CompactionEngine trait）——自动/手动/区域路径。

mod common;
use common::*;

use dsh_compaction::{
    BasicCompactionConfig, BasicCompactionEngine, CompactionEngine, CompactionPolicyConfig,
    CompactionTrigger, ModelCompactPolicyConfig, ModelInfoProvider,
};
use dsh_session::types::EventKind;

/// 固定容量替身（模拟某模型上下文窗口）。
struct FixedWindow(u64);
impl ModelInfoProvider for FixedWindow {
    fn context_window(&self, _provider: &str, _model: &str) -> Result<u64, String> {
        Ok(self.0)
    }
}

fn engine() -> BasicCompactionEngine {
    BasicCompactionEngine::new(BasicCompactionConfig::default()).unwrap()
}

/// 高容量引擎（小窗口 → 易触发）。
fn small_window_engine(window: u64) -> BasicCompactionEngine {
    let mut e = engine();
    e.model_info = Some(Box::new(FixedWindow(window)));
    e
}

/// 大文本 user 消息（每条 tok ≈ ceil(5000/4)+4+4 ≈ 1258，远超 checkpoint 框架）。
fn big_user(session: &dsh_session::runtime::Session, id: &str) {
    append_surface(
        session,
        EventKind::UserMessage,
        user_message_json(id, "z".repeat(5000)),
    );
}

#[test]
fn pressure_no_trigger_below_threshold() {
    let session = new_session("s");
    append_request_header(&session);
    open_turn_step(&session, 1, 1);
    append_surface(&session, EventKind::UserMessage, user_message_json("u0", "hi"));
    let e = small_window_engine(10_000); // threshold=8000
    let result = e
        .compact_if_needed(&session, CompactionTrigger::Pressure, &stub_summarizer("x"))
        .unwrap();
    assert!(result.is_none());
}

#[test]
fn pressure_triggers_and_compacts() {
    let session = new_session("s");
    append_request_header(&session);
    open_turn_step(&session, 1, 1);
    // 小窗口 4000 → threshold 3200；8 条 × 1258 ≈ 10000 >> 3200
    for i in 0..8 {
        big_user(&session, &format!("u{i}"));
    }
    let e = small_window_engine(4000);
    let result = e
        .compact_if_needed(&session, CompactionTrigger::Pressure, &stub_summarizer("compact note"))
        .unwrap();
    assert!(result.is_some());
    let r = result.unwrap();
    assert!(r.shadowed_token_count > 0);
    // 压缩后事件流含 start/summary/end
    let events = session.events();
    let kinds: Vec<&str> = events.iter().map(|ev| ev.kind.as_str()).collect();
    assert!(kinds.contains(&"compaction/start"));
    assert!(kinds.contains(&"compaction/summary"));
    assert!(kinds.contains(&"compaction/end"));
}

#[test]
fn pressure_respects_model_policy() {
    let cfg = BasicCompactionConfig {
        model_policies: Some(vec![ModelCompactPolicyConfig {
            provider: "deepseek".into(),
            model: "deepseek-chat".into(),
            policy: CompactionPolicyConfig {
                threshold_ratio: Some(0.5),
                retain_ratio: Some(0.1),
                ..Default::default()
            },
        }]),
        ..Default::default()
    };
    // 覆盖 policy：window 4000 → threshold 2000；保留 tail ≈1258 < threshold，
    // 一次压缩即可压到阈值下（避免二次压缩吞掉 checkpoint 节点）
    let mut e = BasicCompactionEngine::new(cfg).unwrap();
    e.model_info = Some(Box::new(FixedWindow(4000)));
    let session = new_session("s");
    append_request_header(&session);
    open_turn_step(&session, 1, 1);
    big_user(&session, "u0");
    big_user(&session, "u1");
    let result = e
        .compact_if_needed(&session, CompactionTrigger::Pressure, &stub_summarizer("c"))
        .unwrap();
    assert!(result.is_some());
}

#[test]
fn context_overflow_compacts() {
    let session = new_session("s");
    append_request_header(&session);
    open_turn_step(&session, 1, 1);
    for i in 0..4 {
        big_user(&session, &format!("u{i}"));
    }
    let e = small_window_engine(4000);
    let result = e
        .compact_if_needed(&session, CompactionTrigger::ContextOverflow, &stub_summarizer("c"))
        .unwrap();
    assert!(result.is_some());
}

#[test]
fn no_header_returns_none() {
    let session = new_session("s");
    open_turn_step(&session, 1, 1);
    append_surface(&session, EventKind::UserMessage, user_message_json("u0", "x"));
    let e = small_window_engine(4000);
    let result = e
        .compact_if_needed(&session, CompactionTrigger::Pressure, &stub_summarizer("c"))
        .unwrap();
    assert!(result.is_none());
}

#[test]
fn compact_region_direct() {
    let session = new_session("s");
    open_turn_step(&session, 1, 1);
    // surface 节点 seq 2..（turn 打开占 0,1）
    for i in 0..3 {
        big_user(&session, &format!("u{i}"));
    }
    let e = engine();
    let r = e.compact_region(&session, 2, 2, &stub_summarizer("compact")).unwrap();
    assert_eq!(r.shadowed_seqs, vec![2]);
}

#[test]
fn compact_now_empty_or_leaf_returns_none() {
    let session = new_session("s");
    append_surface(&session, EventKind::UserMessage, user_message_json("u0", "only"));
    let e = engine();
    let result = e.compact_now(&session, &stub_summarizer("c"), None).unwrap();
    assert!(result.is_none());
}

#[test]
fn compact_now_manual_pairs() {
    let session = new_session("s");
    // 无 turn（空闲会话）
    for i in 0..6 {
        big_user(&session, &format!("u{i}"));
    }
    let e = engine();
    let result = e
        .compact_now(&session, &stub_summarizer("manual summary"), Some("cmd-1".into()))
        .unwrap();
    assert!(result.is_some());
    let r = result.unwrap();
    assert_eq!(r.source_command_id.as_deref(), Some("cmd-1"));
    // 手动路径：end 事件 error 为空
    let events = session.events();
    let end = events.iter().rev().find(|ev| ev.kind == EventKind::CompactionEnd).unwrap();
    assert!(end.data.get("error").is_none());
}

#[test]
fn compact_now_rejects_open_turn() {
    let session = new_session("s");
    open_turn_step(&session, 1, 1);
    // 多条消息让 select 返回 Some，然后 Owner::Manual 在 open turn 上报错
    for i in 0..3 {
        big_user(&session, &format!("u{i}"));
    }
    let e = engine();
    let err = e.compact_now(&session, &stub_summarizer("c"), None).unwrap_err();
    assert!(err.message.contains("open turn") || err.message.contains("busy"));
}

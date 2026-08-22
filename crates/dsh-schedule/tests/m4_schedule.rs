//! M4f dsh-schedule 纯语义测试（TDD 红-绿）。
//!
//! 对齐 `deepseek-harness/packages/schedule/schedule/src/domain.ts`：
//! - 错误联合：ScheduleLogError（corrupt_schedule_log）/ ScheduleInputError（六码）。
//! - decode：schedule/change（create/delete/dispatch ± acceptedAt）+ 记录（after/at/every）
//!   精确键 + 规范 UTC instant。
//! - fold：create 不重 id、delete 非活跃拒、every dispatch 推进/终止、seed 分界。
//! - 创建规则：prompt trim 非空、after>0、every>=300、strict future。
//! - resolveEveryOccurrence（锚定对齐）+ scheduleView（overdue/scheduled + deliveryMode）。
//! - M4 范围 time_zone：UTC/GMT 直接解析；IANA local-at → invalid_time_zone（chrono-tz 离线不可用，defer）。

use dsh_schedule::{
    allocate_id_from_seen, create_after_record, create_at_record_from_offset, create_every_record,
    decode_schedule_change, fold_schedule_events, resolve_every_occurrence, schedule_view,
    canonicalize_time_zone, ScheduleInputError, InputCode, LogError,
};
use serde_json::json;

/// 规范 UTC instant 常量。
const NOW: i64 = 1_700_000_000_000; // 2023-11-14T22:13:20Z
const FUT: &str = "2024-01-15T00:00:00.000Z";
const FUT2: &str = "2024-01-15T05:00:00.000Z";

fn after_record(id: &str) -> serde_json::Value {
    json!({ "id": id, "kind": "after", "prompt": "standup", "afterSeconds": 600, "scheduledAt": FUT })
}
fn at_record(id: &str) -> serde_json::Value {
    json!({ "id": id, "kind": "at", "prompt": "deploy", "scheduledAt": FUT })
}
fn every_record(id: &str, every: u64) -> serde_json::Value {
    json!({ "id": id, "kind": "every", "prompt": "sync", "everySeconds": every, "scheduledAt": FUT })
}

// ---- decode ----

#[test]
fn decode_create_and_delete_ok() {
    let create = json!({ "version": 1, "operation": "create", "schedule": after_record("s1") });
    let decoded = decode_schedule_change(&create).expect("create");
    assert_eq!(decoded["operation"], "create");
    let del = json!({ "version": 1, "operation": "delete", "id": "s1" });
    let decoded = decode_schedule_change(&del).expect("delete");
    assert_eq!(decoded["operation"], "delete");
}

#[test]
fn decode_rejects_bad_version_kind_keys() {
    // version 错
    let v = json!({ "version": 2, "operation": "create", "schedule": after_record("s1") });
    assert!(matches!(decode_schedule_change(&v), Err(LogError(_))));
    // 未知 operation
    let o = json!({ "version": 1, "operation": "bogus", "id": "s1" });
    assert!(matches!(decode_schedule_change(&o), Err(LogError(_))));
    // 多余键
    let x = json!({ "version": 1, "operation": "create", "surprise": 1, "schedule": after_record("s1") });
    assert!(matches!(decode_schedule_change(&x), Err(LogError(_))));
}

#[test]
fn decode_every_invalid_interval() {
    let e = json!({ "id": "e1", "kind": "every", "prompt": "sync", "everySeconds": 60, "scheduledAt": FUT });
    let ev = json!({ "version": 1, "operation": "create", "schedule": e });
    assert!(matches!(decode_schedule_change(&ev), Err(LogError(_))), "every <300 拒");
}

/// dispatch：one-shot 拒 acceptedAt；every 必须 acceptedAt。
#[test]
fn decode_dispatch_accepted_at_rules() {
    let d1 = json!({ "version": 1, "operation": "dispatch", "id": "s1" });
    assert!(decode_schedule_change(&d1).is_ok(), "one-shot id-only dispatch ok");
    let d2 = json!({ "version": 1, "operation": "dispatch", "id": "s1", "acceptedAt": FUT });
    assert!(decode_schedule_change(&d2).is_ok(), "every dispatch with acceptedAt ok");
    let d3 = json!({ "version": 1, "operation": "dispatch", "id": "s1", "acceptedAt": "2024-13-40T00:00:00.000Z" });
    assert!(matches!(decode_schedule_change(&d3), Err(LogError(_))), "坏瞬时拒");
}

// ---- fold ----

#[test]
fn fold_create_and_delete() {
    let events = vec![
        json!({ "type": "schedule/change", "data": { "version": 1, "operation": "create", "schedule": after_record("s1") } }),
        json!({ "type": "schedule/change", "data": { "version": 1, "operation": "create", "schedule": at_record("s2") } }),
        json!({ "type": "schedule/change", "data": { "version": 1, "operation": "delete", "id": "s1" } }),
    ];
    let folded = fold_schedule_events(&events).expect("fold");
    let ids: Vec<&str> = folded.active_ids.iter().map(|s| s.as_str()).collect();
    assert_eq!(ids, vec!["s2"], "delete 后仅剩 s2");
    let seen = folded.seen_ids;
    assert!(seen.contains(&"s1".to_string()) && seen.contains(&"s2".to_string()));
}

#[test]
fn fold_rejects_id_reuse_and_inactive_delete() {
    let reuse = vec![
        json!({ "type": "schedule/change", "data": { "version": 1, "operation": "create", "schedule": after_record("s1") } }),
        json!({ "type": "schedule/change", "data": { "version": 1, "operation": "create", "schedule": after_record("s1") } }),
    ];
    assert!(matches!(fold_schedule_events(&reuse), Err(LogError(_))), "id 重用拒");
    let inactive_del = vec![
        json!({ "type": "schedule/change", "data": { "version": 1, "operation": "delete", "id": "nope" } }),
    ];
    assert!(fold_schedule_events(&inactive_del).is_err(), "删非活跃拒");
}

/// every dispatch：越过积压直指最新 occurrence，scheduledAt 推进；耗尽则移除。
#[test]
fn fold_every_dispatch_advances() {
    let events = vec![
        json!({ "type": "schedule/change", "data": { "version": 1, "operation": "create", "schedule": every_record("e1", 3600) } }),
        json!({ "type": "schedule/change", "data": { "version": 1, "operation": "dispatch", "id": "e1", "acceptedAt": FUT2 } }),
    ];
    let folded = fold_schedule_events(&events).expect("fold");
    let rec = folded.records.iter().find(|r| r.id == "e1").expect("e1 存活");
    // every 3600s，anchor=2024-01-15T00:00Z → occurrence=05:00:00Z → next=06:00:00Z
    assert_eq!(rec.scheduled_at.as_str(), "2024-01-15T06:00:00.000Z", "推进到 next");
}

/// 一次性 dispatch（after/at）→ 记录移除。
#[test]
fn fold_one_shot_dispatch_removes() {
    let events = vec![
        json!({ "type": "schedule/change", "data": { "version": 1, "operation": "create", "schedule": after_record("s1") } }),
        json!({ "type": "schedule/change", "data": { "version": 1, "operation": "dispatch", "id": "s1" } }),
    ];
    let folded = fold_schedule_events(&events).expect("fold");
    assert!(folded.active_ids.is_empty());
}

/// seed 分界：seedLength 之前事件不参与（fork 不继承父 active）。
#[test]
fn fold_respects_seed_length() {
    let events = vec![
        json!({ "type": "schedule/change", "data": { "version": 1, "operation": "create", "schedule": after_record("parent") } }),
        json!({ "type": "schedule/change", "data": { "version": 1, "operation": "create", "schedule": after_record("child") } }),
    ];
    let folded = fold_schedule_events_seeded(&events, 1).expect("fold");
    let ids: Vec<&str> = folded.active_ids.iter().map(|s| s.as_str()).collect();
    assert_eq!(ids, vec!["child"], "seed 前父记录不继承");
}

// ---- create ----

#[test]
fn create_after_and_every() {
    let after = create_after_record("a1", "  standup  ", 600, NOW).expect("after");
    assert_eq!(after.prompt, "standup");
    assert_eq!(after.kind, "after");
    assert_eq!(after.after_seconds, Some(600));
    // target = now + 600s > now
    let every = create_every_record("e1", "sync", 3600, NOW).expect("every");
    assert_eq!(every.every_seconds, Some(3600));
}

#[test]
fn create_rejects_invalid_input() {
    let empty_prompt = create_after_record("a1", "   ", 10, NOW).unwrap_err();
    assert!(matches!(empty_prompt, ScheduleInputError { code: InputCode::InvalidPrompt, .. }));
    let bad_after = create_after_record("a1", "x", 0, NOW).unwrap_err();
    assert!(matches!(bad_after, ScheduleInputError { code: InputCode::InvalidRule, .. }));
    let freq_high = create_every_record("e1", "x", 60, NOW).unwrap_err();
    assert!(matches!(freq_high, ScheduleInputError { code: InputCode::FrequencyTooHigh, .. }));
    // not_future：at 指向过去 → NotFuture
    let past = create_at_record_from_offset("a1", "x", "2020-01-01T00:00:00Z", NOW).unwrap_err();
    assert!(matches!(past, ScheduleInputError { code: InputCode::NotFuture, .. }));
}

/// at（数值偏移）→ 规范化 UTC。
#[test]
fn create_at_from_offset() {
    // +08:00 → 8h 前。
    let at = create_at_record_from_offset("a1", "deploy", "2024-01-15T08:00:00+08:00", NOW).expect("at");
    assert_eq!(at.scheduled_at, "2024-01-15T00:00:00.000Z");
}

/// UTC 名称 canonicalize 通过；IANA local-at 名称则 invalid_time_zone（M4 范围受限）。
#[test]
fn canonicalize_zone_limited() {
    assert_eq!(canonicalize_time_zone("UTC").expect("utc"), "UTC");
    // 无 chrono-tz，IANA 名在 M4 范围视为不支持（defer）
    let iana = canonicalize_time_zone("Asia/Shanghai");
    assert!(matches!(iana, Err(ScheduleInputError { code: InputCode::InvalidTimeZone, .. })));
}

// ---- every occurrence + view ----

#[test]
fn every_occurrence_aligned() {
    let occ = resolve_every_occurrence("2024-01-15T00:00:00.000Z", 3600, "2024-01-15T05:30:00.000Z").expect("occ");
    assert_eq!(occ.occurrence_at, "2024-01-15T05:00:00.000Z", "最新对齐 occurrence");
    assert_eq!(occ.next_scheduled_at.as_deref(), Some("2024-01-15T06:00:00.000Z"));
}

#[test]
fn view_overdue_and_scheduled() {
    let now = 1_700_000_000_000_i64; // < FUT
    let v = schedule_view("s1", "after", "prompt", "2024-01-15T00:00:00.000Z", 600, 0, now);
    assert_eq!(v["state"], "scheduled");
    assert_eq!(v["deliveryMode"], "session-local");
    let overdue_now = 2_000_000_000_000_i64; // 2033，远晚于 FUT
    let w = schedule_view("s2", "at", "x", "2024-01-15T00:00:00.000Z", 0, 0, overdue_now);
    assert_eq!(w["state"], "overdue");
}

// 种子分界 fold（helper 调 fold_with_seed）。
fn fold_schedule_events_seeded(
    events: &[serde_json::Value],
    seed: usize,
) -> Result<dsh_schedule::FoldedSchedules, LogError> {
    dsh_schedule::fold_schedule_events_seeded(events, seed)
}

// ---- allocate ----

#[test]
fn allocate_id_without_reuse() {
    let seen = vec!["schedule-1".to_string(), "schedule-2".to_string()];
    assert_eq!(allocate_id_from_seen(&seen), "schedule-3");
    let seen2 = vec!["schedule-1".to_string(), "schedule-3".to_string()];
    // TS 从 size+1 起前扫：size=2 → 尝试 schedule-3（已在用）→ schedule-4。不保证补最小洞。
    assert_eq!(allocate_id_from_seen(&seen2), "schedule-4", "前扫跳过已在用 id");
}

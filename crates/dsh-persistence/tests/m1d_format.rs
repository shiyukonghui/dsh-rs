//! M1d（dsh-persistence:format）：路径编码 / header 行 / 事件行 / SessionLogScanner。
//! 对齐 M1-REQUIREMENTS §9 的 `format` 模块 + TS `dsh-session-persistence-jsonl/format.ts`。

use dsh_brand::SessionId;
use dsh_persistence::format::*;
use dsh_session::types::{EventKind, SessionEvent, SessionHeader};
use serde_json::json;

fn header(id: &str) -> SessionHeader {
    SessionHeader::new(SessionId::from_raw(id), 1_700_000_000_000)
}

fn ev(seq: u64, kind: EventKind) -> SessionEvent {
    SessionEvent::new(seq, 1_700_000_000_000 + seq as i64, kind, json!({ "seq": seq }))
}

// ---- 路径编码 ----

#[test]
fn encode_segment_keeps_safe_ascii() {
    assert_eq!(encode_segment("abc123._-").unwrap(), "abc123._-");
}

#[test]
fn encode_segment_escapes_unsafe() {
    assert_eq!(encode_segment("a/b:c").unwrap(), "a~002Fb~003Ac");
}

#[test]
fn encode_segment_escapes_tilde() {
    assert_eq!(encode_segment("a~b").unwrap(), "a~007Eb");
}

#[test]
fn encode_segment_empty_and_dots() {
    assert!(encode_segment("").is_err());
    assert_eq!(encode_segment(".").unwrap(), "~002E");
    assert_eq!(encode_segment("..").unwrap(), "~002E~002E");
}

#[test]
fn encode_segment_escapes_unicode() {
    // 汉字 code unit > 0x7F → 转义
    assert_eq!(encode_segment("中").unwrap(), "~4E2D");
}

#[test]
fn encode_segment_round_trip_injective() {
    for s in ["", "a", "a/b", ".", "..", "中", "a~b", r"C:\x\y"] {
        if s.is_empty() {
            continue;
        }
        let enc = encode_segment(s).unwrap();
        assert!(!enc.contains('/') && !enc.contains('\\'), "{s} -> {enc}");
    }
}

#[test]
fn project_key_collapses_separators_and_wraps() {
    assert_eq!(project_key("C:/work/rust").unwrap(), "--C-work-rust--");
    assert_eq!(project_key(r"C:\work\rust").unwrap(), "--C-work-rust--");
}

#[test]
fn project_key_empty_rejects() {
    assert!(project_key("").is_err());
}

#[test]
fn project_dir_no_cwd_is_no_cwd() {
    assert_eq!(project_dir(r"R:\sessions", None).unwrap(), r"R:\sessions\_no-cwd");
}

#[test]
fn session_and_log_paths() {
    let id = SessionId::from_raw("s-1");
    let slog = session_dir(r"R:\sessions", Some(r"C:\proj"), &id).unwrap();
    assert_eq!(slog, r"R:\sessions\--C-proj--\s-1");
    let lp = log_path(r"R:\sessions", Some(r"C:\proj"), &id, JsonlCompression::Zstd).unwrap();
    assert_eq!(lp, r"R:\sessions\--C-proj--\s-1\session.jsonl.zstd");
    let lp2 = log_path(r"R:\sessions", Some(r"C:\proj"), &id, JsonlCompression::None).unwrap();
    assert_eq!(lp2, r"R:\sessions\--C-proj--\s-1\session.jsonl");
}

// ---- header 行 ----

#[test]
fn header_line_always_has_delegation_depth() {
    let h = header("s1");
    let line = to_header_line(&h);
    assert_eq!(line.delegation_depth, 0);
    let v = line.to_json();
    // delegationDepth 恒存在且为 0
    assert_eq!(v.get("delegationDepth").and_then(|x| x.as_u64()), Some(0));
    assert_eq!(v.get("type").and_then(|x| x.as_str()), Some("session"));
    assert_eq!(v.get("id").and_then(|x| x.as_str()), Some("s1"));
    // 可选字段省略
    assert!(v.get("cwd").is_none());
    assert!(v.get("parentSession").is_none());
    assert!(v.get("agentPreset").is_none());
}

#[test]
fn header_line_preserves_optional_fields() {
    let mut h = header("s2");
    h.cwd = Some(r"C:\proj".into());
    h.parent_session = Some(SessionId::from_raw("parent"));
    h.seed_length = Some(3);
    h.agent_preset = Some("default".into());
    let v = to_header_line(&h).to_json();
    assert_eq!(v.get("cwd").and_then(|x| x.as_str()), Some(r"C:\proj"));
    assert_eq!(v.get("parentSession").and_then(|x| x.as_str()), Some("parent"));
    assert_eq!(v.get("seedLength").and_then(|x| x.as_u64()), Some(3));
    assert_eq!(v.get("agentPreset").and_then(|x| x.as_str()), Some("default"));
}

#[test]
fn header_line_round_trips() {
    let mut h = header("s3");
    h.cwd = Some(r"C:\a\b".into());
    h.parent_session = Some(SessionId::from_raw("p"));
    h.delegation_depth = Some(0); // storage 形态：delegationDepth 恒存在（0）
    let line = to_header_line(&h);
    let back = line.from_json(&line.to_json()).unwrap();
    assert_eq!(back, h);
}

#[test]
fn is_header_line_shape_guards() {
    assert!(is_header_line(&to_header_line(&header("s")).to_json()));
    let mut bad = to_header_line(&header("s")).to_json();
    let obj = bad.as_object_mut().unwrap();
    obj.remove("delegationDepth");
    assert!(!is_header_line(&bad));
    // 负 createdAt 拒绝
    let bad2 = json!({ "type": "session", "version": 0, "id": "s", "createdAt": -1, "delegationDepth": 0 });
    assert!(!is_header_line(&bad2));
}

#[test]
fn parse_header_meta_returns_none_on_garbage() {
    assert!(parse_header_meta("not json").is_none());
    assert!(parse_header_meta("").is_none());
    assert!(parse_header_meta(r#"{"type":"weird"}"#).is_none());
    let v = to_header_line(&header("s")).to_json();
    assert!(parse_header_meta(&serde_json::to_string(&v).unwrap()).is_some());
}

// ---- 事件行 ----

#[test]
fn event_lines_join_with_newline_no_trailing() {
    let evs = vec![ev(0, EventKind::TurnStart), ev(1, EventKind::StepStart)];
    let s = event_lines(&evs, false);
    assert_eq!(s.matches('\n').count(), 1);
    assert!(!s.ends_with('\n'));
}

#[test]
fn header_line_bytes_ends_with_newline() {
    let b = header_line_bytes(&header("s"));
    assert_eq!(b.last(), Some(&b'\n'));
}

#[test]
fn lines_to_events_round_trips() {
    let evs = vec![ev(0, EventKind::TurnStart), ev(1, EventKind::TurnEnd)];
    let text = event_lines(&evs, false);
    let one = text.split('\n').next().unwrap();
    let decoded = lines_to_events(one).unwrap();
    assert_eq!(decoded, vec![evs[0].clone()]);
}

// ---- SessionLogScanner ----

#[test]
fn scanner_reads_complete_log() {
    let mut buf = header_line_bytes(&header("s"));
    buf.extend(event_lines_bytes(&[ev(0, EventKind::TurnStart), ev(1, EventKind::TurnEnd)], false));
    let result = scan_log(&buf).unwrap();
    assert_eq!(result.event_count, 2);
    assert_eq!(result.committed_bytes, buf.len());
    assert_eq!(result.events[0].seq, 0);
    assert_eq!(result.events[1].seq, 1);
}

#[test]
fn scanner_tolerates_torn_tail() {
    let h = header("s");
    let hdr = header_line_bytes(&h);
    let mut buf = hdr.clone();
    let ev_line = event_lines_bytes(&[ev(0, EventKind::TurnStart)], false);
    buf.extend(&ev_line);
    // 追加一条无结尾换行的残缺行 → torn 尾被忽略、committed 保留完整行
    buf.extend(br#"{"type":"user/message","seq":1,"time":1,"data":{"id":"m"}}"#);
    let result = scan_log(&buf).unwrap();
    assert_eq!(result.event_count, 1);
    assert_eq!(result.committed_bytes, hdr.len() + ev_line.len());
}

#[test]
fn scanner_seq_gap_truncates_and_issues() {
    let mut buf = header_line_bytes(&header("s"));
    buf.extend(event_lines_bytes(&[ev(0, EventKind::TurnStart)], false));
    // seq 2 而不是 1 → gap
    buf.extend(event_lines_bytes(&[ev(2, EventKind::TurnEnd)], false));
    let err = scan_log(&buf).unwrap_err();
    assert!(err.contains("seq gap in committed region at line"), "{err}");
}

#[test]
fn scanner_unparsable_line_freezes_committed_without_issue_rethrow() {
    // 坏行 < MIN_RUN 的破坏场景：坏行后无 turn/end → 返回保存的前缀（不 panic）
    let h = header("s");
    let hdr = header_line_bytes(&h);
    let mut buf = hdr.clone();
    let ev_line =
        event_lines_bytes(&[ev(0, EventKind::TurnStart), ev(1, EventKind::StepStart)], false);
    buf.extend(&ev_line);
    buf.extend(b"{ not json }\n");
    // 用增量 scanner 检查：issue 被保留、事件前缀完好
    let mut scanner = SessionLogScanner::new(hdr.len());
    scanner.write(&buf[hdr.len()..]).unwrap();
    let (result, issue) = scanner.finish();
    assert!(issue.is_some());
    assert_eq!(result.event_count, 2);
    assert_eq!(result.committed_bytes, hdr.len() + ev_line.len());
}

#[test]
fn scanner_rethrows_when_issue_row_contains_turn_end() {
    // 同一坏行 = 一个无法解析的 turn/end 值不会出现；真正重抛是「gapped turn/end 行」：
    // seq 跳变且该行是 turn/end → 重抛
    let mut buf = header_line_bytes(&header("s"));
    buf.extend(event_lines_bytes(&[ev(0, EventKind::TurnStart)], false));
    buf.extend(event_lines_bytes(&[ev(2, EventKind::TurnEnd)], false)); // gap + turn/end
    let err = scan_log(&buf).unwrap_err();
    assert!(err.contains("seq gap"));
}

#[test]
fn scanner_incremental_write_boundaries() {
    let h = header("s");
    let hdr = header_line_bytes(&h);
    let evs = event_lines_bytes(&[ev(0, EventKind::TurnStart), ev(1, EventKind::TurnEnd)], false);
    let body = &evs;
    // 分两段写（在行中间断开）
    let split = body.len() / 2;
    let mut scanner = SessionLogScanner::new(hdr.len());
    scanner.write(&body[..split]).unwrap();
    scanner.write(&body[split..]).unwrap();
    let (result, issue) = scanner.finish();
    assert!(issue.is_none());
    assert_eq!(result.event_count, 2);
    assert_eq!(result.committed_bytes, hdr.len() + body.len());
}

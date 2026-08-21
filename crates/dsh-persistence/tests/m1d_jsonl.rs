//! M1d（dsh-persistence）集成测试：JSONL 后端 + coordinator + write-behind。
//!
//! 权威参考：`dsh-session-persistence-jsonl/index.ts` + `dsh-session-persistence/coordinator.ts`
//! （规范 §A/D/G）。验证：
//! ① JSONL 落地/读取（materialize-on-first-append、追加 seq 连续、read_raw 逐字、
//!    list/listSnapshots）；② 崩溃修复（torn 尾容忍、interruptedTurn 合成 closing）；
//! ③ coordinator 语义（live-turn 拒绝 load、prepare LRU、readFrom 冷折叠）；
//! ④ write-behind 批处理（enqueue/flush/失败保留/pause）。

use std::path::{Path, PathBuf};

use dsh_brand::SessionId;
use dsh_persistence::coordinator::PersistenceCoordinator;
use dsh_persistence::jsonl::{JsonlBackend, JsonlConfig};
use dsh_persistence::seam::{
    PersistenceBackend, PersistenceError, SessionPersistence, SessionPersistenceSnapshot,
};
use dsh_persistence::write_behind::SessionWriteBehind;
use dsh_persistence::zstd::scan_zstd_frames;
use dsh_session::repair::interrupted_turn_closers;
use dsh_session::types::{EventKind, SessionEvent, SessionHeader};

fn tmp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dsh-m1d-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn header(id: &str) -> SessionHeader {
    let mut h = SessionHeader::new(SessionId::from_raw(id), 1_700_000_000_000);
    h.cwd = Some(r"C:\work\proj".into());
    h
}

fn event(seq: u64, kind: EventKind) -> SessionEvent {
    SessionEvent::new(seq, 1_700_000_000_000 + seq as i64, kind, serde_json::json!({ "seq": seq }))
}

fn turn_events(base: u64) -> Vec<SessionEvent> {
    vec![
        SessionEvent::new(
            base,
            1_700_000_000_000 + base as i64,
            EventKind::TurnStart,
            serde_json::json!({ "turn": 1 }),
        ),
        SessionEvent::new(
            base + 1,
            1_700_000_000_000 + base as i64 + 1,
            EventKind::TurnEnd,
            serde_json::json!({ "turn": 1, "reason": "complete" }),
        ),
    ]
}

/// 一个被中断的 turn：TurnStart + StepStart（无 closing）。
fn interrupted_turn_events(base: u64) -> Vec<SessionEvent> {
    vec![
        SessionEvent::new(
            base,
            1_700_000_000_000 + base as i64,
            EventKind::TurnStart,
            serde_json::json!({ "turn": 1 }),
        ),
        SessionEvent::new(
            base + 1,
            1_700_000_000_000 + base as i64 + 1,
            EventKind::StepStart,
            serde_json::json!({ "turn": 1, "step": 1 }),
        ),
    ]
}

// ---- JSONL 后端 ----

fn make_backend(name: &str) -> (JsonlBackend, PathBuf) {
    let root = tmp_root(name);
    let backend = JsonlBackend::new(JsonlConfig {
        root: root.clone(),
        compression: dsh_persistence::format::JsonlCompression::Zstd,
        pack_chunks: true,
    });
    (backend, root)
}

#[test]
fn jsonl_materialize_and_read_raw() {
    let (backend, _root) = make_backend("mat");
    let h = header("s-mat");
    backend.materialize_batch(&h, &turn_events(0)).unwrap();
    // 磁盘上应有 zstd magic
    let loc = backend.locate(&h).unwrap();
    let bytes = std::fs::read(&loc.path).unwrap();
    assert_eq!(&bytes[..4], &[0x28, 0xB5, 0x2F, 0xFD], "zstd frame magic");
    let scan = scan_zstd_frames(&bytes, None).unwrap();
    assert_eq!(scan.frames.len(), 2, "header frame + event frame");
    // read_raw 逐字
    let raw = backend.read_raw(&h.id).unwrap().expect("raw artifact");
    assert_eq!(raw.filename, "session.jsonl");
    let first_line = raw.content.lines().next().unwrap();
    let v: serde_json::Value = serde_json::from_str(first_line).unwrap();
    assert_eq!(v["type"], "session");
    assert_eq!(v["id"], "s-mat");
    assert!(raw.content.ends_with('\n'));
    // load_stored
    let log = backend.load_stored(&h.id).unwrap().expect("stored");
    assert_eq!(log.events.len(), 2);
    assert_eq!(log.events[0].seq, 0);
    assert_eq!(log.events[1].seq, 1);
    assert!(!log.torn);
}

#[test]
fn jsonl_append_round_trips_multiple_batches() {
    let (backend, _root) = make_backend("app");
    let h = header("s-app");
    backend.materialize_batch(&h, &turn_events(0)).unwrap();
    backend.append_events(&h.id, &turn_events(2)).unwrap();
    backend.append_events(&h.id, &turn_events(4)).unwrap();
    let log = backend.load_stored(&h.id).unwrap().unwrap();
    assert_eq!(log.events.len(), 6);
    let seqs: Vec<u64> = log.events.iter().map(|e| e.seq).collect();
    assert_eq!(seqs, vec![0, 1, 2, 3, 4, 5]);
    let raw = backend.read_raw(&h.id).unwrap().unwrap();
    assert_eq!(raw.content.matches('\n').count(), 6 + 1, "header + 6 events");
}

#[test]
fn jsonl_refuses_materialize_twice() {
    let (backend, _root) = make_backend("dup");
    let h = header("s-dup");
    backend.materialize_batch(&h, &turn_events(0)).unwrap();
    let err = backend.materialize_batch(&h, &turn_events(0)).unwrap_err();
    assert!(err.to_string().contains("refusing to materialize"), "{err}");
}

#[test]
fn jsonl_load_tolerates_torn_tail() {
    let (backend, root) = make_backend("torn");
    let h = header("s-torn");
    backend.materialize_batch(&h, &turn_events(0)).unwrap();
    // 人为追加半个帧（zstd 帧 magic + 头几个字节）→ torn
    let loc = backend.locate(&h).unwrap();
    let path = Path::new(&loc.path);
    let mut bytes = std::fs::read(path).unwrap();
    // 追加一个 zstd 帧的开头（magic + descriptor）
    bytes.extend_from_slice(&[0x28, 0xB5, 0x2F, 0xFD, 0x24, 0x00]);
    std::fs::write(path, &bytes).unwrap();
    let log = backend.load_stored(&h.id).unwrap().unwrap();
    assert!(log.torn, "torn tail reported");
    assert_eq!(log.events.len(), 2, "committed prefix only");
    let _ = root;
}

#[test]
fn jsonl_repair_truncates_torn_tail() {
    let (backend, _root) = make_backend("repair");
    let h = header("s-repair");
    backend.materialize_batch(&h, &turn_events(0)).unwrap();
    let loc = backend.locate(&h).unwrap();
    let path = Path::new(&loc.path);
    let mut bytes = std::fs::read(path).unwrap();
    let clean_len = bytes.len();
    bytes.extend_from_slice(&[0x28, 0xB5, 0x2F, 0xFD, 0x24, 0x10, 0x00]);
    std::fs::write(path, &bytes).unwrap();
    let log = backend.load_stored(&h.id).unwrap().unwrap();
    assert!(log.torn);
    // 物理截断到 committed
    let truncate_at = log.truncate_offset.expect("truncate offset");
    assert_eq!(truncate_at as usize, clean_len);
    backend.commit_repair_truncate(&h.id, truncate_at).unwrap();
    let bytes2 = std::fs::read(path).unwrap();
    assert_eq!(bytes2.len(), clean_len, "torn tail removed");
    // 可继续追加
    backend.append_events(&h.id, &turn_events(2)).unwrap();
    let log2 = backend.load_stored(&h.id).unwrap().unwrap();
    assert_eq!(log2.events.len(), 4);
}

#[test]
fn jsonl_plain_compression_round_trips() {
    let root = tmp_root("plain");
    let backend = JsonlBackend::new(JsonlConfig {
        root: root.clone(),
        compression: dsh_persistence::format::JsonlCompression::None,
        pack_chunks: false,
    });
    let h = header("s-plain");
    backend.materialize_batch(&h, &turn_events(0)).unwrap();
    let loc = backend.locate(&h).unwrap();
    assert!(loc.path.ends_with("session.jsonl"), "{}", loc.path);
    let raw = backend.read_raw(&h.id).unwrap().unwrap();
    let v: serde_json::Value = serde_json::from_str(raw.content.lines().next().unwrap()).unwrap();
    assert_eq!(v["type"], "session");    let log = backend.load_stored(&h.id).unwrap().unwrap();
    assert_eq!(log.events.len(), 2);
}

#[test]
fn jsonl_list_and_snapshots() {
    let (backend, _root) = make_backend("list");
    for i in 0..3 {
        let h = header(&format!("s-list-{i}"));
        backend.materialize_batch(&h, &turn_events(0)).unwrap();
    }
    let headers = backend.list_headers().unwrap();
    assert_eq!(headers.len(), 3);
    let snaps: Vec<SessionPersistenceSnapshot> = backend.list_snapshot_headers().unwrap();
    assert_eq!(snaps.len(), 3);
    for s in &snaps {
        assert!(!s.revision.raw().is_empty());
        assert!(s.header.id.raw().starts_with("s-list-"));
    }
}

#[test]
fn jsonl_identity_mismatch_rejected() {
    // 构造一个「目录名 = s-bbb，但首行 header = s-aaa」的物理不匹配 artifact
    let root = tmp_root("idmismatch");
    let backend = JsonlBackend::new(JsonlConfig {
        root: root.clone(),
        compression: dsh_persistence::format::JsonlCompression::None,
        pack_chunks: true,
    });
    let h = header("s-aaa"); // header 说自己是 s-aaa
    // 手动写入错误 header：目录名用 s-bbb（与 header 的 s-aaa 不匹配）
    let line = dsh_persistence::format::to_header_line(&h).to_json();
    let dir = root
        .join("_no-cwd")
        .join(dsh_persistence::format::encode_segment("s-bbb").unwrap());
    std::fs::create_dir_all(&dir).unwrap();
    let content = format!("{}\n", serde_json::to_string(&line).unwrap());
    std::fs::write(dir.join("session.jsonl"), content).unwrap();
    let other = SessionId::from_raw("s-bbb");
    let err = backend.load_stored(&other).unwrap_err();
    assert!(matches!(err, PersistenceError::Corruption(_)), "{err}");
    assert!(err.to_string().contains("identity mismatch"), "{err}");
}

// ---- coordinator ----

fn make_coordinator(name: &str, compression: dsh_persistence::format::JsonlCompression) -> (PersistenceCoordinator, PathBuf) {
    let root = tmp_root(name);
    let backend = JsonlBackend::new(JsonlConfig {
        root: root.clone(),
        compression,
        pack_chunks: true,
    });
    (PersistenceCoordinator::new(Box::new(backend)), root)
}

#[test]
fn coordinator_create_is_lazy_append_materializes() {
    let (coord, root) = make_coordinator("coord-lazy", dsh_persistence::format::JsonlCompression::Zstd);
    let h = header("s-c");
    coord.create(&h).unwrap();
    assert!(!coord.is_materialized(&h.id));
    // create 不落盘
    let entries = std::fs::read_dir(&root).unwrap().count();
    assert_eq!(entries, 0, "lazy create writes nothing");
    coord.append(&h.id, &turn_events(0)).unwrap();
    assert!(coord.is_materialized(&h.id));
    assert_eq!(coord.cursor_of(&h.id), Some(2));
    // append 落盘
    let loc = coord.locate(&h).unwrap();
    assert!(std::fs::metadata(&loc.path).is_ok());
}

#[test]
fn coordinator_append_seq_continuity_enforced() {
    let (coord, _root) = make_coordinator("coord-seq", dsh_persistence::format::JsonlCompression::Zstd);
    let h = header("s-seq");
    coord.create(&h).unwrap();
    coord.append(&h.id, &turn_events(0)).unwrap();
    let err = coord.append(&h.id, &[event(5, EventKind::TurnStart)]).unwrap_err();
    assert!(err.to_string().contains("must start at cursor 2"), "{err}");
}

#[test]
fn coordinator_load_returns_balanced_and_repairs_interrupted_turn() {
    let (coord, _root) = make_coordinator("coord-load", dsh_persistence::format::JsonlCompression::Zstd);
    let h = header("s-load");
    coord.create(&h).unwrap();
    // 写不完一个 turn：TurnStart + StepStart（无 closing）
    coord.append(&h.id, &interrupted_turn_events(0)).unwrap();
    let inspection = coord.load(&h.id).unwrap();
    assert!(inspection.is_balanced(), "crash-repaired load is balanced");
    assert!(inspection.events.last().unwrap().kind == EventKind::TurnEnd);
    // 修复事件已持久化
    let raw = coord.read_raw(&h.id).unwrap().unwrap();
    assert!(raw.content.contains("\"turn/end\"") || raw.content.contains("\"type\":\"turn/end\""));
}

#[test]
fn coordinator_rejects_load_while_live_turn_open() {
    let (coord, _root) = make_coordinator("coord-live", dsh_persistence::format::JsonlCompression::Zstd);
    let h = header("s-live");
    coord.create(&h).unwrap();
    coord.append(&h.id, &turn_events(0)).unwrap(); // 物化
    coord.set_live_turn(&h.id, true);
    let err = coord.load(&h.id).unwrap_err();
    assert!(err.to_string().contains("while its live turn is open"), "{err}");
    let err2 = match coord.prepare(&h.id) {
        Err(e) => e,
        Ok(_) => panic!("prepare must fail while live turn open"),
    };
    assert!(err2.to_string().contains("cannot prepare"), "{err2}");
}

#[test]
fn coordinator_ping_pong_append_matches_read_raw() {
    let (coord, _root) = make_coordinator("coord-pong", dsh_persistence::format::JsonlCompression::Zstd);
    let h = header("s-pong");
    coord.create(&h).unwrap();
    for base in [0u64, 2, 4, 6] {
        coord.append(&h.id, &turn_events(base)).unwrap();
    }
    let inspection = coord.load(&h.id).unwrap();
    assert_eq!(inspection.events.len(), 8);
    // readFrom 冷折叠
    let suffix = coord.read_from(&h.id, 4).unwrap();
    assert_eq!(suffix.events[0].seq, 4);
    assert_eq!(suffix.events.len(), 4);
}

#[test]
fn coordinator_list_snapshots_via_backend() {
    let (coord, _root) = make_coordinator("coord-list", dsh_persistence::format::JsonlCompression::Zstd);
    for i in 0..2 {
        let h = header(&format!("s-cl-{i}"));
        coord.create(&h).unwrap();
        coord.append(&h.id, &turn_events(0)).unwrap();
    }
    let snaps = coord.list_snapshots().unwrap();
    assert_eq!(snaps.len(), 2);
    let headers = coord.list().unwrap();
    assert_eq!(headers.len(), 2);
}

#[test]
fn coordinator_plain_and_zstd_do_not_cross_materialize() {
    // 同一根目录下：zstd 后端已物化 s-cross 后，plain 后端拒绝再次物化同名会话
    let root = tmp_root("cross");
    let backend_z = JsonlBackend::new(JsonlConfig {
        root: root.clone(),
        compression: dsh_persistence::format::JsonlCompression::Zstd,
        pack_chunks: true,
    });
    let coord_z = PersistenceCoordinator::new(Box::new(backend_z));
    let h = header("s-cross");
    coord_z.create(&h).unwrap();
    coord_z.append(&h.id, &turn_events(0)).unwrap();
    let backend_p = JsonlBackend::new(JsonlConfig {
        root: root.clone(),
        compression: dsh_persistence::format::JsonlCompression::None,
        pack_chunks: true,
    });
    let coord_p = PersistenceCoordinator::new(Box::new(backend_p));
    let hp = header("s-cross");
    coord_p.create(&hp).unwrap();
    let err = coord_p.append(&hp.id, &turn_events(0)).unwrap_err();
    assert!(err.to_string().contains("compression"), "{err}");
}

// ---- write-behind ----

#[test]
fn write_behind_enqueue_then_flush_single_batch() {
    let mut wb = SessionWriteBehind::new(200);
    let mut sink = dsh_persistence::write_behind::RecordingSink::new();
    wb.enqueue(event(0, EventKind::TurnStart), 100);
    assert!(wb.has_work());
    wb.flush(&mut sink).unwrap();
    assert!(!wb.has_work());
    assert_eq!(sink.writes.len(), 1);
    assert_eq!(sink.writes[0].len(), 1);
}

#[test]
fn write_behind_tick_deadline_flushes_automatic() {
    let mut wb = SessionWriteBehind::new(200);
    let mut sink = dsh_persistence::write_behind::RecordingSink::new();
    wb.enqueue(event(0, EventKind::TurnStart), 1000);
    // 未到期
    assert!(!wb.tick(&mut sink, 1199));
    assert_eq!(sink.writes.len(), 0);
    // 到期 → automatic 写入
    assert!(wb.tick(&mut sink, 1200));
    assert_eq!(sink.writes.len(), 1);
}

#[test]
fn write_behind_failure_retains_order_and_pauses() {
    let mut wb = SessionWriteBehind::new(200);
    let mut sink = dsh_persistence::write_behind::RecordingSink::new();
    sink.fail_after = Some(0); // 第一次写失败
    wb.enqueue(event(0, EventKind::TurnStart), 1000);
    wb.enqueue(event(1, EventKind::TurnEnd), 1001);
    // 失败保留 + pause
    let err = wb.flush(&mut sink).expect_err("durable write fails");
    assert!(err.contains("durable write failed"), "{err}");
    assert!(wb.is_automatic_paused());
    assert_eq!(wb.pending_len(), 2);
    assert_eq!(wb.pending_events()[0].seq, 0, "order preserved");
    // 恢复：unpause 后再 flush 成功
    sink.fail_after = None;
    wb.enqueue(event(2, EventKind::TurnStart), 1002);
    assert!(!wb.is_automatic_paused(), "enqueue unpauses");
    wb.flush(&mut sink).unwrap();
    assert!(!wb.has_work());
    assert_eq!(sink.writes.len(), 1);
    let seqs: Vec<u64> = sink.writes[0].iter().map(|e| e.seq).collect();
    assert_eq!(seqs, vec![0, 1, 2], "retained + new in order");
}

#[test]
fn write_behind_tick_writes_pending_on_deadline() {
    let mut wb = SessionWriteBehind::new(100);
    let mut sink = dsh_persistence::write_behind::RecordingSink::new();
    wb.enqueue(event(0, EventKind::TurnStart), 1000);
    wb.enqueue(event(1, EventKind::TurnEnd), 1000);
    // 未到期
    assert!(!wb.tick(&mut sink, 1099));
    assert_eq!(sink.writes.len(), 0);
    // 到期 → 后台自动写入一个 batch
    assert!(wb.tick(&mut sink, 1100));
    assert_eq!(sink.writes.len(), 1);
    assert_eq!(sink.writes[0].len(), 2);
    assert!(!wb.has_work());
}

// ---- crash repair（interruptedTurnClosers 单测放 dsh-session；这里验证闭环） ----

#[test]
fn interrupted_turn_closers_sequential_and_balanced() {
    let events = interrupted_turn_events(0);
    let closers = interrupted_turn_closers(&events);
    // StepEnd + TurnEnd
    assert_eq!(closers.len(), 2);
    assert_eq!(closers[0].kind, EventKind::StepEnd);
    assert_eq!(closers[1].kind, EventKind::TurnEnd);
    // seq 连续（2, 3）
    assert_eq!(closers[0].seq, 2);
    assert_eq!(closers[1].seq, 3);
    // time 复用最后真实事件
    assert_eq!(closers[0].time, events[1].time);
}

// ---- seam 双后端一致性（PersistenceBackend trait 形状） ----

#[test]
fn jsonl_backend_implements_backend_contract() {
    fn takes_backend(b: &dyn PersistenceBackend) -> bool {
        b.supports_raw_artifacts()
    }
    let (backend, _root) = make_backend("contract");
    assert!(takes_backend(&backend));
    assert_eq!(backend.locate(&header("c")).unwrap().kind, "jsonl");
}

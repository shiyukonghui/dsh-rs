//! M1d（dsh-persistence:import）：TS JSONL 产物导入工具。
//!
//! 权威参考：迁移计划 §5.5（SessionImport——读取 TS 侧 JSONL 产物，经
//! Session.fromRestore 语义导入 Rust JSONL）。验证：解码 zstd/plaintext artifact、
//! RESTORE 校验、落库、覆盖拒绝、坏事件拒绝。

use std::path::{Path, PathBuf};

use dsh_brand::SessionId;
use dsh_persistence::coordinator::PersistenceCoordinator;
use dsh_persistence::format::JsonlCompression;
use dsh_persistence::import::{import_session_events, import_session_from_artifact};
use dsh_persistence::jsonl::{JsonlBackend, JsonlConfig};
use dsh_persistence::seam::SessionPersistence;
use dsh_session::types::{EventKind, SessionEvent, SessionHeader};

fn tmp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dsh-m1d-import-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn header(id: &str) -> SessionHeader {
    let mut h = SessionHeader::new(SessionId::from_raw(id), 1_700_000_000_000);
    h.cwd = Some(r"C:\work\proj".into());
    h
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

fn make_rust_store(name: &str) -> PersistenceCoordinator {
    let root = tmp_root(&format!("rust-{name}"));
    let backend = JsonlBackend::new(JsonlConfig {
        root,
        compression: JsonlCompression::Zstd,
        pack_chunks: true,
    });
    PersistenceCoordinator::new(Box::new(backend))
}

/// 写一个 TS 侧 plaintext artifact（不带物理编码，模拟 TS 写入的 JSONL）。
fn write_ts_plain_artifact(root: &Path, id: &str, events: &[SessionEvent]) -> PathBuf {
    let h = header(id);
    let encoded = dsh_persistence::format::encode_segment(id).unwrap();
    let dir = root.join("_no-cwd").join(encoded);
    std::fs::create_dir_all(&dir).unwrap();
    let mut content = serde_json::to_string(&dsh_persistence::format::to_header_line(&h).to_json()).unwrap();
    content.push('\n');
    for e in events {
        content.push_str(&serde_json::to_string(&e).unwrap());
        content.push('\n');
    }
    let path = dir.join("session.jsonl");
    std::fs::write(&path, &content).unwrap();
    path
}

/// 写一个 TS 侧 zstd artifact（header 帧 + 事件帧）。
fn write_ts_zstd_artifact(root: &Path, id: &str, events: &[SessionEvent]) -> PathBuf {
    let h = header(id);
    let encoded = dsh_persistence::format::encode_segment(id).unwrap();
    let dir = root.join("_no-cwd").join(encoded);
    std::fs::create_dir_all(&dir).unwrap();
    let mut header_bytes = serde_json::to_string(&dsh_persistence::format::to_header_line(&h).to_json()).unwrap();
    header_bytes.push('\n');
    let mut content = dsh_persistence::zstd::compress_zstd_frame(header_bytes.as_bytes()).unwrap();
    let body = serde_json::to_string(&events).unwrap(); // unused; we encode line by line
    let _ = body;
    let mut lines = String::new();
    for e in events {
        lines.push_str(&serde_json::to_string(&e).unwrap());
        lines.push('\n');
    }
    content.extend_from_slice(&dsh_persistence::zstd::compress_zstd_frame(lines.as_bytes()).unwrap());
    let path = dir.join("session.jsonl.zstd");
    std::fs::write(&path, &content).unwrap();
    path
}

#[test]
fn import_from_plain_artifact_into_rust_jsonl() {
    let ts_root = tmp_root("ts-plain");
    let path = write_ts_plain_artifact(&ts_root, "s-imp-plain", &turn_events(0));
    let store = make_rust_store("plain");
    let result =
        import_session_from_artifact(&store, &path, JsonlCompression::None).unwrap();
    assert_eq!(result.id.raw(), "s-imp-plain");
    assert_eq!(result.event_count, 2);
    // 落库可读
    let inspection = store.load(&result.id).unwrap();
    assert_eq!(inspection.events.len(), 2);
    assert!(inspection.is_balanced());
}

#[test]
fn import_from_zstd_artifact_into_rust_jsonl() {
    let ts_root = tmp_root("ts-zstd");
    let path = write_ts_zstd_artifact(&ts_root, "s-imp-z", &turn_events(0));
    let store = make_rust_store("zstd");
    let result =
        import_session_from_artifact(&store, &path, JsonlCompression::Zstd).unwrap();
    assert_eq!(result.event_count, 2);
    let inspection = store.load(&result.id).unwrap();
    assert_eq!(inspection.events.len(), 2);
}

#[test]
fn import_refuses_overwrite_existing_session() {
    let ts_root = tmp_root("dup");
    let path = write_ts_plain_artifact(&ts_root, "s-imp-dup", &turn_events(0));
    let store = make_rust_store("dup-store");
    import_session_from_artifact(&store, &path, JsonlCompression::None).unwrap();
    let err = import_session_from_artifact(&store, &path, JsonlCompression::None).unwrap_err();
    assert!(err.to_string().contains("already exists"), "{err}");
}

#[test]
fn import_rejects_corrupt_artifact() {
    let ts_root = tmp_root("bad");
    let dir = ts_root.join("_no-cwd").join("s-imp-bad");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("session.jsonl");
    std::fs::write(&path, "not json\n").unwrap();
    let store = make_rust_store("bad-store");
    let err = import_session_from_artifact(&store, &path, JsonlCompression::None).unwrap_err();
    assert!(matches!(err, dsh_persistence::seam::PersistenceError::Corruption(_)), "{err}");
}

#[test]
fn import_session_events_requires_valid_events() {
    let store = make_rust_store("bad-events");
    let h = header("s-imp-bad-events");
    // seq 不连续 → RESTORE 校验拒绝
    let bad = vec![turn_events(0)[0].clone()]; // seq 0 only, but turn unbalanced is fine for restore?
    // 用一个 seq 跳变的
    let e0 = SessionEvent::new(5, 1_700_000_000_005, EventKind::TurnStart, serde_json::json!({ "turn": 1 }));
    let result = import_session_events(&store, &h, &[e0]);
    let err = result.expect_err("non-zero-contiguous seed must be rejected by restore");
    assert!(err.to_string().contains("restore validation") || err.to_string().contains("seq"), "{err}");
    let _ = bad;
}

/// 交叉验证反向（Rust 经 JsonlBackend 写出的 artifact → TS 宿主经 node:zlib 解码）。
/// 由外部脚本（set DSH_CROSSGEN_WRITE=<dir>）驱动；未设置时跳过。
#[test]
fn crossgen_writes_artifact_for_ts_read() {
    let Some(dir) = std::env::var_os("DSH_CROSSGEN_WRITE") else {
        eprintln!("skipping crossgen write: DSH_CROSSGEN_WRITE unset");
        return;
    };
    let root = PathBuf::from(dir).join("rust-write");
    let backend = JsonlBackend::new(JsonlConfig {
        root: root.clone(),
        compression: JsonlCompression::Zstd,
        pack_chunks: true,
    });
    let coord = PersistenceCoordinator::new(Box::new(backend));
    let h = header("s-rust2ts");
    coord.create(&h).unwrap();
    coord.append(&h.id, &turn_events(0)).unwrap();
    // 打印 artifact 路径供 TS 端读取（header 有 cwd → projectDir 编码）
    let proj = dsh_persistence::format::project_dir("", h.cwd.as_deref()).unwrap();
    let encoded = dsh_persistence::format::encode_segment("s-rust2ts").unwrap();
    eprintln!("RUST_ARTIFACT={}", root.join(proj).join(encoded).join("session.jsonl.zstd").display());
}

/// 交叉验证（TS 宿主经 node:zlib 写出的 artifact → Rust 读取）。
/// 由外部脚本（set DSH_CROSSGEN_ARTIFACT=<path>）驱动；未设置时跳过。
#[test]
fn crossgen_reads_ts_written_artifact() {
    let Some(path) = std::env::var_os("DSH_CROSSGEN_ARTIFACT") else {
        eprintln!("skipping crossgen: DSH_CROSSGEN_ARTIFACT unset");
        return;
    };
    let path = PathBuf::from(path);
    let store = make_rust_store("crossgen-store");
    let result = import_session_from_artifact(&store, &path, JsonlCompression::Zstd).unwrap();
    assert_eq!(result.id.raw(), "s-ts2rust");
    assert_eq!(result.event_count, 2);
    let inspection = store.load(&result.id).unwrap();
    assert!(inspection.is_balanced());
    assert_eq!(inspection.events[0].kind, EventKind::TurnStart);
    assert_eq!(inspection.events[1].kind, EventKind::TurnEnd);
}

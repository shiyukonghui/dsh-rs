//! SQLite 持久化后端（M6 step10 复活（D-091）：Q6 历史 backlog 落地）。
//!
//! 权威参考：`deepseek-harness/packages/session/session-persistence-*` 的 coordinator
//! 契约（`PersistenceBackend`）。在 `SessionPersistence` seam 之上提供 SQLite 物理后端：
//! - 每会话两表：`sessions(id, header, revision)` + `events(id, seq, json)`；
//! - 事件以完整 `SessionEvent` serde JSON 落盘（type/seq/time/data/surfaceOp/
//!   sourceEventSeqs/ignorable 全程保真）；
//! - 事务原子：materialize（header+首批次）、append（seq 连续性校验）、repair；
//! - 事务保证 → 无不完整写入：**无 torn 尾**（与 JSONL 物理 torn 语义差异如实记录，
//!   后端仍按契约返回 `torn: false` + `truncate_offset: None`）；
//! - revision = 该会话已持久化写入计数器（`sqlite:<file>:<rev>`，变更即可被观察）。

// rusqlite Connection 具备内部可变性（RefCell），后端可达 `&self` 方法——匹配本
// build 的单线程纪律（D-006）；SQLite 连接非 Send/Sync，按 coordinator 设计单线程持有。
#![allow(clippy::arc_with_non_send_sync)]

use std::path::{Path, PathBuf};

use dsh_brand::SessionId;
use dsh_session::types::{SessionEvent, SessionHeader};

use crate::seam::{
    PersistenceBackend, PersistenceError, SessionLocation, SessionPersistenceCorruptionError,
    SessionPersistenceRevision, SessionPersistenceSnapshot, SessionRawArtifact, StoredLog,
};

/// SQLite 后端配置。
#[derive(Debug, Clone)]
pub struct SqliteConfig {
    /// 数据库文件路径（不存在则创建）。
    pub path: PathBuf,
}

/// SQLite 原生后端（无内存状态；会话逻辑状态由 `PersistenceCoordinator` 持有）。
/// rusqlite 0.40 的 `transaction` 要求 `&mut Connection`——本 build 单线程纪律
/// （D-006）下用 `RefCell` 提供内部可变性（`PersistenceBackend` 仅 `&self`）。
pub struct SqliteBackend {
    path: PathBuf,
    conn: std::cell::RefCell<rusqlite::Connection>,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS sessions (
  id TEXT PRIMARY KEY,
  header TEXT NOT NULL,
  revision INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS events (
  id TEXT NOT NULL,
  seq INTEGER NOT NULL,
  json TEXT NOT NULL,
  PRIMARY KEY (id, seq)
);
";

impl SqliteBackend {
    /// 打开（创建）SQLite 数据库并确保 schema。
    pub fn open(path: &Path) -> Result<Self, PersistenceError> {
        let conn = rusqlite::Connection::open(path).map_err(|e| {
            PersistenceError::Other(format!("sqlite open {}: {e}", path.display()))
        })?;
        conn.execute_batch(SCHEMA)
            .map_err(|e| PersistenceError::Other(format!("sqlite schema {}: {e}", path.display())))?;
        Ok(SqliteBackend {
            path: path.to_path_buf(),
            conn: std::cell::RefCell::new(conn),
        })
    }

    fn db_path(&self) -> String {
        self.path.display().to_string()
    }

    /// 会话 revision（`sqlite:<path>:<write-count>`；变更即可被观察）。
    fn revision_for(&self, id: &SessionId) -> Result<Option<SessionPersistenceRevision>, PersistenceError> {
        let conn = self.conn.borrow();
        let mut stmt = conn
            .prepare("SELECT revision FROM sessions WHERE id = ?1")
            .map_err(|e| PersistenceError::Other(format!("sqlite rev prepare: {e}")))?;
        let mut rows = stmt
            .query_map(rusqlite::params![id.raw()], |r| r.get::<_, i64>(0))
            .map_err(|e| PersistenceError::Other(format!("sqlite rev query: {e}")))?;
        match rows.next() {
            Some(Ok(rev)) => Ok(Some(SessionPersistenceRevision::from_raw(format!(
                "sqlite:{}:{rev}",
                self.db_path()
            )))),
            Some(Err(e)) => Err(PersistenceError::Other(format!("sqlite rev row: {e}"))),
            None => Ok(None),
        }
    }

    fn read_header(&self, id: &SessionId) -> Result<Option<SessionHeader>, PersistenceError> {
        let conn = self.conn.borrow();
        let mut stmt = conn
            .prepare("SELECT header FROM sessions WHERE id = ?1")
            .map_err(|e| PersistenceError::Other(format!("sqlite header prepare: {e}")))?;
        let mut rows = stmt
            .query_map(rusqlite::params![id.raw()], |r| r.get::<_, String>(0))
            .map_err(|e| PersistenceError::Other(format!("sqlite header query: {e}")))?;
        match rows.next() {
            Some(Ok(text)) => serde_json::from_str(&text)
                .map(Some)
                .map_err(|e| PersistenceError::Corruption(SessionPersistenceCorruptionError {
                    message: format!("sqlite header decode {}: {e}", id.raw()),
                    cause: None,
                })),
            Some(Err(e)) => Err(PersistenceError::Other(format!("sqlite header row: {e}"))),
            None => Ok(None),
        }
    }

    /// 读取会话事件（按 seq 有序；校验 0..n 连续——事务库保证物理完整，缺口仅可能
    /// 人为删除 → 视为 corruption）。
    fn read_events(&self, id: &SessionId) -> Result<Vec<SessionEvent>, PersistenceError> {
        let conn = self.conn.borrow();
        let mut stmt = conn
            .prepare("SELECT seq, json FROM events WHERE id = ?1 ORDER BY seq ASC")
            .map_err(|e| PersistenceError::Other(format!("sqlite events prepare: {e}")))?;
        let rows = stmt
            .query_map(rusqlite::params![id.raw()], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(|e| PersistenceError::Other(format!("sqlite events query: {e}")))?;
        let mut out = Vec::new();
        for row in rows {
            let (seq, text) = row.map_err(|e| PersistenceError::Other(format!("sqlite events row: {e}")))?;
            if seq != out.len() as i64 {
                return Err(PersistenceError::Corruption(SessionPersistenceCorruptionError {
                    message: format!(
                        "sqlite session {}: event seq discontinuity at {seq} (expected {})",
                        id.raw(),
                        out.len()
                    ),
                    cause: None,
                }));
            }
            let ev: SessionEvent = serde_json::from_str(&text).map_err(|e| {
                PersistenceError::Corruption(SessionPersistenceCorruptionError {
                    message: format!("sqlite session {}: event {seq} decode: {e}", id.raw()),
                    cause: None,
                })
            })?;
            out.push(ev);
        }
        Ok(out)
    }

    /// 校验并写入一批事件（须从 `next` 续接）；返回写入后的新 revision 计数。
    fn write_events(
        &self,
        tx: &rusqlite::Transaction<'_>,
        id: &SessionId,
        events: &[SessionEvent],
    ) -> Result<(), PersistenceError> {
        let mut stmt = tx
            .prepare("INSERT INTO events (id, seq, json) VALUES (?1, ?2, ?3)")
            .map_err(|e| PersistenceError::Other(format!("sqlite insert prepare: {e}")))?;
        for (i, ev) in events.iter().enumerate() {
            let text = serde_json::to_string(ev)
                .map_err(|e| PersistenceError::Other(format!("sqlite event serialize: {e}")))?;
            stmt.execute(rusqlite::params![id.raw(), ev.seq as i64, text])
                .map_err(|e| {
                    PersistenceError::Invalid(format!(
                        "sqlite append {}: {e} (seq contiguity/duplicate at {} to {})",
                        id.raw(),
                        i,
                        ev.seq
                    ))
                })?;
        }
        Ok(())
    }

    fn bump_revision(
        &self,
        tx: &rusqlite::Transaction<'_>,
        id: &SessionId,
    ) -> Result<(), PersistenceError> {
        tx.execute(
            "UPDATE sessions SET revision = revision + 1 WHERE id = ?1",
            rusqlite::params![id.raw()],
        )
        .map(|_| ())
        .map_err(|e| PersistenceError::Other(format!("sqlite rev bump {}: {e}", id.raw())))
    }
}

impl PersistenceBackend for SqliteBackend {
    fn locate(&self, _meta: &SessionHeader) -> Option<SessionLocation> {
        Some(SessionLocation {
            kind: "sqlite".into(),
            path: self.db_path(),
        })
    }

    fn supports_raw_artifacts(&self) -> bool {
        false
    }

    fn read_raw(&self, _id: &SessionId) -> Result<Option<SessionRawArtifact>, PersistenceError> {
        Ok(None)
    }

    fn load_stored(&self, id: &SessionId) -> Result<Option<StoredLog>, PersistenceError> {
        let Some(meta) = self.read_header(id)? else {
            return Ok(None);
        };
        let events = self.read_events(id)?;
        let revision = self.revision_for(id)?.unwrap_or_else(|| {
            SessionPersistenceRevision::from_raw(format!("sqlite:{}:0", self.db_path()))
        });
        Ok(Some(StoredLog {
            meta,
            events,
            revision,
            // 事务原子写 → 无不完整尾部（与 JSONL 物理 torn 差异如实：语义上无 torn）。
            torn: false,
            truncate_offset: None,
        }))
    }

    fn read_stored_revision(
        &self,
        id: &SessionId,
    ) -> Result<Option<SessionPersistenceRevision>, PersistenceError> {
        self.revision_for(id)
    }

    fn append_batch(
        &self,
        meta: &SessionHeader,
        events: &[SessionEvent],
    ) -> Result<(), PersistenceError> {
        let mut conn = self.conn.borrow_mut();
        let tx = conn
            .transaction()
            .map_err(|e| PersistenceError::Other(format!("sqlite txn: {e}")))?;
        let id = &meta.id;
        let next: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(seq), -1) + 1 FROM events WHERE id = ?1",
                rusqlite::params![id.raw()],
                |r| r.get(0),
            )
            .map_err(|e| PersistenceError::Other(format!("sqlite max seq: {e}")))?;
        if events.first().map(|e| e.seq as i64) != Some(next) {
            return Err(PersistenceError::Invalid(format!(
                "sqlite append {}: expected seq {next}, got {:?}",
                id.raw(),
                events.first().map(|e| e.seq)
            )));
        }
        self.write_events(&tx, id, events)?;
        tx.execute(
            "INSERT OR IGNORE INTO sessions (id, header, revision) VALUES (?1, ?2, 0)",
            rusqlite::params![id.raw(), serde_json::to_string(meta).unwrap_or_default()],
        )
        .map_err(|e| PersistenceError::Other(format!("sqlite append session row: {e}")))?;
        self.bump_revision(&tx, id)?;
        tx.commit()
            .map_err(|e| PersistenceError::Other(format!("sqlite append commit: {e}")))
    }

    fn materialize_batch(
        &self,
        meta: &SessionHeader,
        events: &[SessionEvent],
    ) -> Result<(), PersistenceError> {
        let mut conn = self.conn.borrow_mut();
        let tx = conn
            .transaction()
            .map_err(|e| PersistenceError::Other(format!("sqlite txn: {e}")))?;
        let id = &meta.id;
        let header_text = serde_json::to_string(meta)
            .map_err(|e| PersistenceError::Other(format!("sqlite header serialize: {e}")))?;
        // D-092：create-or-replace（镜像 JSONL 原子覆盖）。coordinator/restore_one
        // 恢复后经 coord.append 重灌同一会话 → 首 append 走 materialize_batch；
        // 「重复拒绝」会让恢复游标错位、后续写失败（M6W-REQUIREMENTS §3 越级发现）。
        // 覆盖为同一会话重写 header + events，事务原子。
        tx.execute(
            "INSERT OR REPLACE INTO sessions (id, header, revision) VALUES (?1, ?2, 0)",
            rusqlite::params![id.raw(), header_text],
        )
        .map_err(|e| PersistenceError::Other(format!("sqlite materialize: {e}")))?;
        tx.execute("DELETE FROM events WHERE id = ?1", rusqlite::params![id.raw()])
            .map_err(|e| PersistenceError::Other(format!("sqlite materialize clear: {e}")))?;
        // 首批次须从 seq 0 连续。
        if events.first().map(|e| e.seq) != Some(0) {
            return Err(PersistenceError::Invalid(format!(
                "sqlite materialize {}: first event seq must be 0",
                id.raw()
            )));
        }
        self.write_events(&tx, id, events)?;
        self.bump_revision(&tx, id)?;
        tx.commit()
            .map_err(|e| PersistenceError::Other(format!("sqlite materialize commit: {e}")))
    }

    fn commit_repair(
        &self,
        id: &SessionId,
        torn_offset: Option<u64>,
        closers: &[SessionEvent],
    ) -> Result<(), PersistenceError> {
        let mut conn = self.conn.borrow_mut();
        let tx = conn
            .transaction()
            .map_err(|e| PersistenceError::Other(format!("sqlite txn: {e}")))?;
        // torn_offset 语义：删除 seq >= torn_offset 的尾部（会话局部行数界；
        // 事务库无物理 torn 字节，以 seq 阈表达截断面——与 JSONL 字节偏移的
        // 差异如实记录）。
        if let Some(offset) = torn_offset {
            tx.execute(
                "DELETE FROM events WHERE id = ?1 AND seq >= ?2",
                rusqlite::params![id.raw(), offset as i64],
            )
            .map_err(|e| PersistenceError::Other(format!("sqlite repair delete: {e}")))?;
        }
        let next: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(seq), -1) + 1 FROM events WHERE id = ?1",
                rusqlite::params![id.raw()],
                |r| r.get(0),
            )
            .map_err(|e| PersistenceError::Other(format!("sqlite repair max seq: {e}")))?;
        // 重编号 closing 事件到续接 seq（reject 原始 seq 不匹配的输入）。
        if closers
            .first()
            .map(|e| e.seq as i64)
            .is_some_and(|s| s != next)
        {
            return Err(PersistenceError::Invalid(format!(
                "sqlite repair {}: closers must start at seq {next}",
                id.raw()
            )));
        }
        self.write_events(&tx, id, closers)?;
        self.bump_revision(&tx, id)?;
        tx.commit()
            .map_err(|e| PersistenceError::Other(format!("sqlite repair commit: {e}")))
    }

    fn list_snapshots(&self) -> Result<Vec<SessionPersistenceSnapshot>, PersistenceError> {
        let conn = self.conn.borrow();
        let mut stmt = conn
            .prepare("SELECT header, revision FROM sessions ORDER BY id ASC")
            .map_err(|e| PersistenceError::Other(format!("sqlite snaps prepare: {e}")))?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .map_err(|e| PersistenceError::Other(format!("sqlite snaps query: {e}")))?;
        let mut out = Vec::new();
        for row in rows {
            let (header_text, rev) =
                row.map_err(|e| PersistenceError::Other(format!("sqlite snaps row: {e}")))?;
            let header: SessionHeader = serde_json::from_str(&header_text).map_err(|e| {
                PersistenceError::Corruption(SessionPersistenceCorruptionError {
                    message: format!("sqlite snap header decode: {e}"),
                    cause: None,
                })
            })?;
            out.push(SessionPersistenceSnapshot {
                header,
                revision: SessionPersistenceRevision::from_raw(format!("sqlite:{}:{rev}", self.db_path())),
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_session::types::EventKind;
    use serde_json::json;

    fn tmp_db(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("dsh-sqlite-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (dir.clone(), dir.join("store.sqlite"))
    }

    fn ev(seq: u64, kind: EventKind, v: serde_json::Value) -> SessionEvent {
        SessionEvent::new(seq, 1000 + seq as i64, kind, v)
    }

    fn hdr(id: &str) -> SessionHeader {
        SessionHeader::new(SessionId::from_raw(id.to_string()), 0)
    }

    /// 后端级往返：materialize + append + load_stored + revision + locate/raw 契约。
    #[test]
    fn sqlite_roundtrip_materialize_append_load_real() {
        let (_dir, db) = tmp_db("roundtrip");
        let backend = SqliteBackend::open(&db).expect("open");
        let meta = hdr("default");
        backend
            .materialize_batch(
                &meta,
                &[
                    ev(0, EventKind::UserMessage, json!({"text": "hi"})),
                    ev(1, EventKind::ToolCall, json!({"tool": "todo_write"})),
                ],
            )
            .expect("materialize");
        backend
            .append_batch(
                &hdr("default"),
                &[
                    ev(2, EventKind::AssistantMessage, json!({"text": "ok"})),
                    ev(3, EventKind::TurnEnd, json!({"reason": "stop"})),
                ],
            )
            .expect("append");
        let stored = backend.load_stored(&SessionId::from_raw("default".to_string())).unwrap().expect("loaded");
        assert_eq!(stored.meta, meta, "header roundtrip");
        assert_eq!(stored.events.len(), 4);
        assert_eq!(stored.events[3].kind, EventKind::TurnEnd);
        assert!(!stored.torn, "transactional db: no torn tail");
        assert!(backend
            .read_stored_revision(&SessionId::from_raw("default".to_string()))
            .unwrap()
            .is_some());
        let loc = backend.locate(&meta).expect("locate");
        assert_eq!(loc.kind, "sqlite");
        assert!(!backend.supports_raw_artifacts());
        assert!(backend.read_raw(&SessionId::from_raw("default".to_string())).unwrap().is_none());
        let snaps = backend.list_snapshots().unwrap();
        assert_eq!(snaps.len(), 1);
        assert_eq!(snaps[0].header.id.raw(), "default");
    }

    /// 落盘/回读跨 reopen 持久（真实数据库文件）。
    #[test]
    fn sqlite_database_durable_across_reopen() {
        let (_dir, db) = tmp_db("durable");
        {
            let backend = SqliteBackend::open(&db).unwrap();
            let meta = hdr("default");
            backend
                .materialize_batch(
                    &meta,
                    &[
                        ev(0, EventKind::UserMessage, json!({"text": "hello"})),
                        ev(1, EventKind::TurnEnd, json!({"reason": "stop"})),
                    ],
                )
                .unwrap();
        }
        // 全新后端打开同一文件——回读事件仍在。
        let backend = SqliteBackend::open(&db).unwrap();
        let stored = backend
            .load_stored(&SessionId::from_raw("default".to_string()))
            .unwrap()
            .expect("reopen load");
        assert_eq!(stored.events.len(), 2);
        assert_eq!(stored.events[0].data["text"], "hello");
    }

    /// materialize = create-or-replace（镜像 JSONL 原子覆盖；供 rehydrate 重灌幂等，
    /// D-092 修正）+ 非连续 append seq 仍 fail-loud。
    #[test]
    fn sqlite_materialize_is_idempotent_create_or_replace_and_seq_gap_rejected() {
        let (_dir, db) = tmp_db("replace");
        let backend = SqliteBackend::open(&db).unwrap();
        let meta = hdr("default");
        backend
            .materialize_batch(&meta, &[ev(0, EventKind::UserMessage, json!({"text": "a"}))])
            .unwrap();
        // 重复 materialize：幂等原子覆盖（新 header + 事件），非 Err。
        let mut meta2 = meta.clone();
        meta2.created_at = 1;
        backend
            .materialize_batch(
                &meta2,
                &[
                    ev(0, EventKind::UserMessage, json!({"text": "b"})),
                    ev(1, EventKind::TurnEnd, json!({"reason": "stop"})),
                ],
            )
            .expect("idempotent overwrite");
        let stored = backend
            .load_stored(&SessionId::from_raw("default".to_string()))
            .unwrap()
            .expect("loaded");
        assert_eq!(stored.meta.created_at, 1, "header overwritten");
        assert_eq!(stored.events.len(), 2, "events replaced");
        assert_eq!(stored.events[0].data["text"], "b");
        // seq 缺口 append 仍 fail-loud。
        let gap = backend.append_batch(
            &meta2,
            &[ev(5, EventKind::TurnEnd, json!({}))],
        );
        assert!(gap.is_err(), "seq gap rejected: {gap:?}");
    }

    /// revision 在写入间变更（变更即可被观察）。
    #[test]
    fn sqlite_revision_changes_on_write() {
        let (_dir, db) = tmp_db("rev");
        let backend = SqliteBackend::open(&db).unwrap();
        let meta = hdr("default");
        backend
            .materialize_batch(&meta, &[ev(0, EventKind::UserMessage, json!({}))])
            .unwrap();
        let r1 = backend
            .read_stored_revision(&SessionId::from_raw("default".to_string()))
            .unwrap()
            .expect("rev1");
        backend
            .append_batch(&hdr("default"), &[ev(1, EventKind::TurnEnd, json!({}))])
            .unwrap();
        let r2 = backend
            .read_stored_revision(&SessionId::from_raw("default".to_string()))
            .unwrap()
            .expect("rev2");
        assert_ne!(r1, r2, "revision changed after append");
    }

    /// commit_repair：截断 torn 尾 + 追加 closing 事件。
    #[test]
    fn sqlite_commit_repair_truncates_and_closes() {
        let (_dir, db) = tmp_db("repair");
        let backend = SqliteBackend::open(&db).unwrap();
        let meta = hdr("default");
        backend
            .materialize_batch(
                &meta,
                &[
                    ev(0, EventKind::UserMessage, json!({"text": "a"})),
                    ev(1, EventKind::ToolCall, json!({"tool": "t"})),
                    ev(2, EventKind::AssistantChunk, json!({"text": "partial"})),
                ],
            )
            .unwrap();
        // torn 尾：删除 seq>=1（半截工具回合），追加 closing turn/end。
        backend
            .commit_repair(
                &SessionId::from_raw("default".to_string()),
                Some(1),
                &[
                    ev(1, EventKind::TurnEnd, json!({"reason": "error"})),
                    ev(2, EventKind::TurnEnd, json!({"reason": "closing"})),
                ],
            )
            .unwrap();
        let stored = backend
            .load_stored(&SessionId::from_raw("default".to_string()))
            .unwrap()
            .expect("loaded");
        let kinds: Vec<String> = stored.events.iter().map(|e| e.kind.as_str().to_string()).collect();
        assert_eq!(kinds, ["user/message", "turn/end", "turn/end"]);
        assert_eq!(stored.events[1].seq, 1);
    }

    /// surface 字段全程保真（surfaceOp/sourceEventSeqs 经 surface 追加的助手构造）。
    #[test]
    fn sqlite_roundtrip_preserves_surface_fields() {
        let (_dir, db) = tmp_db("surface");
        let backend = SqliteBackend::open(&db).unwrap();
        let meta = hdr("default");
        let mut e = ev(0, EventKind::UserMessage, json!({"text": "hi"}));
        // SessionEvent 支持构造 surface 标记（无害可选项）。
        e = e.with_surface_op(dsh_session::types::SurfaceOp::Append);
        e = e.with_source_event_seqs(vec![7, 8]);
        backend.materialize_batch(&meta, &[e]).unwrap();
        e = ev(1, EventKind::TurnEnd, json!({"reason": "stop"}));
        backend.append_batch(&hdr("default"), &[e]).unwrap();
        let stored = backend
            .load_stored(&SessionId::from_raw("default".to_string()))
            .unwrap()
            .expect("loaded");
        assert_eq!(stored.events[0].surface_op(), Some(&dsh_session::types::SurfaceOp::Append));
        assert_eq!(
            stored.events[0].source_event_seqs(),
            Some(&vec![7, 8])
        );
    }

    /// coordinator 无缝：SessionPersistence 缝经 SQLite 后端全链路。
    #[test]
    fn sqlite_plugs_into_coordinator_roundtrip() {
        use crate::coordinator::PersistenceCoordinator;
        use crate::seam::SessionPersistence;
        let (_dir, db) = tmp_db("coord");
        let backend = SqliteBackend::open(&db).unwrap();
        let coord: Box<dyn SessionPersistence> = Box::new(PersistenceCoordinator::new(Box::new(backend)));
        let meta = hdr("default");
        coord.create(&meta).unwrap();
        coord
            .append(
                &SessionId::from_raw("default".to_string()),
                &[
                    ev(0, EventKind::UserMessage, json!({"text": "hi"})),
                    ev(1, EventKind::ToolCall, json!({"tool": "t"})),
                    ev(2, EventKind::AssistantMessage, json!({"text": "ok"})),
                    ev(3, EventKind::TurnEnd, json!({"reason": "stop"})),
                ],
            )
            .unwrap();
        let insp = coord.load(&SessionId::from_raw("default".to_string())).unwrap();
        assert_eq!(insp.events.len(), 4);
        assert!(insp.is_balanced(), "turn/end closed");
        let suffix = coord
            .read_from(&SessionId::from_raw("default".to_string()), 2)
            .unwrap();
        assert_eq!(suffix.events.len(), 2);
        assert_eq!(suffix.events[0].seq, 2);
        let list = coord.list().unwrap();
        assert_eq!(list.len(), 1);
    }
}

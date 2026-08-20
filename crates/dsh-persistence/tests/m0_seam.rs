//! dsh-persistence seam 契约测试。
//!
//! 权威参考：`deepseek-harness/packages/session/session-persistence/src/{index,coordinator,revision}.ts`。
//! 断言：① 缝类型（Inspection/Preparation/Location/Snapshot/RawArtifact/Revision）形状与语义；
//! ② Preparation RAII（释放一次、幂等、发布后跳过）；③ 错误分层与方向感知拒绝文本；
//! ④ 缝 trait 可被任意后端实现（用内存 mock 验证 create/append/load/list 全形状）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use dsh_brand::SessionId;
use dsh_persistence::{
    session_format_version_refusal, PersistenceError, SessionFormatUnsupportedError,
    SessionInspection, SessionLocation, SessionPersistence, SessionPersistenceCorruptionError,
    SessionPersistenceRevision, SessionPersistenceSnapshot, SessionPreparation, SessionRawArtifact,
};
use dsh_session::types::{EventKind, SessionEvent, SessionHeader, SESSION_FORMAT_VERSION};

#[test]
fn persistence_constants_match_ts() {
    assert_eq!(dsh_persistence::DEFAULT_PREPARED_SESSION_CACHE_SIZE, 5);
    assert_eq!(dsh_persistence::DEFAULT_WRITE_BATCH_MAX_DELAY_MS, 200);
}

#[test]
fn revision_is_opaque_string_newtype() {
    let r = SessionPersistenceRevision::from_raw("store1::5");
    assert_eq!(r.raw(), "store1::5");
    // serde：透明字符串（revision 在 wire 上是纯字符串）
    let v = serde_json::to_value(&r).unwrap();
    assert_eq!(v, serde_json::json!("store1::5"));
    let back: SessionPersistenceRevision = serde_json::from_value(v).unwrap();
    assert_eq!(back, r);
    assert_ne!(r, SessionPersistenceRevision::from_raw("other"));
}

#[test]
fn format_version_refusal_is_direction_aware() {
    // 新版本 → 提示升级 harness
    let newer = session_format_version_refusal("s1", SESSION_FORMAT_VERSION + 1);
    assert!(newer.contains("newer harness"), "should point at upgrade: {newer}");
    assert!(!newer.contains("corrupt"));
    // 非新版本（含缺失迁移路径的 equal/旧版本）→ 声明无升级路径
    // （SESSION_FORMAT_VERSION=0 时实际不存在更低版本；TS 的对称 else 分支由 equal 覆盖）
    let not_newer = session_format_version_refusal("s1", SESSION_FORMAT_VERSION);
    assert!(not_newer.contains("older than the supported"), "should state no upgrade path: {not_newer}");
}

#[test]
fn error_hierarchy_differentiates_damage_from_unsupported() {
    let loc = SessionLocation { kind: "jsonl".into(), path: "/tmp/s.jsonl".into() };
    let unsupported = SessionFormatUnsupportedError {
        message: "session \"s1\" uses log format v1...".into(),
        location: Some(loc),
    };
    assert!(unsupported.location().is_some());
    let e: PersistenceError = unsupported.into();
    assert!(matches!(e, PersistenceError::Unsupported(_)));

    let corruption =
        SessionPersistenceCorruptionError { message: "failed validation".into(), cause: None };
    let e2: PersistenceError = corruption.into();
    assert!(matches!(e2, PersistenceError::Corruption(_)));

    // 两者语义不同：unsupported 是「原样可读但本 build 解释不了」，corruption 是「坏了」
    assert_ne!(
        PersistenceError::Corruption(SessionPersistenceCorruptionError {
            message: "x".into(),
            cause: None,
        })
        .to_string(),
        PersistenceError::Unsupported(SessionFormatUnsupportedError {
            message: "x".into(),
            location: None,
        })
        .to_string(),
    );
}

#[test]
fn preparation_releases_once_and_idempotently() {
    let releases = Arc::new(AtomicUsize::new(0));
    let hook = {
        let releases = Arc::clone(&releases);
        move || {
            releases.fetch_add(1, Ordering::SeqCst);
        }
    };
    {
        let prep = SessionPreparation::new(
            SessionId::from_raw("s1"),
            vec![SessionEvent::end_seed(0, 1)],
            Some(Box::new(hook)),
        );
        assert_eq!(prep.events().len(), 1);
        // 手动释放 + Drop 都必须幂等
        prep.release();
        prep.release();
    }
    assert_eq!(releases.load(Ordering::SeqCst), 1, "release must run exactly once");
}

#[test]
fn preparation_publication_consumes_release() {
    let releases = Arc::new(AtomicUsize::new(0));
    let hook = {
        let releases = Arc::clone(&releases);
        move || {
            releases.fetch_add(1, Ordering::SeqCst);
        }
    };
    let prep = SessionPreparation::new(
        SessionId::from_raw("s2"),
        vec![],
        Some(Box::new(hook)),
    );
    // 发布成功 → 释放回调对已发布预备为空操作
    prep.mark_published();
    drop(prep);
    assert_eq!(releases.load(Ordering::SeqCst), 0, "published preparation must not release");
}

#[test]
fn inspection_is_balanced_check_on_turn_boundary() {
    let mut events = vec![
        SessionEvent::new(0, 1, EventKind::TurnStart, serde_json::json!({"turn": 1})),
        SessionEvent::new(1, 2, EventKind::TurnEnd, serde_json::json!({
            "turn": 1, "reason": { "kind": "completed" }
        })),
    ];
    let balanced = SessionInspection {
        meta: SessionHeader::new(SessionId::from_raw("s1"), 1),
        events,
    };
    assert!(balanced.is_balanced());

    events = vec![SessionEvent::new(0, 1, EventKind::TurnStart, serde_json::json!({"turn": 1}))];
    let open = SessionInspection {
        meta: SessionHeader::new(SessionId::from_raw("s1"), 1),
        events,
    };
    assert!(!open.is_balanced(), "open turn must not be balanced");
}

#[test]
fn session_persistence_trait_is_backend_implementable_and_roundtrips() {
    // 内存 mock 后端实现完整 seam，形状走路 create/append/load/list/snapshots/locate/read_raw
    let store = MockPersistence::new();
    let header = SessionHeader {
        version: 0,
        id: SessionId::from_raw("sess-1"),
        created_at: 1000,
        cwd: Some("/w".into()),
        parent_session: None,
        seed_length: Some(0),
        origin: None,
        delegation_depth: None,
        agent_preset: None,
    };
    store.create(&header).expect("create");

    let events = vec![
        SessionEvent::new(0, 1001, EventKind::TurnStart, serde_json::json!({"turn": 1})),
        SessionEvent::new(1, 1002, EventKind::UserMessage, serde_json::json!({
            "id": "m1", "role": "user", "content": [{"type":"text","text":"hi"}], "source": {"kind":"user"}
        })).with_surface_op(dsh_session::types::SurfaceOp::Append),
        SessionEvent::new(2, 1003, EventKind::TurnEnd, serde_json::json!({
            "turn": 1, "reason": { "kind": "completed" }
        })),
    ];
    store.append(&header.id, &events).expect("append");

    // locate：JSONL 类后端每会话一 artifact
    let location = store.locate(&header).expect("locate");
    assert_eq!(location.kind, "jsonl");
    assert!(location.path.contains("sess-1"));
    // 支持 raw artifact → 读回原文
    assert!(store.supports_raw_artifacts());
    let raw = store.read_raw(&header.id).expect("read_raw").expect("artifact exists");
    assert!(raw.content.contains("turn/start"));
    assert_eq!(raw.filename, "sess-1");

    // load → 平衡 inspection
    let inspection = store.load(&header.id).expect("load");
    assert!(inspection.is_balanced());
    assert_eq!(inspection.events.len(), 3);
    assert_eq!(inspection.meta.cwd.as_deref(), Some("/w"));

    // read_from（watermark 后缀读）
    let suffix = store.read_from(&header.id, 1).expect("read_from");
    assert_eq!(suffix.events.len(), 2);
    assert_eq!(suffix.events[0].seq, 1);

    // list + list_snapshots
    assert_eq!(store.list().expect("list").len(), 1);
    let snap = store.list_snapshots().expect("snapshots");
    assert_eq!(snap.len(), 1);
    assert_eq!(snap[0].header.id.raw(), "sess-1");
    assert_eq!(snap[0].revision.raw(), "rev-7");

    // prepare（unpublished Session 预备）
    let prep = store.prepare(&header.id).expect("prepare");
    assert_eq!(prep.events().len(), 3);
    drop(prep);

    // 缺失会话 → NotFound
    let missing = SessionId::from_raw("nope");
    assert!(matches!(store.load(&missing), Err(PersistenceError::NotFound(_))));
}

// ---- 内存 mock 后端（证明 seam 可由任意后端实现；`&self` = 共享 Service 语义） ----

struct MockPersistence {
    sessions: std::cell::RefCell<HashMap<String, (SessionHeader, Vec<SessionEvent>)>>,
}

impl MockPersistence {
    fn new() -> Self {
        Self { sessions: std::cell::RefCell::new(HashMap::new()) }
    }
}

impl SessionPersistence for MockPersistence {
    fn locate(&self, meta: &SessionHeader) -> Option<SessionLocation> {
        Some(SessionLocation {
            kind: "jsonl".into(),
            path: format!("/tmp/{}", meta.id.raw()),
        })
    }
    fn supports_raw_artifacts(&self) -> bool {
        true
    }
    fn read_raw(
        &self,
        id: &SessionId,
    ) -> Result<Option<SessionRawArtifact>, PersistenceError> {
        let sessions = self.sessions.borrow();
        let (meta, events) = sessions
            .get(id.raw())
            .ok_or_else(|| PersistenceError::NotFound(id.clone()))?;
        let mut content = String::new();
        for e in events.iter() {
            content.push_str(&serde_json::to_string(e).unwrap());
            content.push('\n');
        }
        Ok(Some(SessionRawArtifact {
            meta: meta.clone(),
            filename: id.raw().to_string(),
            content,
        }))
    }
    fn create(&self, meta: &SessionHeader) -> Result<(), PersistenceError> {
        self.sessions
            .borrow_mut()
            .entry(meta.id.raw().to_string())
            .or_insert_with(|| (meta.clone(), Vec::new()));
        Ok(())
    }
    fn append(&self, id: &SessionId, events: &[SessionEvent]) -> Result<(), PersistenceError> {
        let mut sessions = self.sessions.borrow_mut();
        let entry = sessions
            .get_mut(id.raw())
            .ok_or_else(|| PersistenceError::NotFound(id.clone()))?;
        let cursor = entry.1.len() as u64;
        for (i, e) in events.iter().enumerate() {
            if e.seq != cursor + i as u64 {
                return Err(PersistenceError::Invalid(format!(
                    "append seq mismatch for {id}: expected {}, got {}",
                    cursor + i as u64,
                    e.seq
                )));
            }
        }
        entry.1.extend_from_slice(events);
        Ok(())
    }
    fn prepare(&self, id: &SessionId) -> Result<SessionPreparation, PersistenceError> {
        let sessions = self.sessions.borrow();
        let (_, events) = sessions
            .get(id.raw())
            .ok_or_else(|| PersistenceError::NotFound(id.clone()))?;
        Ok(SessionPreparation::new(id.clone(), events.clone(), None))
    }
    fn load(&self, id: &SessionId) -> Result<SessionInspection, PersistenceError> {
        let sessions = self.sessions.borrow();
        let (meta, events) = sessions
            .get(id.raw())
            .ok_or_else(|| PersistenceError::NotFound(id.clone()))?;
        Ok(SessionInspection { meta: meta.clone(), events: events.clone() })
    }
    fn inspect(&self, id: &SessionId) -> Result<SessionInspection, PersistenceError> {
        self.load(id)
    }
    fn read_from(
        &self,
        id: &SessionId,
        from_seq: u64,
    ) -> Result<dsh_persistence::SessionSuffix, PersistenceError> {
        let sessions = self.sessions.borrow();
        let (meta, events) = sessions
            .get(id.raw())
            .ok_or_else(|| PersistenceError::NotFound(id.clone()))?;
        Ok(dsh_persistence::SessionSuffix {
            meta: meta.clone(),
            events: events.iter().filter(|e| e.seq >= from_seq).cloned().collect(),
        })
    }
    fn list(&self) -> Result<Vec<SessionHeader>, PersistenceError> {
        Ok(self.sessions.borrow().values().map(|(m, _)| m.clone()).collect())
    }
    fn list_snapshots(&self) -> Result<Vec<SessionPersistenceSnapshot>, PersistenceError> {
        Ok(self
            .sessions
            .borrow()
            .values()
            .map(|(m, _)| SessionPersistenceSnapshot {
                header: m.clone(),
                revision: SessionPersistenceRevision::from_raw("rev-7"),
            })
            .collect())
    }
}

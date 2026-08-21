//! 持久化协调器（M1d：`dsh-persistence:coordinator`）。
//!
//! 权威参考：`deepseek-harness/packages/session/session-persistence/src/coordinator.ts`
//! （规范 §A、§G）。在 `PersistenceBackend` 之上实现 `SessionPersistence` seam：
//! - `create` 惰性：登记 meta，物理落盘推迟到首次 append（materialize-on-first-append）；
//! - `append` 需 seq 连续（first == cursor）、防重、经 write-behind 耐用 barrier；
//! - `load` 提交冷恢复：读出存储日志 → torn 尾截断修复 + `interruptedTurnClosers`
//!   合成 closing 事件；live turn 打开时拒绝 load（用 live Session）；
//! - `prepare` = LRU 预备 + `isPreparedSourceCurrent`（readStoredRevision === source.revision）；
//! - `readFrom` 是 detached watermark 原语（冷折叠；不发布不修复）；
//! - `list` = 元数据轻读。
//!
//! M1d 单线程纪律（D-006）：write-behind 的批次窗口由服务层以 `tick`/`flush` 显式
//! 推进；本 coordinator 的公开 `append` 承诺**耐用**（立即 drain），后台 batch 窗口
//! 属于 future live-session 层。

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use dsh_brand::SessionId;
use dsh_session::repair::interrupted_turn_closers;
use dsh_session::types::{EventKind, SessionEvent, SessionHeader};

use crate::seam::{
    PersistenceBackend, PersistenceError, SessionInspection, SessionLocation,
    SessionPersistence, SessionPersistenceCorruptionError, SessionPersistenceSnapshot,
    SessionPreparation, SessionRawArtifact, SessionSuffix, StoredLog,
    DEFAULT_PREPARED_SESSION_CACHE_SIZE,
};
use crate::write_behind::{BatchSink, SessionWriteBehind};

/// 每会话 /* durable sink */ —— 把批次交给后端 append（物化不在此路径）。
struct CoordinatorSink<'a> {
    backend: &'a dyn PersistenceBackend,
    id: SessionId,
}

impl<'a> BatchSink for CoordinatorSink<'a> {
    fn write(&mut self, batch: &[SessionEvent]) -> Result<(), String> {
        // 追加只需 id 定位；backend 以 meta.id 定位 artifact
        let placeholder = SessionHeader::new(self.id.clone(), 0);
        self.backend
            .append_batch(&placeholder, batch)
            .map_err(|e| e.to_string())
    }
}

/// 会话的协调器内存状态（镜像 TS coordinator `EnrolledSession`）。
struct SessionState {
    /// 已登记的会话 meta（create 时给定；materialize 时落盘）。
    header: SessionHeader,
    /// 从存储读回的权威 meta（materialize 后）。
    stored_header: Option<SessionHeader>,
    /// 逻辑事件 cursor = 已持久化的 next seq。
    cursor: u64,
    /// 是否已发生物理物化。
    materialized: bool,
    /// live turn 是否打开（owner 语义：关闭前不允许 load/prepare）。
    live_turn_open: bool,
    /// 写批控制器（后台窗口推进由服务层驱动）。
    write_behind: Rc<RefCell<SessionWriteBehind>>,
}

/// 持久化协调器：在 `PersistenceBackend` 之上实现公开 `SessionPersistence`。
pub struct PersistenceCoordinator {
    backend: Box<dyn PersistenceBackend>,
    states: RefCell<HashMap<SessionId, SessionState>>,
    /// LRU 预备缓存（TS 缺省 5）。
    prepared: RefCell<VecDeque<SessionId>>,
    prepared_cache_size: usize,
}

impl PersistenceCoordinator {
    pub fn new(backend: Box<dyn PersistenceBackend>) -> Self {
        PersistenceCoordinator {
            backend,
            states: RefCell::new(HashMap::new()),
            prepared: RefCell::new(VecDeque::new()),
            prepared_cache_size: DEFAULT_PREPARED_SESSION_CACHE_SIZE,
        }
    }

    /// 会话是否已有物化日志（协调器内存视图）。
    pub fn is_materialized(&self, id: &SessionId) -> bool {
        self.states
            .borrow()
            .get(id)
            .map(|s| s.materialized)
            .unwrap_or(false)
    }

    /// 会话事件 cursor（协调器内存视图；未登记会话 None）。
    pub fn cursor_of(&self, id: &SessionId) -> Option<u64> {
        self.states.borrow().get(id).map(|s| s.cursor)
    }

    pub fn backend(&self) -> &dyn PersistenceBackend {
        &*self.backend
    }

    /// 会话 live turn 状态（owner 语义控制）。
    pub fn set_live_turn(&self, id: &SessionId, open: bool) {
        if let Some(s) = self.states.borrow_mut().get_mut(id) {
            s.live_turn_open = open;
        }
    }

    /// 推进 write-behind 的 automatic 窗口（服务层调用）。返回是否触发后台写入。
    pub fn tick(&self, id: &SessionId, now_ms: u64) -> bool {
        let wb = {
            let states = self.states.borrow();
            let Some(state) = states.get(id) else {
                return false;
            };
            if !state.materialized {
                return false;
            }
            state.write_behind.clone()
        };
        let mut sink = CoordinatorSink { backend: &*self.backend, id: id.clone() };
        let mut guard = wb.borrow_mut();
        guard.tick(&mut sink, now_ms)
    }

    /// 显式 quiescence flush（服务层在批次窗口到期 / 会话关闭时调用）。
    pub fn flush(&self, id: &SessionId) -> Result<(), PersistenceError> {
        let wb = {
            let states = self.states.borrow();
            let Some(state) = states.get(id) else {
                return Err(PersistenceError::NotFound(id.clone()));
            };
            if !state.materialized {
                return Ok(());
            }
            state.write_behind.clone()
        };
        let mut sink = CoordinatorSink { backend: &*self.backend, id: id.clone() };
        let mut guard = wb.borrow_mut();
        guard.flush(&mut sink).map_err(PersistenceError::Other)?;
        Ok(())
    }
}

impl SessionPersistence for PersistenceCoordinator {
    fn locate(&self, meta: &SessionHeader) -> Option<SessionLocation> {
        self.backend.locate(meta)
    }

    fn supports_raw_artifacts(&self) -> bool {
        self.backend.supports_raw_artifacts()
    }

    fn read_raw(&self, id: &SessionId) -> Result<Option<SessionRawArtifact>, PersistenceError> {
        self.backend.read_raw(id)
    }

    fn create(&self, meta: &SessionHeader) -> Result<(), PersistenceError> {
        let id = meta.id.clone();
        let mut states = self.states.borrow_mut();
        if states.contains_key(&id) {
            return Ok(()); // 已登记：幂等
        }
        states.insert(
            id,
            SessionState {
                header: meta.clone(),
                stored_header: None,
                cursor: 0,
                materialized: false,
                live_turn_open: false,
                write_behind: Rc::new(RefCell::new(SessionWriteBehind::new(
                    crate::seam::DEFAULT_WRITE_BATCH_MAX_DELAY_MS,
                ))),
            },
        );
        Ok(())
    }

    fn append(&self, id: &SessionId, events: &[SessionEvent]) -> Result<(), PersistenceError> {
        if events.is_empty() {
            return Ok(());
        }
        let mut states = self.states.borrow_mut();
        let state = states
            .get_mut(id)
            .ok_or_else(|| PersistenceError::NotFound(id.clone()))?;
        // seq 连续 + cursor 对齐（first == cursor）
        if events[0].seq != state.cursor {
            return Err(PersistenceError::Invalid(format!(
                "append to \"{id}\" must start at cursor {}, got {}",
                state.cursor, events[0].seq
            )));
        }
        // 首次 append → 物化
        if !state.materialized {
            self.backend
                .materialize_batch(&state.header, events)?;
            if let Ok(Some(log)) = self.backend.load_stored(id) {
                state.stored_header = Some(log.meta);
            }
            state.materialized = true;
            state.cursor = state.cursor.saturating_add(events.len() as u64);
            return Ok(());
        }
        // 后续 append 经 write-behind 耐用写入
        let mut sink = CoordinatorSink { backend: &*self.backend, id: id.clone() };
        {
            let mut wb = state.write_behind.borrow_mut();
            for e in events {
                wb.enqueue(e.clone(), 0);
            }
            // M1d append 承诺耐用：立即 drain quiescence
            wb.flush(&mut sink).map_err(PersistenceError::Other)?;
        }
        state.cursor = state.cursor.saturating_add(events.len() as u64);
        Ok(())
    }

    fn prepare(&self, id: &SessionId) -> Result<SessionPreparation, PersistenceError> {
        let stored = self.backend.load_stored(id)?;
        let Some(log) = stored else {
            return Err(PersistenceError::NotFound(id.clone()));
        };
        // live turn 打开 → 拒绝
        let states = self.states.borrow();
        if let Some(s) = states.get(id) {
            if s.live_turn_open {
                return Err(PersistenceError::Invalid(format!(
                    "cannot prepare session \"{id}\" while it is live"
                )));
            }
        }
        // isPreparedSourceCurrent：readStoredRevision === source.revision
        let current = self.backend.read_stored_revision(id)?.ok_or_else(|| {
            PersistenceError::NotFound(id.clone())
        })?;
        if current != log.revision {
            return Err(PersistenceError::Other(format!(
                "session \"{id}\" changed on disk while preparing (stale read)"
            )));
        }
        drop(states);
        self.cache_prepared(id);
        Ok(SessionPreparation::new(id.clone(), log.events, None))
    }

    fn load(&self, id: &SessionId) -> Result<SessionInspection, PersistenceError> {
        // live turn 打开 → 拒绝（用 live Session）
        {
            let states = self.states.borrow();
            if let Some(s) = states.get(id) {
                if s.live_turn_open {
                    return Err(PersistenceError::Invalid(format!(
                        "cannot load session \"{id}\" while its live turn is open; use the live Session or wait for the turn to close"
                    )));
                }
            }
        }
        let Some(log) = self.backend.load_stored(id)? else {
            return Err(PersistenceError::NotFound(id.clone()));
        };
        let stored = self.commit_repair(&log)?;
        Ok(SessionInspection { meta: stored.meta, events: stored.events })
    }

    fn inspect(&self, id: &SessionId) -> Result<SessionInspection, PersistenceError> {
        let Some(log) = self.backend.load_stored(id)? else {
            return Err(PersistenceError::NotFound(id.clone()));
        };
        // inspect 不发布、不修复；仅检视
        Ok(SessionInspection { meta: log.meta, events: log.events })
    }

    fn read_from(&self, id: &SessionId, from_seq: u64) -> Result<SessionSuffix, PersistenceError> {
        let Some(log) = self.backend.load_stored(id)? else {
            return Err(PersistenceError::NotFound(id.clone()));
        };
        // detached watermark：不发布不修复，仅截取后缀
        Ok(SessionSuffix {
            meta: log.meta,
            events: log.events.into_iter().filter(|e| e.seq >= from_seq).collect(),
        })
    }

    fn list(&self) -> Result<Vec<SessionHeader>, PersistenceError> {
        self.backend
            .list_snapshots()
            .map(|s| s.into_iter().map(|x| x.header).collect())
    }

    fn list_snapshots(&self) -> Result<Vec<SessionPersistenceSnapshot>, PersistenceError> {
        self.backend.list_snapshots()
    }
}

impl PersistenceCoordinator {
    /// 提交冷恢复：torn 截断 + 关闭 events 合成 + 持久补齐（对应 TS `commitRepair`）。
    /// 返回修复后、以平衡 turn/end 收尾的存储日志。
    fn commit_repair(&self, log: &StoredLog) -> Result<SessionInspection, PersistenceError> {
        let id = log.meta.id.clone();
        // 合成 closing 事件（未平衡 turn 时）
        let last = log.events.last();
        let needs_closing = last.map(|e| e.kind != EventKind::TurnEnd).unwrap_or(false);
        if needs_closing {
            let closers = interrupted_turn_closers(&log.events);
            let torn_offset = if log.torn { log.truncate_offset } else { None };
            self.backend
                .commit_repair(&id, torn_offset, &closers)
                .map_err(|e| {
                    PersistenceError::Corruption(SessionPersistenceCorruptionError {
                        message: format!("repair append failed for \"{id}\": {e}"),
                        cause: None,
                    })
                })?;
        } else if log.torn {
            // 仅 torn 尾：物理截断（无 closing 事件）
            self.backend
                .commit_repair(&id, log.truncate_offset, &[])
                .map_err(|e| {
                    PersistenceError::Corruption(SessionPersistenceCorruptionError {
                        message: format!("torn repair failed for \"{id}\": {e}"),
                        cause: None,
                    })
                })?;
        }
        // 重读（repair 后）
        let repaired = self.backend.load_stored(&id)?.ok_or_else(|| {
            PersistenceError::NotFound(id.clone())
        })?;
        Ok(SessionInspection { meta: repaired.meta, events: repaired.events })
    }

    fn cache_prepared(&self, id: &SessionId) {
        let mut cache = self.prepared.borrow_mut();
        cache.retain(|cached| cached != id);
        cache.push_back(id.clone());
        while cache.len() > self.prepared_cache_size {
            cache.pop_front();
        }
    }

    /// 是否命中预备缓存（测试/诊断）。
    pub fn is_prepared_cached(&self, id: &SessionId) -> bool {
        self.prepared.borrow().iter().any(|cached| cached == id)
    }

    /// 访问预备缓存当前条目（测试/诊断）。
    pub fn prepared_len(&self) -> usize {
        self.prepared.borrow().len()
    }
}

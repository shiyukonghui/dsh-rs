//! M1e SessionHost——把 WASM loop 的 `SessionLog` 事件 adopt 进 dsh-session store，
//! 并挂载持久化（dsh-persistence coordinator 的 event 回调）。
//!
//! 第一性原理（D-020）：loop 语义不改——WASM 组件仍经 WIT `session::append` 写
//! `SessionHandle`（`Arc<Mutex<SessionLog>>`，Send+Sync）；`session.prompt` 在
//! `run_turn` 后把 SessionLog 的新事件 **adopt** 进目标 `dsh::session::Session`
//! （类型化 `EventKind` + `Value` + `SurfaceIntent`）→ 触发 store 的
//! `session/event` 观察者 → 持久化（coordinator create/append）+ 事件下链
//! （`EventSink`，Send+Sync 供 SSE/WS 线程 drain）。
//!
//! 单线程纪律（D-004/D-006）：所有 store/coordinator 操作发生在调用线程（web 的
//! RPC thread 单线程顺序分派）；跨线程只走 `EventSink`（`Arc<Mutex<..>>`）。

use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use dsh_brand::SessionId;
use dsh_persistence::coordinator::PersistenceCoordinator;
use dsh_persistence::jsonl::{JsonlBackend, JsonlConfig};
use dsh_persistence::sqlite::SqliteBackend;
use dsh_persistence::{PersistenceBackend, SessionPersistence};
use dsh_session::runtime::Session;
use dsh_session::store::{SessionForkSource, SessionStore};
use dsh_session::types::{
    CreateSessionOptions, EventKind, SessionEvent, SessionHeader, SurfaceIntent, SurfaceOp,
};
use serde_json::Value;

/// 事件下链日志：`(sessionId, SessionEvent)`（append-only；Send+Sync）。
/// store 的 `session/event` 观察者在 append 提交后 push；SSE/WS 线程各自持有
/// 自己的游标（`sink_since`）增量读——多连接互不抢数据。
pub type EventSink = Arc<Mutex<Vec<(String, SessionEvent)>>>;

/// SessionHost：dsh-session store + 可选持久化挂载。
pub struct SessionHost {
    /// 权威 session store（Rc/RefCell，单线程）。
    pub store: Rc<SessionStore>,
    /// 持久化协调器（可选：root 缺省时纯内存）。
    pub coord: Option<Rc<PersistenceCoordinator>>,
    /// 事件下链日志（Send+Sync）。
    pub sink: EventSink,
    /// 持久化后端种类诊断（"mem"/"jsonl"/"sqlite"）。
    kind: &'static str,
}

impl SessionHost {
    /// 纯内存 SessionHost（无持久化根；测试 / 未配置 session 根时使用）。
    pub fn in_memory() -> Rc<Self> {
        Self::new_from_backend("mem", None)
    }

    /// 从持久化根构造：JSONL 后端 + coordinator 挂载 + 恢复既有快照。
    /// 快照恢复失败不阻断构造（返回的 host 仍可用；错误在调用方诊断）。
    pub fn with_root(root: &Path) -> Rc<Self> {
        let backend = JsonlBackend::new(JsonlConfig {
            root: root.to_path_buf(),
            ..Default::default()
        });
        let host = Self::new_from_backend("jsonl", Some(Box::new(backend)));
        host.restore_all();
        host
    }

    /// 从 SQLite 文件构造（M6W，D-092）：SqliteBackend → coordinator → 观察者 →
    /// restore_all。父目录不存在则 create_dir_all（镜像 JSONL 惰性建根）。
    /// 打开/建 schema 失败 → `Err`（fail-loud，调用方负责 boot 时终止——绝不静默
    /// 降级到内存）。
    pub fn with_sqlite(path: &Path) -> Result<Rc<Self>, String> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("create sqlite parent {}: {e}", parent.display()))?;
            }
        }
        let backend = SqliteBackend::open(path)
            .map_err(|e| format!("sqlite store {}: {e}", path.display()))?;
        let host = Self::new_from_backend("sqlite", Some(Box::new(backend)));
        host.restore_all();
        Ok(host)
    }

    /// 持久化后端种类（诊断/测试断言）。
    pub fn persistence_kind(&self) -> &'static str {
        self.kind
    }

    /// 观察者接线唯一来源（D-092）：任何后端（jsonl/sqlite/内存）共用同一
    /// create/append/flush 持久化副作用 + 下链。
    fn new_from_backend(
        kind: &'static str,
        backend: Option<Box<dyn PersistenceBackend>>,
    ) -> Rc<Self> {
        let coord = backend.map(|b| Rc::new(PersistenceCoordinator::new(b)));
        let host = Rc::new(SessionHost {
            store: Rc::new(SessionStore::new()),
            coord,
            sink: Arc::new(Mutex::new(Vec::new())),
            kind,
        });
        // 挂载观察者：append 提交后 → 持久化 + 下链。
        // 注意：闭包不捕获 `host.store`（否则 store→观察者→store 引用环泄漏）。
        let coord_weak = host.coord.clone();
        let sink = host.sink.clone();
        host.store.on_event(Box::new(move |session, event| {
            if let Some(coord) = &coord_weak {
                let _ = coord.create(session.header());
                let _ = coord.append(session.id(), std::slice::from_ref(event));
            }
            sink.lock().unwrap().push((session.id().to_string(), event.clone()));
        }));
        let coord_flush = host.coord.clone();
        host.store.on_flush(Box::new(move |session| {
            if let Some(coord) = &coord_flush {
                let _ = coord.flush(session.id());
            }
        }));
        host
    }

    /// 下链日志当前长度（连接握手 `lastSeq` 锚点）。
    pub fn sink_len(&self) -> usize {
        self.sink.lock().unwrap().len()
    }

    /// 取 `from` 起的下链帧（调用方各自游标）。
    pub fn sink_since(&self, from: usize) -> Vec<(String, SessionEvent)> {
        let log = self.sink.lock().unwrap();
        log.iter().skip(from).cloned().collect()
    }

    /// 恢复持久化根下的全部快照进 store（幂等；会话已 live 则跳过）。
    /// `session/flush` 后的快照经 `coord.load` 读出 → `Session::from_restore` →
    /// `store.enter` + `announce`。
    pub fn restore_all(&self) -> usize {
        let Some(coord) = &self.coord else { return 0 };
        let ids = coord.list().unwrap_or_default();
        let mut restored = 0;
        for header in ids {
            let id = header.id.clone();
            if self.store.is_live(&id) {
                continue;
            }
            // 用 inspect（不发布不修复）读取存储日志。
            let Ok(inspection) = coord.inspect(&id) else { continue };
            if let Ok(true) = self.restore_one(&id, inspection.meta, inspection.events) {
                restored += 1;
            }
        }
        restored
    }

    /// 恢复单个会话：`from_restore` → enter + announce；同时把 seed 回填
    /// coordinator cursor（append 首事件要求 cursor 对齐）。
    /// 返回 `Ok(true)` = 已恢复；`Ok(false)` = 已存在/空。
    fn restore_one(
        &self,
        id: &SessionId,
        header: SessionHeader,
        events: Vec<SessionEvent>,
    ) -> Result<bool, String> {
        if self.store.is_live(id) {
            return Ok(false);
        }
        if events.is_empty() {
            return Ok(false);
        }
        let session =
            Session::from_restore(id.clone(), &events, &header).map_err(|e| e.to_string())?;
        let rc = Rc::new(session);
        self.store.enter(&rc).map_err(|e| e.to_string())?;
        let _ = self.store.announce(&rc);
        // coordinator cursor 对齐：以 live 会话的完整事件表（含 seed 边界标记
        // `session/end-seed`）回灌——cursor 必须等于下一个 live append 的 seq。
        if let Some(coord) = &self.coord {
            let _ = coord.create(&header);
            let full = rc.events();
            let _ = coord.append(id, &full);
        }
        Ok(true)
    }

    /// mint 一个唯一 `s{n}` 会话 id 并创建空会话（web `session.create`）。
    pub fn create_new(&self) -> Result<String, String> {
        let mut n = self.store.list().len() as u64 + 1;
        loop {
            let candidate = format!("s{n}");
            let sid = SessionId::from_raw(candidate.clone());
            if !self.store.is_live(&sid) {
                self.store
                    .create(
                        Some(sid),
                        &CreateSessionOptions { seed: None, meta: None },
                    )
                    .map_err(|e| e.to_string())?;
                return Ok(candidate);
            }
            n += 1;
        }
    }

    /// 取或建目标会话（adopt 时惰性创建；id 即前端 sessionId）。
    pub fn session(&self, id: &str) -> Result<Rc<Session>, String> {
        let sid = SessionId::from_raw(id.to_string());
        if let Some(s) = self.store.get(&sid) {
            return Ok(s);
        }
        self.store
            .create(Some(sid), &CreateSessionOptions { seed: None, meta: None })
            .map_err(|e| e.to_string())
    }

    /// adopt：把 WASM loop（SessionLog）产生的一批 `(kind, payload)` 事件回放到
    /// 目标会话（类型化 + 自动 seq/time + surface 校验）。返回 adopt 数。
    pub fn adopt(
        &self,
        session_id: &str,
        events: &[(String, Vec<u8>)],
    ) -> Result<usize, String> {
        let session = self.session(session_id)?;
        let mut n = 0;
        for (kind, payload) in events {
            let kind_ev = EventKind::from_str(kind);
            let data: Value = serde_json::from_slice(payload).unwrap_or(Value::Null);
            // surface：仅 surface-eligible 事件（user/assistant/tool message）
            // 携带 Append 操作；开放 turn 的边界事件（turn/start 等）不带。
            let surface = if dsh_session::types::is_surface_event_type(kind) {
                Some(SurfaceIntent {
                    surface_op: SurfaceOp::Append,
                    source_event_seqs: None,
                })
            } else {
                None
            };
            session
                .append(kind_ev, data, surface.as_ref())
                .map_err(|e| format!("adopt \"{kind}\": {e}"))?;
            n += 1;
        }
        Ok(n)
    }

    /// 会话事件列表（前端 `session.history` 读模型）。
    pub fn events(&self, session_id: &str) -> Vec<SessionEvent> {
        let sid = SessionId::from_raw(session_id.to_string());
        self.store
            .get(&sid)
            .map(|s| s.events())
            .unwrap_or_default()
    }

    /// 会话下一 seq（`session.history` 的 hasMore 计算；缺省会话不存在 → 0）。
    pub fn seq_of(&self, session_id: &str) -> u64 {
        let sid = SessionId::from_raw(session_id.to_string());
        self.store
            .get(&sid)
            .map(|s| s.seq())
            .unwrap_or(0)
    }

    /// 全部 live 会话（创建顺序）。
    pub fn list(&self) -> Vec<Rc<Session>> {
        self.store.list()
    }

    /// 会话是否 live。
    pub fn is_live(&self, session_id: &str) -> bool {
        let sid = SessionId::from_raw(session_id.to_string());
        self.store.is_live(&sid)
    }

    /// 显式 flush（`session.flush` → coordinator flush 批量落盘）。
    pub fn flush(&self, session_id: &str) -> Result<(), String> {
        let sid = SessionId::from_raw(session_id.to_string());
        let session = self
            .store
            .get(&sid)
            .ok_or_else(|| format!("session \"{session_id}\" not live"))?;
        self.store.flush(&session);
        Ok(())
    }

    /// fork：从 live 源会话创建子会话（默认边界 = 源尾部）；返回子 id。
    pub fn fork(&self, source_id: &str) -> Result<String, String> {
        let src = SessionId::from_raw(source_id.to_string());
        let child = self
            .store
            .fork(&SessionForkSource::Id(src.clone()), None, None)
            .map_err(|e| e.message)?;
        let id = child.id().to_string();
        // coordinator 对齐子会话 seed cursor。
        if let Some(coord) = &self.coord {
            let _ = coord.create(child.header());
            let evs = child.events();
            let _ = coord.append(&child.id().clone(), &evs);
        }
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn raw_echo_turn(text: &str) -> Vec<(String, Vec<u8>)> {
        vec![
            ("turn/start".into(), serde_json::to_vec(&json!({"turn": 1})).unwrap()),
            ("step/start".into(), serde_json::to_vec(&json!({"turn": 1, "step": 1})).unwrap()),
            (
                "user/message".into(),
                serde_json::to_vec(&json!({
                    "id": "u1", "role": "user",
                    "content": [{"type": "text", "text": text}],
                    "source": {"kind": "user"},
                }))
                .unwrap(),
            ),
            (
                "assistant/message".into(),
                serde_json::to_vec(&json!({
                    "turn": 1, "step": 1,
                    "message": {
                        "id": "a1", "role": "assistant",
                        "content": [{"type": "text", "text": format!("echo: {text}")}],
                        "source": {"kind": "model", "provider": "mock", "model": "mock"},
                    },
                }))
                .unwrap(),
            ),
            ("step/end".into(), serde_json::to_vec(&json!({"turn": 1, "step": 1})).unwrap()),
            ("turn/end".into(), serde_json::to_vec(&json!({"turn": 1, "reason": "completed"})).unwrap()),
        ]
    }

    #[test]
    fn adopt_stamps_typed_events_with_time_and_seq() {
        let host = SessionHost::in_memory();
        let n = host.adopt("default", &raw_echo_turn("hi")).unwrap();
        assert_eq!(n, 6);
        let evs = host.events("default");
        assert_eq!(evs.len(), 6);
        // seq 连续 0..5；time 为 epoch ms（> 0）。
        for (i, e) in evs.iter().enumerate() {
            assert_eq!(e.seq, i as u64);
            assert!(e.time > 0, "event {i} has real time");
        }
        // 类型化 kind：前端 `session.event.type` 关键集合。
        assert_eq!(evs[0].kind.as_str(), "turn/start");
        assert_eq!(evs[2].kind.as_str(), "user/message");
        assert_eq!(evs[3].kind.as_str(), "assistant/message");
        assert_eq!(evs[5].kind.as_str(), "turn/end");
    }

    #[test]
    fn adopt_fires_downlink_sink() {
        let host = SessionHost::in_memory();
        assert_eq!(host.sink_len(), 0);
        host.adopt("default", &raw_echo_turn("hi")).unwrap();
        assert_eq!(host.sink_len(), 6);
        let frames = host.sink_since(0);
        assert_eq!(frames.len(), 6);
        assert_eq!(frames[0].0, "default");
        assert_eq!(frames[0].1.kind.as_str(), "turn/start");
        // 增量游标：from=6 之后无新事件。
        assert!(host.sink_since(6).is_empty());
        // 第二轮 → 游标推进。
        host.adopt("s2", &raw_echo_turn("x")).unwrap();
        assert_eq!(host.sink_since(6).len(), 6);
        assert_eq!(host.sink_since(6)[0].0, "s2");
    }

    #[test]
    fn adopt_creates_session_lazily() {
        let host = SessionHost::in_memory();
        assert!(!host.is_live("s1"));
        host.adopt("s1", &raw_echo_turn("x")).unwrap();
        assert!(host.is_live("s1"));
        assert_eq!(host.seq_of("s1"), 6);
        assert_eq!(host.seq_of("missing"), 0);
    }

    #[test]
    fn create_new_mints_unique_ids() {
        let host = SessionHost::in_memory();
        let a = host.create_new().unwrap();
        let b = host.create_new().unwrap();
        assert_ne!(a, b);
        assert!(host.is_live(&a));
        assert!(host.is_live(&b));
        assert_eq!(host.events(&a).len(), 0);
        assert_eq!(host.seq_of(&a), 0);
    }

    #[test]
    fn unknown_kind_is_lossless() {
        let host = SessionHost::in_memory();
        host.adopt(
            "default",
            &[("no/such/event".into(), serde_json::to_vec(&json!({"a": 1})).unwrap())],
        )
        .unwrap();
        let evs = host.events("default");
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].kind.as_str(), "no/such/event");
        assert_eq!(evs[0].data["a"], 1);
    }

    #[test]
    fn history_hasmore_shape_from_seq() {
        let host = SessionHost::in_memory();
        host.adopt("default", &raw_echo_turn("hi")).unwrap();
        assert_eq!(host.seq_of("default"), 6);
    }

    #[test]
    fn fork_creates_child_with_parent_seed() {
        let host = SessionHost::in_memory();
        host.adopt("parent", &raw_echo_turn("hi")).unwrap();
        let child_id = host.fork("parent").unwrap();
        assert_ne!(child_id, "parent");
        let evs = host.events(&child_id);
        // 子会话 = 父 6 事件 + seed 边界标记（session/end-seed）→ 7。
        assert_eq!(evs.len(), 7);
        for (i, e) in evs.iter().enumerate() {
            assert_eq!(e.seq, i as u64);
        }
        // 首 6 条与父一致；第 7 条是边界标记。
        let parent_evs = host.events("parent");
        for (i, e) in evs[..6].iter().enumerate() {
            assert_eq!(e.kind, parent_evs[i].kind);
            assert_eq!(e.data, parent_evs[i].data);
        }
        assert_eq!(evs[6].kind.as_str(), "session/end-seed");
    }

    #[test]
    fn flush_noop_for_absent_session() {
        let host = SessionHost::in_memory();
        assert!(host.flush("nope").is_err());
    }

    // ---- 持久化挂载 ----

    fn tmp_root(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("dsh-m1e-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    /// SQLite 文件路径（父目录**不**预创建——验证 with_sqlite 自行建父目录）。
    fn tmp_sqlite_file(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir()
            .join(format!("dsh-sqlite-web-{tag}-{}", std::process::id()))
            .join("store.sqlite")
    }

    // ---- SQLite 持久化挂载（M6W，D-092）----

    /// A1：with_sqlite 落盘 → 冷重启同文件 → 恢复快照（含 end-seed 边界标记）。
    #[test]
    fn with_sqlite_restart_restores_snapshot_into_store() {
        let file = tmp_sqlite_file("restart");
        {
            let host = SessionHost::with_sqlite(&file).expect("sqlite");
            assert_eq!(host.persistence_kind(), "sqlite");
            host.adopt("default", &raw_echo_turn("hi")).unwrap();
            host.flush("default").unwrap();
        }
        {
            let host = SessionHost::with_sqlite(&file).expect("sqlite");
            assert!(host.is_live("default"));
            let evs = host.events("default");
            assert_eq!(evs.len(), 7);
            assert_eq!(evs[6].kind.as_str(), "session/end-seed");
            assert_eq!(evs[3].kind.as_str(), "assistant/message");
            assert_eq!(evs[3].data["message"]["content"][0]["text"], "echo: hi");
        }
        let _ = std::fs::remove_dir_all(file.parent().unwrap()).ok();
    }

    /// A2：恢复后继续 adopt → seq 连续（游标对齐，不被 materialize 回灌打断）。
    #[test]
    fn with_sqlite_restore_then_adopt_continues_seq() {
        let file = tmp_sqlite_file("cont");
        {
            let host = SessionHost::with_sqlite(&file).expect("sqlite");
            host.adopt("default", &raw_echo_turn("hi")).unwrap();
            host.flush("default").unwrap();
        }
        {
            let host = SessionHost::with_sqlite(&file).expect("sqlite");
            host.adopt("default", &raw_echo_turn("bye")).unwrap();
            let evs = host.events("default");
            assert_eq!(evs.len(), 13);
            assert_eq!(evs[12].seq, 12);
            assert_eq!(evs[10].kind.as_str(), "assistant/message");
            assert_eq!(evs[10].data["message"]["content"][0]["text"], "echo: bye");
            assert_eq!(evs[12].kind.as_str(), "turn/end");
            host.flush("default").unwrap();
        }
        let _ = std::fs::remove_dir_all(file.parent().unwrap()).ok();
    }

    /// 诊断：persistence_kind 反映后端装配。
    #[test]
    fn persistence_kind_reports_backend() {
        assert_eq!(SessionHost::in_memory().persistence_kind(), "mem");
        let root = tmp_root("kind-jsonl");
        assert_eq!(SessionHost::with_root(&root).persistence_kind(), "jsonl");
        let file = tmp_sqlite_file("kind-sqlite");
        assert_eq!(
            SessionHost::with_sqlite(&file)
                .expect("sqlite")
                .persistence_kind(),
            "sqlite"
        );
        let _ = std::fs::remove_dir_all(&root).ok();
        let _ = std::fs::remove_dir_all(file.parent().unwrap()).ok();
    }

    #[test]
    fn persistence_mount_writes_coordinator_cursor() {
        let root = tmp_root("mount");
        let host = SessionHost::with_root(&root);
        host.adopt("default", &raw_echo_turn("hi")).unwrap();
        host.flush("default").unwrap();
        let coord = host.coord.clone().expect("coord present");
        assert!(coord.is_materialized(&SessionId::from_raw("default".to_string())));
        assert_eq!(
            coord.cursor_of(&SessionId::from_raw("default".to_string())),
            Some(6)
        );
        let _ = std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn restart_restores_snapshot_into_store() {
        let root = tmp_root("restart");
        {
            let host = SessionHost::with_root(&root);
            host.adopt("default", &raw_echo_turn("hi")).unwrap();
            host.flush("default").unwrap();
        }
        {
            // 重启：新 host 从同一根恢复快照。
            let host = SessionHost::with_root(&root);
            assert!(host.is_live("default"));
            let evs = host.events("default");
            // 恢复时 from_restore 会补 session/end-seed 边界标记 → 7。
            assert_eq!(evs.len(), 7);
            assert_eq!(evs[6].kind.as_str(), "session/end-seed");
            assert_eq!(evs[3].kind.as_str(), "assistant/message");
            assert_eq!(evs[3].data["message"]["content"][0]["text"], "echo: hi");
        }
        let _ = std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn restore_then_adopt_continues_seq() {
        let root = tmp_root("cont");
        {
            let host = SessionHost::with_root(&root);
            host.adopt("default", &raw_echo_turn("hi")).unwrap();
            host.flush("default").unwrap();
        }
        {
            let host = SessionHost::with_root(&root);
            host.adopt("default", &raw_echo_turn("bye")).unwrap();
            let evs = host.events("default");
            // 6 (第一轮) + end-seed(6) + 6 (第二轮, seq 7..12) → 13。
            assert_eq!(evs.len(), 13);
            assert_eq!(evs[12].seq, 12);
            // 第二轮助理消息在 index 10（seq 10：7..12 中第 4 条）。
            assert_eq!(evs[10].kind.as_str(), "assistant/message");
            assert_eq!(evs[10].data["message"]["content"][0]["text"], "echo: bye");
            assert_eq!(evs[12].kind.as_str(), "turn/end");
            host.flush("default").unwrap();
        }
        let _ = std::fs::remove_dir_all(&root).ok();
    }
}

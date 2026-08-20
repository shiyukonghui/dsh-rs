//! 内存 SessionStore（对齐
//! `deepseek-harness/packages/core/session/src/index.ts` 的 `SessionStore`）。
//!
//! 持久化不在本模块实现——持久化插件订阅 `session/event`、在 `session/flush`/dispose 时落盘。
//! M1 内用最小观察者表（无 Cordis 事件总线）：
//! - `session/created` / `session/disposed` 同步广播；
//! - `session/event` 在 append 提交后同步通知（per-listener containment）；
//! - `session/flush` 同步 drain 回调（M1 无 async；后续桥到服务层线程）。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use dsh_brand::SessionId;

use crate::runtime::{Session, SessionError};
use crate::types::{CreateSessionMeta, CreateSessionOptions, SessionEvent, SessionHeader};

/// fork 拒绝码（对齐 TS `SessionForkErrorCode`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionForkErrorCode {
    SessionNotFound,
    SessionNotLive,
    SessionAlreadyExists,
    InvalidBoundary,
    OpenTurn,
}

/// fork 拒绝错误。
#[derive(Debug, Clone)]
pub struct SessionForkError {
    pub message: String,
    pub code: SessionForkErrorCode,
}

/// fork 源：live Session 对象或其 store id。
#[derive(Clone)]
pub enum SessionForkSource<'a> {
    Object(&'a Session),
    Id(SessionId),
}

/// 一个 store 条目的最小状态。
struct StoreEntry {
    session: Rc<Session>,
    /// 已发布（announce 过）——dispose 时发配对通知。
    announced: bool,
}

type EventCallback = Box<dyn Fn(&Session, &SessionEvent)>;
type SessionCallback = Box<dyn Fn(&Session)>;

/// 内存 session store（`ctx.sessions` 的 Rust 形态）。
///
/// 观察者回调通过 `Rc` 共享持久化插件状态；`append` 后同步触发 `session/event`。
#[derive(Default)]
pub struct SessionStore {
    store: RefCell<HashMap<SessionId, StoreEntry>>,
    counter: RefCell<u64>,
    on_created: RefCell<Vec<SessionCallback>>,
    on_disposed: RefCell<Vec<SessionCallback>>,
    on_event: RefCell<Vec<EventCallback>>,
    on_flush: RefCell<Vec<SessionCallback>>,
}

impl SessionStore {
    pub fn new() -> Self {
        SessionStore::default()
    }

    /// 订阅 `session/created`（同步广播；抛错按订阅序传播——M1 无 Cordis 的 rollback 语义，
    /// 由调用方（agent-loop 归壳）在 create 事务里处理回滚）。
    pub fn on_created(&self, cb: SessionCallback) {
        self.on_created.borrow_mut().push(cb);
    }

    /// 订阅 `session/disposed`。
    pub fn on_disposed(&self, cb: SessionCallback) {
        self.on_disposed.borrow_mut().push(cb);
    }

    /// 订阅 `session/event`（append 提交后同步触发；per-listener containment）。
    pub fn on_event(&self, cb: EventCallback) {
        self.on_event.borrow_mut().push(cb);
    }

    /// 订阅 `session/flush`（同步 drain）。
    pub fn on_flush(&self, cb: SessionCallback) {
        self.on_flush.borrow_mut().push(cb);
    }

    /// 构造 Session（不进入 store）——普通创建（seed/meta）。
    pub fn prepare(&self, id: Option<SessionId>, options: &CreateSessionOptions) -> Result<Session, SessionError> {
        let session_id = match id {
            Some(id) => id,
            None => self.mint_id(),
        };
        if self.is_live(&session_id) {
            return Err(SessionError(format!("session \"{session_id}\" already exists")));
        }
        let meta = options.meta.clone().unwrap_or_default();
        let header = SessionHeader {
            version: crate::types::SESSION_FORMAT_VERSION,
            id: session_id.clone(),
            created_at: meta.created_at.unwrap_or(now_ms()),
            cwd: meta.cwd.clone(),
            parent_session: meta.parent_session.clone(),
            seed_length: meta.seed_length,
            origin: meta.origin,
            delegation_depth: meta.delegation_depth,
            agent_preset: meta.agent_preset.clone(),
        };
        Session::create(session_id, options.seed.as_deref(), Some(&header))
    }

    /// 创建并进入 + 发布一个会话。
    pub fn create(self: &Rc<Self>, id: Option<SessionId>, options: &CreateSessionOptions) -> Result<Rc<Session>, SessionError> {
        let session = self.prepare(id, options)?;
        let rc = Rc::new(session);
        self.enter(&rc)?;
        self.announce(&rc)?;
        Ok(rc)
    }

    /// 进入一个 prepare 过的会话：安装 append 发布钩子 + 加入 store。
    /// 不发布 `session/created`——调用方先 detach 再 announce（rollback 安全）。
    pub fn enter(self: &Rc<Self>, session: &Rc<Session>) -> Result<(), SessionError> {
        let id = session.id().clone();
        if self.is_live(&id) {
            return Err(SessionError(format!("session \"{id}\" already exists")));
        }
        // 安装 append 发布钩子：把事件转发给 store 的 `session/event` 观察者。
        // 用 Weak 避免 store→session→store 引用环；剪枝在闭包内完成。
        let store_weak = Rc::downgrade(self);
        let session_weak = Rc::downgrade(session);
        session.set_event_observer(Some(Box::new(move |event: &SessionEvent| {
            let Some(store) = store_weak.upgrade() else { return };
            let Some(s) = session_weak.upgrade() else { return };
            for cb in store.on_event.borrow().iter() {
                cb(&s, event);
            }
        })));
        self.store.borrow_mut().insert(
            id,
            StoreEntry {
                session: session.clone(),
                announced: false,
            },
        );
        Ok(())
    }

    /// 发布 `session/created`（一次；同步广播）。
    pub fn announce(&self, session: &Rc<Session>) -> Result<(), SessionError> {
        let id = session.id().clone();
        let mut store = self.store.borrow_mut();
        let entry = store.get_mut(&id).ok_or_else(|| {
            SessionError(format!("session \"{id}\" is not live in this store"))
        })?;
        if entry.announced {
            return Err(SessionError(format!("session \"{id}\" was already announced")));
        }
        entry.announced = true;
        drop(store);
        for cb in self.on_created.borrow().iter() {
            cb(session);
        }
        Ok(())
    }

    /// 移除一个会话并发布配对 dispose（若已 announce）。
    /// 返回被移除的会话引用。
    pub fn dispose(&self, id: &SessionId) -> Option<Rc<Session>> {
        let removed = self.store.borrow_mut().remove(id);
        if let Some(entry) = &removed {
            if entry.announced {
                for cb in self.on_disposed.borrow().iter() {
                    cb(&entry.session);
                }
            }
        }
        removed.map(|e| e.session)
    }

    /// `session/flush` 同步 drain。
    pub fn flush(&self, session: &Rc<Session>) -> usize {
        let callbacks = self.on_flush.borrow();
        let count = callbacks.len();
        for cb in callbacks.iter() {
            cb(session);
        }
        count
    }

    /// 查找 live 会话。
    pub fn get(&self, id: &SessionId) -> Option<Rc<Session>> {
        self.store.borrow().get(id).map(|e| e.session.clone())
    }

    /// 全部 live 会话（创建顺序）。
    pub fn list(&self) -> Vec<Rc<Session>> {
        self.store.borrow().values().map(|e| e.session.clone()).collect()
    }

    /// 会话是否 live。
    pub fn is_live(&self, id: &SessionId) -> bool {
        self.store.borrow().contains_key(id)
    }

    /// mint 唯一 session id（`session-<n>`）。
    fn mint_id(&self) -> SessionId {
        let mut counter = self.counter.borrow_mut();
        loop {
            *counter += 1;
            let id = SessionId::from_raw(format!("session-{counter}"));
            if !self.store.borrow().contains_key(&id) {
                return id;
            }
        }
    }

    /// 从 live 源会话创建 live 子会话（fork）。
    pub fn fork(
        self: &Rc<Self>,
        source: &SessionForkSource<'_>,
        boundary: Option<u64>,
        child_session_id: Option<SessionId>,
    ) -> Result<Rc<Session>, SessionForkError> {
        if let Some(child_id) = &child_session_id {
            if self.is_live(child_id) {
                return Err(SessionForkError {
                    message: format!("session \"{child_id}\" already exists"),
                    code: SessionForkErrorCode::SessionAlreadyExists,
                });
            }
        }
        let live_source = self.resolve_fork_source(source)?;
        let seed = fork_seed(live_source.as_ref(), boundary)?;
        let mut meta = CreateSessionMeta {
            parent_session: Some(live_source.id().clone()),
            seed_length: Some(seed.len() as u64),
            ..Default::default()
        };
        meta.cwd = live_source.header().cwd.clone();
        let session = self
            .create(
                child_session_id,
                &CreateSessionOptions {
                    seed: Some(seed),
                    meta: Some(meta),
                },
            )
            .map_err(|e| SessionForkError {
                message: e.0,
                code: SessionForkErrorCode::SessionAlreadyExists,
            })?;
        Ok(session)
    }

    fn resolve_fork_source<'a>(
        &self,
        source: &'a SessionForkSource<'a>,
    ) -> Result<Rc<Session>, SessionForkError> {
        let id = match source {
            SessionForkSource::Id(id) => id.clone(),
            SessionForkSource::Object(s) => s.id().clone(),
        };
        let live = self
            .get(&id)
            .ok_or_else(|| SessionForkError {
                message: format!("session \"{id}\" not found"),
                code: SessionForkErrorCode::SessionNotFound,
            })?;
        match source {
            SessionForkSource::Id(_) => Ok(live),
            // 对象源必须与 live store 条目是同一个 Session（按指针 identity）。
            SessionForkSource::Object(s) => {
                if std::ptr::eq(Rc::as_ptr(&live), *s) {
                    Ok(live)
                } else {
                    Err(SessionForkError {
                        message: format!("session \"{id}\" is not the live store instance"),
                        code: SessionForkErrorCode::SessionNotLive,
                    })
                }
            }
        }
    }
}

/// 从 live 源会话计算 fork seed（稳定前缀）。
fn fork_seed(session: &Session, requested_boundary: Option<u64>) -> Result<Vec<SessionEvent>, SessionForkError> {
    let events = session.events();
    let last_seq = events.last().map(|e| e.seq);
    let boundary = match requested_boundary {
        Some(b) => b,
        None => match last_seq {
            Some(s) => s,
            None => return Ok(Vec::new()),
        },
    };
    let Some(last) = last_seq else {
        return Ok(Vec::new());
    };
    if boundary >= events.len() as u64 {
        return Err(SessionForkError {
            message: format!(
                "fork boundary {boundary} does not exist in session \"{}\" (last seq: {last})",
                session.id()
            ),
            code: SessionForkErrorCode::InvalidBoundary,
        });
    }
    if events[boundary as usize].seq != boundary {
        return Err(SessionForkError {
            message: format!(
                "fork boundary {boundary} does not match a contiguous event seq in session \"{}\"",
                session.id()
            ),
            code: SessionForkErrorCode::InvalidBoundary,
        });
    }
    // 前缀内最后一个 turn 边界：turn/start 结尾 → 落在 open turn 内
    let last_turn_boundary = events[..=boundary as usize]
        .iter()
        .rev()
        .find(|e| matches!(e.kind, crate::types::EventKind::TurnStart | crate::types::EventKind::TurnEnd));
    if let Some(b) = last_turn_boundary {
        if b.kind == crate::types::EventKind::TurnStart {
            let turn = b.data.get("turn").cloned().unwrap_or(serde_json::Value::Null);
            return Err(SessionForkError {
                message: format!(
                    "fork boundary {boundary} in session \"{}\" ends inside open turn {turn}",
                    session.id()
                ),
                code: SessionForkErrorCode::OpenTurn,
            });
        }
    }
    Ok(events[..=boundary as usize].to_vec())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 便捷：构造以 `session/created` 广播的 store（DI 形态）。
impl SessionStore {
    /// 创建一个 store + 一条默认会话（对齐 web.rs boot 的 seed default）。
    pub fn with_default_session(self) -> (Rc<Self>, Rc<Session>) {
        let rc = Rc::new(self);
        let session = rc
            .create(None, &CreateSessionOptions { seed: None, meta: None })
            .expect("default session creation");
        (rc, session)
    }
}

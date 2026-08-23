//! dsh-terminal 会话注册表（M5-DESIGN §6.1）。
//!
//! 职责（逐字参考 `terminal/src/registry.ts`）：Branded 会话表、owner=精确 Agent 授权
//! （ForeignSession）、每会话仅一个 active send（SEND_ACTIVE）、同名后端不得重复打开
//! （DuplicateBackend）、后端打开失败回滚（崩溃回滚）、dispose 关门。前端计时
//! （wait_reason/status/truncated）由后端拥有，注册表只守卫 + 派发。

use crate::types::{
    TerminalBackendKind, TerminalConfig, TerminalError, TerminalErrorCode, TerminalSendRequest,
    TerminalSendResult, TerminalSessionId, TerminalSessionStatus, TerminalSessionView,
    TerminalSignal,
};
use std::collections::HashMap;

/// 注册表可驱动的后端抽象（真实 bash-pty 见 `backend.rs`；测试用内存替身）。
pub trait TerminalBackend {
    /// 后端打开真实会话（分配 PTY/进程）；失败 → Err（开回调用方回滚）。
    fn open(&mut self, owner: &str, cfg: &TerminalConfig) -> Result<(), TerminalError>;
    /// 写入输入并等交付判定（含 viewport/wait_reason/status/truncated）。
    fn send(&mut self, req: &TerminalSendRequest) -> Result<TerminalSendResult, TerminalError>;
    /// 从滚动缓冲读（最多 max_read_bytes）。
    fn read(&mut self, max_read_bytes: usize) -> Result<String, TerminalError>;
    fn signal(&mut self, sig: TerminalSignal) -> Result<(), TerminalError>;
    fn close(&mut self) -> Result<(), TerminalError>;
    fn label(&self) -> &str;
    fn kind(&self) -> TerminalBackendKind;
}

/// 后端提供者：给定配置构造一个活的后端实例。
pub type BackendProvider = Box<dyn Fn(TerminalConfig) -> Box<dyn TerminalBackend>>;

/// owner 存活校验钩子（缺省 = 恒在）。
pub type OwnerLiveness = Box<dyn Fn(&str) -> bool>;

/// 一个已注册的后端定义（open 用其名字引用）。
#[derive(Debug, Clone)]
pub struct BackendDefinition {
    pub id: String,
    pub kind: TerminalBackendKind,
    pub label: String,
}

struct TerminalSession {
    id: TerminalSessionId,
    owner: String,
    name: Option<String>,
    backend: Box<dyn TerminalBackend>,
    cfg: TerminalConfig,
    busy: bool,
    status: TerminalSessionStatus,
    def: BackendDefinition,
}

/// 终端会话注册表（单线程服务员模型，与 suite 一致）。
pub struct TerminalSessionService {
    providers: HashMap<String, BackendProvider>,
    sessions: HashMap<TerminalSessionId, TerminalSession>,
    next_id: u64,
    disposing: bool,
    owner_live: Option<OwnerLiveness>,
}

impl Default for TerminalSessionService {
    fn default() -> Self {
        TerminalSessionService::new()
    }
}

impl TerminalSessionService {
    pub fn new() -> TerminalSessionService {
        TerminalSessionService {
            providers: HashMap::new(),
            sessions: HashMap::new(),
            next_id: 1,
            disposing: false,
            owner_live: None,
        }
    }

    /// 注册后端类型；同名类型重复 → Err(DuplicateBackend)（参考 registerBackend）。
    pub fn register_backend(
        &mut self,
        def: BackendDefinition,
        provider: BackendProvider,
    ) -> Result<(), TerminalError> {
        if self.providers.contains_key(&def.id) {
            return Err(TerminalError::new(
                TerminalErrorCode::DuplicateBackend,
                format!("a PTY backend named \"{}\" is already registered", def.id),
            ));
        }
        self.providers.insert(def.id.clone(), provider);
        Ok(())
    }

    /// 挂接 owner 存活校验（缺省 = 恒在；宿主接线时注入精确 Agent 存活检查）。
    pub fn set_owner_liveness(&mut self, live: OwnerLiveness) {
        self.owner_live = Some(live);
    }

    pub fn is_disposing(&self) -> bool {
        self.disposing
    }

    /// 打开（按 owner 隔离）会话；`name` 可选（同 owner 内唯一 → DuplicateName）。
    /// 后端打开失败 → 关闭残留并 Err（崩溃回滚）、不发布半开会话。
    pub fn open(
        &mut self,
        owner: &str,
        backend_id: &str,
        name: Option<&str>,
        cfg: TerminalConfig,
    ) -> Result<TerminalSessionId, TerminalError> {
        if self.disposing {
            return Err(TerminalError::new(
                TerminalErrorCode::ServiceDisposing,
                "PTY service is disposing".to_string(),
            ));
        }
        if let Some(name) = name {
            if name.is_empty() {
                return Err(TerminalError::new(
                    TerminalErrorCode::DuplicateName,
                    "PTY session name must be non-empty".to_string(),
                ));
            }
            if self
                .sessions
                .values()
                .any(|s| s.owner == owner && s.name.as_deref() == Some(name))
            {
                return Err(TerminalError::new(
                    TerminalErrorCode::DuplicateName,
                    format!("PTY session name \"{name}\" already exists for this owner"),
                ));
            }
        }
        if let Some(live) = &self.owner_live {
            if !live(owner) {
                return Err(TerminalError::new(
                    TerminalErrorCode::OwnerNotLive,
                    format!("PTY owner is no longer live: {owner}"),
                ));
            }
        }
        let provider = self.providers.get(backend_id).ok_or_else(|| {
            TerminalError::new(
                TerminalErrorCode::NoBackend,
                format!("no PTY backend registered for \"{backend_id}\""),
            )
        })?;
        let mut backend = (provider)(cfg.clone());
        if let Err(e) = backend.open(owner, &cfg) {
            // 开失败：尽量关掉半开资源再回传错误。
            let _ = backend.close();
            return Err(e);
        }
        let id = TerminalSessionId::from_raw(format!("pty-{}", self.next_id));
        self.next_id += 1;
        let def = BackendDefinition {
            id: backend_id.to_string(),
            kind: backend.kind(),
            label: backend.label().to_string(),
        };
        self.sessions.insert(
            id.clone(),
            TerminalSession {
                id: id.clone(),
                owner: owner.to_string(),
                backend,
                name: name.map(|n| n.to_string()),
                status: TerminalSessionStatus::Running,
                cfg,
                busy: false,
                def,
            },
        );
        Ok(id)
    }

    fn session_mut(
        &mut self,
        owner: &str,
        id: &TerminalSessionId,
    ) -> Result<&mut TerminalSession, TerminalError> {
        self.require_owner(owner, id)?;
        let session = self.sessions.get_mut(id).expect("checked by require_owner");
        Ok(session)
    }

    fn require_owner(&self, owner: &str, id: &TerminalSessionId) -> Result<(), TerminalError> {
        let session = self.sessions.get(id).ok_or_else(|| {
            TerminalError::new(
                TerminalErrorCode::NoSession,
                format!("no terminal session: {id}"),
            )
        })?;
        if session.owner != owner {
            return Err(TerminalError::new(
                TerminalErrorCode::ForeignSession,
                format!("terminal session {id} belongs to another owner"),
            ));
        }
        Ok(())
    }

    /// 发送并等交付；同会话并发/重入 send → SEND_ACTIVE。
    pub fn send(
        &mut self,
        owner: &str,
        id: &TerminalSessionId,
        req: &TerminalSendRequest,
    ) -> Result<TerminalSendResult, TerminalError> {
        let session = self.session_mut(owner, id)?;
        if session.busy {
            return Err(TerminalError::new(
                TerminalErrorCode::SendActive,
                format!("terminal session {id} has an active send"),
            ));
        }
        session.busy = true;
        let result = session.backend.send(req);
        let session = self.sessions.get_mut(id).expect("checked above");
        session.busy = false;
        let result = result?;
        if matches!(
            result.session_status,
            TerminalSessionStatus::Exited | TerminalSessionStatus::Aborted
        ) {
            session.status = result.session_status;
        }
        Ok(result)
    }

    pub fn read(&mut self, owner: &str, id: &TerminalSessionId) -> Result<String, TerminalError> {
        let session = self.session_mut(owner, id)?;
        let max = session.cfg.max_read_bytes;
        session.backend.read(max)
    }

    pub fn signal(
        &mut self,
        owner: &str,
        id: &TerminalSessionId,
        sig: TerminalSignal,
    ) -> Result<(), TerminalError> {
        let session = self.session_mut(owner, id)?;
        session.backend.signal(sig)?;
        Ok(())
    }

    /// 关闭并移除会话。
    pub fn close(&mut self, owner: &str, id: &TerminalSessionId) -> Result<(), TerminalError> {
        let session = self.session_mut(owner, id)?;
        session.backend.close()?;
        self.sessions.remove(id);
        Ok(())
    }

    pub fn view(
        &self,
        owner: &str,
        id: &TerminalSessionId,
    ) -> Result<TerminalSessionView, TerminalError> {
        self.require_owner(owner, id)?;
        Ok(self.to_view(self.sessions.get(id).expect("checked above")))
    }

    pub fn list(&self) -> Vec<TerminalSessionView> {
        self.sessions.values().map(|s| self.to_view(s)).collect()
    }

    fn to_view(&self, s: &TerminalSession) -> TerminalSessionView {
        TerminalSessionView {
            id: s.id.clone(),
            owner: s.owner.clone(),
            name: s.name.clone(),
            label: s.def.label.clone(),
            status: s.status,
            backend: s.def.id.clone(),
        }
    }

    /// 服务关门：拒绝新 open，关闭并清空全部会话。
    pub fn dispose(&mut self) {
        self.disposing = true;
        let ids: Vec<TerminalSessionId> = self.sessions.keys().cloned().collect();
        for id in ids {
            if let Some(s) = self.sessions.get_mut(&id) {
                let _ = s.backend.close();
            }
        }
        self.sessions.clear();
    }
}

//! `AgentRegistry`：活体 Agent 注册表 + 有序生命周期 + factory seam + initiator
//! 作用域（sync 版）。对齐报告 §A.3/A.4。

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use dsh_llm::Message;
use dsh_scope::{scope_target, ScopeCarrier, ScopeKey};
use dsh_session::{Session, SessionId};

use crate::agent_bus::{AgentBus, AgentListener, ChainListener};
use crate::inbox::Inbox;
use crate::types::{AgentCancelCause, AgentOptions, AgentStatus, InboxTarget};

// ---------------------------------------------------------------------------
// Agent（interface）：id/options/session/inbox/status/ctx + 驱动方法接口
// ---------------------------------------------------------------------------

/// agent-scoped 上下文视图（共享总线的带标签视角）。
#[derive(Clone)]
pub struct AgentCtx {
    bus: AgentBus,
    tag: ScopeKey,
}

impl AgentCtx {
    pub fn on(&self, name: &str, global: bool, cb: AgentListener) {
        self.bus.on(name, global, Some(self.tag.clone()), cb);
    }
    pub fn on_chain(&self, name: &str, global: bool, cb: ChainListener) {
        self.bus.on_chain(name, global, Some(self.tag.clone()), cb);
    }
    pub fn scope_key(&self) -> &ScopeKey {
        &self.tag
    }
    pub fn bus(&self) -> &AgentBus {
        &self.bus
    }
}

/// 活体 Agent（Rust 侧 interface 的 struct 形态）。
pub struct Agent {
    pub id: dsh_session::SessionId,
    pub options: AgentOptions,
    pub session: Rc<Session>,
    pub inbox: Inbox,
    pub status: Cell<AgentStatus>,
    pub ctx: AgentCtx,
    pub scope: ScopeKey,
}

impl Agent {
    /// 构造（未注册）。inbox 从 session 重建。
    pub fn new(
        id: dsh_session::SessionId,
        session: Rc<Session>,
        options: AgentOptions,
        bus: AgentBus,
        scope: ScopeKey,
    ) -> Result<Self, String> {
        let inbox = Inbox::new(session.clone())?;
        Ok(Agent {
            id,
            options,
            session,
            inbox,
            status: Cell::new(AgentStatus::Idle),
            ctx: AgentCtx {
                bus,
                tag: scope.clone(),
            },
            scope,
        })
    }

    /// 驱动/输入路由（M2d-2 只做 inbox 落账；唤醒钩子在 M2e 由 loop 注入）。
    pub fn append_input(&self, message: Message, target: InboxTarget) -> Result<(), String> {
        self.inbox.append_msg(target, message)
    }

    pub fn cancel(&self, _cause: AgentCancelCause, _keep_inbox: bool) {
        // M2e 由 loop 消费；本层只保留签名（status 转移 + 事件发射在 loop）。
        let _ = _cause;
        let _ = _keep_inbox;
    }
}

/// Agent 的 live 事件投影（`agent` 字段的 JSON 形态）。
pub fn agent_value(agent: &Agent) -> serde_json::Value {
    serde_json::json!({ "id": agent.id })
}

/// `agentCarrier(agent)` = `scopeTarget(agent, agent)`（无状态路由对象）。
pub fn agent_carrier(agent: &Agent) -> ScopeCarrier {
    scope_target(Some(agent.scope.clone()), None)
}

// ---------------------------------------------------------------------------
// AgentRegistry
// ---------------------------------------------------------------------------

struct AgentEntry {
    id: dsh_session::SessionId,
    agent: Rc<Agent>,
    owner: Option<dsh_session::SessionId>,
    carrier: ScopeCarrier,
    announced: Cell<bool>,
    announcing: Cell<bool>,
    detach_requested: Cell<bool>,
}

/// factory 缝（M2e 由 agent-loop 提供）。
pub trait AgentFactory {
    fn create_agent(
        &self,
        owner: Option<dsh_session::SessionId>,
        options: &CreateAgentOptions,
    ) -> Result<Rc<Agent>, String>;
    fn resume_agent(
        &self,
        owner: Option<dsh_session::SessionId>,
        options: &ResumeAgentOptions,
    ) -> Result<Rc<Agent>, String>;
}

#[derive(Debug, Clone)]
pub struct CreateAgentOptions {
    pub session_id: dsh_session::SessionId,
    pub options: Option<AgentOptions>,
}

#[derive(Debug, Clone)]
pub struct ResumeAgentOptions {
    pub resume_session_id: dsh_session::SessionId,
    pub options: Option<AgentOptions>,
}

/// initiator 生命周期（sync 版）：'active' → 'closing' → 'disposed'。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitiatorPhase {
    Active,
    Closing,
    Disposed,
}

struct InitiatorState {
    phase: InitiatorPhase,
    stack: Vec<Option<dsh_session::SessionId>>,
}

pub struct AgentRegistry {
    bus: AgentBus,
    store: RefCell<HashMap<dsh_session::SessionId, Rc<AgentEntry>>>,
    order: RefCell<Vec<dsh_session::SessionId>>,
    factory: RefCell<Option<Rc<dyn AgentFactory>>>,
    initiator: RefCell<InitiatorState>,
}

/// 一个边界守卫：退出作用域时弹出栈顶（含 panic）。
struct InitiatorGuard<'a> {
    registry: &'a AgentRegistry,
    armed: Cell<bool>,
}
impl<'a> Drop for InitiatorGuard<'a> {
    fn drop(&mut self) {
        if self.armed.replace(false) {
            self.registry.initiator.borrow_mut().stack.pop();
        }
    }
}

impl AgentRegistry {
    pub fn new(bus: AgentBus) -> Self {
        AgentRegistry {
            bus,
            store: RefCell::new(HashMap::new()),
            order: RefCell::new(Vec::new()),
            factory: RefCell::new(None),
            initiator: RefCell::new(InitiatorState {
                phase: InitiatorPhase::Active,
                stack: Vec::new(),
            }),
        }
    }

    pub fn bus(&self) -> &AgentBus {
        &self.bus
    }

    /// 便捷构造：不与注册表绑定 bus 的独立 agent。
    pub fn new_agent(
        &self,
        id: dsh_session::SessionId,
        session: Rc<Session>,
        options: AgentOptions,
    ) -> Rc<Agent> {
        Rc::new(
            Agent::new(id, session, options, self.bus.clone(), ScopeKey::new()).expect("agent"),
        )
    }

    // ---- 登记 / 生命周期 ----

    /// `register(agent)`：enter（id/session 一致性 + 重复检查）→ announce（created
    /// 可 veto）→ 返回**确切 disposer**（detach；load-bearing 生命周期链必须 yield）。
    /// created 监听器同步抛 → register 抛同错误 + 回滚（disposed:vetoed）。
    pub fn register<'a>(
        &'a self,
        agent: Rc<Agent>,
        owner: Option<dsh_session::SessionId>,
    ) -> Result<Rc<dyn Fn() + 'a>, String> {
        // enter：authoritative 碰撞边界 + 幂等 detach（announcing 时延后）
        let entry = self.enter(agent, owner)?;
        if let Err(e) = self.announce(&entry) {
            // veto 发布回滚：disposed 会发出（entry.announced=true）
            self.detach_entered(&entry);
            return Err(e);
        }
        Ok(Rc::new(move || self.detach(&entry)))
    }

    /// `enter(agent, owner)`：校验 + 建 entry + 返回幂等 detach（defer 语义由
    /// `detach` 依据 `announcing` 裁决——并入 register 返回的 disposer 表达）。
    fn enter(
        &self,
        agent: Rc<Agent>,
        owner: Option<dsh_session::SessionId>,
    ) -> Result<Rc<AgentEntry>, String> {
        if agent.id != *agent.session.id() {
            return Err(format!(
                "agent id \"{}\" does not match session id \"{}\"",
                agent.id,
                agent.session.id()
            ));
        }
        if self.store.borrow().contains_key(&agent.id) {
            return Err(format!("agent \"{}\" is already registered", agent.id));
        }
        let entry = Rc::new(AgentEntry {
            id: agent.id.clone(),
            carrier: agent_carrier(&agent),
            owner,
            agent,
            announced: Cell::new(false),
            announcing: Cell::new(false),
            detach_requested: Cell::new(false),
        });
        self.store.borrow_mut().insert(entry.id.clone(), entry.clone());
        self.order.borrow_mut().push(entry.id.clone());
        Ok(entry)
    }

    /// `announce(agent)`：发布 created；同步 veto 以 Err 返回；finally 复位
    /// announcing 并处理 detachRequested（延后到同步 dispatch unwind）。
    fn announce(&self, entry: &Rc<AgentEntry>) -> Result<(), String> {
        if !self.store.borrow().contains_key(&entry.id) {
            return Err(format!("agent \"{}\" is not live in this registry", entry.id));
        }
        if entry.announced.get() || entry.announcing.get() {
            return Err(format!("agent \"{}\" was already announced", entry.id));
        }
        entry.announcing.set(true);
        entry.announced.set(true);
        let veto = self
            .bus
            .emit_veto(&entry.carrier, "agent/created", serde_json::json!({ "agent": agent_value(&entry.agent) }));
        // finally：复位 announcing + 处理延后 detach
        entry.announcing.set(false);
        if entry.detach_requested.get() {
            self.detach_entered(entry);
        }
        veto
    }

    /// 幂等 detach；announcing 中 → 延后（detachRequested）。
    fn detach(&self, entry: &Rc<AgentEntry>) {
        if entry.announcing.get() {
            entry.detach_requested.set(true);
            return;
        }
        self.detach_entered(entry);
    }

    fn detach_entered(&self, entry: &Rc<AgentEntry>) {
        // stale 能力不能删同 id 的替换生命周期
        let is_current = self
            .store
            .borrow()
            .get(&entry.id)
            .map(|e| Rc::ptr_eq(e, entry))
            .unwrap_or(false);
        if !is_current {
            return;
        }
        self.store.borrow_mut().remove(&entry.id);
        self.order.borrow_mut().retain(|id| id != &entry.id);
        let announced = entry.announced.get();
        if !announced {
            return; // 无 created 即无 disposed
        }
        self.emit_disposed(entry);
    }

    fn emit_disposed(&self, entry: &Rc<AgentEntry>) {
        let mut warns = Vec::new();
        self.bus.emit(
            &entry.carrier,
            "agent/disposed",
            serde_json::json!({ "agent": agent_value(&entry.agent) }),
            &mut |raw| warns.push(format!("agent \"{}\": agent/disposed listener threw: {raw}", entry.id)),
        );
        let _ = warns;
    }

    /// 单独拆离（测试用）：直接 detach。
    pub fn dispose(&self, agent: &Rc<Agent>) {
        let entry = self.store.borrow().get(&agent.id).cloned();
        if let Some(entry) = entry {
            self.detach(&entry);
        }
    }

    /// `enter(agent, owner)` 公开形态：只登记不发布（无事件），返回幂等 detach
    /// disposer（announced=false → detach 不发射 disposed）。随后 `announce_by_id`
    /// 发布。通常用 `register`（enter + announce + veto 回滚）一步完成。
    pub fn enter_agent<'a>(
        &'a self,
        agent: Rc<Agent>,
        owner: Option<dsh_session::SessionId>,
    ) -> Result<Rc<dyn Fn() + 'a>, String> {
        let entry = self.enter(agent, owner)?;
        Ok(Rc::new(move || self.detach(&entry)))
    }

    /// `announce(agent)` 公开形态：按 id 查 entry 后发布。
    pub fn announce_by_id(&self, id: &dsh_session::SessionId) -> Result<(), String> {
        let entry = self.store.borrow().get(id).cloned().ok_or_else(|| {
            format!("agent \"{id}\" is not live in this registry")
        })?;
        self.announce(&entry)
    }

    // ---- 查询 ----

    pub fn get(&self, id: &dsh_session::SessionId) -> Option<Rc<Agent>> {
        self.store.borrow().get(id).map(|e| e.agent.clone())
    }

    pub fn is_owned_by(&self, id: &dsh_session::SessionId, owner: &dsh_session::SessionId) -> bool {
        self.store
            .borrow()
            .get(id)
            .map(|e| e.owner.as_ref() == Some(owner))
            .unwrap_or(false)
    }

    /// 注册序的新数组。
    pub fn list(&self) -> Vec<Rc<Agent>> {
        let order = self.order.borrow();
        let store = self.store.borrow();
        order
            .iter()
            .filter_map(|id| store.get(id).map(|e| e.agent.clone()))
            .collect()
    }

    /// `owner === undefined` 的 live agent（注册序）。
    pub fn roots(&self) -> Vec<Rc<Agent>> {
        let order = self.order.borrow();
        let store = self.store.borrow();
        order
            .iter()
            .filter_map(|id| store.get(id).filter(|e| e.owner.is_none()).map(|e| e.agent.clone()))
            .collect()
    }

    // ---- factory seam ----

    /// `setFactory(factory)`：幂等写入一次；已注册 → 抛。
    /// 返回**确切的 disposer**（单发，清空 slot）。
    pub fn set_factory<'a>(
        &'a self,
        factory: Rc<dyn AgentFactory>,
    ) -> Result<Rc<dyn Fn() + 'a>, String> {
        if self.factory.borrow().is_some() {
            return Err("an agent factory is already registered".to_string());
        }
        self.factory.borrow_mut().replace(factory);
        Ok(Rc::new(move || {
            self.factory.borrow_mut().take();
        }))
    }

    pub fn require_factory(&self) -> Result<Rc<dyn AgentFactory>, String> {
        self.factory
            .borrow()
            .clone()
            .ok_or_else(|| "no agent factory registered (load an agent-loop plugin)".to_string())
    }

    pub fn create(&self, options: &CreateAgentOptions) -> Result<Rc<Agent>, String> {
        let owner = self.current_initiator().ok().flatten();
        let factory = self.require_factory()?;
        factory.create_agent(owner, options)
    }

    pub fn resume(&self, options: &ResumeAgentOptions) -> Result<Rc<Agent>, String> {
        let owner = self.current_initiator().ok().flatten();
        let factory = self.require_factory()?;
        factory.resume_agent(owner, options)
    }

    // ---- initiator 边界（sync 版；无 Promise drain —— D-030 声明） ----

    /// `withInitiator(agent, op)` / `withoutInitiator(op)` 的统一实现。
    pub fn run_with_initiator<R>(
        &self,
        agent: Option<SessionId>,
        body: impl FnOnce() -> R,
    ) -> Result<R, String> {
        self.assert_initiators_active()?;
        self.initiator.borrow_mut().stack.push(agent);
        let guard = InitiatorGuard {
            registry: self,
            armed: Cell::new(true),
        };
        let result = body();
        guard.armed.set(false);
        self.initiator.borrow_mut().stack.pop();
        Ok(result)
    }

    pub fn with_initiator<R>(&self, agent: &Agent, body: impl FnOnce() -> R) -> Result<R, String> {
        self.run_with_initiator(Some(agent.id.clone()), body)
    }

    pub fn without_initiator<R>(&self, body: impl FnOnce() -> R) -> Result<R, String> {
        self.run_with_initiator(None, body)
    }

    /// 当前 inherited agent（读取需 active——'closing'/'disposed' 都拒）。
    pub fn current_initiator(&self) -> Result<Option<SessionId>, String> {
        self.assert_initiators_active()?;
        Ok(self.initiator.borrow().stack.last().cloned().flatten())
    }

    pub fn require_initiator(&self) -> Result<SessionId, String> {
        self.current_initiator()?
            .ok_or_else(|| "no initiating agent is active".to_string())
    }

    pub fn initiator_phase(&self) -> InitiatorPhase {
        self.initiator.borrow().phase
    }

    fn assert_initiators_active(&self) -> Result<(), String> {
        if self.initiator.borrow().phase != InitiatorPhase::Active {
            return Err("agent initiator scope is disposed".to_string());
        }
        Ok(())
    }

    /// `closeInitiators()`：'active' → 'closing'（拒新边界）。
    pub fn close_initiators(&self) {
        let mut s = self.initiator.borrow_mut();
        if s.phase == InitiatorPhase::Active {
            s.phase = InitiatorPhase::Closing;
        }
    }

    /// `disposeInitiators()`：sync 版直接转 disposed（无异步 drain）。
    pub fn dispose_initiators(&self) {
        let mut s = self.initiator.borrow_mut();
        if s.phase != InitiatorPhase::Disposed {
            s.phase = InitiatorPhase::Disposed;
            s.stack.clear();
        }
    }
}

// 供测试便利：把 Agent 的 scope 暴露给组装。
pub fn agent_scope(agent: &Agent) -> ScopeKey {
    agent.scope.clone()
}

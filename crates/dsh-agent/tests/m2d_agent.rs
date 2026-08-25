//! M2d-2：注册表生命周期 / factory seam / sync initiator / dispatch 融合 /
//! agent-invariant（移植 agent.spec.ts 的 registry、agent-initiator.spec.ts（sync 版）、
//! dispatch、invariant.spec.ts 相关场景；错误消息逐字）。

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use dsh_agent::{
    agent_carrier, agent_value, assemble_context_for, emit_agent_event, fuse_agent, Agent,
    AgentBus, AgentEventDispatch, AgentFactory, AgentInvariant, AgentRegistry, AgentStatus,
    CreateAgentOptions, NextFn, ResumeAgentOptions,
};
use dsh_session::{
    store::SessionStore, CreateSessionMeta, CreateSessionOptions, Session, SessionId,
};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn store() -> Arc<SessionStore> {
    Arc::new(SessionStore::new())
}

fn session(store: &Arc<SessionStore>, id: &str) -> Arc<Session> {
    store
        .create(
            Some(SessionId(id.to_string())),
            &CreateSessionOptions {
                seed: None,
                meta: Some(CreateSessionMeta {
                    seed_length: Some(0),
                    ..Default::default()
                }),
            },
        )
        .unwrap()
}

struct TestWorld {
    reg: Rc<AgentRegistry>,
}

impl TestWorld {
    fn new() -> Self {
        TestWorld {
            reg: Rc::new(AgentRegistry::new(AgentBus::new())),
        }
    }
    /// 便捷：构造与 session id 一致的 agent（inbox 从空 session 重建）。
    fn agent(&self, id: &str) -> Rc<Agent> {
        let s = session(&store(), id);
        Rc::new(
            Agent::new(
                SessionId(id.to_string()),
                s,
                Default::default(),
                self.reg.bus().clone(),
                dsh_scope::ScopeKey::new(),
            )
            .unwrap(),
        )
    }
    /// 注册并返回 disposer（borrow 到 self 生命周期）。
    fn register<'a>(&'a self, a: Rc<Agent>) -> Rc<dyn Fn() + 'a> {
        self.reg.register(a, None).expect("register should succeed")
    }
}

fn global_log(_bus: &AgentBus) -> Rc<RefCell<Vec<String>>> {
    Rc::new(RefCell::new(Vec::new()))
}

/// 全局监听某事件并把名字/载荷写入 log。
fn listen(bus: &AgentBus, name: &'static str, log: Rc<RefCell<Vec<String>>>) {
    bus.on(
        name,
        true,
        None,
        Rc::new(move |got, _payload| log.borrow_mut().push(got.to_string())),
    );
}

// ---------------------------------------------------------------------------
// registry：登记 / 生命周�序列
// ---------------------------------------------------------------------------

#[test]
fn register_lists_roots_and_disposes() {
    let w = TestWorld::new();
    let bus = w.reg.bus().clone();
    let created = global_log(&bus);
    let disposed = global_log(&bus);
    listen(&bus, "agent/created", created.clone());
    listen(&bus, "agent/disposed", disposed.clone());

    let a = w.agent("a");
    let child = w.agent("child");
    w.register(a.clone());
    w.reg
        .register(child.clone(), Some(SessionId("a".into())))
        .unwrap();

    assert_eq!(w.reg.list().len(), 2);
    assert_eq!(w.reg.list()[0].id, SessionId("a".into()));
    assert_eq!(w.reg.list()[1].id, SessionId("child".into()));
    // roots：owner === undefined
    let roots = w.reg.roots();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].id, SessionId("a".into()));
    // get
    assert!(w.reg.get(&SessionId("a".into())).is_some());
    assert!(w.reg.get(&SessionId("nope".into())).is_none());

    // dispose a → 只发一次 disposed
    w.reg.dispose(&a);
    w.reg.dispose(&a);
    assert_eq!(disposed.borrow().iter().filter(|s| *s == "agent/disposed").count(), 1);
    w.reg.dispose(&child);
    assert!(w.reg.list().is_empty());
    // created 仍两次（enter 各自一次）
    assert_eq!(created.borrow().len(), 2);
}

#[test]
fn enter_identity_mismatch_rejects() {
    let w = TestWorld::new();
    let s = session(&store(), "session-a");
    let stray = Rc::new(
        Agent::new(
            SessionId("other".into()),
            s,
            Default::default(),
            w.reg.bus().clone(),
            dsh_scope::ScopeKey::new(),
        )
        .unwrap(),
    );
    let err = w.reg.register(stray, None).err().expect("must reject");
    assert_eq!(
        err,
        "agent id \"other\" does not match session id \"session-a\""
    );
}

#[test]
fn duplicate_register_rejects() {
    let w = TestWorld::new();
    let a = w.agent("dup");
    w.register(a.clone());
    let err = w.reg.register(a, None).err().expect("must reject");
    assert_eq!(err, "agent \"dup\" is already registered");
}

#[test]
fn enter_announce_separation_and_repeat_announce() {
    let w = TestWorld::new();
    let bus = w.reg.bus().clone();
    let created = global_log(&bus);
    let disposed = global_log(&bus);
    listen(&bus, "agent/created", created.clone());
    listen(&bus, "agent/disposed", disposed.clone());

    let a = w.agent("sep");
    // enter 不发布
    let detach = w.reg.enter_agent(a.clone(), None).unwrap();
    assert!(created.borrow().is_empty(), "enter must not emit created");
    // announce → created
    w.reg.announce_by_id(&SessionId("sep".into())).unwrap();
    assert_eq!(created.borrow().len(), 1);
    // 重复 announce → 拒
    let err = w.reg.announce_by_id(&SessionId("sep".into())).err().unwrap();
    assert_eq!(err, "agent \"sep\" was already announced");
    // detach → disposed；再 detach no-op（幂等）
    detach();
    detach();
    assert_eq!(disposed.borrow().len(), 1);
    assert!(w.reg.get(&SessionId("sep".into())).is_none());
}

#[test]
fn created_veto_rolls_back_to_disposed() {
    let w = TestWorld::new();
    let bus = w.reg.bus().clone();
    // created listener 同步抛 → register 抛同错误 + 回滚（disposed:vetoed）
    bus.on(
        "agent/created",
        true,
        None,
        Rc::new(|_, _| panic!("boom from created")),
    );
    let disposed = global_log(&bus);
    listen(&bus, "agent/disposed", disposed.clone());
    let a = w.agent("veto");
    let err = w.reg.register(a.clone(), None).err().unwrap();
    assert_eq!(err, "boom from created");
    // 回滚：disposed 发出一次，且 entry 已拆
    assert_eq!(
        disposed.borrow().iter().filter(|s| *s == "agent/disposed").count(),
        1
    );
    assert!(w.reg.get(&SessionId("veto".into())).is_none());
}

#[test]
fn created_listener_detach_defers_to_unwind() {
    let w = TestWorld::new();
    let bus = w.reg.bus().clone();
    let log: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let a = w.agent("def");

    // 第一个 created listener：记 'first' → detach（延后）→ 记 'after-detach'
    let log1 = log.clone();
    let reg = w.reg.clone();
    let a1 = a.clone();
    bus.on(
        "agent/created",
        true,
        None,
        Rc::new(move |_, _| {
            log1.borrow_mut().push("first".into());
            reg.dispose(&a1);
            log1.borrow_mut().push("after-detach".into());
        }),
    );
    // 第二个 created listener：仍收到 created（detach 延后），且 entry 仍 live
    let log2 = log.clone();
    bus.on(
        "agent/created",
        true,
        None,
        Rc::new(move |_, p| {
            assert_eq!(p["agent"]["id"], "def");
            log2.borrow_mut().push("second".into());
        }),
    );
    // disposed listener
    let log3 = log.clone();
    bus.on(
        "agent/disposed",
        true,
        None,
        Rc::new(move |_, _| log3.borrow_mut().push("disposed".into())),
    );

    w.register(a.clone());
    let after = log.borrow().clone();
    assert_eq!(after, vec!["first", "after-detach", "second", "disposed"]);
    assert!(w.reg.get(&SessionId("def".into())).is_none());
}

#[test]
fn stale_disposer_cannot_remove_replacement() {
    let w = TestWorld::new();
    let a1 = w.agent("stale");
    let d1 = w.register(a1.clone());
    // 用首个 disposer 拆掉
    d1();
    // 同 id 替换（新 session/entry）
    let a2 = w.agent("stale");
    let _d2 = w.register(a2.clone());
    // 旧 disposer 再跑 → 不能删替换 entry
    d1();
    assert!(w.reg.get(&SessionId("stale".into())).is_some());
}

#[test]
fn owner_tracking() {
    let w = TestWorld::new();
    let parent = w.agent("p");
    let child = w.agent("c");
    w.register(parent.clone());
    w.reg
        .register(child.clone(), Some(SessionId("p".into())))
        .unwrap();
    assert!(w.reg.is_owned_by(&SessionId("c".into()), &SessionId("p".into())));
    // root 的 owner 是 undefined → 不自拥有
    assert!(!w.reg.is_owned_by(&SessionId("p".into()), &SessionId("p".into())));
    assert!(!w.reg.is_owned_by(&SessionId("c".into()), &SessionId("q".into())));
}

#[test]
fn announce_on_non_live_entry_rejects() {
    let w = TestWorld::new();
    let err = w
        .reg
        .announce_by_id(&SessionId("gone".into()))
        .err()
        .unwrap();
    assert_eq!(err, "agent \"gone\" is not live in this registry");
}

// ---------------------------------------------------------------------------
// registry：factory seam
// ---------------------------------------------------------------------------

#[allow(clippy::type_complexity)]
struct FakeFactory {
    calls: Rc<RefCell<Vec<(Option<SessionId>, SessionId)>>>,
    bus: AgentBus,
}

impl AgentFactory for FakeFactory {
    fn create_agent(
        &self,
        owner: Option<SessionId>,
        options: &CreateAgentOptions,
    ) -> Result<Rc<Agent>, String> {
        self.calls
            .borrow_mut()
            .push((owner, options.session_id.clone()));
        let id = options.session_id.raw();
        let s = session(&store(), id);
        Ok(Rc::new(
            Agent::new(
                options.session_id.clone(),
                s,
                Default::default(),
                self.bus.clone(),
                dsh_scope::ScopeKey::new(),
            )
            .unwrap(),
        ))
    }
    fn resume_agent(
        &self,
        owner: Option<SessionId>,
        options: &ResumeAgentOptions,
    ) -> Result<Rc<Agent>, String> {
        self.calls
            .borrow_mut()
            .push((owner, options.resume_session_id.clone()));
        let s = session(&store(), options.resume_session_id.raw());
        Ok(Rc::new(
            Agent::new(
                options.resume_session_id.clone(),
                s,
                Default::default(),
                self.bus.clone(),
                dsh_scope::ScopeKey::new(),
            )
            .unwrap(),
        ))
    }
}

#[test]
fn factory_seam_rejects_without_factory_and_single_registration() {
    let w = TestWorld::new();
    let opts = CreateAgentOptions {
        session_id: SessionId("f".into()),
        options: None,
    };
    // 无 factory → create/resume 拒
    let err = w.reg.create(&opts).err().unwrap();
    assert_eq!(err, "no agent factory registered (load an agent-loop plugin)");

    let calls = Rc::new(RefCell::new(Vec::new()));
    let factory = Rc::new(FakeFactory {
        calls: calls.clone(),
        bus: w.reg.bus().clone(),
    });
    let disposer = w.reg.set_factory(factory).unwrap();
    // 第二个 factory → 拒
    let err2 = w
        .reg
        .set_factory(Rc::new(FakeFactory {
            calls: Rc::new(RefCell::new(Vec::new())),
            bus: w.reg.bus().clone(),
        }))
        .err()
        .unwrap();
    assert_eq!(err2, "an agent factory is already registered");

    // create 委托（owner = 当前 initiator，栈外为 None）
    let created = w.reg.create(&opts).unwrap();
    assert_eq!(created.id, SessionId("f".into()));
    assert_eq!(calls.borrow().len(), 1);
    assert_eq!(calls.borrow()[0].0, None);

    // disposer 清空 slot → create 再拒
    disposer();
    let err3 = w.reg.create(&opts).err().unwrap();
    assert_eq!(err3, "no agent factory registered (load an agent-loop plugin)");
}

// ---------------------------------------------------------------------------
// initiator（sync 版）
// ---------------------------------------------------------------------------

#[test]
fn initiator_current_require_nesting_and_disposal() {
    let w = TestWorld::new();
    let a = w.agent("a");
    let b = w.agent("b");
    // 栈外
    assert_eq!(w.reg.current_initiator().unwrap(), None);
    let err = w.reg.require_initiator().err().unwrap();
    assert_eq!(err, "no initiating agent is active");

    // with child
    w.reg
        .with_initiator(&a, || {
            assert_eq!(w.reg.current_initiator().unwrap(), Some(SessionId("a".into())));
            assert_eq!(w.reg.require_initiator().unwrap(), SessionId("a".into()));
            // 嵌套
            w.reg
                .with_initiator(&b, || {
                    assert_eq!(
                        w.reg.current_initiator().unwrap(),
                        Some(SessionId("b".into()))
                    );
                    // without → 无
                    w.reg
                        .without_initiator(|| {
                            assert_eq!(w.reg.current_initiator().unwrap(), None);
                        })
                        .unwrap();
                    // 嵌套里仍 b
                    assert_eq!(w.reg.require_initiator().unwrap(), SessionId("b".into()));
                    w.reg.run_with_initiator(Some(SessionId("a".into())), || {
                        assert_eq!(w.reg.require_initiator().unwrap(), SessionId("a".into()));
                    })
                    .unwrap();
                })
                .unwrap();
            // 回来后仍是 a
            assert_eq!(w.reg.require_initiator().unwrap(), SessionId("a".into()));
        })
        .unwrap();
    // 栈空
    assert_eq!(w.reg.current_initiator().unwrap(), None);

    // close → 写/读都拒
    w.reg.close_initiators();
    let err2 = w.reg.without_initiator(|| {}).err().unwrap();
    assert_eq!(err2, "agent initiator scope is disposed");
    let err3 = w.reg.current_initiator().err().unwrap();
    assert_eq!(err3, "agent initiator scope is disposed");

    // dispose → 同样拒（且已清栈）
    w.reg.dispose_initiators();
    let err4 = w.reg.with_initiator(&a, || {}).err().unwrap();
    assert_eq!(err4, "agent initiator scope is disposed");
}

#[test]
fn initiator_scope_restores_after_panic() {
    let w = TestWorld::new();
    // 内部 panic → 栈守卫弹出，栈外不受污染
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = w.reg.without_initiator(|| -> Result<(), String> {
            panic!("inner boom");
        });
    }));
    assert!(r.is_err());
    assert_eq!(w.reg.current_initiator().unwrap(), None);
}

// ---------------------------------------------------------------------------
// dispatch：融合 / 通知包含 / serial / one-shot
// ---------------------------------------------------------------------------

#[test]
fn emit_fuses_agent_and_injects_subject_over_conflict() {
    let w = TestWorld::new();
    let bus = w.reg.bus().clone();
    let got: Rc<RefCell<Option<Value>>> = Rc::new(RefCell::new(None));
    let got2 = got.clone();
    bus.on(
        "agent/inbox/inserted",
        true,
        None,
        Rc::new(move |_, p| *got2.borrow_mut() = Some(p.clone())),
    );
    let a = w.agent("fus");
    let carrier = agent_carrier(&a);
    let d = AgentEventDispatch::new(bus, carrier);
    d.emit(
        &a,
        "agent/inbox/inserted",
        json!({ "agent": "WRONG", "message": { "id": "m1" } }),
        &mut Vec::new(),
    );
    let payload = got.borrow().clone().unwrap();
    // 冲突的 agent 字段被注入 subject 覆盖
    assert_eq!(payload["agent"]["id"], "fus");
    assert_eq!(payload["message"]["id"], "m1");
}

#[test]
fn emit_containment_not_veto_and_no_starvation() {
    let w = TestWorld::new();
    let bus = w.reg.bus().clone();
    let log: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    // 第一个监听器抛
    let log1 = log.clone();
    bus.on(
        "agent/boom",
        true,
        None,
        Rc::new(move |_, _| {
            log1.borrow_mut().push("first".into());
            panic!("listener boom");
        }),
    );
    // 第二个仍收到（不 starve）
    let log2 = log.clone();
    bus.on(
        "agent/boom",
        true,
        None,
        Rc::new(move |_, _| log2.borrow_mut().push("second".into())),
    );
    let a = w.agent("boom");
    let mut warns = Vec::new();
    emit_agent_event(&bus, &a, "agent/boom", json!({}), &mut warns);
    // 通知不可 veto：第二个监听器未饿死
    assert_eq!(log.borrow()[..], vec!["first", "second"]);
    // warn 逐字
    assert_eq!(warns.len(), 1);
    assert_eq!(warns[0], "agent event \"agent/boom\" listener threw: listener boom");
}

#[test]
fn serial_fuses_subject_and_respects_next() {
    let w = TestWorld::new();
    let bus = w.reg.bus().clone();
    let order: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    // 串行监听器：改载荷 + 继续
    let o1 = order.clone();
    bus.on_chain(
        "agent/turn-stopping",
        true,
        None,
        Rc::new(move |mut p: Value, next: NextFn| {
            let agent_id = p["agent"]["id"].as_str().unwrap().to_string();
            o1.borrow_mut().push(format!("ln1:{agent_id}"));
            p["turn"] = json!(4); // 替换参数
            next(p)
        }),
    );
    let o2 = order.clone();
    bus.on_chain(
        "agent/turn-stopping",
        true,
        None,
        Rc::new(move |p: Value, next: NextFn| {
            o2.borrow_mut().push(format!("ln2:{}", p["turn"]));
            next(p)
        }),
    );
    let a = w.agent("ser");
    let carrier = agent_carrier(&a);
    let d = AgentEventDispatch::new(bus.clone(), carrier);
    let innermost_called = Rc::new(Cell::new(false));
    let ic = innermost_called.clone();
    let result = d.serial(
        &a,
        "agent/turn-stopping",
        json!({ "turn": 3, "signal": "cancel" }),
        Rc::new(move |p| {
            ic.set(true);
            p
        }),
    );
    assert!(innermost_called.get());
    assert_eq!(order.borrow()[..], vec!["ln1:ser", "ln2:4"]);
    assert_eq!(result["turn"], 4);
}

#[test]
fn emit_agent_event_oneshot_and_fuse_conflict() {
    let w = TestWorld::new();
    let bus = w.reg.bus().clone();
    let got: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let got2 = got.clone();
    bus.on(
        "agent/x",
        true,
        None,
        Rc::new(move |_, p| *got2.borrow_mut() = Some(p["agent"]["id"].as_str().unwrap().to_string())),
    );
    let a = w.agent("one");
    emit_agent_event(&bus, &a, "agent/x", json!({}), &mut Vec::new());
    assert_eq!(got.borrow().as_deref(), Some("one"));
}

#[test]
fn assemble_context_for_sets_scope_to_agent() {
    let w = TestWorld::new();
    let a = w.agent("asm");
    let ctx = assemble_context_for(&a);
    assert_eq!(ctx.scope, Some(a.scope.clone()));
    // S3（D-107）：组装上下文携带组装者会话身份 —— 供 standing 等的 Fn 段按
    // 自身会话折叠（None = 无身份组装 → 回退全局源）。
    assert_eq!(ctx.session_id.as_deref(), Some(a.session.id().raw()));
}

// ---------------------------------------------------------------------------
// invariant：同态 status 转移拒绝
// ---------------------------------------------------------------------------

#[test]
fn invariant_rejects_noop_status_transition() {
    let w = TestWorld::new();
    let bus = w.reg.bus().clone();
    let fails: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let f = fails.clone();
    AgentInvariant::install(&bus, Rc::new(move |m| f.borrow_mut().push(m)));
    let a = w.agent("inv");

    let mut warns = Vec::new();
    emit_agent_event(&bus, &a, "agent/status", json!({ "status": "running" }), &mut warns);
    emit_agent_event(&bus, &a, "agent/status", json!({ "status": "running" }), &mut warns);
    emit_agent_event(&bus, &a, "agent/status", json!({ "status": "idle" }), &mut warns);
    emit_agent_event(&bus, &a, "agent/status", json!({ "status": "idle" }), &mut warns);

    assert_eq!(
        fails.borrow()[..],
        vec![
            "agent/status repeated running (no-op transition)",
            "agent/status repeated idle (no-op transition)"
        ]
    );
}

// ---------------------------------------------------------------------------
// model-selection：组装捕获 + 请求路由装配
// ---------------------------------------------------------------------------

use dsh_agent::{install_model_selection, ModelSelection, ModelSelectionRef};
use dsh_brand::ReasoningEffortId;
use dsh_system_prompt::{AssembleContext, Config, SystemPrompt};
use std::cell::RefCell as StdRefCell;
use std::rc::Rc as StdRc;

fn sp() -> Rc<SystemPrompt> {
    Rc::new(SystemPrompt::new(&Config::default(), StdRc::new(|| {})).unwrap())
}

#[test]
fn model_selection_overrides_assembly_variables_and_assemble_request() {
    let w = TestWorld::new();
    let bus = w.reg.bus().clone();
    let sp = sp();
    let sel: StdRc<StdRefCell<ModelSelectionRef>> = StdRc::new(StdRefCell::new(ModelSelectionRef {
        current: Some(ModelSelection {
            provider: "litellm".into(),
            model: "deepseek-r1".into(),
            reasoning_effort: Some(ReasoningEffortId::from_raw("high")),
        }),
        ..Default::default()
    }));
    let agent = w.agent("ms");
    install_model_selection(&sp, &bus, &agent.scope, sel.clone());

    // 1) 组装侧：variables 被覆盖，assembled 捕获 current
    let ctx = AssembleContext{
        scope: Some(agent.scope.clone()),
        session_id: None,
    };
    let assembly = sp.assemble(&ctx).unwrap();
    let vars: std::collections::HashMap<&str, String> = assembly
        .variables
        .iter()
        .filter_map(|(k, v)| v.as_ref().map(|v| (k.as_str(), v.clone())))
        .collect();
    assert_eq!(vars.get("provider").map(String::as_str), Some("litellm"));
    assert_eq!(vars.get("model").map(String::as_str), Some("deepseek-r1"));
    // assembled 快照 == 进入时的 current
    assert_eq!(
        sel.borrow().assembled.as_ref().map(|s| s.model.as_str()),
        Some("deepseek-r1")
    );

    // 2) 请求侧：payload 中的 provider/model 被 selected 覆盖，reasoningEffort 先剥后恢
    let payload = json!({
        "provider": "old-provider",
        "model": "old-model",
        "maxTokens": 1024,
        "reasoningEffort": "medium"
    });
    let carrier = agent_carrier(&agent);
    let result = bus
        .waterfall(&carrier, "agent/request", payload, StdRc::new(|p| p));
    assert_eq!(result["provider"], "litellm");
    assert_eq!(result["model"], "deepseek-r1");
    assert_eq!(result["maxTokens"], 1024);
    assert_eq!(result["reasoningEffort"], "high");
}

#[test]
fn model_selection_assembled_unset_passes_request_through() {
    let w = TestWorld::new();
    let bus = w.reg.bus().clone();
    let sp = sp();
    let sel: StdRc<StdRefCell<ModelSelectionRef>> =
        StdRc::new(StdRefCell::new(ModelSelectionRef::default()));
    let agent = w.agent("ms2");
    install_model_selection(&sp, &bus, &agent.scope, sel.clone());

    // 未组装（assembled None）→ 请求原样透传
    let payload = json!({ "provider": "x", "model": "y", "reasoningEffort": "low" });
    let carrier = agent_carrier(&agent);
    let result = bus.waterfall(&carrier, "agent/request", payload, StdRc::new(|p| p));
    assert_eq!(result["provider"], "x");
    assert_eq!(result["reasoningEffort"], "low");
}

#[test]
fn model_selection_no_reasoning_effort_strips_inherited() {
    let w = TestWorld::new();
    let bus = w.reg.bus().clone();
    let sp = sp();
    let sel: StdRc<StdRefCell<ModelSelectionRef>> = StdRc::new(StdRefCell::new(ModelSelectionRef {
        current: Some(ModelSelection {
            provider: "p".into(),
            model: "m".into(),
            reasoning_effort: None,
        }),
        ..Default::default()
    }));
    let agent = w.agent("ms3");
    install_model_selection(&sp, &bus, &agent.scope, sel.clone());
    // 先组装一次以设置 assembled
    sp.assemble(&AssembleContext{
        scope: Some(agent.scope.clone()),
        session_id: None,
    })
    .unwrap();
    let payload = json!({ "provider": "x", "model": "y", "reasoningEffort": "medium" });
    let carrier = agent_carrier(&agent);
    let result = bus.waterfall(&carrier, "agent/request", payload, StdRc::new(|p| p));
    // selected.reasoningEffort 缺省 → 继承的 reasoningEffort 无条件剥离（不恢复）
    assert!(result.get("reasoningEffort").is_none());
    assert_eq!(result["provider"], "p");
}

// 供编译期使用（避免未用 import 告警）
#[allow(dead_code)]
fn _touch(agent: &Agent) -> Value {
    agent_value(agent)
}
#[allow(dead_code)]
fn _fuse(agent: &Agent) -> Value {
    fuse_agent(agent, json!({}))
}
#[allow(dead_code)]
fn _status(a: &Agent) -> AgentStatus {
    a.status.get()
}

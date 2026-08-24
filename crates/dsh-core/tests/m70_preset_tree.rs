//! M70：agent-scope preset subtree（C 段 K1——dsh-core M1 isolate 作用域落地为
//! 「组合挂载原语」）。复刻 harness `mount.ts` 的三个事实：
//! - 子树挂 agent scope（fiber 继承 scope 标签；root=1 为「未打标」，全局可见）；
//! - 挂载树随 fiber 展开（unmount 卸载整棵子树）；
//! - root-realm 泄漏守卫（preset 行把 service 发布进 ROOT realm → 可审计出泄漏）。
//!
//! 语义对齐 harness `agents/scope.ts` 的 scopeOf 标签 + filter（untagged 全局接受；
//! tagged 仅本 agent）。ScopeKey = 不透明 ScopeId，单键。注意：`on`/`provide` 需要
//! 活着的 current fiber——一律在插件 apply 内注册。
#![allow(clippy::arc_with_non_send_sync)] // Listener = Arc<dyn Fn…>（单线程纪律，与 dsh-core lib 同允）

mod common;
use common::*;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use dsh_core::*;

fn listener(n: Rc<Cell<u32>>) -> Listener {
    Arc::new(move |_c, _a, _n| {
        n.set(n.get() + 1);
        HookResult::Continue
    })
}

/// 挂载的 agent 子树内注册监听器 + 提供隔离服务；子 fiber 继承 scope；
/// root 监听器（untagged）对 agent 可见，agent 监听器对 root 不可见；
/// 卸载整棵子树展开。audit 干净。
#[test]
fn agent_subtree_isolates_hooks_and_unmount_disposes() {
    let cordis = Cordis::new();

    // root 监听器（scope 1，untagged）——全局可见
    let root_n = Rc::new(Cell::new(0));
    {
        let n = root_n.clone();
        let r0 = FnPlugin::new("root-listener", &[], move |ctx, _cfg| {
            assert_eq!(ctx.current_scope(), 1, "root plugin runs at root scope");
            ctx.on("evt", listener(n.clone())).unwrap();
            Ok(EffectOutcome::None)
        });
        cordis.plugin(r0, json!({})).unwrap();
    }

    // 挂载一个新的 agent scope
    let (scope, unmount) = cordis.mount_scope().expect("mount_scope mints");
    assert_ne!(scope, 1, "agent scope must not be the root scope");

    let agent_n = Rc::new(Cell::new(0));
    let child_ran = Rc::new(RefCell::new(false));
    let a_n = agent_n.clone();
    let cr = child_ran.clone();
    let a = FnPlugin::new("sess-a", &[], move |ctx, _cfg| {
        assert_ne!(
            ctx.current_scope(),
            1,
            "plugin under the mount runs inside the agent scope"
        );
        ctx.on("evt", listener(a_n.clone()))
            .expect("agent listener registers");
        // 服务隔离到当前 scope（agent realm；非 root）
        let s = ctx.current_scope();
        ctx.isolate("svc", s).expect("isolate sets agent realm");
        ctx.provide("svc", Arc::new(42u32)).unwrap();
        // 子 fiber（嵌套注册）继承同一 scope
        let cr2 = cr.clone();
        let child = FnPlugin::new("sess-a-child", &[], move |child_ctx, _cfg| {
            assert_eq!(
                child_ctx.current_scope(),
                s,
                "child fiber inherits the agent scope"
            );
            *cr2.borrow_mut() = true;
            Ok(EffectOutcome::None)
        });
        ctx.plugin(child, json!({}))
            .expect("child plugin registers");
        Ok(EffectOutcome::None)
    });
    let a_id = cordis.plugin(a, json!({})).expect("agent plugin registers");
    assert_eq!(cordis.fiber_state(a_id), Some(FiberState::Active));
    assert!(
        *child_ran.borrow(),
        "nested child fiber ran (deferred during parent registration)"
    );

    // root 视角不可见 agent 的隔离服务
    assert!(
        cordis.get_typed::<u32>("svc").is_none(),
        "root cannot see an agent-isolated service"
    );

    // root 分发不触发 agent 监听器
    {
        let r = FnPlugin::new("root-emitter", &[], move |ctx, _cfg| {
            assert_eq!(ctx.current_scope(), 1, "root plugin runs at root scope");
            ctx.emit("evt", vec![]);
            Ok(EffectOutcome::None)
        });
        let r_id = cordis.plugin(r, json!({})).unwrap();
        assert_eq!(cordis.fiber_state(r_id), Some(FiberState::Active));
    }
    assert_eq!(root_n.get(), 1, "root listener fired once");
    assert_eq!(
        agent_n.get(),
        0,
        "agent listener is invisible to root dispatch"
    );

    // 审计：本子树无泄漏（隔离服务在 agent realm；无 root-realm 发布）
    assert!(
        cordis.audit_subtree(scope).is_empty(),
        "isolated subtree audits clean"
    );

    // 卸载：整棵子树（含子 fiber）展开
    unmount(&cordis);
    assert!(
        cordis.fiber_state(a_id).is_none()
            || cordis.fiber_state(a_id) == Some(FiberState::Disposed),
        "agent subtree fiber disposed on unmount"
    );
}

/// harness mount.ts 的 root-realm 泄漏规则：preset 行把 service 发布进 ROOT realm
/// （未置于 isolate realm）→ 被守卫审计出。默认 provide（未 isolate）落在 root。
#[test]
fn subtree_service_published_to_root_is_detected_as_leak() {
    let cordis = Cordis::new();
    let (scope, _unmount) = cordis.mount_scope().expect("mount");
    let leaked = FnPlugin::new("leaky", &[], |ctx, _cfg| {
        // 故意不 isolate：服务落在 ROOT realm（scopes[svc2]）
        ctx.provide("svc2", Arc::new(7u32)).unwrap();
        Ok(EffectOutcome::None)
    });
    cordis.plugin(leaked, json!({})).unwrap();

    let leaks = cordis.audit_subtree(scope);
    assert!(
        leaks
            .iter()
            .any(|l| l.contains("svc2") && l.contains("root")),
        "expected a root-realm service leak, got: {leaks:?}"
    );
}

/// 两个并行的 agent scope 监听器互不可见（per-session 隔离）。
#[test]
fn two_agent_scopes_do_not_see_each_others_listeners() {
    let cordis = Cordis::new();
    let (scope_a, _ua) = cordis.mount_scope().unwrap();
    let (scope_b, _ub) = cordis.mount_scope().unwrap();
    assert_ne!(scope_a, scope_b, "each mount mints a distinct scope");

    let a_n = Rc::new(Cell::new(0));
    let a_n2 = a_n.clone();
    let a = FnPlugin::new("sess-a", &[], move |ctx, _cfg| {
        ctx.on("evt", listener(a_n2.clone())).unwrap();
        Ok(EffectOutcome::None)
    });
    cordis.plugin(a, json!({})).unwrap();

    let b_n = Rc::new(Cell::new(0));
    let b_n2 = b_n.clone();
    let b = FnPlugin::new("sess-b", &[], move |ctx, _cfg| {
        ctx.on("evt", listener(b_n2.clone())).unwrap();
        // B 的 dispatch 只看到自己的 + root；看不到 A 的
        ctx.emit("evt", vec![]);
        Ok(EffectOutcome::None)
    });
    let _b_id = cordis.plugin(b, json!({})).unwrap();

    assert_eq!(
        a_n.get(),
        0,
        "session B dispatch must not reach session A listener"
    );
    assert_eq!(
        b_n.get(),
        1,
        "session B listener fired from its own dispatch"
    );
}

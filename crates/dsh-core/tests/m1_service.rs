//! M1：Service trait / provide_service、set 所有者校验、accessor/mixin、internal 事件派发。

mod common;
use common::*;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use dsh_core::*;

struct CounterService;

impl Service for CounterService {
    fn service_name(&self) -> &'static str {
        "counter"
    }
}

/// provide_service：按 service_name 注册，依赖方可注入读取。
#[test]
fn provide_service_registers_by_name() {
    let cordis = Cordis::new();
    let provider = FnPlugin::new("provider", &[], |ctx, _cfg| {
        ctx.provide_service(Arc::new(CounterService)).unwrap();
        Ok(EffectOutcome::None)
    });
    cordis.plugin(provider, json!({})).unwrap();

    let consumer = FnPlugin::new("consumer", &["counter"], |ctx, _cfg| {
        assert!(ctx.get_typed::<CounterService>("counter").is_some());
        Ok(EffectOutcome::None)
    });
    let cid = cordis.plugin(consumer, json!({})).unwrap();
    assert_eq!(cordis.fiber_state(cid), Some(FiberState::Active));
}

/// check 谓词：false 时依赖方保持 PENDING；换 true 的提供者后加载。
#[test]
fn check_gates_dependents() {
    let cordis = Cordis::new();
    let consumer = FnPlugin::new("consumer", &["svc"], |_ctx, _cfg| Ok(EffectOutcome::None));
    let cid = cordis.plugin(consumer, json!({})).unwrap();
    assert_eq!(cordis.fiber_state(cid), Some(FiberState::Pending));

    let bad = FnPlugin::new("bad", &[], |ctx, _cfg| {
        ctx.provide_with("svc", Arc::new(1u32), Some(Box::new(|| false))).unwrap();
        Ok(EffectOutcome::None)
    });
    let bid = cordis.plugin(bad, json!({})).unwrap();
    assert_eq!(cordis.fiber_state(bid), Some(FiberState::Active));
    assert_eq!(cordis.fiber_state(cid), Some(FiberState::Pending));

    cordis.unload(bid).unwrap();
    let good = FnPlugin::new("good", &[], |ctx, _cfg| {
        ctx.provide_with("svc", Arc::new(2u32), Some(Box::new(|| true))).unwrap();
        Ok(EffectOutcome::None)
    });
    let gid = cordis.plugin(good, json!({})).unwrap();
    assert_eq!(cordis.fiber_state(cid), Some(FiberState::Active));
    let _ = gid;
}

/// set：仅提供者 fiber 可覆盖（Cordis `cannot set ... in multiple fibers`）。
#[test]
fn set_requires_owner_fiber() {
    let cordis = Cordis::new();
    let owner = FnPlugin::new("owner", &[], |ctx, _cfg| {
        ctx.provide("x", Arc::new(1u32)).unwrap();
        ctx.set("x", Arc::new(2u32)).unwrap();
        Ok(EffectOutcome::None)
    });
    cordis.plugin(owner, json!({})).unwrap();

    let thief = FnPlugin::new("thief", &["x"], |ctx, _cfg| {
        let r = ctx.set("x", Arc::new(3u32));
        assert!(matches!(r, Err(CordisError::MultipleFibers(_))));
        Ok(EffectOutcome::None)
    });
    let tid = cordis.plugin(thief, json!({})).unwrap();
    assert_eq!(cordis.fiber_state(tid), Some(FiberState::Active));

    // 值未被篡改（仍为 2）
    let reader = FnPlugin::new("reader", &["x"], |ctx, _cfg| {
        let v = ctx.get_typed::<u32>("x").expect("x present");
        assert_eq!(*v, 2);
        Ok(EffectOutcome::None)
    });
    let rid = cordis.plugin(reader, json!({})).unwrap();
    assert_eq!(cordis.fiber_state(rid), Some(FiberState::Active));
}

/// accessor：get/set 钩子、可读可写、随 fiber 卸载移除。
#[test]
fn accessor_get_set_and_lifecycle() {
    let holder: Rc<RefCell<Value>> = Rc::new(RefCell::new(json!({"v": 1})));
    let holder2 = holder.clone();
    let cordis = Cordis::new();
    let plugin = FnPlugin::new("acc", &[], move |ctx, _cfg| {
        let h_get = holder2.clone();
        let h_set = holder2.clone();
        ctx.accessor(
            "computed",
            Rc::new(move |_ctx| Some(h_get.borrow().clone())),
            Some(Rc::new(move |_ctx, v| {
                *h_set.borrow_mut() = v;
                true
            })),
        )
        .unwrap();
        Ok(EffectOutcome::None)
    });
    let fid = cordis.plugin(plugin, json!({})).unwrap();

    assert_eq!(cordis.get_value("computed"), Some(json!({"v": 1})));
    cordis.set("computed", Arc::new(json!({"v": 2}))).unwrap();
    assert_eq!(cordis.get_value("computed"), Some(json!({"v": 2})));

    cordis.unload(fid).unwrap();
    assert_eq!(cordis.get_value("computed"), None);
}

/// accessor 与同名服务冲突报错。
#[test]
fn accessor_conflicts_with_existing_property() {
    let cordis = Cordis::new();
    let plugin = FnPlugin::new("acc2", &[], |ctx, _cfg| {
        ctx.provide("dup", Arc::new(json!(1))).unwrap();
        let r = ctx.accessor("dup", Rc::new(|_| None), None);
        assert!(matches!(r, Err(CordisError::AlreadyRegistered(_))));
        Ok(EffectOutcome::None)
    });
    let fid = cordis.plugin(plugin, json!({})).unwrap();
    assert_eq!(cordis.fiber_state(fid), Some(FiberState::Active));
}

/// mixin：把 JSON 服务的成员转发为访问器。
#[test]
fn mixin_forwards_service_members() {
    let cordis = Cordis::new();
    let plugin = FnPlugin::new("cfg", &[], |ctx, _cfg| {
        ctx.provide("config", Arc::new(json!({"a": 1, "b": 2}))).unwrap();
        ctx.mixin("config", &["a", "b"]).unwrap();
        Ok(EffectOutcome::None)
    });
    let fid = cordis.plugin(plugin, json!({})).unwrap();
    assert_eq!(cordis.get_value("a"), Some(json!(1)));
    assert_eq!(cordis.get_value("b"), Some(json!(2)));

    cordis.unload(fid).unwrap();
    assert_eq!(cordis.get_value("a"), None);
}

/// internal/plugin 与 internal/status 派发到钩子。
#[test]
fn internal_plugin_and_status_events_dispatch() {
    let log = log();
    let log2 = log.clone();
    let cordis = Cordis::new();
    let host = FnPlugin::new("host", &[], move |ctx, _cfg| {
        let l = log2.clone();
        ctx.on(
            "internal/plugin",
            make_listener(move |_ctx, args, _next| {
                let name = args.first().and_then(|a| a.as_str()).unwrap_or("");
                push(&l, format!("plugin:{name}"));
                HookResult::Continue
            }),
        )
        .unwrap();
        let l = log2.clone();
        ctx.on(
            "internal/status",
            make_listener(move |_ctx, args, _next| {
                let name = args.first().and_then(|a| a.as_str()).unwrap_or("");
                let to = args.get(2).and_then(|a| a.as_str()).unwrap_or("");
                push(&l, format!("status:{name}:{to}"));
                HookResult::Continue
            }),
        )
        .unwrap();
        Ok(EffectOutcome::None)
    });
    cordis.plugin(host, json!({})).unwrap();

    let plain = FnPlugin::new("plain", &[], |_ctx, _cfg| Ok(EffectOutcome::None));
    let fid = cordis.plugin(plain, json!({})).unwrap();
    assert_eq!(cordis.fiber_state(fid), Some(FiberState::Active));

    let s = snapshot(&log);
    assert!(s.iter().any(|e| e == "plugin:plain"), "log: {s:?}");
    assert!(s.iter().any(|e| e == "status:plain:Active"), "log: {s:?}");
}

/// M43：`internal/get` waterfall 拦截——监听器返回替代值（短路 inner 查表）。
#[test]
fn internal_get_intercept_overrides_service_read() {
    let log = log();
    let log2 = log.clone();
    let cordis = Cordis::new();

    // 提供 Value 服务 "cfg"
    let provider = FnPlugin::new("provider", &[], |ctx, _cfg| {
        ctx.provide("cfg", Arc::new(json!({"k": "real"}))).unwrap();
        Ok(EffectOutcome::None)
    });
    cordis.plugin(provider, json!({})).unwrap();

    // 拦截器：internal/get 命中 "cfg" → 返回替代值（不调用 next）
    let host = FnPlugin::new("interceptor", &[], move |ctx, _cfg| {
        let l = log2.clone();
        ctx.on_with(
            "internal/get",
            make_listener(move |_ctx, args, _next| {
                let name = args.first().and_then(|a| a.as_str()).unwrap_or("");
                push(&l, format!("get:{name}"));
                if name == "cfg" {
                    HookResult::Returned(Some(json!({"k": "intercepted"})))
                } else {
                    HookResult::Continue
                }
            }),
            true, // global：不受作用域过滤
            false,
        )
        .unwrap();
        Ok(EffectOutcome::None)
    });
    cordis.plugin(host, json!({})).unwrap();

    // get_value("cfg") → 拦截器返回替代值
    let v = cordis.get_value("cfg").expect("cfg readable");
    assert_eq!(v, json!({"k": "intercepted"}), "internal/get intercepted read");
    assert_eq!(snapshot(&log), vec!["get:cfg"]);

    // 未拦截的服务名 → 走 inner 查表（None）
    let none = cordis.get_value("nope");
    assert!(none.is_none(), "{none:?}");
}

/// M43：`internal/set` waterfall 拦截——监听器 veto 写入（不调用 next）。
#[test]
fn internal_set_intercept_vetoes_write() {
    let cordis = Cordis::new();

    // 拦截器先行：internal/set 命中 "cfg" → veto（返回 false，不调用 next）
    let host = FnPlugin::new("veto", &[], |ctx, _cfg| {
        ctx.on_with(
            "internal/set",
            make_listener(move |_ctx, args, _next| {
                let name = args.first().and_then(|a| a.as_str()).unwrap_or("");
                if name == "cfg" {
                    HookResult::Returned(Some(json!(false))) // veto：不写
                } else {
                    HookResult::Continue
                }
            }),
            true,
            false,
        )
        .unwrap();
        Ok(EffectOutcome::None)
    });
    cordis.plugin(host, json!({})).unwrap();

    // 提供者：apply 内 set_value（对齐 Cordis `ctx.set` 的所有者校验）
    let provider2 = FnPlugin::new("provider2", &[], |ctx, _cfg| {
        ctx.provide("cfg", Arc::new(json!({"k": 1}))).unwrap();
        let result = ctx.set_value("cfg", json!({"k": 99}));
        assert!(result.is_err(), "vetoed write returns error");
        Ok(EffectOutcome::None)
    });
    cordis.plugin(provider2, json!({})).unwrap();

    assert_eq!(cordis.get_value("cfg"), Some(json!({"k": 1})), "value unchanged");
}

/// M43：`internal/set` 未拦截时正常写入（next 委托 inner；提供者 fiber 内 set）。
#[test]
fn internal_set_passthrough_writes() {
    let cordis = Cordis::new();
    let provider = FnPlugin::new("provider", &[], |ctx, _cfg| {
        ctx.provide("cfg", Arc::new(json!({"k": 1}))).unwrap();
        // 提供者 fiber 内写（对齐 Cordis `ctx.set` 的所有者校验）
        ctx.set_value("cfg", json!({"k": 42})).unwrap();
        Ok(EffectOutcome::None)
    });
    cordis.plugin(provider, json!({})).unwrap();

    assert_eq!(cordis.get_value("cfg"), Some(json!({"k": 42})));
}

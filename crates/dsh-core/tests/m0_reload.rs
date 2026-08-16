//! §5.3 场景 2：依赖驱动重载 —— inject 门控、提供/撤销、重载、重复注册。

mod common;
use common::*;

use std::rc::Rc;
use std::sync::Arc;

use dsh_core::*;

/// A inject "b"；无 b 时 A 保持 Pending，B 提供 b 后 A 自动加载。
#[test]
fn dependency_gates_fiber_load() {
    let log = log();
    let log2 = log.clone();
    let cordis = Cordis::new();

    // A：依赖 b
    let a_log = log2.clone();
    let a = FnPlugin::new("a", &["b"], move |_ctx, _cfg| {
        push(&a_log, "apply:a");
        Ok(EffectOutcome::None)
    });
    let a_fid = cordis.plugin(a, json!({})).unwrap();
    assert_eq!(cordis.fiber_state(a_fid), Some(FiberState::Pending));
    assert_eq!(cordis.fiber_epoch(a_fid), Some(None)); // INACTIVE

    // B：提供 b
    let b_log = log2.clone();
    let b = FnPlugin::new("b", &[], move |ctx, _cfg| {
        push(&b_log, "apply:b");
        ctx.provide("b", Arc::new("svc")).unwrap();
        Ok(EffectOutcome::None)
    });
    let b_fid = cordis.plugin(b, json!({})).unwrap();
    assert_eq!(cordis.fiber_state(b_fid), Some(FiberState::Active));

    // A 因 b 出现而加载
    assert_eq!(cordis.fiber_state(a_fid), Some(FiberState::Active));
    assert_eq!(cordis.fiber_epoch(a_fid), Some(Some(":2".to_string())));
    assert_eq!(snapshot(&log), vec!["apply:b", "apply:a"]);
}

/// 撤销提供（卸载 provider）→ 依赖方 A 卸载，epoch 回到 INACTIVE。
#[test]
fn unprovide_unloads_dependents() {
    let log = log();
    let log2 = log.clone();
    let cordis = Cordis::new();

    let a_log = log2.clone();
    let a = FnPlugin::new("a", &["b"], move |ctx, _cfg| {
        push(&a_log, "apply:a");
        let l = a_log.clone();
        ctx.effect(
            "cleanup",
            Box::new(move |_ctx| {
                Ok(EffectOutcome::One(Rc::new(move |_ctx| {
                    push(&l, "dis:a");
                })))
            }),
        )
        .unwrap();
        Ok(EffectOutcome::None)
    });
    let a_fid = cordis.plugin(a, json!({})).unwrap();

    let b_log = log2.clone();
    let b = FnPlugin::new("b", &[], move |ctx, _cfg| {
        push(&b_log, "apply:b");
        ctx.provide("b", Arc::new("svc")).unwrap();
        Ok(EffectOutcome::None)
    });
    let b_fid = cordis.plugin(b, json!({})).unwrap();
    assert_eq!(cordis.fiber_state(a_fid), Some(FiberState::Active));

    // 卸载 B → b 消失 → A 卸载（disposers 逆序运行）
    cordis.unload(b_fid).unwrap();
    assert_eq!(cordis.fiber_state(a_fid), Some(FiberState::Pending));
    assert_eq!(cordis.fiber_epoch(a_fid), Some(None));

    let log_now = snapshot(&log);
    let last = log_now.last().map(|s| s.as_str());
    assert_eq!(last, Some("dis:a"));
}

/// 重新提供 → A 重载（apply 再次运行）。
#[test]
fn reprovide_reloads_dependents() {
    let log = log();
    let log2 = log.clone();
    let cordis = Cordis::new();

    let a_log = log2.clone();
    let a = FnPlugin::new("a", &["b"], move |_ctx, _cfg| {
        push(&a_log, "apply:a");
        Ok(EffectOutcome::None)
    });
    let a_fid = cordis.plugin(a, json!({})).unwrap();

    let b1 = FnPlugin::new("b", &[], move |ctx, _cfg| {
        ctx.provide("b", Arc::new("one")).unwrap();
        Ok(EffectOutcome::None)
    });
    let b1_fid = cordis.plugin(b1, json!({})).unwrap();
    assert_eq!(cordis.fiber_state(a_fid), Some(FiberState::Active));

    // 撤销 b
    cordis.unload(b1_fid).unwrap();
    assert_eq!(cordis.fiber_state(a_fid), Some(FiberState::Pending));

    // 重新提供（新实现）
    let b2 = FnPlugin::new("b2", &[], move |ctx, _cfg| {
        ctx.provide("b", Arc::new("two")).unwrap();
        Ok(EffectOutcome::None)
    });
    let b2_fid = cordis.plugin(b2, json!({})).unwrap();
    assert_eq!(cordis.fiber_state(b2_fid), Some(FiberState::Active));
    assert_eq!(cordis.fiber_state(a_fid), Some(FiberState::Active));
    assert_eq!(snapshot(&log), vec!["apply:a", "apply:a"]);
}

/// 无依赖插件立即加载。
#[test]
fn plugin_without_inject_loads_immediately() {
    let log = log();
    let log2 = log.clone();
    let cordis = Cordis::new();
    let plugin = FnPlugin::new("p", &[], move |_ctx, _cfg| {
        push(&log2, "apply:p");
        Ok(EffectOutcome::None)
    });
    let fid = cordis.plugin(plugin, json!({})).unwrap();
    assert_eq!(cordis.fiber_state(fid), Some(FiberState::Active));
    assert_eq!(snapshot(&log), vec!["apply:p"]);
}

/// 依赖的服务可被 get_typed 读取。
#[test]
fn injected_service_is_readable_typed() {
    let cordis = Cordis::new();

    let provider = FnPlugin::new("provider", &[], |ctx, _cfg| {
        ctx.provide("counter", Arc::new(42u32)).unwrap();
        Ok(EffectOutcome::None)
    });
    let _pid = cordis.plugin(provider, json!({})).unwrap();

    let consumer = FnPlugin::new("consumer", &["counter"], |ctx, _cfg| {
        let v = ctx.get_typed::<u32>("counter").expect("counter present");
        assert_eq!(*v, 42);
        Ok(EffectOutcome::None)
    });
    let cid = cordis.plugin(consumer, json!({})).unwrap();
    assert_eq!(cordis.fiber_state(cid), Some(FiberState::Active));
}

/// 同名服务重复注册报错（Cordis `service "<name>" has been registered`）。
#[test]
fn duplicate_provide_fails() {
    let cordis = Cordis::new();
    let dup = FnPlugin::new("dup", &[], |ctx, _cfg| {
        ctx.provide("x", Arc::new(1u32)).unwrap();
        let r = ctx.provide("x", Arc::new(2u32));
        assert!(matches!(r, Err(CordisError::AlreadyRegistered(_))));
        Ok(EffectOutcome::None)
    });
    let fid = cordis.plugin(dup, json!({})).unwrap();
    assert_eq!(cordis.fiber_state(fid), Some(FiberState::Active));
}

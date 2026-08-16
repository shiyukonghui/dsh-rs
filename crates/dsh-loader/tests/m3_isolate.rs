//! §5.3 场景 6：isolate（LocalRealm / GlobalRealm / GC）+ intercept entry 选项。
#![allow(clippy::arc_with_non_send_sync)]

mod common;
use common::*;

use std::sync::Arc;

use dsh_core::*;
use dsh_loader::*;

fn options(id: &str, name: &str) -> EntryOptions {
    EntryOptions::new(id, name)
}

/// 提供 svc 的插件（值可区分来源）。
fn provide_svc(name: &'static str, value: &'static str) -> Arc<dyn Plugin> {
    Arc::new(FnPlugin::new(name, &[], move |ctx, _cfg| {
        ctx.provide("svc", Arc::new(value.to_string())).unwrap();
        Ok(EffectOutcome::None)
    }))
}

/// 注入 svc 并断言其值的插件。
fn expect_svc(name: &'static str, expected: &'static str) -> Arc<dyn Plugin> {
    let expected = expected.to_string();
    Arc::new(FnPlugin::new(name, &["svc"], move |ctx, _cfg| {
        let v = ctx
            .get_typed::<String>("svc")
            .expect("svc present in this scope");
        assert_eq!(*v, expected);
        Ok(EffectOutcome::None)
    }))
}

/// LocalRealm：入口本地作用域内的 svc 对根作用域不可见。
#[test]
fn local_realm_isolates_service() {
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();

    // A：isolate svc:true，在本地 realm 提供 svc
    loader.register_plugin("pa", provide_svc("pa", "A-svc"));
    let mut a = options("a", "pa");
    a.isolate.insert("svc".into(), json!(true));
    loader.create(a).unwrap();

    // B：根作用域注入 svc → 看不到 A 的本地 svc → Pending
    loader.register_plugin("pb", expect_svc("pb", "ROOT-svc"));
    loader.create(options("b", "pb")).unwrap();
    let bid = loader.fiber("b").expect("B fiber created");
    assert_eq!(cordis.fiber_state(bid), Some(FiberState::Pending));

    // C：根作用域提供 svc → B 加载并看到 ROOT-svc
    loader.register_plugin("pc", provide_svc("pc", "ROOT-svc"));
    loader.create(options("c", "pc")).unwrap();
    assert_eq!(cordis.fiber_state(bid), Some(FiberState::Active));

    // A 的本地 realm 与根作用域各自独立：移除 C → B 回到 Pending，A 不受影响
    loader.remove("c").unwrap();
    assert_eq!(cordis.fiber_state(bid), Some(FiberState::Pending));
    assert!(loader.fiber("a").is_some());
}

/// GlobalRealm：同 label 的入口共享作用域。
#[test]
fn global_realm_shares_scope() {
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();

    loader.register_plugin("ga", provide_svc("ga", "SHARED-svc"));
    let mut a = options("a", "ga");
    a.isolate.insert("svc".into(), json!("shared"));
    loader.create(a).unwrap();

    loader.register_plugin("gb", expect_svc("gb", "SHARED-svc"));
    let mut b = options("b", "gb");
    b.isolate.insert("svc".into(), json!("shared"));
    loader.create(b).unwrap();
    let bfid = loader.fiber("b").expect("B sees A's svc in shared realm");
    assert_eq!(cordis.fiber_state(bfid), Some(FiberState::Active));
}

/// realm GC：移除入口后本地 realm 清理、无引用的全局 realm 清理。
#[test]
fn realm_gc_after_remove() {
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();

    loader.register_plugin("ga", provide_svc("ga", "x"));
    let mut a = options("a", "ga");
    a.isolate.insert("svc".into(), json!(true));
    loader.create(a).unwrap();
    assert!(!loader.state.borrow().local_realms.is_empty());

    loader.remove("a").unwrap();
    assert!(loader.state.borrow().local_realms.is_empty());

    // 全局 realm：创建 → 移除 → 无引用则清理
    let mut b = options("b", "ga");
    b.isolate.insert("svc".into(), json!("shared"));
    loader.create(b).unwrap();
    assert!(loader.state.borrow().global_realms.contains_key("shared"));
    loader.remove("b").unwrap();
    assert!(!loader.state.borrow().global_realms.contains_key("shared"));
}

/// intercept entry 选项：注入到 fiber 的 intercept，resolve_config 可见。
#[test]
fn intercept_entry_option_merges() {
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();

    let plugin = Arc::new(FnPlugin::new("pi", &[], |ctx, _cfg| {
        assert_eq!(ctx.resolve_config("srv", None, None), json!({"a": 1, "b": 2}));
        Ok(EffectOutcome::None)
    }));
    loader.register_plugin("pi", plugin);
    let mut e = options("e", "pi");
    e.intercept.insert("srv".into(), json!({"a": 1, "b": 2}));
    loader.create(e).unwrap();
    assert!(loader.fiber("e").is_some());
}

//! B3 HMR 模块热更：`replace_plugin` 身份换代 → 受影响 entry reload 新实现；同实现幂等；
//! 依赖方经 fiber uid/epoch 自动重活（externals 同构）。
#![allow(clippy::arc_with_non_send_sync)]

mod common;
use common::*;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use dsh_core::*;
use dsh_loader::*;

/// 记录版本号的插件（apply 逐个记录）。
fn ver_plugin(name: &'static str, v: &'static str, log: &Rc<RefCell<Vec<String>>>) -> Arc<dyn Plugin> {
    let log = log.clone();
    Arc::new(FnPlugin::new(name, &[], move |_ctx, _cfg| {
        push(&log, format!("apply:{name}:{v}"));
        Ok(EffectOutcome::None)
    }))
}

/// T1：`replace_plugin` 换代 → 以旧身份加载的 entry 自动 reload 新实现（entry 保真、identity 更新）。
#[test]
fn replace_plugin_reloads_entry_with_new_impl() {
    let log = log();
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    loader.register_plugin("p", ver_plugin("p", "v1", &log));
    let eid = loader.create(EntryOptions::new("e", "p")).unwrap();
    assert_eq!(snapshot(&log), vec!["apply:p:v1"]);
    let id1 = loader.entry_identity(&eid).expect("identity recorded");

    let n = loader.replace_plugin("p", ver_plugin("p", "v2", &log)).unwrap();
    assert_eq!(n, 1, "one stale entry reloaded");
    assert_eq!(snapshot(&log), vec!["apply:p:v1", "apply:p:v2"]);
    assert_ne!(loader.entry_identity(&eid).unwrap(), id1, "entry on new impl");
    assert_eq!(cordis.fiber_state(loader.fiber(&eid).unwrap()), Some(FiberState::Active));
}

/// T2：换代提供者实现 → 依赖方经 epoch 自动重活（externals→全重载 同构）。
#[test]
fn replace_plugin_revives_dependency_consumer() {
    let log = log();
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();

    let provider = FnPlugin::new("svc", &[], |ctx, _cfg| {
        ctx.provide("svc", Arc::new(json!("v1")))?;
        Ok(EffectOutcome::None)
    });
    loader.register_plugin("svc", Arc::new(provider));
    let consumer = FnPlugin::new("consumer", &["svc"], {
        let log = log.clone();
        move |_ctx, _cfg| {
            push(&log, "consumer-applied");
            Ok(EffectOutcome::None)
        }
    });
    loader.register_plugin("consumer", Arc::new(consumer));
    loader.create(EntryOptions::new("s", "svc")).unwrap();
    loader.create(EntryOptions::new("c", "consumer")).unwrap();
    assert_eq!(cordis.fiber_state(loader.fiber("c").unwrap()), Some(FiberState::Active));
    assert_eq!(snapshot(&log), vec!["consumer-applied"]);

    // 换代 provider 实现（新 apply 仍 provide svc）
    let provider2 = FnPlugin::new("svc", &[], |ctx, _cfg| {
        ctx.provide("svc", Arc::new(json!("v2")))?;
        Ok(EffectOutcome::None)
    });
    let n = loader.replace_plugin("svc", Arc::new(provider2)).unwrap();
    assert_eq!(n, 1, "provider entry reloaded");

    // 依赖方经 uid/epoch 变迁自动重活（重新 apply）
    assert_eq!(cordis.fiber_state(loader.fiber("c").unwrap()), Some(FiberState::Active));
    assert_eq!(
        snapshot(&log),
        vec!["consumer-applied", "consumer-applied"],
        "consumer re-applied after provider impl replace"
    );
}

/// T3：`replace_plugin` 同实现（同一 Arc）→ 幂等：Ok(0)、无 generation 递增、无 reload。
#[test]
fn replace_plugin_same_impl_is_noop() {
    let log = log();
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    let p = ver_plugin("p", "v1", &log);
    loader.register_plugin("p", p.clone());
    loader.create(EntryOptions::new("e", "p")).unwrap();
    assert_eq!(snapshot(&log), vec!["apply:p:v1"]);
    let g1 = loader.plugin_generation("p").unwrap();

    let n = loader.replace_plugin("p", p.clone()).unwrap();
    assert_eq!(n, 0, "same impl → no reload");
    assert_eq!(loader.plugin_generation("p").unwrap(), g1, "no generation bump");
    assert_eq!(snapshot(&log), vec!["apply:p:v1"], "no reload");
}

/// T4：`replace_plugin` 返回受影响数；`stale_entry_ids` 观测换代前后 stale 集。
#[test]
fn replace_plugin_counts_stale_entries() {
    let log = log();
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    loader.register_plugin("p", ver_plugin("p", "v1", &log));
    loader.create(EntryOptions::new("e1", "p")).unwrap();
    loader.create(EntryOptions::new("e2", "p")).unwrap();
    assert_eq!(loader.stale_entry_ids("p").len(), 0, "entries on current impl → no stale");

    let n = loader.replace_plugin("p", ver_plugin("p", "v2", &log)).unwrap();
    assert_eq!(n, 2, "two entries reloaded");
    assert_eq!(loader.stale_entry_ids("p").len(), 0, "all reloaded → no stale now");
    assert_eq!(
        snapshot(&log),
        vec!["apply:p:v1", "apply:p:v1", "apply:p:v2", "apply:p:v2"]
    );
}

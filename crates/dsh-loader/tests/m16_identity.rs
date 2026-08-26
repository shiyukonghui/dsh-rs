//! A1 插件身份键（对齐 harness「回调为身份」）：同名同实现=同身份、同名新实现=新身份。
#![allow(clippy::arc_with_non_send_sync)]

mod common;
use common::*;

use std::sync::Arc;

use dsh_core::*;
use dsh_loader::*;

/// 场景 A1-a（T4）：同名**同一实现**（同一 Arc）重复注册 → 幂等：身份不变、generation 不变。
#[test]
fn same_impl_re_register_is_idempotent_same_identity() {
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    let p = Arc::new(FnPlugin::noop("svc"));

    loader.register_plugin("svc", p.clone());
    let id1 = loader.plugin_identity("svc").expect("registered");
    let g1 = loader.plugin_generation("svc").expect("generation");

    loader.register_plugin("svc", p.clone()); // 同一 Arc
    let id2 = loader.plugin_identity("svc").expect("registered");
    let g2 = loader.plugin_generation("svc").expect("generation");

    assert_eq!(id1, id2, "same impl must keep same identity");
    assert_eq!(g1, g2, "same impl must keep same generation");
}

/// 场景 A1-b（T3）：同名**新实现**（不同 Arc）注册 → 新身份、generation 递增（harness re-import）。
#[test]
fn new_impl_re_register_is_new_identity() {
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    let p1 = Arc::new(FnPlugin::noop("svc"));
    let p2 = Arc::new(FnPlugin::noop("svc")); // 不同实现实例

    loader.register_plugin("svc", p1);
    let id1 = loader.plugin_identity("svc").expect("registered");
    let g1 = loader.plugin_generation("svc").expect("generation");

    loader.register_plugin("svc", p2); // 同名新实现
    let id2 = loader.plugin_identity("svc").expect("registered");
    let g2 = loader.plugin_generation("svc").expect("generation");

    assert_ne!(id1, id2, "new impl must get a new identity");
    assert!(g2 > g1, "generation must increment on new impl");
}

/// 场景 A1-c：load 时 Entry 记录解析所用的身份；换实现重挂载后 Entry 记录新身份。
#[test]
fn entry_records_identity_of_resolved_impl() {
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    let p1 = Arc::new(FnPlugin::noop("svc"));
    loader.register_plugin("svc", p1.clone());
    let id_at_register = loader.plugin_identity("svc").unwrap();

    let eid = loader.create(EntryOptions::new("s", "svc")).unwrap();
    let entry_id = loader.entry_identity(&eid).expect("entry records resolved identity");
    assert_eq!(entry_id, id_at_register);

    // 同名新实现 + remove + recreate → entry 记录新身份
    let p2 = Arc::new(FnPlugin::noop("svc"));
    loader.register_plugin("svc", p2);
    let id2 = loader.plugin_identity("svc").unwrap();
    assert_ne!(entry_id, id2);

    loader.remove(&eid).unwrap();
    let eid2 = loader.create(EntryOptions::new("s", "svc")).unwrap();
    assert_eq!(loader.entry_identity(&eid2).unwrap(), id2);
}

/// 场景 A1-d：未知 name 无身份（load_plugin 仍按既有 fail-loud 报 unknown plugin）。
#[test]
fn unknown_plugin_has_no_identity() {
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    assert!(loader.plugin_identity("ghost").is_none());
    assert!(loader.plugin_generation("ghost").is_none());
}

//! beyond 目标 Phase HMR：宿主侧插件模块热更生命周期（e2e 集成测试）。
//!
//! 锁定 D-162 契约：`Loader::sync_plugin(name, PluginEvent)` 的
//! Register（幂等/换代）/ Replace（hot-swap）/ Delete（保留但 inert，可复活）。
//! 覆盖 A1 身份语义（同 Arc 幂等、新 Arc 换代、generation 递增）与 case-4 合法性
//! （删除后 entry 不自禁用、无 `disable:` 写回；再注册可复活）。
#![allow(clippy::arc_with_non_send_sync)]

mod common;
use common::*;

use std::sync::{Arc, Mutex};

use dsh_core::*;
use dsh_loader::*;

/// 记录 apply 版本标记的插件（每次 apply 推入 tag）。
fn version_plugin(name: &'static str, tag: &'static str, seen: Arc<Mutex<Vec<String>>>) -> Arc<dyn Plugin> {
    let tag = tag.to_string();
    Arc::new(FnPlugin::new(name, &[], move |_ctx, _cfg| {
        seen.lock().unwrap().push(tag.clone());
        Ok(EffectOutcome::None)
    }))
}

/// e2e：Register v1 → 同 Arc 幂等 → Replace v2 → Delete（保留但 inert）→ Register v3 复活。
#[test]
fn host_plugin_hmr_lifecycle() {
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    let v1 = version_plugin("p", "v1", seen.clone());
    let v2 = version_plugin("p", "v2", seen.clone());
    let v3 = version_plugin("p", "v3", seen.clone());

    // 1) Register v1 → create entry a → Active，entry 解析到 v1 身份
    let out = loader.sync_plugin("p", PluginEvent::Register(v1.clone())).unwrap();
    assert!(out.reloaded.is_empty() && out.disposed == 0 && out.retained.is_empty());
    let id1 = loader.plugin_identity("p").expect("registered");
    assert_eq!(loader.plugin_generation("p"), Some(1));
    loader.create(EntryOptions::new("a", "p")).unwrap();
    let af = loader.fiber("a").expect("entry fiber");
    assert_eq!(cordis.fiber_state(af), Some(FiberState::Active));
    assert_eq!(loader.entry_identity("a"), Some(id1.clone()));

    // 2) 同 Arc 再 Register → 幂等：无 reload、身份/generation 不变
    let out = loader.sync_plugin("p", PluginEvent::Register(v1.clone())).unwrap();
    assert!(out.reloaded.is_empty(), "same Arc register is idempotent: nothing reloaded");
    assert_eq!(loader.plugin_generation("p"), Some(1));
    assert_eq!(loader.entry_identity("a"), Some(id1.clone()));

    // 3) Replace v2 → 换代（新身份、generation=2）+ entry a reload 新实现
    let out = loader.sync_plugin("p", PluginEvent::Replace(v2.clone())).unwrap();
    assert_eq!(out.reloaded, vec!["a".to_string()]);
    let id2 = loader.plugin_identity("p").expect("registered");
    assert!(id1 != id2, "new Arc = new identity");
    assert_eq!(loader.plugin_generation("p"), Some(2));
    assert_eq!(loader.entry_identity("a"), Some(id2.clone()), "entry reloaded to new identity");
    let af = loader.fiber("a").expect("entry fiber after replace");
    assert_eq!(cordis.fiber_state(af), Some(FiberState::Active));

    // 4) Delete → fiber 逝（Disposed）；entry 保留但 inert（不自禁用、无 disable 写回）
    let out = loader.sync_plugin("p", PluginEvent::Delete).unwrap();
    assert_eq!(out.disposed, 1, "one surviving fiber disposed");
    assert_eq!(out.retained, vec!["a".to_string()]);
    let disposed_fid = loader.fiber("a").expect("entry keeps fiber ref");
    assert_eq!(
        cordis.fiber_state(disposed_fid),
        Some(FiberState::Disposed),
        "surviving fiber unloaded"
    );
    assert!(loader.plugin_identity("p").is_none(), "module unregistered");
    let a = loader
        .entry_options()
        .into_iter()
        .find(|e| e.id == "a")
        .expect("entry retained");
    assert_eq!(a.name, "p");
    assert!(!a.disabled, "deleted module → entry NOT disabled (case-4 legal)");
    let writes = loader.take_writes();
    assert!(
        !writes.iter().any(|w| w.starts_with("disable:")),
        "no disable write-back after Delete"
    );

    // 5) Register v3 复活 → entry a 重挂载新实现（新 fiber）
    let out = loader.sync_plugin("p", PluginEvent::Register(v3.clone())).unwrap();
    assert_eq!(out.reloaded, vec!["a".to_string()], "revived entry reloaded with v3");
    let id3 = loader.plugin_identity("p").expect("registered");
    assert!(id2 != id3, "v3 = yet another identity");
    // Delete 已清空记录 → re-register 是全新 lineage（generation 从 1 起，新身份 token）
    assert_eq!(loader.plugin_generation("p"), Some(1), "delete cleared record; re-register = fresh lineage");
    assert_eq!(loader.entry_identity("a"), Some(id3.clone()));
    let revived_fid = loader.fiber("a").expect("revived fiber");
    assert!(revived_fid != disposed_fid, "revive starts a NEW fiber");
    assert_eq!(cordis.fiber_state(revived_fid), Some(FiberState::Active));

    // 各版本实现各 apply 一次
    let mut tags = seen.lock().unwrap().clone();
    tags.sort();
    assert_eq!(
        tags,
        vec!["v1".to_string(), "v2".to_string(), "v3".to_string()],
        "v1 (create), v2 (replace), v3 (revive) each applied once"
    );
}

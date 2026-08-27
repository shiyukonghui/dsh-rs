//! A1 收口（case-4 验证）：插件从 registry 删除（`remove_plugin` = cordis `registry.delete`）→
//! 其存活 fiber 被 dispose；该 fiber 的 dispose 经 seven_case case-4（模块消失）判为**合法**——
//! entry **不**落 disabled、**无** `disable:` 写回。对照：插件仍注册时 self-dispose → case-7
//! entry 落 disabled（既有语义复锁）。
#![allow(clippy::arc_with_non_send_sync)]

mod common;
use common::*;

use std::sync::Arc;

use dsh_core::*;
use dsh_loader::*;

/// A1（T1，红核心）：`remove_plugin("p")` → 移除 core+loader 记录、dispose 存活 fiber、
/// entry 不自禁用（case-4 合法路径）、无 `disable:` 写回。
#[test]
fn remove_plugin_disposes_and_does_not_disable_entry() {
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    loader.register_plugin("p", Arc::new(FnPlugin::noop("p")));
    loader.create(EntryOptions::new("a", "p")).unwrap();
    assert_eq!(loader.entry_identity("a"), loader.plugin_identity("p"));

    // 记录仍在时：一条 disable 也不该有（对照组由 T2 覆盖）
    let n = loader.remove_plugin("p").unwrap();
    assert_eq!(n, 1, "one live fiber disposed");

    // case-4 合法：entry 不自禁用、无 disable 写回
    let opts = loader.entry_options();
    let a = opts.iter().find(|e| e.id == "a").expect("entry a kept");
    assert!(!a.disabled, "module deleted → self-dispose is legal, entry NOT disabled");
    assert!(
        !loader.take_writes().iter().any(|w| w.contains("disable:a")),
        "no disable write for deleted module's entry"
    );
    // 记录已移除
    assert!(loader.plugin_identity("p").is_none());
    assert!(loader.plugin_generation("p").is_none());
    let _ = &cordis;
}

/// A1（T2，对照）：插件仍注册时 self-dispose → seven_case case-7 → entry 落 disabled + disable 写回。
#[test]
fn self_dispose_while_registered_disables_entry() {
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    loader.register_plugin("p", Arc::new(FnPlugin::noop("p")));
    loader.create(EntryOptions::new("a", "p")).unwrap();

    // 插件仍注册 → 外部触发该 fiber dispose（等价 fiber 自处置）
    let fid = loader.fiber("a").expect("a fiber");
    cordis.unload(fid).unwrap();

    let opts = loader.entry_options();
    let a = opts.iter().find(|e| e.id == "a").expect("entry a kept");
    assert!(
        a.disabled,
        "self-dispose while plugin registered → entry disabled (case-7)"
    );
    assert!(
        loader.take_writes().iter().any(|w| w.contains("disable:a")),
        "disable write recorded"
    );
}

/// A1（T3）：`remove_plugin` 未注册名 → Ok(0) 幂等、无副作用。
#[test]
fn remove_plugin_unknown_is_noop() {
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    let n = loader.remove_plugin("ghost").unwrap();
    assert_eq!(n, 0);
    assert!(loader.take_writes().is_empty());
    let _ = &cordis;
}

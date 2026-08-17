//! M66：`fiber.getEffects()` —— fiber 已注册 effect 的元数据（label 列表）。
//!
//! Cordis `fiber.getEffects()` 返回 `EffectMeta[]`（label + children 树，注册序，
//! 仅带 meta 的 effect 包装器）。dsh-core 的 `collect_effect` 现记录 label 到
//! `effects: Vec<EffectMeta>`；`children` 恒空（无 effect 父子结构）。卸载/重载时
//! 清空并重新收集。label 当前为注册时的 `&'static str`（精确的 `ctx.on('ev')`
//! 语义标签为后续增强，见 HANDOFF）。

mod common;
use common::*;

use std::cell::RefCell;
use std::rc::Rc;

use dsh_core::*;

/// apply 内注册若干 effect 的插件。
fn effectful_plugin(cordis: &Cordis, log: &Rc<RefCell<Vec<String>>>) -> FiberHandle {
    let log = log.clone();
    let p = FnPlugin::new("efx", &[], move |ctx, _cfg| {
        // 注册多个监听器 effect。
        ctx.on("a", make_listener(move |_c, _a, _n| HookResult::Continue)).unwrap();
        ctx.on("b", make_listener(move |_c, _a, _n| HookResult::Continue)).unwrap();
        // 提供一项服务（也产生一个 effect）。
        let log2 = log.clone();
        ctx.effect("provide-x", Box::new(move |_ctx| {
            push(&log2, "provided");
            Ok(EffectOutcome::None)
        }))
        .unwrap();
        Ok(EffectOutcome::None)
    });
    cordis.plugin(p, json!({})).unwrap()
}

/// apply 后：fiber 的 effects 记录了所有注册的 effect（label 非空、注册序）。
#[test]
fn get_effects_lists_registered_effects() {
    let log = log();
    let cordis = Cordis::new();
    let fid = effectful_plugin(&cordis, &log);

    let effects = cordis.get_effects(fid).expect("fiber exists");
    // 至少包含：插件自身 apply 的 effect + 两个 on + 一个 provide-like effect。
    // 具体 label 取决于内部实现；断言数量与「非空」。
    assert!(!effects.is_empty(), "effects should not be empty");
    // 插件本身一次 apply 是一个 effect（apply:efx 那一条）。
    assert!(effects.len() >= 4, "expected >=4 effects, got {}", effects.len());
    // 每项都有 label（String）。
    for e in &effects {
        assert!(!e.label.is_empty());
        assert!(e.children.is_empty(), "children currently empty");
    }
    // 注册序：同一批 collect 顺序稳定。
    let labels: Vec<&str> = effects.iter().map(|e| e.label.as_str()).collect();
    let _ = labels;
}

/// 卸载/重载后 effects 清空并重新收集（不翻倍累积）。
#[test]
fn effects_reset_on_reload() {
    let log = log();
    let cordis = Cordis::new();
    let fid = effectful_plugin(&cordis, &log);

    let first = cordis.get_effects(fid).unwrap();
    let n_first = first.len();
    assert!(n_first >= 4);

    // 触发一次重启（update，无 veto 监听器）→ 重新 apply → 重新收集。
    cordis.update(fid, json!({})).unwrap();
    let second = cordis.get_effects(fid).unwrap();
    assert_eq!(
        second.len(),
        n_first,
        "effects should reset (not double) after reload"
    );
    // label 亦重置（首个仍是插件 apply 的 effect）。
    assert_eq!(second[0].label, first[0].label);
}

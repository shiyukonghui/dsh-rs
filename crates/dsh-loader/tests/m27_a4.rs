//! beyond 目标 Phase A4：注入快照 / unprovide 唤醒顺序 / 父链 walk（核心层确定性锁定）。
//!
//! 与 golden（loader-22/24）互补：golden 承担字节级可表达面；本 file 承担
//! A4 语义中受异步交错影响（cordis `Promise.all` 并发 disposer 时序）而无法
//! 稳定进 golden 的确定性断言：
//! - T1 unprovide 唤醒顺序：依赖方先被唤醒（Unloading→Pending），之后 provider 的
//!   后续 disposer 才运行，且其自访问已 → absent（remove 先于后来 disposer）。
//! - T2 3 层 walk + 组隔离边界：消费者沿父链走 3 层到根作用域 provider；组 isolate
//!   边界（嵌套组入口 isolate 应用）阻止越界（本轮修复点）。
//! - T3 reload 注入快照：config-only 更新走就地 reload（身份不变），apply 见新配置，
//!   依赖方去活/重活并解析到新值。
#![allow(clippy::arc_with_non_send_sync)]

mod common;
use common::*;

use std::rc::Rc;
use std::sync::{Arc, Mutex};

use dsh_core::*;
use dsh_loader::*;

fn options(id: &str, name: &str) -> EntryOptions {
    EntryOptions::new(id, name)
}

/// 注册「后续 disposer」的 effect 钩子（A4：卸载逆序时先于 provide？——注册于 provide 之前，
/// 逆序后运行于 provide 移除**之后**；读取非 strict 自访问）。
/// 返回 disposer 可用于 `dispose-effect` 定向（此处无需）。
#[allow(clippy::type_complexity)]
fn later_self_access_disposer(check: Arc<Mutex<Option<String>>>) -> Box<dyn Fn(&Cordis) -> Result<EffectOutcome, CordisError>> {
    Box::new(move |_ctx| {
        let check = check.clone();
        Ok(EffectOutcome::One(Rc::new(move |ctx| {
            let present = ctx.get("svc").is_some();
            *check.lock().unwrap() = Some(if present { "present" } else { "absent" }.to_string());
        })))
    })
}

/// T1：unprovide 唤醒顺序——依赖方先唤醒（Unloading→Pending），provider 后续 disposer
/// 后运行且自访问已 absent（remove 先于后来 disposer）。
#[test]
fn unprovide_wakeup_order() {
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();

    let order: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let cons_order = order.clone();
    let later_check: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    // consumer：apply 记录 + 注册 disposer（卸载时记录 consumer-unloading）
    let consumer = FnPlugin::new("consumer", &["svc"], {
        let cons_order = cons_order.clone();
        move |ctx, _cfg| {
            cons_order.lock().unwrap().push("consumer-applied".into());
            let dis = cons_order.clone();
            ctx.effect("consumer-unload-label", Box::new(move |_ctx| {
                let dis = dis.clone();
                Ok(EffectOutcome::One(Rc::new(move |_ctx| {
                    dis.lock().unwrap().push("consumer-unloading".into());
                })))
            }))?;
            Ok(EffectOutcome::None)
        }
    });

    // provider：先注册后续 disposer（A4 位置），再 provide svc
    let provider = FnPlugin::new("provider", &[], {
        let later_check = later_check.clone();
        let order = order.clone();
        move |ctx, _cfg| {
            let later_check2 = later_check.clone();
            let order2 = order.clone();
            ctx.effect("provider-later", later_self_access_disposer(later_check2))?;
            ctx.effect("provider-later-order", Box::new(move |_ctx| {
                let order = order2.clone();
                Ok(EffectOutcome::One(Rc::new(move |_ctx| {
                    order.lock().unwrap().push("provider-later".into());
                })))
            }))?;
            ctx.provide("svc", Arc::new("v1".to_string())).unwrap();
            Ok(EffectOutcome::None)
        }
    });

    loader.register_plugin("provider", Arc::new(provider));
    loader.register_plugin("consumer", Arc::new(consumer));
    loader.create(options("p", "provider")).unwrap();
    loader.create(options("c", "consumer")).unwrap();

    let pf = loader.fiber("p").unwrap();
    let cf = loader.fiber("c").unwrap();
    assert_eq!(cordis.fiber_state(pf), Some(FiberState::Active));
    assert_eq!(cordis.fiber_state(cf), Some(FiberState::Active));

    // 移除 provider → 卸载顺序：provide 移除 → notify（consumer 先唤醒）→ 后续 disposer
    loader.remove("p").unwrap();
    assert_eq!(cordis.fiber_state(pf), Some(FiberState::Disposed), "provider disposed");
    assert_eq!(
        cordis.fiber_state(cf),
        Some(FiberState::Pending),
        "dependent woken back to Pending after unprovide"
    );
    // 顺序：consumer 先断言 applied →（卸载）consumer-unloading 先于 provider-later
    let seq = order.lock().unwrap().clone();
    assert_eq!(
        seq,
        vec![
            "consumer-applied".to_string(),
            "consumer-unloading".to_string(),
            "provider-later".to_string(),
        ],
        "notify/wake dependents before provider's later disposers"
    );
    // 后续 disposer 的自访问：provide 移除先于后续 disposer → absent
    assert_eq!(
        later_check.lock().unwrap().as_deref(),
        Some("absent"),
        "self-access after remove = absent"
    );
}

/// T2：3 层父链 walk + 组隔离边界（A4 修复点：组入口 isolate 应用）。
#[test]
fn group_isolate_boundary_and_3level_walk() {
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    loader.register_plugin("prov", Arc::new(FnPlugin::new(
        "prov",
        &[],
        |ctx, _cfg| {
            ctx.provide("svc", Arc::new("ROOT".to_string())).unwrap();
            Ok(EffectOutcome::None)
        },
    )));
    loader.register_plugin("cons", Arc::new(FnPlugin::new("cons", &["svc"], |_ctx, _cfg| {
        Ok(EffectOutcome::None)
    })));
    loader.register_plugin("blocked", Arc::new(FnPlugin::new("blocked", &["svc"], |_ctx, _cfg| {
        Ok(EffectOutcome::None)
    })));

    // 顶层组 g1：provider + 内层组 gInner（无 isolate）→ consumer 沿父链 walk 3 层到根
    let mut g1 = options("g1", "group");
    g1.group = true;
    g1.config = json!([
        { "id": "prov", "name": "prov" },
        { "id": "gInner", "name": "group", "group": true, "config": [
            { "id": "cons", "name": "cons" }
        ] }
    ]);
    loader.create(g1).unwrap();
    let cf = loader.fiber("cons").expect("consumer mounted");
    assert_eq!(
        cordis.fiber_state(cf),
        Some(FiberState::Active),
        "consumer walks 3 levels (cons→gInner→g1→root) to provider"
    );

    // 隔离边界组 gIso：组入口 isolate svc → 子入口 b 的注入在组 realm 无实现 → 停住（Pending）
    let mut giso = options("gIso", "group");
    giso.group = true;
    giso.isolate.insert("svc".into(), json!(true));
    giso.config = json!([ { "id": "b", "name": "blocked" } ]);
    loader.create(giso).unwrap();
    let bf = loader.fiber("b").expect("blocked mounted");
    assert_eq!(
        cordis.fiber_state(bf),
        Some(FiberState::Pending),
        "isolate boundary stops the walk; blocked must NOT resolve the root provider"
    );
    // 组内 provider/consumer 状态不受影响
    assert_eq!(cordis.fiber_state(loader.fiber("prov").unwrap()), Some(FiberState::Active));
    assert_eq!(cordis.fiber_state(cf), Some(FiberState::Active));
}

/// T3：config-only 更新 → 就地 reload（身份不变 = 分支 3），apply 见新配置（快照），
/// 依赖方去活/重活并解析到新值。
#[test]
fn reload_applies_new_config_and_reactivates_dependent() {
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();

    let provider_seen: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
    let cons_seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    let provider = FnPlugin::new("provider", &[], {
        let seen = provider_seen.clone();
        move |ctx, cfg| {
            let v = cfg["v"].as_i64().unwrap_or(-1);
            seen.lock().unwrap().push(v);
            ctx.provide("svc", Arc::new(v.to_string())).unwrap();
            Ok(EffectOutcome::None)
        }
    });
    let consumer = FnPlugin::new("consumer", &["svc"], {
        let seen = cons_seen.clone();
        move |ctx, _cfg| {
            let v = ctx.get_typed::<String>("svc").map(|s| s.as_str().to_string());
            if let Some(s) = v {
                seen.lock().unwrap().push(s);
            }
            Ok(EffectOutcome::None)
        }
    });

    loader.register_plugin("provider", Arc::new(provider));
    loader.register_plugin("consumer", Arc::new(consumer));

    let mut p0 = options("p", "provider");
    p0.config = json!({ "v": 1 });
    loader.create(p0).unwrap();
    loader.create(options("c", "consumer")).unwrap();
    let pf = loader.fiber("p").unwrap();
    let cf = loader.fiber("c").unwrap();
    assert_eq!(cordis.fiber_state(pf), Some(FiberState::Active));
    assert_eq!(cordis.fiber_state(cf), Some(FiberState::Active));

    // config-only update：就地 reload，身份不变；apply 见新配置；依赖方重活解析到新值
    let mut p2 = options("p", "provider");
    p2.config = json!({ "v": 2 });
    loader.update("p", p2).unwrap();

    assert_eq!(cordis.fiber_state(pf), Some(FiberState::Active), "provider re-Active");
    assert_eq!(cordis.fiber_state(cf), Some(FiberState::Active), "consumer re-Active");
    assert_eq!(
        loader.fiber("p").unwrap(),
        pf,
        "in-place reload keeps the fiber/identity (not a replace)"
    );
    assert_eq!(
        provider_seen.lock().unwrap().clone(),
        vec![1, 2],
        "provider re-applied with the new config (store snapshot)"
    );
    assert_eq!(
        cons_seen.lock().unwrap().clone(),
        vec!["1".to_string(), "2".to_string()],
        "dependent reactivated and resolved to the new value"
    );
}

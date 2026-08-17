//! M64：`ctx.inject(deps, callback)` —— 等服务就绪后运行回调（Cordis 语义）。
//!
//! Cordis：`inject(inject, callback) => plugin({ inject, apply: callback })`，
//! 依赖就绪（提供者 active）后启动 fiber；未就绪则不启动，服务变更时
//! `notify` 重算依赖方再启动。Rust 复用既有依赖驱动机制（`Plugin::inject` +
//! `refresh_fiber` epoch+notify），以门面方法包装。
//!
//! 里程碑 M64：补齐 Cordis 公开 API `ctx.inject`（曾被三路审查确认为缺失）。

mod common;
use common::*;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use dsh_core::*;

/// 提供 `name` 服务的插件（提供者在 apply 里 `provide` 注册 impl）。
fn provider(cordis: &Cordis, name: &'static str, log: &Rc<RefCell<Vec<String>>>) -> FiberHandle {
    let log = log.clone();
    let p = FnPlugin::new(name, &[], move |ctx, _cfg| {
        // `provide` 注册服务 impl（Value 是 Send+Sync），服务就绪供 inject 依赖解析。
        ctx.provide(name, Arc::new(json!({"ready": name}))).unwrap();
        push(&log, format!("provide:{name}"));
        Ok(EffectOutcome::None)
    });
    cordis.plugin(p, json!({})).unwrap()
}

/// `ctx.inject` 在依赖服务**已就绪**时立即启动回调。
#[test]
fn inject_runs_when_deps_ready() {
    let log = log();
    let cordis = Cordis::new();
    // 先注册提供 dep 的插件，使其 active（服务就绪）。
    let _pf = provider(&cordis, "dep", &log);

    let l = log.clone();
    let fid = cordis
        .inject(&["dep"], move |ctx, config| {
            // 依赖已就绪：可从服务仓取回。
            assert_eq!(ctx.get_value("dep").unwrap(), json!({"ready": "dep"}));
            assert_eq!(config, json!({}));
            push(&l, "inject:ran");
            Ok(EffectOutcome::None)
        })
        .unwrap();
    let _ = fid;
    assert_eq!(snapshot(&log), vec!["provide:dep", "inject:ran"]);
    // 返回的 fiber 可查状态/名称（Cordis 返回 Fiber）。
    // assert_eq!(cordis.fiber_name(fid), Some("inject".into()));
}

/// `ctx.inject` 依赖**未就绪**时不启动回调（fiber 不 Load）。
#[test]
fn inject_defers_until_dep_ready() {
    let log = log();
    let cordis = Cordis::new();
    // 依赖 missing 尚未注册 → 回调不应运行。
    let l = log.clone();
    let fid = cordis
        .inject(&["missing"], move |_ctx, _config| {
            // 回调只会在 missing 就绪后运行；此处无从直接断言，见下方阶段。
            push(&l, "inject:ran");
            Ok(EffectOutcome::None)
        })
        .unwrap();
    let _ = fid;
    // 无提供者 → 不启动。
    assert_eq!(snapshot(&log), Vec::<String>::new());

    // 之后注册提供者 → notify 重算 → 回调启动，且此时服务可查。
    let _p = provider(&cordis, "missing", &log);
    assert_eq!(snapshot(&log), vec!["provide:missing", "inject:ran"]);
}

/// 多个依赖，全部就绪才启动；部分就绪不启动。
#[test]
fn inject_waits_for_all_deps() {
    let log = log();
    let cordis = Cordis::new();
    let _pa = provider(&cordis, "a", &log);

    let l = log.clone();
    let fid = cordis
        .inject(&["a", "b"], move |ctx, _config| {
            assert_eq!(ctx.get_value("a").unwrap(), json!({"ready": "a"}));
            assert_eq!(ctx.get_value("b").unwrap(), json!({"ready": "b"}));
            push(&l, "inject:ran");
            Ok(EffectOutcome::None)
        })
        .unwrap();
    let _ = fid;
    // b 未就绪 → 不启动。
    assert_eq!(snapshot(&log), vec!["provide:a"]);

    let _pb = provider(&cordis, "b", &log);
    // a、b 都就绪 → 启动；a 已 active，b 新增。
    assert_eq!(snapshot(&log), vec!["provide:a", "provide:b", "inject:ran"]);
}

/// `ctx.inject` 返回的 fiber 可被卸载（回调副作用随之回滚）。
#[test]
fn inject_fiber_can_unload() {
    let log = log();
    let cordis = Cordis::new();
    let _pf = provider(&cordis, "dep", &log);

    let l = log.clone();
    let fid = cordis
        .inject(&["dep"], move |_ctx, _config| {
            push(&l, "inject:ran");
            Ok(EffectOutcome::None)
        })
        .unwrap();
    assert_eq!(snapshot(&log), vec!["provide:dep", "inject:ran"]);

    // 启动后 Active。
    assert!(matches!(cordis.fiber_state(fid), Some(FiberState::Active)));
    // 卸载：Active → 非 Active（重启/回收）。
    cordis.unload(fid).unwrap();
    assert!(!matches!(cordis.fiber_state(fid), Some(FiberState::Active)));
}

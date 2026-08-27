//! A2 收口复查锁点（D-171）：`!!js` eval_scope 服务绑定一致性修复 + 行为锁定。
//!
//! - T-L1：disabled 绑入口上下文——provider Active 后，消费方 `disabled_expr` 引用
//!   注入服务应求值**不**禁用（fork `Entry.evaluate` 在 loader ctx 根化 Context 求值；
//!   修复前顶层无 current fiber → 服务不可见 → fail-closed 误禁用，红）。
//! - T-L2：interpolate 多 `__jsExpr` 节点**原子性**——任一失败 → 整树保留原 config。
//! - T-L3：config 插值绑**目标视图**——隔离组 gIso(svc) 内消费方读本地 svc（非根 svc）
//!   （DIV-6-2：调用方可见 ≠ 目标可见；修复前按调用方 current 误取根 svc）。
//! - T-L4：`entry.options.inject` 并入 fiber inject（fork `internal/plugin` 同径）——entry
//!   声明依赖参与服务门控且 `!!js` 可读。
#![allow(clippy::arc_with_non_send_sync)]

mod common;
use common::*;

use std::sync::Arc;

use dsh_core::*;
use dsh_loader::*;

fn entry(id: &str, name: &str, config: Value) -> EntryOptions {
    let mut o = EntryOptions::new(id, name);
    o.config = config;
    o
}

/// T-L1：disabled 绑入口上下文——服务可见则求值不禁用。
#[test]
fn disabled_expr_resolves_entry_context_service() {
    let log = log();
    let log2 = log.clone();
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    loader.register_plugin(
        "prov",
        Arc::new(FnPlugin::new("prov", &["svc"], move |_ctx, config| {
            push(&log2, format!("prov-applied:{}", config.get("k").cloned().unwrap_or(Value::Null)));
            Ok(EffectOutcome::None)
        })),
    );
    loader.register_plugin(
        "svcp",
        Arc::new(FnPlugin::new("svcp", &[], move |ctx, _cfg| {
            ctx.provide("svc", Arc::new(serde_json::json!({"flag": true})))?;
            Ok(EffectOutcome::None)
        })),
    );
    loader.create(entry("svcp", "svcp", json!({}))).unwrap();
    let mut e = entry("prov", "prov", json!({"k": 1}));
    e.disabled_expr = Some("svc.flag ? false : true".to_string());
    loader.create(e).unwrap();
    let snap = snapshot(&log);
    assert_eq!(
        snap,
        vec!["prov-applied:1"],
        "disabled expr should evaluate against entry context (service visible) and not disable"
    );
}

/// T-L2：interpolate 多 `__jsExpr` 节点原子性——任一失败整树保留原 config。
#[test]
fn interpolate_failure_is_atomic() {
    let log = log();
    let log2 = log.clone();
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    loader.register_plugin(
        "p",
        Arc::new(FnPlugin::new("p", &[], move |_ctx, config| {
            push(&log2, format!("raw:{}", serde_json::to_string(&config).unwrap()));
            Ok(EffectOutcome::None)
        })),
    );
    loader
        .create(entry(
            "p",
            "p",
            json!({
                "ok": {"__jsExpr": "1 + 1"},
                "bad": {"__jsExpr": "undefined_name.boom"},
            }),
        ))
        .unwrap();
    let snap = snapshot(&log);
    assert!(
        snap.iter().all(|s| s.contains("__jsExpr")),
        "atomic fallback: both __jsExpr nodes preserved on any failure: {snap:?}"
    );
}

/// T-L3：config 插值绑目标视图——隔离组内读本地服务（非根服务）。
#[tokio::test(flavor = "current_thread")]
async fn config_interp_uses_target_realm_service() {
    let log = log();
    let log2 = log.clone();
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    loader.register_plugin(
        "rootp",
        Arc::new(FnPlugin::new("rootp", &[], move |ctx, _cfg| {
            ctx.provide("svc", Arc::new(serde_json::json!({"v": "root"})))?;
            Ok(EffectOutcome::None)
        })),
    );
    loader.register_plugin(
        "local",
        Arc::new(FnPlugin::new("local", &[], move |ctx, _cfg| {
            ctx.provide("svc", Arc::new(serde_json::json!({"v": "local"})))?;
            Ok(EffectOutcome::None)
        })),
    );
    loader.register_plugin(
        "p",
        Arc::new(FnPlugin::new("p", &["svc"], move |_ctx, config| {
            let v = config.get("v").and_then(|x| x.as_str()).unwrap_or("");
            push(&log2, format!("p-v:{v}"));
            Ok(EffectOutcome::None)
        })),
    );
    // 组 gIso 隔离 svc；组内 local 提供本地 svc、p 消费（config 读 svc.v）
    let mut g = entry("gIso", "group", json!([
        { "id": "rootp2", "name": "local" },
        { "id": "p2", "name": "p", "config": { "v": { "__jsExpr": "svc.v" } } }
    ]));
    g.group = true;
    g.isolate.insert("svc".to_string(), Value::Bool(true));
    loader.create_async(g).await.unwrap();
    let snap = snapshot(&log);
    assert!(
        snap.contains(&"p-v:local".to_string()),
        "target view: isolated consumer reads LOCAL svc (not root): {snap:?}"
    );
}

/// T-L4：entry 声明 inject 并入 fiber——参与门控且 `!!js` 可读。
#[test]
fn entry_declared_inject_merges_into_fiber() {
    let log = log();
    let log2 = log.clone();
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    loader.register_plugin(
        "svcp",
        Arc::new(FnPlugin::new("svcp", &[], move |ctx, _cfg| {
            ctx.provide("svc", Arc::new(serde_json::json!({"k": 42})))?;
            Ok(EffectOutcome::None)
        })),
    );
    // 插件自身不声明 inject；依赖由 entry 声明（fork `Inject.resolve(entry.options.inject, fiber.inject)`）
    loader.register_plugin(
        "p",
        Arc::new(FnPlugin::new("p", &[], move |_ctx, config| {
            let port = config.get("port").and_then(|x| x.as_i64()).unwrap_or(-1);
            push(&log2, format!("port:{port}"));
            Ok(EffectOutcome::None)
        })),
    );
    loader.create(entry("svcp", "svcp", json!({}))).unwrap();
    let mut e = entry("p", "p", json!({"port": {"__jsExpr": "svc.k"}}));
    e.inject.push("svc".to_string());
    loader.create(e).unwrap();
    let snap = snapshot(&log);
    assert_eq!(
        snap,
        vec!["port:42"],
        "entry-level inject merges into fiber inject and is readable by !!js"
    );
}

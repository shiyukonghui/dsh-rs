//! A2 !!js 求值作用域绑定注入服务：`internal/config` 插值时 ctx = 目标纤维注入服务
//! （成员 `ctx.svc` + 裸标识符 `svc`，显式键 config/process/env/ctx 优先）；失败 fail-loud。
#![allow(clippy::arc_with_non_send_sync)]

mod common;
use common::*;

use std::sync::Arc;

use dsh_core::*;
use dsh_loader::*;

fn options(id: &str, name: &str, config: Value) -> EntryOptions {
    let mut o = EntryOptions::new(id, name);
    o.config = config;
    o
}

/// T1：裸标识符读注入服务——config `{"__jsExpr": "svc.k"}` → apply 得到注入服务的值。
#[test]
fn js_expr_reads_injected_service_bare_identifier() {
    let log = log();
    let log2 = log.clone();
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    loader.register_plugin(
        "prov",
        Arc::new(FnPlugin::new("prov", &[], move |ctx, _cfg| {
            ctx.provide("svc", Arc::new(serde_json::json!({ "k": 42 })))?;
            Ok(EffectOutcome::None)
        })),
    );
    loader.register_plugin(
        "p",
        Arc::new(FnPlugin::new("p", &["svc"], move |_ctx, config| {
            let port = config.get("port").and_then(|v| v.as_i64()).unwrap_or(-1);
            push(&log2, format!("port:{port}"));
            Ok(EffectOutcome::None)
        })),
    );
    loader.create(options("prov", "prov", json!({}))).unwrap();
    loader
        .create(options("p", "p", json!({"name": "x", "port": {"__jsExpr": "svc.k"}})))
        .unwrap();
    assert_eq!(snapshot(&log), vec!["port:42"], "bare identifier reads injected service");
}

/// T2：ctx 成员访问 + 显式键优先——服务名与显式键（config）冲突不覆盖；
/// `ctx.config.tag` 读服务值，`config.tag` 读显式配置。
#[test]
fn js_expr_ctx_member_and_explicit_key_precedence() {
    let log = log();
    let log2 = log.clone();
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    loader.register_plugin(
        "prov",
        Arc::new(FnPlugin::new("prov", &[], move |ctx, _cfg| {
            ctx.provide("config", Arc::new(serde_json::json!({ "tag": "SVC" })))?;
            Ok(EffectOutcome::None)
        })),
    );
    loader.register_plugin(
        "p",
        Arc::new(FnPlugin::new("p", &["config"], move |_ctx, config| {
            let via_ctx = config.get("viaCtx").and_then(|v| v.as_str()).unwrap_or("");
            let from_cfg = config.get("fromConfig").and_then(|v| v.as_str()).unwrap_or("");
            push(&log2, format!("{via_ctx}/{from_cfg}"));
            Ok(EffectOutcome::None)
        })),
    );
    loader.create(options("prov", "prov", json!({}))).unwrap();
    loader
        .create(options(
            "p",
            "p",
            json!({
                "tag": "CFG",
                "viaCtx": {"__jsExpr": "ctx.config.tag"},
                "fromConfig": {"__jsExpr": "config.tag"},
            }),
        ))
        .unwrap();
    assert_eq!(
        snapshot(&log),
        vec!["SVC/CFG"],
        "ctx member reads service value; explicit config key wins for bare `config`"
    );
}

/// T3：引用未注入服务 → 求值失败 → fail-loud（保留原 config 节点 + eval-error 写回标记）。
#[test]
fn js_expr_unknown_service_fails_loud_keeps_config() {
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
        .create(options("p", "p", json!({"x": {"__jsExpr": "nope.x"}})))
        .unwrap();
    // config 原样保留（含 __jsExpr 节点），未被求值替换
    let snap = snapshot(&log);
    assert!(
        snap.iter().any(|s| s.starts_with("raw:") && s.contains("__jsExpr")),
        "config kept on eval failure: {snap:?}"
    );
    // 写回记录标记 eval-error（fail loud）
    assert!(
        loader.state.borrow().writes.iter().any(|w| w.starts_with("eval-error:")),
        "eval-error write-back marker: {:?}",
        loader.state.borrow().writes
    );
}

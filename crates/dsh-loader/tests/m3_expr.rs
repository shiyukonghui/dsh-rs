//! M3：loader `!!js` 表达式——disabled_expr 门控 + internal/config 插值。
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

/// disabled_expr：`config.env === 'prod'` 时禁用；更新 config 后热切换。
#[test]
fn disabled_expr_gates_and_hot_reloads() {
    let log = log();
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    let log2 = log.clone();
    loader.register_plugin(
        "p",
        Arc::new(FnPlugin::new("p", &[], move |_ctx, _cfg| {
            push(&log2, "apply:p");
            Ok(EffectOutcome::None)
        })),
    );

    // env=prod → 禁用（不加载）
    let mut e = options("e", "p", json!({"env": "prod"}));
    e.disabled_expr = Some("config.env === 'prod'".to_string());
    loader.create(e).unwrap();
    assert!(loader.fiber("e").is_none());
    assert!(loader.is_disabled("e"));
    assert_eq!(snapshot(&log), Vec::<String>::new());

    // env=dev → 启用（分支 1：未启动 → start）
    loader
        .update("e", options("e", "p", json!({"env": "dev"})))
        .unwrap();
    assert!(loader.fiber("e").is_some());
    assert!(!loader.is_disabled("e"));
    assert_eq!(snapshot(&log), vec!["apply:p"]);

    // env=prod → 禁用（分支 2：卸载）
    loader
        .update("e", options("e", "p", json!({"env": "prod"})))
        .unwrap();
    assert!(loader.fiber("e").is_none());
    assert!(loader.is_disabled("e"));
}

/// 表达式求值失败 → fail-closed（视为禁用）。
#[test]
fn disabled_expr_eval_failure_fails_closed() {
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    loader.register_plugin("p", Arc::new(FnPlugin::noop("p")));
    let mut e = options("e", "p", json!({}));
    e.disabled_expr = Some("config.undefined_field.boom".to_string());
    loader.create(e).unwrap();
    assert!(loader.fiber("e").is_none());
    assert!(loader.is_disabled("e"));
}

/// internal/config 插值：config 中的 `{"__jsExpr": ...}` 节点在 apply 前求值。
#[test]
fn internal_config_interpolates_js_expr() {
    let log = log();
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    let log2 = log.clone();
    loader.register_plugin(
        "p",
        Arc::new(FnPlugin::new("p", &[], move |_ctx, config| {
            let doubled = config.get("doubled").and_then(|v| v.as_i64()).unwrap_or(-1);
            let name = config.get("name").and_then(|v| v.as_str()).unwrap_or("");
            push(&log2, format!("{name}:{doubled}"));
            Ok(EffectOutcome::None)
        })),
    );

    loader
        .create(options(
            "e",
            "p",
            json!({"name": "x", "k": 21, "doubled": {"__jsExpr": "config.k * 2"}}),
        ))
        .unwrap();
    assert_eq!(snapshot(&log), vec!["x:42"]);
}

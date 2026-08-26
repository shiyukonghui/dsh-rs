//! A5 对象形态 inject（`inject: { svc: cfg }`）：插件注入配置成为本 fiber 自身 intercept
//! 层最内层 → `resolve_config` 以最高优先级合并；键即依赖；沿父链对子代可见。
#![allow(clippy::arc_with_non_send_sync)]

mod common;
use common::*;

use std::sync::Arc;

use dsh_core::*;

/// 注册 srv 提供者（对象形态 inject 的键即依赖 → 消费者需 srv Active 才装载）。
fn with_svc_provider(cordis: &Cordis) {
    cordis
        .plugin(
            FnPlugin::new("svc", &[], |ctx, _cfg| {
                ctx.provide("srv", Arc::new(json!({})))?;
                Ok(EffectOutcome::None)
            }),
            json!({}),
        )
        .unwrap();
}

/// T1：子插件对象形态 inject 配置为**最内层**——优先级高于父纤维 `ctx.intercept`，同键后者覆盖。
#[test]
fn obj_inject_config_wins_over_parent_intercept() {
    let log = log();
    let plug_log = log.clone();
    let cordis = Cordis::new();
    with_svc_provider(&cordis);

    let parent = FnPlugin::new("parent", &[], {
        move |ctx, _cfg| {
            ctx.intercept("srv", json!({"a": 1, "p": 1}))?;
            // 嵌套挂载子（parent = 本 fiber）：子声明 inject_config {srv:{a:9,b:2}}
            let value = plug_log.clone();
            let child = FnPlugin::new(
                "child",
                &[],
                move |c2, _cfg2| {
                    let merged = c2.resolve_config("srv", None, None);
                    push(&value, format!("merged:{}", serde_json::to_string(&merged).unwrap()));
                    Ok(EffectOutcome::None)
                },
            )
            .with_inject_config("srv", json!({"a": 9, "b": 2}));
            let _f = ctx.plugin(child, json!({}))?;
            Ok(EffectOutcome::None)
        }
    });
    cordis.plugin(parent, json!({})).unwrap();

    let snap = snapshot(&log);
    assert!(
        snap.contains(&"merged:{\"a\":9,\"b\":2,\"p\":1}".to_string()),
        "child object-form inject config wins over parent intercept (a:9 child / p:1 parent retained): {snap:?}"
    );
}

/// T2：`resolve_config(srv, base, head)` —— base 最低优先级、head 最高、中间含注入层。
#[test]
fn obj_inject_base_head_ordering() {
    let log = log();
    let plug_log = log.clone();
    let cordis = Cordis::new();
    with_svc_provider(&cordis);

    let parent = FnPlugin::new("parent", &[], {
        move |ctx, _cfg| {
            ctx.intercept("srv", json!({"p": 1}))?;
            let value = plug_log.clone();
            let child = FnPlugin::new(
                "child",
                &[],
                move |c2, _cfg2| {
                    let merged = c2.resolve_config("srv", Some(json!({"b": 0})), Some(json!({"h": 9})));
                    push(&value, format!("merged:{}", serde_json::to_string(&merged).unwrap()));
                    Ok(EffectOutcome::None)
                },
            )
            .with_inject_config("srv", json!({"a": 9, "b": 2}));
            let _f = ctx.plugin(child, json!({}))?;
            Ok(EffectOutcome::None)
        }
    });
    cordis.plugin(parent, json!({})).unwrap();

    let snap = snapshot(&log);
    let line = snap.iter().find(|s| s.starts_with("merged:")).expect("child merged line");
    let merged: serde_json::Value = serde_json::from_str(line.trim_start_matches("merged:")).unwrap();
    let obj = merged.as_object().unwrap();
    // 浅合并 Object.assign 语义：base 最低优先级（被注入层覆盖 b:2）、head 最高优先级（h:9 幸存）、
    // a:9 子注入层、p:1 父 intercept 保留。
    assert_eq!(obj.get("a").and_then(|v| v.as_i64()), Some(9), "inject-layer value {line}");
    assert_eq!(obj.get("b").and_then(|v| v.as_i64()), Some(2), "base lowest → overridden {line}");
    assert_eq!(obj.get("h").and_then(|v| v.as_i64()), Some(9), "head highest precedence {line}");
    assert_eq!(obj.get("p").and_then(|v| v.as_i64()), Some(1), "parent intercept retained {line}");
}

/// T3：父插件对象形态 inject 配置沿父链对**子代**可见。
#[test]
fn obj_inject_config_visible_to_child_via_parent_chain() {
    let log = log();
    let plug_log = log.clone();
    let cordis = Cordis::new();
    with_svc_provider(&cordis);

    // 父自身声明注入配置（键即依赖 → 父也需 srv Active）
    let parent = FnPlugin::new("parent", &[], {
        move |ctx, _cfg| {
            let value = plug_log.clone();
            let child = FnPlugin::new(
                "child",
                &[],
                move |c2, _cfg2| {
                    let merged = c2.resolve_config("srv", None, None);
                    push(&value, format!("merged:{}", serde_json::to_string(&merged).unwrap()));
                    Ok(EffectOutcome::None)
                },
            );
            let _f = ctx.plugin(child, json!({}))?;
            Ok(EffectOutcome::None)
        }
    })
    .with_inject_config("srv", json!({"p": 1}));
    cordis.plugin(parent, json!({})).unwrap();

    let snap = snapshot(&log);
    assert!(
        snap.contains(&"merged:{\"p\":1}".to_string()),
        "parent object-form inject config visible to child: {snap:?}"
    );
}

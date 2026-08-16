//! §5.3 场景 1：effect 语义 —— 逆序、幂等、嵌套、错误、inactive。

mod common;
use common::*;

use std::cell::RefCell;
use std::rc::Rc;

use dsh_core::*;

/// 插件 apply 注册 3 个 effect，每个在注册时与卸载时各写一条日志。
#[test]
fn disposers_run_in_reverse_registration_order() {
    let log = log();
    let log2 = log.clone();
    let plugin = FnPlugin::new("e", &[], move |ctx, _cfg| {
        for i in 1..=3 {
            let l = log2.clone();
            ctx.effect(
                "effect",
                Box::new(move |_ctx| {
                    push(&l, format!("reg{i}"));
                    Ok(EffectOutcome::One(Rc::new(move |_ctx| {
                        push(&l, format!("dis{i}"));
                    })))
                }),
            )
            .unwrap();
        }
        Ok(EffectOutcome::None)
    });

    let cordis = Cordis::new();
    let fid = cordis.plugin(plugin, json!({})).unwrap();
    assert_eq!(cordis.fiber_state(fid), Some(FiberState::Active));
    assert_eq!(snapshot(&log), vec!["reg1", "reg2", "reg3"]);

    cordis.unload(fid).unwrap();
    // 逆序执行：dis3, dis2, dis1
    let tail: Vec<String> = snapshot(&log).into_iter().skip(3).collect();
    assert_eq!(tail, vec!["dis3", "dis2", "dis1"]);
}

/// 返回的 disposer 可共享、幂等：调用两次只执行一次。
#[test]
fn disposer_is_idempotent() {
    let log = log();
    let log2 = log.clone();
    let captured: Rc<RefCell<Option<Disposer>>> = Rc::new(RefCell::new(None));
    let captured2 = captured.clone();

    let plugin = FnPlugin::new("idem", &[], move |ctx, _cfg| {
        let l = log2.clone();
        let d = ctx
            .effect(
                "effect",
                Box::new(move |_ctx| {
                    Ok(EffectOutcome::One(Rc::new(move |_ctx| {
                        push(&l, "ran");
                    })))
                }),
            )
            .unwrap();
        *captured2.borrow_mut() = Some(d);
        Ok(EffectOutcome::None)
    });

    let cordis = Cordis::new();
    let _fid = cordis.plugin(plugin, json!({})).unwrap();

    let d = captured.borrow().clone().expect("disposer captured");
    d(&cordis);
    d(&cordis); // 第二次调用为 no-op
    assert_eq!(snapshot(&log), vec!["ran"]);
}

/// effect 内再注册 effect：外层注册时内层已入列，卸载逆序时外层先跑。
#[test]
fn nested_effect_runs_inner_first_in_reverse_order() {
    let log = log();
    let log2 = log.clone();
    let plugin = FnPlugin::new("nest", &[], move |ctx, _cfg| {
        let l = log2.clone();
        ctx.effect(
            "outer",
            Box::new(move |ctx| {
                // 内层 effect 在外层 body 中注册
                let inner_log = l.clone();
                ctx.effect(
                    "inner",
                    Box::new(move |_ctx| {
                        push(&inner_log, "inner-reg");
                        Ok(EffectOutcome::One(Rc::new(move |_ctx| {
                            push(&inner_log, "inner-dis");
                        })))
                    }),
                )
                .unwrap();
                push(&l, "outer-reg");
                Ok(EffectOutcome::One(Rc::new(move |_ctx| {
                    push(&l, "outer-dis");
                })))
            }),
        )
        .unwrap();
        Ok(EffectOutcome::None)
    });

    let cordis = Cordis::new();
    let fid = cordis.plugin(plugin, json!({})).unwrap();
    cordis.unload(fid).unwrap();

    assert_eq!(
        snapshot(&log),
        vec!["inner-reg", "outer-reg", "outer-dis", "inner-dis"]
    );
}

/// effect body 返回 Err → effect() 返回 Err。
#[test]
fn effect_body_error_propagates() {
    let plugin = FnPlugin::new("err", &[], |ctx, _cfg| {
        let r = ctx.effect(
            "boom",
            Box::new(|_ctx| Err(CordisError::Internal("boom".to_string()))),
        );
        assert!(matches!(r, Err(CordisError::Internal(_))));
        Ok(EffectOutcome::None)
    });
    let cordis = Cordis::new();
    let fid = cordis.plugin(plugin, json!({})).unwrap();
    // 插件主体正常完成（错误被 effect 层拦截，未污染 fiber）
    assert_eq!(cordis.fiber_state(fid), Some(FiberState::Active));
}

/// 在无当前 fiber（根上下文）下注册 effect → INACTIVE_EFFECT。
#[test]
fn effect_outside_fiber_is_inactive() {
    let cordis = Cordis::new();
    let r = cordis.effect("x", Box::new(|_| Ok(EffectOutcome::None)));
    assert!(matches!(r, Err(CordisError::InactiveEffect)));
}

/// 卸载后注册的 effect 报 INACTIVE_EFFECT（apply 内 dispose 自身）。
#[test]
fn effect_after_fiber_dispose_is_inactive() {
    let log = log();
    let log2 = log.clone();
    let plugin = FnPlugin::new("selfdis", &[], move |ctx, _cfg| {
        let fid = ctx.current_fiber().unwrap();
        let l = log2.clone();
        // disposer 内再次注册 effect 应失败
        ctx.effect(
            "guard",
            Box::new(move |_ctx| {
                Ok(EffectOutcome::One(Rc::new(move |ctx| {
                    let r = ctx.effect("late", Box::new(|_| Ok(EffectOutcome::None)));
                    assert!(matches!(r, Err(CordisError::InactiveEffect)));
                    push(&l, "guard-ran");
                })))
            }),
        )
        .unwrap();
        let _ = fid;
        Ok(EffectOutcome::None)
    });
    let cordis = Cordis::new();
    let fid = cordis.plugin(plugin, json!({})).unwrap();
    cordis.unload(fid).unwrap();
    assert_eq!(snapshot(&log), vec!["guard-ran"]);
}

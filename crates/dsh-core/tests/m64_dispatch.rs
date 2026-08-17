//! M64：`internal/dispatch` 统一派发钩子。
//!
//! Cordis `EventsService.dispatch`（统一入口）在派发**非 internal** 事件前同步
//! emit `internal/dispatch(type, name, args, thisArg)`：emit 语义（无 next、返回
//! 值丢弃），参数 = (分派模式, 事件名, 剩余参数, target)。模式值照抄：
//! parallel 报 "emit"（Cordis 怪癖）。internal/ 前缀事件跳过。dsh-core 各公开
//! 分派方法（emit/bail/serial/parallel/waterfall）在收集监听器前派发该钩子。

mod common;
use common::*;

use std::cell::RefCell;
use std::rc::Rc;

use dsh_core::*;

/// 宿主：apply 内注册 `internal/dispatch` 监听器，记录 (mode, name)。
fn host_logger(cordis: &Cordis, log: &Rc<RefCell<Vec<String>>>) -> FiberHandle {
    let log = log.clone();
    let p = FnPlugin::new("host", &[], move |ctx, _cfg| {
        let l = log.clone();
        ctx.on("internal/dispatch", make_listener(move |_ctx, args, _next| {
            // args = [mode, name, rest..., thisArg]
            let mode = args.first().and_then(|v| v.as_str()).unwrap_or("?");
            let name = args.get(1).and_then(|v| v.as_str()).unwrap_or("?");
            push(&l, format!("{mode}:{name}"));
            HookResult::Continue
        }))
        .unwrap();
        Ok(EffectOutcome::None)
    });
    cordis.plugin(p, json!({})).unwrap()
}

/// 各分派模式都触发 `internal/dispatch`，且带正确 mode 与 name。
#[test]
fn every_mode_reports_internal_dispatch() {
    let log = log();
    let cordis = Cordis::new();
    let _h = host_logger(&cordis, &log);

    cordis.emit("ev", vec![json!(1)]);
    cordis.bail("ev", vec![json!(2)]);
    cordis.serial("ev", vec![json!(3)]);
    cordis.parallel("ev", vec![json!(4)]);
    cordis.waterfall("ev", vec![json!(5)], Box::new(|_a| Some(json!(6))));

    // 顺序 = 调用序；mode：emit→emit, bail→bail, serial→serial,
    // parallel→emit（Cordis 怪癖）, waterfall→waterfall。
    assert_eq!(
        snapshot(&log),
        vec![
            "emit:ev",
            "bail:ev",
            "serial:ev",
            "emit:ev", // parallel
            "waterfall:ev",
        ]
    );
}

/// `internal/` 前缀事件**不**触发 `internal/dispatch`（跳过规则）。
#[test]
fn internal_events_are_skipped() {
    let log = log();
    let cordis = Cordis::new();
    let _h = host_logger(&cordis, &log);

    cordis.emit("internal/x", vec![json!(1)]);
    // 未注册 internal/dispatch 的普通派发也走钩子（此处 internal 跳过才有意义）
    assert_eq!(snapshot(&log), Vec::<String>::new());
}

/// 有真实监听器时仍正常派发（钩子不改变原派发；请勿误判次序）。
#[test]
fn real_dispatch_still_runs_with_same_args() {
    let log = log();
    let cordis = Cordis::new();
    let _h = host_logger(&cordis, &log);

    let l = log.clone();
    let fid = cordis.plugin(FnPlugin::new("listener", &[], {
        let l = l.clone();
        move |ctx, _cfg| {
            let l2 = l.clone();
            ctx.on(
                "real",
                make_listener(move |_ctx, args, _next| {
                    let v = args.first().cloned();
                    if let Some(n) = v.and_then(|x| x.as_u64()) {
                        push(&l2, format!("real:{n}"));
                    }
                    HookResult::Continue
                }),
            )
            .unwrap();
            Ok(EffectOutcome::None)
        }
    }), json!({}))
    .unwrap();
    let _ = fid;

    cordis.emit("real", vec![json!(42)]);
    assert_eq!(snapshot(&log), vec!["emit:real", "real:42"]);
}

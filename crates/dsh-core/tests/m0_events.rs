//! §5.3 场景 3：事件分派 —— emit/bail/serial/parallel/waterfall 语义。

mod common;
use common::*;

use dsh_core::*;

fn host(cordis: &Cordis, name: &'static str, body: impl Fn(&Cordis) + 'static) -> FiberHandle {
    let plugin = FnPlugin::new(name, &[], move |ctx, _cfg| {
        body(ctx);
        Ok(EffectOutcome::None)
    });
    cordis.plugin(plugin, json!({})).unwrap()
}

/// emit：注册顺序调用，忽略返回值。
#[test]
fn emit_runs_in_registration_order() {
    let log = log();
    let log2 = log.clone();
    let cordis = Cordis::new();
    let _fid = host(&cordis, "h", move |ctx| {
        let l = log2.clone();
        ctx.on("e1", make_listener(move |_ctx, _args, _next| {
            push(&l, "a");
            HookResult::Continue
        }))
        .unwrap();
        let l = log2.clone();
        ctx.on("e1", make_listener(move |_ctx, _args, _next| {
            push(&l, "b");
            HookResult::Continue
        }))
        .unwrap();
    });
    cordis.emit("e1", vec![json!(1)]);
    assert_eq!(snapshot(&log), vec!["a", "b"]);
}

/// prepend：后注册但插到前面。
#[test]
fn prepend_listener_runs_first() {
    let log = log();
    let log2 = log.clone();
    let cordis = Cordis::new();
    let _fid = host(&cordis, "h", move |ctx| {
        let l = log2.clone();
        ctx.on("e1", make_listener(move |_ctx, _args, _next| {
            push(&l, "a");
            HookResult::Continue
        }))
        .unwrap();
        let l = log2.clone();
        ctx.on_with("e1", make_listener(move |_ctx, _args, _next| {
            push(&l, "p");
            HookResult::Continue
        }), false, true)
        .unwrap();
    });
    cordis.emit("e1", vec![]);
    assert_eq!(snapshot(&log), vec!["p", "a"]);
}

/// serial / bail：首个 bail 值（非 null/false/undefined）即停，后续不调用。
#[test]
fn serial_stops_at_first_bail() {
    let log = log();
    let log2 = log.clone();
    let cordis = Cordis::new();
    let _fid = host(&cordis, "h", move |ctx| {
        let l = log2.clone();
        ctx.on("e1", make_listener(move |_ctx, _args, _next| {
            push(&l, "one");
            HookResult::Returned(Some(json!("x")))
        }))
        .unwrap();
        let l = log2.clone();
        ctx.on("e1", make_listener(move |_ctx, _args, _next| {
            push(&l, "two");
            HookResult::Continue
        }))
        .unwrap();
    });
    let r = cordis.serial("e1", vec![]);
    assert_eq!(r, Some(json!("x")));
    assert_eq!(snapshot(&log), vec!["one"]);
}

/// bail 与 serial 相同（同步短路）。
#[test]
fn bail_short_circuits_sync() {
    let log = log();
    let log2 = log.clone();
    let cordis = Cordis::new();
    let _fid = host(&cordis, "h", move |ctx| {
        let l = log2.clone();
        ctx.on("e1", make_listener(move |_ctx, _args, _next| {
            push(&l, "one");
            HookResult::Returned(Some(json!(false)))
        }))
        .unwrap();
        let l = log2.clone();
        ctx.on("e1", make_listener(move |_ctx, _args, _next| {
            push(&l, "two");
            HookResult::Returned(Some(json!("y")))
        }))
        .unwrap();
    });
    // false 不算 bail → 继续到 two
    let r = cordis.bail("e1", vec![]);
    assert_eq!(r, Some(json!("y")));
    assert_eq!(snapshot(&log), vec!["one", "two"]);
}

/// parallel（M0 同步）：全部监听器都运行。
#[test]
fn parallel_runs_all_listeners() {
    let log = log();
    let log2 = log.clone();
    let cordis = Cordis::new();
    let _fid = host(&cordis, "h", move |ctx| {
        for i in 1..=3 {
            let l = log2.clone();
            ctx.on("e1", make_listener(move |_ctx, _args, _next| {
                push(&l, format!("{i}"));
                HookResult::Continue
            }))
            .unwrap();
        }
    });
    cordis.parallel("e1", vec![]);
    assert_eq!(snapshot(&log), vec!["1", "2", "3"]);
}

/// waterfall：全部委托 → inner 被调用，结果穿过链。
#[test]
fn waterfall_delegates_to_inner() {
    let log = log();
    let log2 = log.clone();
    let cordis = Cordis::new();
    let _fid = host(&cordis, "h", move |ctx| {
        let l = log2.clone();
        ctx.on("wf", make_listener(move |_ctx, _args, next| {
            push(&l, "l1");
            match next {
                Some(n) => HookResult::Returned(n(_ctx, _args)),
                None => HookResult::Continue,
            }
        }))
        .unwrap();
        let l = log2.clone();
        ctx.on("wf", make_listener(move |_ctx, _args, next| {
            push(&l, "l2");
            match next {
                Some(n) => HookResult::Returned(n(_ctx, _args)),
                None => HookResult::Continue,
            }
        }))
        .unwrap();
    });
    let r = cordis.waterfall(
        "wf",
        vec![],
        Box::new(|_args| Some(json!("inner"))),
    );
    assert_eq!(r, Some(json!("inner")));
    assert_eq!(snapshot(&log), vec!["l1", "l2"]);
}

/// waterfall：外层包装（修改结果 + 修改共享 args）。
#[test]
fn waterfall_wraps_result_and_mutates_args() {
    let cordis = Cordis::new();
    let _fid = host(&cordis, "h", |ctx| {
        // 外层：包装结果
        ctx.on("wf", make_listener(|_ctx, _args, next| match next {
            Some(n) => {
                let inner = n(_ctx, _args);
                HookResult::Returned(inner.map(|v| {
                    json!(format!("wrapped:{}", v.as_str().unwrap_or("<non-str>")))
                }))
            }
            None => HookResult::Continue,
        }))
        .unwrap();
        // 内层：修改 args 后委托
        ctx.on("wf", make_listener(|_ctx, args, next| {
            args.push(json!("mutated"));
            match next {
                Some(n) => HookResult::Returned(n(_ctx, args)),
                None => HookResult::Continue,
            }
        }))
        .unwrap();
    });
    let r = cordis.waterfall(
        "wf",
        vec![],
        Box::new(|args| Some(json!(format!("inner:{}", args.len())))),
    );
    // 内层先跑（注册顺序外层在列表前？prepend 未指定 → 注册顺序：外层先注册）
    // 外层委托 → 内层修改 args（len=1）→ inner 看到 len=1 → 外层再包装
    assert_eq!(r, Some(json!("wrapped:inner:1")));
}

/// waterfall：短路（不调用 next）→ inner 不被调用。
#[test]
fn waterfall_short_circuits() {
    let main_log = log();
    let log2 = main_log.clone();
    let cordis = Cordis::new();
    let _fid = host(&cordis, "h", move |ctx| {
        let l = log2.clone();
        ctx.on("wf", make_listener(move |_ctx, _args, _next| {
            push(&l, "l1");
            HookResult::Returned(Some(json!("stop")))
        }))
        .unwrap();
        let l = log2.clone();
        ctx.on("wf", make_listener(move |_ctx, _args, _next| {
            push(&l, "l2");
            HookResult::Continue
        }))
        .unwrap();
    });
    let inner_log = log();
    let inner_log2 = inner_log.clone();
    let r = cordis.waterfall(
        "wf",
        vec![],
        Box::new(move |_args| {
            push(&inner_log2, "inner");
            Some(json!("inner"))
        }),
    );
    assert_eq!(r, Some(json!("stop")));
    assert_eq!(snapshot(&main_log), vec!["l1"]);
    assert_eq!(snapshot(&inner_log), Vec::<String>::new());
}

/// 重入安全：监听器内再 emit（等价 JS 动态作用域重入），无借用冲突。
#[test]
fn listener_can_reenter_emit() {
    let log = log();
    let log2 = log.clone();
    let cordis = Cordis::new();
    let _fid = host(&cordis, "h", move |ctx| {
        let l = log2.clone();
        ctx.on("e1", make_listener(move |ctx, _args, _next| {
            push(&l, "e1-start");
            ctx.emit("e2", vec![]);
            push(&l, "e1-end");
            HookResult::Continue
        }))
        .unwrap();
        let l = log2.clone();
        ctx.on("e2", make_listener(move |_ctx, _args, _next| {
            push(&l, "e2");
            HookResult::Continue
        }))
        .unwrap();
    });
    cordis.emit("e1", vec![]);
    assert_eq!(snapshot(&log), vec!["e1-start", "e2", "e1-end"]);
}

/// waterfall：next 可多次调用（等价 JS `cbs.shift()` 消耗下一个监听器）。
#[test]
fn waterfall_next_can_be_called_twice() {
    let log = log();
    let log2 = log.clone();
    let cordis = Cordis::new();
    let _fid = host(&cordis, "h", move |ctx| {
        let l = log2.clone();
        ctx.on("wf", make_listener(move |_ctx, _args, next| {
            push(&l, "l1");
            // 第一次 next → l2；第二次 next → inner
            let first: Option<Value> = next.and_then(|n| n(_ctx, _args));
            let second: Option<Value> = next.and_then(|n| n(_ctx, _args));
            HookResult::Returned(second.or(first))
        }))
        .unwrap();
        let l = log2.clone();
        ctx.on("wf", make_listener(move |_ctx, _args, next| {
            push(&l, "l2");
            match next {
                Some(n) => HookResult::Returned(n(_ctx, _args)),
                None => HookResult::Continue,
            }
        }))
        .unwrap();
    });
    let r = cordis.waterfall(
        "wf",
        vec![],
        Box::new(|_args| Some(json!("inner"))),
    );
    assert_eq!(r, Some(json!("inner")));
    assert_eq!(snapshot(&log), vec!["l1", "l2"]);
}

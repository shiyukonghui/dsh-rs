//! §M7 async 基建：async listener / parallel_async / serial_async / yield_now /
//! fiber_await / async disposer（对应 HANDOFF §7 方向 1）。

mod common;
use common::*;

use std::sync::Arc;

use dsh_core::*;
use futures_util::future::LocalBoxFuture;

/// 由闭包构造异步监听器（返回 `Result<HookResult, CordisError>`，parallel 聚合错误）。
fn async_listener<F>(f: F) -> AsyncListener
where
    F: Fn(&Cordis, Vec<Value>) -> LocalBoxFuture<'static, Result<HookResult, CordisError>>
        + 'static,
{
    Arc::new(f)
}

/// 立即完成的异步监听器（闭包同步返回 HookResult）。
fn async_listener_ok<F>(f: F) -> AsyncListener
where
    F: Fn(&Cordis, Vec<Value>) -> HookResult + Clone + 'static,
{
    async_listener(move |ctx, args| {
        let f = f.clone();
        let ctx = ctx.clone();
        Box::pin(async move { Ok(f(&ctx, args)) })
    })
}

/// 挂载一个一次性插件（apply 内执行 body）。
fn host(cordis: &Cordis, name: &'static str, body: impl Fn(&Cordis) + 'static) -> FiberHandle {
    let plugin = FnPlugin::new(name, &[], move |ctx, _cfg| {
        body(ctx);
        Ok(EffectOutcome::None)
    });
    cordis.plugin(plugin, json!({})).unwrap()
}

/// parallel_async：注册顺序调用全部异步监听器。
#[tokio::test]
async fn parallel_async_runs_all_async_listeners() {
    let log = log();
    let log2 = log.clone();
    let cordis = Cordis::new();
    let _fid = host(&cordis, "h", move |ctx| {
        let l = log2.clone();
        ctx.on_async(
            "e1",
            async_listener_ok(move |_ctx, _args| {
                push(&l, "a");
                HookResult::Continue
            }),
            false,
            false,
        )
        .unwrap();
        let l = log2.clone();
        ctx.on_async(
            "e1",
            async_listener_ok(move |_ctx, _args| {
                push(&l, "b");
                HookResult::Continue
            }),
            false,
            false,
        )
        .unwrap();
    });
    cordis.parallel_async("e1", vec![json!(1)]).await.unwrap();
    assert_eq!(snapshot(&log), vec!["a", "b"]);
}

/// M60：`parallel_async` 返回 Promise.all 结果数组（对齐 Cordis `ctx.parallel`
/// ——各监听器返回值；`Continue` → null）。
#[tokio::test]
async fn parallel_async_returns_result_values() {
    let log = log();
    let log2 = log.clone();
    let cordis = Cordis::new();
    let _fid = host(&cordis, "h", move |ctx| {
        let l = log2.clone();
        ctx.on_async(
            "e1",
            async_listener_ok(move |_ctx, _args| {
                push(&l, "a");
                HookResult::Continue
            }),
            false,
            false,
        )
        .unwrap();
        let l = log2.clone();
        ctx.on_async(
            "e1",
            async_listener_ok(move |_ctx, _args| {
                push(&l, "b");
                HookResult::Returned(Some(json!("b-val")))
            }),
            false,
            false,
        )
        .unwrap();
    });
    let results = cordis.parallel_async("e1", vec![]).await.unwrap();
    // Continue → null；Returned → 值（注册顺序）
    assert_eq!(results, vec![json!(null), json!("b-val")]);
    assert_eq!(snapshot(&log), vec!["a", "b"]);
}

/// parallel_async：全部执行（allSettled），错误聚合为 AggregateError。
#[tokio::test]
async fn parallel_async_aggregates_errors_but_runs_all() {
    let log = log();
    let log2 = log.clone();
    let cordis = Cordis::new();
    let _fid = host(&cordis, "h", move |ctx| {
        let l = log2.clone();
        ctx.on_async(
            "e1",
            async_listener(move |_ctx, _args| {
                let l = l.clone();
                Box::pin(async move {
                    push(&l, "one");
                    Err(CordisError::Internal("boom".to_string()))
                })
            }),
            false,
            false,
        )
        .unwrap();
        let l = log2.clone();
        ctx.on_async(
            "e1",
            async_listener_ok(move |_ctx, _args| {
                push(&l, "two");
                HookResult::Continue
            }),
            false,
            false,
        )
        .unwrap();
    });
    let err = cordis.parallel_async("e1", vec![]).await.unwrap_err();
    // allSettled：两个都执行
    assert_eq!(snapshot(&log), vec!["one", "two"]);
    assert_eq!(err.errors.len(), 1);
    assert!(matches!(err.errors[0], CordisError::Internal(ref s) if s == "boom"));
}

/// serial_async：顺序 await，首个 bail 值即停。
#[tokio::test]
async fn serial_async_stops_at_first_bail() {
    let log = log();
    let log2 = log.clone();
    let cordis = Cordis::new();
    let _fid = host(&cordis, "h", move |ctx| {
        let l = log2.clone();
        ctx.on_async(
            "e1",
            async_listener_ok(move |_ctx, _args| {
                push(&l, "one");
                HookResult::Returned(Some(json!("x")))
            }),
            false,
            false,
        )
        .unwrap();
        let l = log2.clone();
        ctx.on_async(
            "e1",
            async_listener_ok(move |_ctx, _args| {
                push(&l, "two");
                HookResult::Continue
            }),
            false,
            false,
        )
        .unwrap();
    });
    let result = cordis.serial_async("e1", vec![]).await.unwrap();
    assert_eq!(result, Some(json!("x")));
    assert_eq!(snapshot(&log), vec!["one"]);
}

/// serial_async：异步监听器错误向上传播。
#[tokio::test]
async fn serial_async_propagates_listener_error() {
    let cordis = Cordis::new();
    let _fid = host(&cordis, "h", move |ctx| {
        ctx.on_async(
            "e1",
            async_listener(|_ctx, _args| {
                Box::pin(async move { Err(CordisError::Internal("kaboom".to_string())) })
            }),
            false,
            false,
        )
        .unwrap();
    });
    let err = cordis.serial_async("e1", vec![]).await.unwrap_err();
    assert!(matches!(err, CordisError::Internal(ref s) if s == "kaboom"));
}

/// yield_now：让出后另一任务可先推进（交错）。
#[tokio::test]
async fn yield_now_allows_interleaving() {
    let log = log();
    let log2 = log.clone();
    let log3 = log.clone();
    let (a, b) = tokio::join!(
        async move {
            push(&log2, "a1");
            Cordis::yield_now().await;
            push(&log2, "a2");
        },
        async move {
            push(&log3, "b1");
        },
    );
    let _ = (a, b);
    // a1 先执行，yield 后 b1 可能插入，但 a2 一定在最后
    let snap = snapshot(&log);
    assert_eq!(snap.first().map(|s| s.as_str()), Some("a1"));
    assert_eq!(snap.last().map(|s| s.as_str()), Some("a2"));
}

/// fiber_await：插件加载完成后返回 Ok，fiber 为 Active。
#[tokio::test]
async fn fiber_await_returns_after_load() {
    let cordis = Cordis::new();
    let fid = host(&cordis, "h", |_ctx| {});
    cordis.fiber_await(fid).await.unwrap();
    assert_eq!(cordis.fiber_state(fid), Some(FiberState::Active));
}

/// unload_async：异步 disposer（EffectOutcome::Async）在卸载时执行。
#[tokio::test]
async fn unload_async_runs_async_disposer() {
    let log = log();
    let log2 = log.clone();
    let cordis = Cordis::new();
    let plugin = FnPlugin::new("p", &[], move |_ctx, _cfg| {
        let l = log2.clone();
        Ok(EffectOutcome::Async(Box::pin(async move {
            push(&l, "async-disposer");
            EffectOutcome::None
        })))
    });
    let fid = cordis.plugin(plugin, json!({})).unwrap();
    assert_eq!(snapshot(&log), Vec::<String>::new());
    cordis.unload_async(fid).await.unwrap();
    assert_eq!(snapshot(&log), vec!["async-disposer"]);
}

/// M24：同步 `unload` 无法 await 异步 disposer——同步 disposer 仍执行、
/// 异步 disposer 显式记录（`async-disposers-skipped` trace），不静默丢弃。
#[test]
fn sync_unload_skips_async_disposer_with_trace() {
    let log = log();
    let log2 = log.clone();
    let cordis = Cordis::new();
    // 插件 q：注册一个异步 effect（EffectOutcome::Async）→ async_disposers
    let plugin2 = FnPlugin::new("q", &[], move |ctx, _cfg| {
        let l = log2.clone();
        ctx.effect(
            "async-effect",
            Box::new(move |_| {
                Ok(EffectOutcome::Async(Box::pin(async move {
                    push(&l, "async-disposer");
                    EffectOutcome::None
                })))
            }),
        )?;
        Ok(EffectOutcome::None)
    });
    let fid2 = cordis.plugin(plugin2, json!({})).unwrap();
    cordis.unload(fid2).unwrap();
    // 异步 disposer 未执行（同步路径无法 await）但显式记录
    assert_eq!(snapshot(&log), Vec::<String>::new(), "async disposer not run");
    let trace = cordis.take_trace();
    assert!(
        trace.iter().any(|l| l == "async-disposers-skipped"),
        "async disposer skip recorded: {trace:?}"
    );

    // 对照：unload_async 完整执行异步 disposer
    let log3 = log.clone();
    let plugin3 = FnPlugin::new("r", &[], move |ctx, _cfg| {
        let l = log3.clone();
        ctx.effect(
            "async-effect",
            Box::new(move |_| {
                Ok(EffectOutcome::Async(Box::pin(async move {
                    push(&l, "async-disposer-ran");
                    EffectOutcome::None
                })))
            }),
        )?;
        Ok(EffectOutcome::None)
    });
    let fid3 = cordis.plugin(plugin3, json!({})).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(cordis.unload_async(fid3)).unwrap();
    assert_eq!(snapshot(&log), vec!["async-disposer-ran"]);
}

/// on_async 随 fiber 卸载自动移除（副作用语义）。
#[tokio::test]
async fn async_listener_removed_on_unload_async() {
    let log = log();
    let log2 = log.clone();
    let cordis = Cordis::new();
    let fid = host(&cordis, "h", move |ctx| {
        let l = log2.clone();
        ctx.on_async(
            "e1",
            async_listener_ok(move |_ctx, _args| {
                push(&l, "fired");
                HookResult::Continue
            }),
            false,
            false,
        )
        .unwrap();
    });
    cordis.parallel_async("e1", vec![]).await.unwrap();
    assert_eq!(snapshot(&log), vec!["fired"]);
    cordis.unload_async(fid).await.unwrap();
    // 卸载后 async listener 已移除
    cordis.parallel_async("e1", vec![]).await.unwrap();
    assert_eq!(snapshot(&log), vec!["fired"]);
}

/// M13：emit 对 async listener fire-and-forget——经 spawn 钩子驱动（不 await）。
/// 宿主注入 tokio LocalSet 驱动；emit 返回后 async listener 已执行。
#[tokio::test]
async fn emit_fire_and_forgets_async_listener() {
    let log = log();
    let log2 = log.clone();
    let cordis = Cordis::new();
    // 宿主注入 spawn 钩子：LocalSet 内 spawn_local 驱动（fire-and-forget）
    let local = std::rc::Rc::new(tokio::task::LocalSet::new());
    let local2 = local.clone();
    cordis.set_spawn(move |fut| {
        local2.spawn_local(fut);
    });

    let _fid = host(&cordis, "h", move |ctx| {
        let l = log2.clone();
        ctx.on_async(
            "e1",
            async_listener_ok(move |_ctx, _args| {
                push(&l, "async-fired");
                HookResult::Continue
            }),
            false,
            false,
        )
        .unwrap();
    });

    // emit 同步返回；async listener 经 LocalSet 调度
    cordis.emit("e1", vec![]);
    // run_until 驱动 LocalSet 内已 spawn 的任务（多次 yield 让任务完成）
    local
        .run_until(async {
            for _ in 0..4 {
                tokio::task::yield_now().await;
            }
        })
        .await;
    assert_eq!(snapshot(&log), vec!["async-fired"], "emit fired async listener via spawn hook");
}

/// M18：bail/serial 对 async listener fire-and-forget——调用但不 await
/// （等价 Cordis `Reflect.apply` 丢弃 Promise；bail 值不可同步判定 → 继续）。
#[tokio::test]
async fn bail_and_serial_fire_and_forget_async_listener() {
    let log = log();
    let log2 = log.clone();
    let cordis = Cordis::new();
    let local = std::rc::Rc::new(tokio::task::LocalSet::new());
    let local2 = local.clone();
    cordis.set_spawn(move |fut| {
        local2.spawn_local(fut);
    });

    let _fid = host(&cordis, "h", move |ctx| {
        let l = log2.clone();
        ctx.on_async(
            "e1",
            async_listener_ok(move |_ctx, _args| {
                push(&l, "async-fired");
                HookResult::Returned(Some(serde_json::json!("bail-value")))
            }),
            false,
            false,
        )
        .unwrap();
    });

    // bail：async listener 被调用（副作用执行）但返回值不参与 bail 判定
    cordis.bail("e1", vec![]);
    // serial：同样 fire-and-forget
    cordis.serial("e1", vec![]);
    local
        .run_until(async {
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
        })
        .await;
    assert_eq!(
        snapshot(&log),
        vec!["async-fired", "async-fired"],
        "bail + serial both fired async listener"
    );
}

/// M18：waterfall 对 async listener fire-and-forget——调用但不 await，
/// 链继续（inner 仍执行）。
#[tokio::test]
async fn waterfall_fire_and_forgets_async_listener() {
    let log = log();
    let log2 = log.clone();
    let cordis = Cordis::new();
    let local = std::rc::Rc::new(tokio::task::LocalSet::new());
    let local2 = local.clone();
    cordis.set_spawn(move |fut| {
        local2.spawn_local(fut);
    });

    let _fid = host(&cordis, "h", move |ctx| {
        let l = log2.clone();
        ctx.on_async(
            "e1",
            async_listener_ok(move |_ctx, _args| {
                push(&l, "async-fired");
                HookResult::Continue
            }),
            false,
            false,
        )
        .unwrap();
    });

    // waterfall：async listener 被调用；inner 同步执行返回
    let result = cordis.waterfall(
        "e1",
        vec![],
        Box::new(|_args| Some(serde_json::json!("inner-result"))),
    );
    assert_eq!(result, Some(serde_json::json!("inner-result")));
    local
        .run_until(async {
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
        })
        .await;
    assert_eq!(snapshot(&log), vec!["async-fired"], "waterfall fired async listener");
}

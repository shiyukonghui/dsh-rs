//! A6 异步生成器 effect（cordis `[Service.init]` 完整形态）：`EffectOutcome::Stream`
//! 的逐项收集 / 卸载逆序 / epoch 中途取消 / 失败前 disposer 保留。
#![allow(clippy::arc_with_non_send_sync)]

mod common;
use common::*;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use dsh_core::*;
use dsh_loader::*;
use futures_util::stream::{self, StreamExt};

type Log = Rc<RefCell<Vec<String>>>;

/// 记录 tag 的一次性 disposer（运行即 `dispose:{tag}`）。
fn d(log: &Log, tag: &'static str) -> Disposer {
    let log = log.clone();
    make_disposer(Box::new(move |_ctx| push(&log, format!("dispose:{tag}"))))
}

/// T1：逐项收集（跨 await 边界步进）+ 生成完成 Active + 卸载**逆序**（C,B,A）。
#[test]
fn gen_stream_collects_in_order_and_unloads_reversed() {
    let log = log();
    let plug_log = log.clone(); // 进入 FnPlugin 体（`Fn` 可重入）的日志句柄
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();

    let plugin = FnPlugin::new("g", &[], move |_ctx, _cfg| {
        // 流在体内**每次调用**新建（生成器每次 apply 新迭代器）。
        let log = plug_log.clone();
        let s = stream::unfold(0u8, move |state| {
            let log = log.clone();
            async move {
                // 每步产出即被驱动方轮询 → 证明逐项步进（跨 await 边界）。
                let item = match state {
                    0 => {
                        push(&log, "gen:yield:A");
                        Some(Ok(d(&log, "A")))
                    }
                    1 => {
                        push(&log, "gen:await:m1");
                        push(&log, "gen:yield:B");
                        Some(Ok(d(&log, "B")))
                    }
                    2 => {
                        push(&log, "gen:yield:C");
                        Some(Ok(d(&log, "C")))
                    }
                    _ => None,
                };
                item.map(|it| (it, state + 1))
            }
        })
        .boxed_local();
        Ok(EffectOutcome::Stream(s))
    });
    loader.register_plugin("g", Arc::new(plugin));
    let eid = loader.create(EntryOptions::new("e", "g")).unwrap();
    assert_eq!(cordis.fiber_state(loader.fiber(&eid).unwrap()), Some(FiberState::Active));
    assert_eq!(
        snapshot(&log),
        vec!["gen:yield:A", "gen:await:m1", "gen:yield:B", "gen:yield:C"],
        "per-step collection across await boundaries, in order"
    );

    loader.remove(&eid).unwrap();
    let tail: Vec<_> = snapshot(&log)
        .into_iter()
        .filter(|s| s.starts_with("dispose:"))
        .collect();
    assert_eq!(tail, vec!["dispose:C", "dispose:B", "dispose:A"], "reverse unload order");
}

/// T2：epoch 中途取消——生成器某步体内同步翻转自身 epoch（卸掉依赖提供者）。
/// 忠实 cordis `_execute` async-iterator：flip 步产出的 B **先**被收集，循环顶部
/// pre-check 之后命中 epoch 变化 → 后续（C）不再收集；fiber 不 finish 到 Active，
/// 转入卸载，已收集 disposer（A/B）保留并运行。
#[test]
fn gen_stream_mid_cancel_on_epoch_change() {
    let log = log();
    let plug_log = log.clone();
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();

    loader.register_plugin("svc", Arc::new(FnPlugin::new("svc", &[], |ctx, _cfg| {
        ctx.provide("svc", Arc::new(json!("v")))?;
        Ok(EffectOutcome::None)
    })));
    loader.create(EntryOptions::new("prov", "svc")).unwrap();

    let consumer = FnPlugin::new("consumer", &["svc"], {
        let loader = loader.clone();
        move |_ctx, _cfg| {
            let log = plug_log.clone();
            let loader = loader.clone();
            let s = stream::unfold(0u8, move |state| {
                let log = log.clone();
                let loader = loader.clone();
                async move {
                    let item = match state {
                        0 => {
                            push(&log, "gen:yield:A");
                            Some(Ok(d(&log, "A")))
                        }
                        1 => {
                            // 生成器体内同步翻转自身 epoch（卸掉依赖提供者）——
                            // 等价 cordis `_setEpoch` 在跑动中被 `_refresh` 触发。
                            push(&log, "gen:flip-prov");
                            loader.remove("prov").unwrap();
                            push(&log, "gen:yield:B");
                            Some(Ok(d(&log, "B")))
                        }
                        2 => {
                            push(&log, "gen:yield:C");
                            Some(Ok(d(&log, "C")))
                        }
                        _ => None,
                    };
                    item.map(|it| (it, state + 1))
                }
            })
            .boxed_local();
            Ok(EffectOutcome::Stream(s))
        }
    });
    loader.register_plugin("consumer", Arc::new(consumer));
    let eid = loader.create(EntryOptions::new("c", "consumer")).unwrap();

    let snap = snapshot(&log);
    assert!(snap.contains(&"gen:yield:A".to_string()));
    assert!(snap.contains(&"gen:flip-prov".to_string()));
    assert!(
        snap.contains(&"gen:yield:B".to_string()),
        "item yielded by the flipping step is collected (cordis semantics)"
    );
    assert!(
        !snap.contains(&"gen:yield:C".to_string()),
        "mid-cancel stops further collection after epoch flip"
    );
    if let Some(fid) = loader.fiber(&eid) {
        assert_ne!(cordis.fiber_state(fid), Some(FiberState::Active), "consumer not Active");
    }
    let snap = snapshot(&log);
    assert!(snap.contains(&"dispose:A".to_string()), "collected A retained & run");
    assert!(snap.contains(&"dispose:B".to_string()), "collected B retained & run");
}

/// T3：init 失败前 disposer 保留——yield A 后某步 Err → loader 按既有 fail-loud
/// 约定把错误向上传播（`create` Err + 回滚）；失败前已收集的 A 在回滚卸载中保留并
/// 执行（不泄漏）；B 从未被收集。
#[test]
fn gen_stream_fail_retains_collected_disposers() {
    let log = log();
    let plug_log = log.clone();
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();

    let plugin = FnPlugin::new("g", &[], move |_ctx, _cfg| {
        let log = plug_log.clone();
        let s = stream::unfold(0u8, move |state| {
            let log = log.clone();
            async move {
                let item = match state {
                    0 => {
                        push(&log, "gen:yield:A");
                        Some(Ok(d(&log, "A")))
                    }
                    1 => {
                        push(&log, "gen:fail:boom");
                        Some(Err(CordisError::Internal("boom".into())))
                    }
                    _ => None,
                };
                item.map(|it| (it, state + 1))
            }
        })
        .boxed_local();
        Ok(EffectOutcome::Stream(s))
    });
    loader.register_plugin("g", Arc::new(plugin));

    let err = loader.create(EntryOptions::new("e", "g")).unwrap_err();
    assert!(err.to_string().contains("boom"), "fail-loud propagation of generator error");
    let snap = snapshot(&log);
    assert!(snap.contains(&"gen:yield:A".to_string()));
    assert!(snap.contains(&"gen:fail:boom".to_string()));
    assert!(!snap.contains(&"gen:yield:B".to_string()));
    assert!(snap.contains(&"dispose:A".to_string()), "pre-failure disposer retained & run on rollback");
}

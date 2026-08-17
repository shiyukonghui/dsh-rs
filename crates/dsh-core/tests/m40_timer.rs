//! M40：timer 服务——`Cordis::timeout`/`interval`/`debounce`/`throttle`。
//!
//! 对应 deepseek-harness `vendor/timer`（`ctx.timeout`/`interval`/`debounce`/
//! `throttle`）：生命周期绑定的调度原语——timer 经 `ctx.effect` 注册 disposer
//! （fiber 卸载清除），回调在 fiber Active 时执行。
//!
//! 单线程纪律：无事件循环——宿主注入时钟（`set_timer_clock`）+ 事件循环中
//! 调 `drive_timers()`（CLI 已有 50ms 循环）；到期且 fiber 仍 Active 才执行
//! 回调；fiber 卸载（disposer）清除未到期 timer。

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use dsh_core::*;

mod common;
use common::*;

/// 可控时钟（测试用）。
#[derive(Clone, Default)]
struct FakeClock {
    now: Rc<AtomicU64>,
}

impl FakeClock {
    fn advance(&self, ms: u64) {
        self.now.fetch_add(ms, Ordering::SeqCst);
    }
    fn get(&self) -> u64 {
        self.now.load(Ordering::SeqCst)
    }
}

/// 建一个带可控时钟的 Cordis。
fn cordis_with_clock() -> (Cordis, FakeClock) {
    let cordis = Cordis::new();
    let clock = FakeClock::default();
    let c2 = clock.clone();
    cordis.set_timer_clock(move || c2.get());
    (cordis, clock)
}

/// M41：timeout_async 的 future 形态。
type TimeoutFut = futures_util::future::LocalBoxFuture<'static, Result<(), CordisError>>;

/// timeout：宿主驱动时钟推进后触发；一次性。
#[test]
fn timer_timeout_fires_after_delay() {
    let (cordis, clock) = cordis_with_clock();

    let fired = Rc::new(RefCell::new(0));
    let fired2 = fired.clone();
    let plugin = FnPlugin::new("timer-user", &[], move |ctx, _cfg| {
        let fired3 = fired2.clone();
        ctx.timeout(Rc::new(move || *fired3.borrow_mut() += 1), 100)
            .expect("timeout registers");
        Ok(EffectOutcome::None)
    });
    let fid = cordis.plugin(plugin, json!({})).unwrap();
    assert_eq!(cordis.fiber_state(fid), Some(FiberState::Active));

    // 50ms：未到期 → 不触发
    clock.advance(50);
    cordis.drive_timers();
    assert_eq!(*fired.borrow(), 0, "not due yet");

    // 再 60ms（合计 110 > 100）：触发
    clock.advance(60);
    cordis.drive_timers();
    assert_eq!(*fired.borrow(), 1, "fired once");

    // 一次性：再次驱动不重复触发
    clock.advance(100);
    cordis.drive_timers();
    assert_eq!(*fired.borrow(), 1, "one-shot fired once");

    cordis.unload(fid).unwrap();
}

/// timeout 的 disposer：fiber 卸载清除未到期 timer（生命周期绑定）。
#[test]
fn timer_timeout_disposed_on_unload() {
    let (cordis, clock) = cordis_with_clock();

    let fired = Rc::new(RefCell::new(0));
    let fired2 = fired.clone();
    let plugin = FnPlugin::new("timer-dispose", &[], move |ctx, _cfg| {
        let fired3 = fired2.clone();
        ctx.timeout(Rc::new(move || *fired3.borrow_mut() += 1), 100)
            .expect("timeout registers");
        Ok(EffectOutcome::None)
    });
    let fid = cordis.plugin(plugin, json!({})).unwrap();

    // 卸载 fiber → timer 清除 → 时间推进不触发
    cordis.unload(fid).unwrap();
    clock.advance(200);
    cordis.drive_timers();
    assert_eq!(*fired.borrow(), 0, "timer cleared on unload");
}

/// interval：周期触发（每次到期重新排期）。
#[test]
fn timer_interval_fires_repeatedly() {
    let (cordis, clock) = cordis_with_clock();

    let ticks = Rc::new(RefCell::new(0));
    let ticks2 = ticks.clone();
    let plugin = FnPlugin::new("timer-interval", &[], move |ctx, _cfg| {
        let ticks3 = ticks2.clone();
        ctx.interval(Rc::new(move || *ticks3.borrow_mut() += 1), 50)
            .expect("interval registers");
        Ok(EffectOutcome::None)
    });
    let fid = cordis.plugin(plugin, json!({})).unwrap();

    clock.advance(50);
    cordis.drive_timers();
    assert_eq!(*ticks.borrow(), 1, "first tick");

    clock.advance(50);
    cordis.drive_timers();
    assert_eq!(*ticks.borrow(), 2, "second tick");

    clock.advance(50);
    cordis.drive_timers();
    assert_eq!(*ticks.borrow(), 3, "third tick");

    cordis.unload(fid).unwrap();
}

/// interval 的 disposer：fiber 卸载停止周期触发。
#[test]
fn timer_interval_disposed_on_unload() {
    let (cordis, clock) = cordis_with_clock();

    let ticks = Rc::new(RefCell::new(0));
    let ticks2 = ticks.clone();
    let plugin = FnPlugin::new("timer-interval-dispose", &[], move |ctx, _cfg| {
        let ticks3 = ticks2.clone();
        ctx.interval(Rc::new(move || *ticks3.borrow_mut() += 1), 50)
            .expect("interval registers");
        Ok(EffectOutcome::None)
    });
    let fid = cordis.plugin(plugin, json!({})).unwrap();

    clock.advance(50);
    cordis.drive_timers();
    assert_eq!(*ticks.borrow(), 1);

    cordis.unload(fid).unwrap();
    clock.advance(200);
    cordis.drive_timers();
    assert_eq!(*ticks.borrow(), 1, "interval stopped on unload");
}

/// debounce：delay 内多次调用只执行最后一次（每次调用重置计时）。
#[test]
fn timer_debounce_fires_once_after_idle() {
    let (cordis, clock) = cordis_with_clock();

    // 把 debounced 函数存到共享槽，apply 外调用
    let slot: Rc<RefCell<Option<TimerFn>>> = Rc::new(RefCell::new(None));
    let calls = Rc::new(RefCell::new(Vec::<u32>::new()));

    let slot2 = slot.clone();
    let calls2 = calls.clone();
    let plugin = FnPlugin::new("timer-debounce", &[], move |ctx, _cfg| {
        let calls3 = calls2.clone();
        let (d, _disposer) = ctx
            .debounce(Rc::new(move |v: Value| {
                calls3.borrow_mut().push(v.as_u64().unwrap_or(0) as u32);
            }), 50)
            .expect("debounce creates");
        *slot2.borrow_mut() = Some(d);
        Ok(EffectOutcome::None)
    });
    let _ = cordis.plugin(plugin, json!({})).unwrap();

    let debounced = slot.borrow().as_ref().expect("debounced set").clone();
    // 三次调用：t=0 调 1，t=20 调 2，t=40 调 3（每次重置 50ms 计时）
    debounced(json!(1));
    clock.advance(20);
    debounced(json!(2));
    clock.advance(20);
    debounced(json!(3));
    // t=40，距最后调用 0ms → 不触发
    cordis.drive_timers();
    assert_eq!(*calls.borrow(), Vec::<u32>::new(), "debounce pending");

    // 推进 60ms（t=100，距最后调用 60 > 50）→ 只触发最后一次
    clock.advance(60);
    cordis.drive_timers();
    assert_eq!(*calls.borrow(), vec![3u32], "debounced to last call");
}

/// debounce 的 disposer：fiber 卸载取消 pending 执行。
#[test]
fn timer_debounce_disposed_on_unload() {
    let (cordis, clock) = cordis_with_clock();

    let calls = Rc::new(RefCell::new(0));
    let calls2 = calls.clone();
    let slot: Rc<RefCell<Option<TimerFn>>> = Rc::new(RefCell::new(None));
    let slot2 = slot.clone();
    let plugin = FnPlugin::new("timer-debounce-dispose", &[], move |ctx, _cfg| {
        let calls3 = calls2.clone();
        let (d, _disposer) = ctx
            .debounce(Rc::new(move |_: Value| *calls3.borrow_mut() += 1), 50)
            .expect("debounce creates");
        *slot2.borrow_mut() = Some(d);
        Ok(EffectOutcome::None)
    });
    let fid = cordis.plugin(plugin, json!({})).unwrap();
    let debounced = slot.borrow().as_ref().expect("debounced set").clone();
    debounced(json!(1));

    // 卸载 → pending debounce 取消 → 推进不触发
    cordis.unload(fid).unwrap();
    clock.advance(200);
    cordis.drive_timers();
    assert_eq!(*calls.borrow(), 0, "debounce cancelled on unload");
}

/// throttle：leading edge 立即执行；窗口内调用排 trailing（delay 后执行最后
/// 一次，对齐 Cordis 默认 noTrailing=false）。
#[test]
fn timer_throttle_fires_leading_and_trailing() {
    let (cordis, clock) = cordis_with_clock();

    let calls = Rc::new(RefCell::new(Vec::<u32>::new()));
    let calls2 = calls.clone();
    let slot: Rc<RefCell<Option<TimerFn>>> = Rc::new(RefCell::new(None));
    let slot2 = slot.clone();
    let plugin = FnPlugin::new("timer-throttle", &[], move |ctx, _cfg| {
        let calls3 = calls2.clone();
        let (t, _disposer) = ctx
            .throttle(Rc::new(move |v: Value| {
                calls3.borrow_mut().push(v.as_u64().unwrap_or(0) as u32);
            }), 100)
            .expect("throttle creates");
        *slot2.borrow_mut() = Some(t);
        Ok(EffectOutcome::None)
    });
    let _ = cordis.plugin(plugin, json!({})).unwrap();
    let throttled = slot.borrow().as_ref().expect("throttled set").clone();

    throttled(json!(1)); // 立即执行（leading）
    throttled(json!(2)); // 100ms 窗口内 → 不立即，排 trailing

    // leading 已执行；trailing 未到期
    assert_eq!(*calls.borrow(), vec![1u32], "leading call executed");

    // 推进 150ms → trailing 执行窗口内最后一次调用 (2)
    clock.advance(150);
    cordis.drive_timers();
    assert_eq!(*calls.borrow(), vec![1u32, 2u32], "trailing call executed");
}

/// M41：`timeout_async(delay)`（等价 `await ctx.timeout(delay): Promise`）——
/// delay 后 resolve。在插件 apply（active fiber）内构造 future，apply 外 await。
#[tokio::test]
async fn timer_timeout_async_resolves_after_delay() {
    let (cordis, clock) = cordis_with_clock();

    let slot: Rc<RefCell<Option<TimeoutFut>>> = Rc::new(RefCell::new(None));
    let slot2 = slot.clone();
    let plugin = FnPlugin::new("timer-timeout-async", &[], move |ctx, _cfg| {
        let fut = ctx.timeout_async(100);
        *slot2.borrow_mut() = Some(fut);
        Ok(EffectOutcome::None)
    });
    let _fid = cordis.plugin(plugin, json!({})).unwrap();

    let mut fut = slot.borrow_mut().take().expect("fut set");
    // 同步驱动：推进时钟 + 手动 poll future（noop waker；current_thread 单任务）
    let mut result: Option<Result<(), CordisError>> = None;
    for _ in 0..6 {
        if result.is_some() {
            break;
        }
        clock.advance(30);
        cordis.drive_timers();
        let waker = futures_util::task::noop_waker();
        let mut cx = std::task::Context::from_waker(&waker);
        if let std::task::Poll::Ready(r) = futures_util::Future::poll(
            std::pin::Pin::new(&mut fut),
            &mut cx,
        ) {
            result = Some(r);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let r = result.expect("timeout_async resolved within budget");
    assert_eq!(r.unwrap(), (), "timeout_async resolves after delay");
}

/// M41：`timeout_async` 在 fiber 卸载后返回 Err（对齐 Promise reject on dispose）。
#[tokio::test]
async fn timer_timeout_async_rejects_on_unload() {
    let (cordis, _clock) = cordis_with_clock();

    let slot: Rc<RefCell<Option<TimeoutFut>>> = Rc::new(RefCell::new(None));
    let slot2 = slot.clone();
    let plugin = FnPlugin::new("timer-timeout-async-reject", &[], move |ctx, _cfg| {
        let fut = ctx.timeout_async(100000);
        *slot2.borrow_mut() = Some(fut);
        Ok(EffectOutcome::None)
    });
    let fid = cordis.plugin(plugin, json!({})).unwrap();

    // 卸载 fiber → disposer 置 cancelled → future poll 返回 Err
    cordis.unload(fid).unwrap();
    let mut fut = slot.borrow_mut().take().expect("fut set");
    let waker = futures_util::task::noop_waker();
    let mut cx = std::task::Context::from_waker(&waker);
    let result: std::task::Poll<Result<(), CordisError>> =
        futures_util::Future::poll(std::pin::Pin::new(&mut fut), &mut cx);
    match result {
        std::task::Poll::Ready(Err(e)) => {
            assert!(e.to_string().contains("disposed"), "{e}");
        }
        other => panic!("expected Ready(Err) after unload, got {other:?}"),
    }
}

/// M41：`interval_ticks(delay)`（等价 `for await (const _ of ctx.interval())`）——
/// 每 delay 产出一个 tick。手动 poll 驱动（noop waker + 推进时钟）。
#[tokio::test]
async fn timer_interval_ticks_yields_periodically() {
    let (cordis, clock) = cordis_with_clock();

    let slot: Rc<RefCell<Option<IntervalTicks>>> = Rc::new(RefCell::new(None));
    let slot2 = slot.clone();
    let plugin = FnPlugin::new("timer-interval-ticks", &[], move |ctx, _cfg| {
        let stream = ctx.interval_ticks(50);
        *slot2.borrow_mut() = Some(stream);
        Ok(EffectOutcome::None)
    });
    let _fid = cordis.plugin(plugin, json!({})).unwrap();

    let mut stream = slot.borrow_mut().take().expect("stream set");
    use futures_util::Stream;
    let waker = futures_util::task::noop_waker();
    let mut cx = std::task::Context::from_waker(&waker);
    let mut ticks = 0;
    // 推进时钟并 poll：每 50ms 一个 tick，共 3 个
    for _ in 0..6 {
        clock.advance(25);
        cordis.drive_timers();
        loop {
            match Stream::poll_next(std::pin::Pin::new(&mut stream), &mut cx) {
                std::task::Poll::Ready(Some(())) => ticks += 1,
                std::task::Poll::Ready(None) => break,
                std::task::Poll::Pending => break,
            }
        }
    }
    assert_eq!(ticks, 3, "three ticks over 150ms (25ms * 6 polls)");
}

/// M46：`set_timeout`/`set_interval` 别名——语义等价 timeout/interval
/// （对齐 `vendor/timer` 的 deprecated `setTimeout`/`setInterval`）。
#[test]
fn timer_set_timeout_alias_fires_once() {
    let (cordis, clock) = cordis_with_clock();

    let fired = Rc::new(RefCell::new(0));
    let fired2 = fired.clone();
    let plugin = FnPlugin::new("timer-set-timeout", &[], move |ctx, _cfg| {
        let fired3 = fired2.clone();
        ctx.set_timeout(Rc::new(move || *fired3.borrow_mut() += 1), 100)
            .expect("set_timeout registers");
        Ok(EffectOutcome::None)
    });
    let fid = cordis.plugin(plugin, json!({})).unwrap();

    clock.advance(50);
    cordis.drive_timers();
    assert_eq!(*fired.borrow(), 0, "not due yet");

    clock.advance(60);
    cordis.drive_timers();
    assert_eq!(*fired.borrow(), 1, "alias fired once");

    // 一次性：再次驱动不重复
    clock.advance(100);
    cordis.drive_timers();
    assert_eq!(*fired.borrow(), 1, "alias one-shot");

    cordis.unload(fid).unwrap();
}

/// M46：`set_interval` 别名周期触发 + 卸载停止。
#[test]
fn timer_set_interval_alias_repeats() {
    let (cordis, clock) = cordis_with_clock();

    let ticks = Rc::new(RefCell::new(0));
    let ticks2 = ticks.clone();
    let plugin = FnPlugin::new("timer-set-interval", &[], move |ctx, _cfg| {
        let ticks3 = ticks2.clone();
        ctx.set_interval(Rc::new(move || *ticks3.borrow_mut() += 1), 50)
            .expect("set_interval registers");
        Ok(EffectOutcome::None)
    });
    let fid = cordis.plugin(plugin, json!({})).unwrap();

    clock.advance(50);
    cordis.drive_timers();
    assert_eq!(*ticks.borrow(), 1, "first tick");

    clock.advance(50);
    cordis.drive_timers();
    assert_eq!(*ticks.borrow(), 2, "second tick");

    cordis.unload(fid).unwrap();
    clock.advance(200);
    cordis.drive_timers();
    assert_eq!(*ticks.borrow(), 2, "stopped on unload");
}

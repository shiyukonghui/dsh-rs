//! M64：`fiber.update(config, noSave)` 的 veto 语义与 noSave 参数。
//!
//! Cordis：`update(config, noSave=false)` → `waterfall('internal/update', config,
//! noSave, next => restart())`。监听器不调 `next`（或 return bail）则**veto**，
//! 内建 restart 不执行；`noSave` 提示持久化钩子跳过写回（loader 写回监听器依赖）。
//!
//! dsh-core 以 `fid`（Value::u64）替代 Cordis 的 `this=Fiber`，参数形状
//! `[fid, config, noSave]`（loader `write_back` 依赖该形状）。veto 由 waterfall 链
//! 保证；本测试锁定该语义。监听器须在插件 apply 内注册（需要 active fiber）。

mod common;
use common::*;

use std::cell::RefCell;
use std::rc::Rc;

use dsh_core::*;

/// 插件：apply 时 push 日志（重启会再 apply → 日志追加）。
fn counting_plugin(cordis: &Cordis, name: &'static str, log: &Rc<RefCell<Vec<String>>>) -> FiberHandle {
    let log = log.clone();
    let p = FnPlugin::new(name, &[], move |_ctx, _cfg| {
        push(&log, format!("apply:{name}"));
        Ok(EffectOutcome::None)
    });
    cordis.plugin(p, json!({"v": 1})).unwrap()
}

/// 宿主插件：apply 内注册 `internal/update` 拦截（veto / 放行），并按 `behavior`
/// 记录到日志。
fn host_with_update_listener(
    cordis: &Cordis,
    log: &Rc<RefCell<Vec<String>>>,
    veto: bool,
) -> FiberHandle {
    let log = log.clone();
    let p = FnPlugin::new("host", &[], move |ctx, _cfg| {
        let l = log.clone();
        let listener = make_listener(move |ctx2, args, next| match (veto, next) {
            // veto：不调 next，return bail true → 内建 restart 被短路。
            (true, _) => {
                push(&l, "veto");
                HookResult::Returned(Some(json!(true)))
            }
            // 放行：调 next 委托内建 restart。
            (false, Some(n)) => {
                let r = n(ctx2, args);
                HookResult::Returned(r)
            }
            (false, None) => HookResult::Continue,
        });
        ctx.on("internal/update", listener).unwrap();
        Ok(EffectOutcome::None)
    });
    cordis.plugin(p, json!({})).unwrap()
}

/// internal/update 监听器**不调 next**（return bail）→ veto，fiber 不重启。
#[test]
fn update_is_vetoed_when_listener_does_not_continue() {
    let log = log();
    let cordis = Cordis::new();
    let fid = counting_plugin(&cordis, "p", &log);
    // 先注册 veto 拦截宿主。
    host_with_update_listener(&cordis, &log, true);
    // 清掉 apply 日志，聚焦 update 后的行为。
    assert_eq!(snapshot(&log), vec!["apply:p"]);

    // veto 的 update 不应导致再次 apply（fiber 不重启）。
    cordis.update(fid, json!({"v": 2})).unwrap();
    // 仅 veto 日志，无第二次 apply:p。
    assert_eq!(snapshot(&log), vec!["apply:p", "veto"]);
}

/// 监听器调用 next → 放行，fiber 重启（再次 apply）。
#[test]
fn update_restarts_when_listener_continues() {
    let log = log();
    let cordis = Cordis::new();
    let fid = counting_plugin(&cordis, "p", &log);
    host_with_update_listener(&cordis, &log, false);
    assert_eq!(snapshot(&log), vec!["apply:p"]);

    cordis.update(fid, json!({"v": 2})).unwrap();
    // 放行 → 重启 → 再次 apply:p。
    assert_eq!(snapshot(&log), vec!["apply:p", "apply:p"]);
}

/// 无监听器时 update 正常重启（默认 inner=restart）。
#[test]
fn update_restarts_without_listeners() {
    let log = log();
    let cordis = Cordis::new();
    let fid = counting_plugin(&cordis, "p", &log);

    cordis.update(fid, json!({"v": 2})).unwrap();
    assert_eq!(snapshot(&log), vec!["apply:p", "apply:p"]);
}

/// veto 的 update **不应**把 config 生效（`fiber_config` 保持旧值）——
/// Cordis `this.config` 在 waterfall inner 内赋值，veto 时更新被短路。
#[test]
fn vetoed_update_does_not_apply_config() {
    let log = log();
    let cordis = Cordis::new();
    let fid = counting_plugin(&cordis, "p", &log);
    host_with_update_listener(&cordis, &log, true);
    assert_eq!(cordis.fiber_config(fid), Some(json!({"v": 1})));

    cordis.update(fid, json!({"v": 2})).unwrap();
    // veto → 不重启、且 config 未生效（保持 v=1）。
    assert_eq!(snapshot(&log), vec!["apply:p", "veto"]);
    assert_eq!(cordis.fiber_config(fid), Some(json!({"v": 1})));
}

/// 放行的 update 使新 config 生效。
#[test]
fn continued_update_applies_config() {
    let log = log();
    let cordis = Cordis::new();
    let fid = counting_plugin(&cordis, "p", &log);
    host_with_update_listener(&cordis, &log, false);
    assert_eq!(cordis.fiber_config(fid), Some(json!({"v": 1})));

    cordis.update(fid, json!({"v": 2})).unwrap();
    // 放行 → 重启 + config 生效。
    assert_eq!(cordis.fiber_config(fid), Some(json!({"v": 2})));
}

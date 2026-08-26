//! A3 动态 check spike：`provide_with(name, value, check)` 谓词在**运行时**再求值
//! 触发点与 cordis 对齐（reflect.ts notify → `_checkImpl`）：
//! - provide-while-Active / unprovide（disposer）→ notify（已有）；
//! - provider 重载（update_with → 卸载 run_unload 跑 provide disposer → 重 apply 再 provide）
//!   → 依赖方按谓词重算（check 翻转生效）；
//! - 纯 check 翻转**无 notify** → 依赖方保持原状态（cordis 非反应式同位）。
//!   静态 check 门已被 m7_await::await_gated_by_check_predicate + scenario-10 golden 锁定；
//!   本测为动态翻转面（m-series，A3 spike）。
#![allow(clippy::arc_with_non_send_sync)]

mod common;
use common::*;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use dsh_core::*;
use dsh_loader::*;

/// 动态 check 翻转：谓词 false → 依赖方 Pending；provider 重载后谓词 true → 依赖方激活；
/// false 重载 → 依赖方返回 Pending；纯翻转（无重载）→ 非反应式（cordis 同位）。
#[tokio::test]
async fn dynamic_check_flip_gates_dependent_via_provider_reload() {
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    let flag = Arc::new(AtomicBool::new(false));

    let provider = FnPlugin::new("svc", &[], {
        let flag = flag.clone();
        move |ctx, _cfg| {
            let f = flag.clone();
            ctx.provide_with("svc", Arc::new(json!("v1")), Some(Box::new(move || f.load(Ordering::SeqCst))))?;
            Ok(EffectOutcome::None)
        }
    });
    loader.register_plugin("svc", Arc::new(provider));
    loader.register_plugin(
        "consumer",
        Arc::new(FnPlugin::new("consumer", &["svc"], |_ctx, _cfg| Ok(EffectOutcome::None))),
    );

    loader.create(EntryOptions::new("s", "svc")).unwrap();
    loader.create(EntryOptions::new("c", "consumer")).unwrap();
    loader.await_idle().await.unwrap();

    let sf = loader.fiber("s").unwrap();
    let cf = loader.fiber("c").unwrap();

    // 初始 check=false → provider Active，consumer Pending（静态门，已被 m7/golden 锁定）
    assert_eq!(cordis.fiber_state(sf), Some(FiberState::Active));
    assert_eq!(cordis.fiber_state(cf), Some(FiberState::Pending));

    // 纯 check 翻转（无 notify 触发点）→ consumer 仍 Pending（cordis 非反应式同位）
    flag.store(true, Ordering::SeqCst);
    assert_eq!(cordis.fiber_state(cf), Some(FiberState::Pending), "no notify → non-reactive (cordis parity)");

    // provider 重载（update_with → 卸载 re-provide → notify）→ check=true → consumer 激活
    cordis.update_with(sf, json!({"v": 2}), false).unwrap();
    loader.await_idle().await.unwrap();
    assert_eq!(
        cordis.fiber_state(cf),
        Some(FiberState::Active),
        "check flipped true + reload notify → dependent activates"
    );

    // check=false + 重载 → consumer 返回 Pending
    flag.store(false, Ordering::SeqCst);
    cordis.update_with(sf, json!({"v": 3}), false).unwrap();
    loader.await_idle().await.unwrap();
    assert_eq!(
        cordis.fiber_state(cf),
        Some(FiberState::Pending),
        "check flipped false + reload notify → dependent deactivates"
    );

    // 再次 true + 重载 → 激活（可往返）
    flag.store(true, Ordering::SeqCst);
    cordis.update_with(sf, json!({"v": 4}), false).unwrap();
    loader.await_idle().await.unwrap();
    assert_eq!(cordis.fiber_state(cf), Some(FiberState::Active));
}

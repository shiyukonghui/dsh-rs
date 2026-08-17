//! §M7 loader async：`Loader::await_idle`（EntryTree.await 等价）。

// 同 dsh-core：单线程运行时，`Arc` 仅共享所有权（见 dsh-core lib.rs 说明）。
#![allow(clippy::arc_with_non_send_sync)]

mod common;
use common::*;

use std::sync::Arc;

use dsh_core::*;
use dsh_loader::*;

/// await_idle：入口树稳定后立即返回（同步核心）。
#[tokio::test]
async fn await_idle_returns_when_tree_idle() {
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    loader.register_plugin("a", Arc::new(FnPlugin::noop("a")));
    loader.register_plugin("b", Arc::new(FnPlugin::noop("b")));

    loader.create(EntryOptions::new("e1", "a")).unwrap();
    loader.create(EntryOptions::new("e2", "b")).unwrap();

    // 全部入口 fiber 已是 Active；await_idle 不阻塞
    loader.await_idle().await.unwrap();

    assert_eq!(loader.entries().len(), 2);
    for e in loader.entries() {
        assert!(!e.disabled);
        let f = e.fiber.unwrap();
        assert_eq!(cordis.fiber_state(f), Some(FiberState::Active));
    }
}

/// await_idle：依赖门控的入口收敛后全部 Active（provider 后加载 consumer）。
#[tokio::test]
async fn await_idle_after_dependency_gate() {
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    // provider 插件：apply 时提供 svc 服务
    let provider = FnPlugin::new("svc", &[], |ctx, _cfg| {
        ctx.provide("svc", Arc::new(json!("v1")))?;
        Ok(EffectOutcome::None)
    });
    loader.register_plugin("svc", Arc::new(provider));
    // consumer 依赖 "svc"：先建 consumer（依赖缺失保持 Pending），再建 provider 触发加载
    let consumer = FnPlugin::new("consumer", &["svc"], |_ctx, _cfg| Ok(EffectOutcome::None));
    loader.register_plugin("consumer", Arc::new(consumer));

    loader
        .create(EntryOptions {
            id: "c".to_string(),
            name: "consumer".to_string(),
            ..EntryOptions::new("c", "consumer")
        })
        .unwrap();
    // consumer 因缺依赖未加载（Pending）
    let c_fiber = loader.fiber("c").unwrap();
    assert_eq!(cordis.fiber_state(c_fiber), Some(FiberState::Pending));

    loader.create(EntryOptions::new("s", "svc")).unwrap();
    loader.await_idle().await.unwrap();

    // provider 提供 svc 后 consumer 也加载完成
    assert_eq!(cordis.fiber_state(loader.fiber("s").unwrap()), Some(FiberState::Active));
    assert_eq!(cordis.fiber_state(c_fiber), Some(FiberState::Active));
}

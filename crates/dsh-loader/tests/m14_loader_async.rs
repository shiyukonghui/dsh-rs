//! M14：loader 事务 allSettled（async）——对应 Cordis `EntryGroup.update(config)`：
//! 全部 create 都执行（一个失败不阻断其他）、错误聚合（单失败原错误 / 多失败
//! AggregateError）、失败整事务回滚（移除新建 + 重建旧配置）。
#![allow(clippy::arc_with_non_send_sync)]

mod common;
use common::*;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use dsh_core::*;
use dsh_loader::*;

fn plugin_a(log: &Rc<RefCell<Vec<String>>>) -> Arc<dyn Plugin> {
    let log = log.clone();
    Arc::new(FnPlugin::new("a", &[], move |_ctx, config| {
        push(&log, format!("apply:a:{}", config.get("k").and_then(|v| v.as_i64()).unwrap_or(0)));
        Ok(EffectOutcome::None)
    }))
}

fn options(id: &str, name: &str, config: Value) -> EntryOptions {
    let mut o = EntryOptions::new(id, name);
    o.config = config;
    o
}

/// sync_async 部分失败：1 个未知插件失败，**create 阶段 allSettled**（e1/e3 都
/// 被尝试并 apply），最终整事务回滚（新建全部移除）；错误聚合（1 个失败 = 原错误）。
#[tokio::test]
async fn sync_async_partial_failure_keeps_others() {
    let log = log();
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    loader.register_plugin("a", plugin_a(&log));

    let entries = vec![
        options("e1", "a", json!({"k": 1})),
        options("e2", "zzz", json!({})), // 未知插件 → 失败
        options("e3", "a", json!({"k": 3})),
    ];
    let err = loader.sync_async(&entries).await.unwrap_err();
    assert_eq!(err.errors.len(), 1, "single failure");
    assert!(err.errors[0].to_string().contains("unknown plugin"), "{:?}", err.errors[0]);

    // allSettled：e1/e3 在 e2 失败前都被创建并 apply（不阻断）
    assert_eq!(snapshot(&log), vec!["apply:a:1", "apply:a:3"]);
    // 但整事务回滚：新建的 e1/e3 已被移除
    assert!(loader.fiber("e1").is_none(), "rollback removed e1");
    assert!(loader.fiber("e2").is_none());
    assert!(loader.fiber("e3").is_none(), "rollback removed e3");
}

/// sync_async 多失败：错误聚合为 AggregateError（全部失败都保留）。
#[tokio::test]
async fn sync_async_multiple_failures_aggregate() {
    let log = log();
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    loader.register_plugin("a", plugin_a(&log));

    let entries = vec![
        options("e1", "zzz", json!({})),
        options("e2", "a", json!({"k": 2})),
        options("e3", "yyy", json!({})),
    ];
    let err = loader.sync_async(&entries).await.unwrap_err();
    assert_eq!(err.errors.len(), 2, "aggregate both failures: {:?}", err.errors);
    // allSettled：e2 被创建并 apply（不阻断）
    assert_eq!(snapshot(&log), vec!["apply:a:2"]);
    // 整事务回滚：e2 也被移除
    assert!(loader.fiber("e2").is_none(), "rollback removed e2");
}

/// sync_async 失败回滚：旧配置在运行 → sync 新配置含失败 → 新建的移除、旧配置重建。
#[tokio::test]
async fn sync_async_rollback_restores_old_config() {
    let log = log();
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    loader.register_plugin("a", plugin_a(&log));

    // 初始：e1(a,k=1) 运行中
    loader.sync_async(&[options("e1", "a", json!({"k": 1}))]).await.unwrap();
    let old_fid = loader.fiber("e1").unwrap();
    assert_eq!(snapshot(&log), vec!["apply:a:1"]);

    // sync 新配置：e1 热更（k=2）+ 新建 e2（成功）+ e3（失败）
    let entries = vec![
        options("e1", "a", json!({"k": 2})),
        options("e2", "a", json!({"k": 4})),
        options("e3", "zzz", json!({})),
    ];
    let err = loader.sync_async(&entries).await.unwrap_err();
    assert_eq!(err.errors.len(), 1, "{:?}", err.errors);

    // 回滚：e2（新建）已移除；e1 回到旧配置 k=1 且仍运行
    assert!(loader.fiber("e2").is_none());
    let e1_fid = loader.fiber("e1").unwrap();
    assert_eq!(cordis.fiber_state(e1_fid), Some(FiberState::Active));
    let cfg = loader
        .state
        .borrow()
        .entries
        .get("e1")
        .map(|e| e.options.config.clone())
        .unwrap();
    assert_eq!(cfg, json!({"k": 1}), "e1 options restored to old config");
    let _ = old_fid;
}

/// sync_async 全成功：更新既有（热更）、创建新增、移除缺席。
#[tokio::test]
async fn sync_async_success_removes_absent() {
    let log = log();
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    loader.register_plugin("a", plugin_a(&log));

    loader.sync_async(&[options("e1", "a", json!({"k": 1}))]).await.unwrap();

    // 新配置：e1 热更（k=2）、e2 新增
    let entries = vec![
        options("e1", "a", json!({"k": 2})),
        options("e2", "a", json!({"k": 3})),
    ];
    loader.sync_async(&entries).await.unwrap();

    let e1_fid = loader.fiber("e1").unwrap();
    assert_eq!(cordis.fiber_state(e1_fid), Some(FiberState::Active));
    assert_eq!(loader.fiber("e2").map(|f| cordis.fiber_state(f)), Some(Some(FiberState::Active)));
    assert_eq!(snapshot(&log), vec!["apply:a:1", "apply:a:2", "apply:a:3"]);

    // 再 sync 空配置 → 全部移除
    loader.sync_async(&[]).await.unwrap();
    assert!(loader.fiber("e1").is_none());
    assert!(loader.fiber("e2").is_none());
}

/// create_async / update_async / remove_async：async 生命周期基本路径。
#[tokio::test]
async fn async_entry_lifecycle() {
    let log = log();
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    loader.register_plugin("a", plugin_a(&log));

    let id = loader.create_async(options("e", "a", json!({"k": 1}))).await.unwrap();
    let fid = loader.fiber(&id).unwrap();
    assert_eq!(cordis.fiber_state(fid), Some(FiberState::Active));

    loader.update_async(&id, options("e", "a", json!({"k": 2}))).await.unwrap();
    let fid2 = loader.fiber(&id).unwrap();
    assert_eq!(cordis.fiber_state(fid2), Some(FiberState::Active));
    assert_eq!(snapshot(&log), vec!["apply:a:1", "apply:a:2"]);

    loader.remove_async(&id).await.unwrap();
    assert!(loader.fiber(&id).is_none());
    assert_eq!(cordis.fiber_state(fid2), Some(FiberState::Disposed));
}

/// sync_async 重复 id：与同步 sync 一致报错。
#[tokio::test]
async fn sync_async_duplicate_id_fails() {
    let log = log();
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    loader.register_plugin("a", plugin_a(&log));

    let entries = vec![
        options("e1", "a", json!({})),
        options("e1", "a", json!({})),
    ];
    let err = loader.sync_async(&entries).await.unwrap_err();
    assert!(err.errors.iter().any(|e| e.to_string().contains("duplicate")), "{:?}", err.errors);
}

/// Include::load_async：读取 YAML → sync_async 事务装载；部分失败 → 回滚。
#[tokio::test]
async fn include_load_async_transaction() {
    let dir = std::env::temp_dir().join(format!("dsh-m14-include-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.yml");
    std::fs::write(&path, "- id: e1\n  name: a\n  config: { k: 1 }\n- id: e2\n  name: zzz\n").unwrap();

    let log = log();
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    loader.register_plugin("a", plugin_a(&log));
    let include = Include::new(&loader, &path, vec![]);

    // e2 未知插件 → 事务失败 → 回滚：e1 也被移除
    let err = include.load_async().await.unwrap_err();
    assert_eq!(err.errors.len(), 1, "{:?}", err.errors);
    assert!(loader.fiber("e1").is_none(), "rollback removed e1");
    assert_eq!(snapshot(&log), vec!["apply:a:1"]);

    // 修复文件后重载成功
    std::fs::write(&path, "- id: e1\n  name: a\n  config: { k: 1 }\n- id: e2\n  name: a\n  config: { k: 2 }\n").unwrap();
    include.refresh_async().await.unwrap();
    assert_eq!(loader.fiber("e1").map(|f| cordis.fiber_state(f)), Some(Some(FiberState::Active)));
    assert_eq!(loader.fiber("e2").map(|f| cordis.fiber_state(f)), Some(Some(FiberState::Active)));
    assert_eq!(snapshot(&log), vec!["apply:a:1", "apply:a:1", "apply:a:2"]);

    std::fs::remove_dir_all(&dir).ok();
}

/// M27：Group apply 异步化（`EffectOutcome::Await`）——async 路径下 Group 在
/// 子入口全部 Active 后才 Active（等价 TS `[Service.init]` await update）。
#[tokio::test]
async fn group_await_children_before_active() {
    let log = log();
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    loader.register_plugin("a", plugin_a(&log));

    let mut g = options("g", "g", json!([{ "id": "c1", "name": "a", "config": {"k": 1} }, { "id": "c2", "name": "a", "config": {"k": 2} }]));
    g.group = true;
    loader.create_async(g).await.unwrap();

    // Group fiber 存在；子入口全部 Active
    let gfid = loader.fiber("g").unwrap();
    let c1 = loader.fiber("c1").unwrap();
    let c2 = loader.fiber("c2").unwrap();
    assert_eq!(cordis.fiber_state(c1), Some(FiberState::Active));
    assert_eq!(cordis.fiber_state(c2), Some(FiberState::Active));
    assert_eq!(cordis.fiber_state(gfid), Some(FiberState::Active));
    assert_eq!(snapshot(&log), vec!["apply:a:1", "apply:a:2"]);

    // 卸载 → Group 先 Unloading，子入口递归 stop
    loader.remove_async("g").await.unwrap();
    assert!(loader.fiber("c1").is_none());
    assert!(loader.fiber("c2").is_none());
    assert_eq!(cordis.fiber_state(c1), Some(FiberState::Disposed));
    assert_eq!(cordis.fiber_state(c2), Some(FiberState::Disposed));
    assert_eq!(cordis.fiber_state(gfid), Some(FiberState::Disposed));
}

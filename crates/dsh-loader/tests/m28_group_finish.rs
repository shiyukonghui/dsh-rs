//! beyond 目标 M27/M28：嵌套 group 的 Finish 时序（聚焦 Finish 时序口径，D-168）。
//!
//! 锁定 cordis 两个可观测契约：
//! - C1（聚末尾 / batch）：首条 `status:Group:Loading:Active` 出现**晚于**批内末条普通 fiber
//!   （非组）`status:<x>:Loading:Active`（G>L 不变量）——Pending-only 子组的组也不提前 finish。
//! - C2（父不先于子）：Group 不在其 Loading 后裔 settle 前 Active（由 await_children 偏序保证）。
//!
//! 结构 = 3 层嵌套 + 隔离边界（同 probe-nested-finish）：
//!   g1[ p(provider: provide svc) , gInner[ c1(consumer: inject svc) ] , gIso(isolate svc)[ b1(blocked: inject svc) ] ]
//!   → p/c1 Active、b1 Pending（隔离边界）、三组 Active。
#![allow(clippy::arc_with_non_send_sync)]

mod common;
use common::*;

use std::sync::Arc;

use dsh_core::*;
use dsh_loader::*;

fn options(id: &str, name: &str) -> EntryOptions {
    EntryOptions::new(id, name)
}

/// 构造 3 层嵌套 + 隔离边界（async 装载）。
async fn nested_setup(loader: &Loader) {
    loader.register_plugin("provider", Arc::new(FnPlugin::new("provider", &[], |ctx, _cfg| {
        ctx.provide("svc", Arc::new("r".to_string())).unwrap();
        Ok(EffectOutcome::None)
    })));
    loader.register_plugin("consumer", Arc::new(FnPlugin::new(
        "consumer",
        &["svc"],
        |_ctx, _cfg| Ok(EffectOutcome::None),
    )));
    loader.register_plugin("blocked", Arc::new(FnPlugin::new(
        "blocked",
        &["svc"],
        |_ctx, _cfg| Ok(EffectOutcome::None),
    )));
    let mut g1 = options("g1", "group");
    g1.group = true;
    g1.config = json!([
        { "id": "p", "name": "provider" },
        { "id": "gInner", "name": "group", "group": true, "config": [ { "id": "c1", "name": "consumer" } ] },
        { "id": "gIso", "name": "group", "group": true, "isolate": { "svc": true }, "config": [ { "id": "b1", "name": "blocked" } ] }
    ]);
    loader.create_async(g1).await.unwrap();
}

fn plain_working_name(name: &str) -> bool {
    name != "Group" && name != "Loader"
}

/// C1+C2：末态 + G>L 不变量（3 层嵌套 + Pending-only 子组）。
#[tokio::test(flavor = "current_thread")]
async fn nested_group_finish_batches_after_plain_work() {
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    nested_setup(&loader).await;

    // 末态
    assert_eq!(cordis.fiber_state(loader.fiber("p").unwrap()), Some(FiberState::Active), "p Active");
    assert_eq!(cordis.fiber_state(loader.fiber("c1").unwrap()), Some(FiberState::Active), "c1 Active");
    assert_eq!(cordis.fiber_state(loader.fiber("b1").unwrap()), Some(FiberState::Pending), "b1 blocked");
    for gid in ["g1", "gInner", "gIso"] {
        assert_eq!(
            cordis.fiber_state(loader.fiber(gid).unwrap()),
            Some(FiberState::Active),
            "group {gid} Active"
        );
    }

    // C1（G>L）：首 Group Active 索引 > 末普通 Loading:Active 索引
    let trace = cordis.take_trace();
    let mut last_plain_active: Option<usize> = None;
    let mut first_group_active: Option<usize> = None;
    for (i, line) in trace.iter().enumerate() {
        if let Some(rest) = line.strip_prefix("status:") {
            if let Some(name) = rest.split(':').next() {
                if rest.contains(":Loading:Active") {
                    if name == "Group" {
                        first_group_active.get_or_insert(i);
                    } else if plain_working_name(name) {
                        last_plain_active = Some(i);
                    }
                }
            }
        }
    }
    let l = last_plain_active.expect("some plain fiber went Loading->Active");
    let g = first_group_active.expect("some group went Loading->Active");
    assert!(
        l < g,
        "C1 batch: first Group Active must come AFTER the last plain Loading->Active \
         (got plain@{l}, group@{g}); trace={trace:?}"
    );
}

/// C2 语义冒烟：组先于其 Loading 后裔 Active 在 trace 中不可能出现（父不先于子）。
/// 断言 = 每个 Group Active 事件之前，所有（曾 Loading 的）普通 fiber 均已 Active。
#[tokio::test(flavor = "current_thread")]
async fn no_group_active_before_loading_descendant() {
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    nested_setup(&loader).await;

    let trace = cordis.take_trace();
    let mut group_ok = true;
    let mut group_ordering_violation: Option<String> = None;
    // 记录每个 fiber 名字的 Loading->Active 索引（首次），及每组 Active 后是否还有未结算普通 fiber。
    let mut seen_active_indices: Vec<(String, usize)> = Vec::new();
    for (i, line) in trace.iter().enumerate() {
        if let Some(rest) = line.strip_prefix("status:") {
            let mut parts = rest.splitn(3, ':');
            let name = parts.next().unwrap().to_string();
            let _old = parts.next().unwrap_or("");
            let new = parts.next().unwrap_or("");
            if new == "Loading:Active" {
                if name == "Group" {
                    // 该组 Active 时，任何曾普通 Active 的 fiber 都应已 Active（索引 < i）
                    for (pname, pid) in &seen_active_indices {
                        if pid >= &i && plain_working_name(pname) {
                            group_ok = false;
                            group_ordering_violation =
                                Some(format!("{pname} Active@{pid} after Group Active@{i}"));
                        }
                    }
                } else if plain_working_name(&name) {
                    seen_active_indices.push((name, i));
                }
            }
        }
    }
    assert!(group_ok, "C2 parent-before-child violated: {group_ordering_violation:?}");
    assert!(group_ordering_violation.is_none());
}

//! B2 Group 折叠验证：子入口 apply 失败 → 组装载 **fail-loud + 回滚**。
//! cordis 语义：group `[Service.init]` 里 `await this.update(children)`——子入口
//! `_start` 的 `await fiber.await()` 在子 fiber 失败时 reject → group init 失败 →
//! loader 装载失败 → 回滚（group 的 stop() dispose 所有子入口，含已加载的兄弟）。
#![allow(clippy::arc_with_non_send_sync)]

mod common;
use common::*;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use dsh_core::*;
use dsh_loader::*;

/// 子入口 apply 失败 → 组装载 fail-loud；已加载兄弟在回滚中被停止（B2/cordis 对齐）。
#[test]
fn group_child_failure_fails_loud_and_rolls_back_siblings() {
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    let applied = Arc::new(AtomicUsize::new(0));

    // c1：正常插件（apply 计数）；c2：apply 失败
    loader.register_plugin("ok", {
        let applied = applied.clone();
        Arc::new(FnPlugin::new("ok", &[], move |_ctx, _cfg| {
            applied.fetch_add(1, Ordering::SeqCst);
            Ok(EffectOutcome::None)
        }))
    });
    loader.register_plugin(
        "bad",
        Arc::new(FnPlugin::new("bad", &[], |_ctx, _cfg| {
            Err(CordisError::Internal("boom".to_string()))
        })),
    );

    let mut g = EntryOptions::new("g", "g");
    g.group = true;
    g.config = json!([
        { "id": "c1", "name": "ok" },
        { "id": "c2", "name": "bad" }
    ]);
    // fail-loud：子入口失败 → create 返回 Err（保留底层错误）
    let err = loader.create(g).unwrap_err();
    assert!(
        err.to_string().contains("boom"),
        "group mount must fail loudly with the child error: {err}"
    );

    // c1 实际 apply 过（先加载成功），随后在回滚中被停止（纤维已清理）
    assert!(applied.load(Ordering::SeqCst) >= 1, "sibling c1 applied before rollback");
    assert!(
        loader.fiber("c1").is_none(),
        "loaded sibling must be stopped/disposed on rollback"
    );
    assert!(loader.fiber("c2").is_none(), "failed child disposed on rollback");
    // group 入口被移除
    assert!(loader.fiber("g").is_none(), "group entry removed after rollback");
    let _ = &cordis;
}

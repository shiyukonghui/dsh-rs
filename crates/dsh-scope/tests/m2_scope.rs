//! M2a: dsh-scope 语义测试（移植 `packages/core/scope` 的 scope/store/invariant 三个
//! spec 的 24 条可观察行为，见 analysis/m2/scope-report.md §7）。
//!
//! 差异记录（DECISIONS D-023）：create_scope 的 dispose 为同步幂等（无异步
//! quiescence）；事件派发经迷你 ScopedContext 总线（非完整 Cordis）。

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use dsh_scope::*;
use serde_json::{json, Value};

fn k() -> ScopeKey {
    ScopeKey::new()
}

/// 便捷插入（`Result<Undo, String>` 的 unwrap，避免 Debug bound）。
fn ins(e: &NamedEntries<u8>, name: &str, v: u8) -> Undo {
    e.insert(name, v).unwrap_or_else(|e| panic!("insert failed: {e}"))
}

// ---------------------------------------------------------------------------
// scope.spec
// ---------------------------------------------------------------------------

#[test]
fn create_scope_tags_contexts_and_derived_contexts_nearest_wins() {
    let ctx = ScopedContext::new();
    let outer_key = k();
    let inner_key = k();
    let outer = create_scope(&ctx, outer_key.clone(), Default::default()).unwrap();
    let inner = create_scope(&outer.ctx, inner_key.clone(), Default::default()).unwrap();

    assert_eq!(ctx.scope_of(), None, "base ctx untagged");
    assert_eq!(outer.ctx.scope_of(), Some(&outer_key), "outer tagged");
    assert_eq!(
        outer.ctx.extend().scope_of(),
        Some(&outer_key),
        "tag inherits through extend"
    );
    assert_eq!(inner.ctx.scope_of(), Some(&inner_key), "nearest tag wins");
}

#[test]
fn create_scope_is_usable_synchronously_before_backing_fiber_activates() {
    let ctx = ScopedContext::new();
    let key = k();
    let scope = create_scope(&ctx, key, Default::default()).unwrap();
    // 创建后立刻可用：注册 + 读取标签都立即生效
    let events = Rc::new(RefCell::new(Vec::new()));
    let ev = events.clone();
    scope.ctx.on("tick", false, Box::new(move |_, _| {
        ev.borrow_mut().push("registered".to_string());
    }));
    let ev = events.clone();
    scope.on_dispose(move || {
        ev.borrow_mut().push("disposed".to_string());
    });
    assert_eq!(*events.borrow(), Vec::<String>::new());
    scope.dispose();
    assert_eq!(*events.borrow(), vec!["disposed".to_string()]);
}

#[test]
fn create_scope_dispose_is_idempotent_and_shares_memo() {
    let ctx = ScopedContext::new();
    let key = k();
    let scope = create_scope(&ctx, key, Default::default()).unwrap();
    let runs = Rc::new(RefCell::new(0));
    {
        let r = runs.clone();
        scope.on_dispose(move || *r.borrow_mut() += 1);
    }
    scope.dispose();
    scope.dispose(); // 重复调用 no-op
    scope.dispose();
    assert_eq!(*runs.borrow(), 1, "dispose memoized");
}

#[test]
fn create_scope_disposes_registered_disposers_in_reverse_order() {
    let ctx = ScopedContext::new();
    let key = k();
    let scope = create_scope(&ctx, key, Default::default()).unwrap();
    let order = Rc::new(RefCell::new(Vec::new()));
    let o = order.clone();
    scope.on_dispose(move || o.borrow_mut().push("outer".to_string()));
    let o = order.clone();
    scope.on_dispose(move || o.borrow_mut().push("inner".to_string()));
    scope.dispose();
    assert_eq!(*order.borrow(), vec!["inner".to_string(), "outer".to_string()]);
}

#[test]
fn scope_target_routes_scoped_listeners_by_key_untagged_global() {
    let root = ScopedContext::new();
    let key_a = k();
    let key_b = k();
    let scope_a = create_scope(&root, key_a.clone(), Default::default()).unwrap();
    let scope_b = create_scope(&root, key_b.clone(), Default::default()).unwrap();

    let heard = Rc::new(RefCell::new(Vec::new()));
    let h = heard.clone();
    root.on("ping", true, Box::new(move |_, _| h.borrow_mut().push("global".to_string())));
    let h = heard.clone();
    scope_a.ctx.on("ping", false, Box::new(move |_, _| h.borrow_mut().push("A".to_string())));
    let h = heard.clone();
    scope_b.ctx.on("ping", false, Box::new(move |_, _| h.borrow_mut().push("B".to_string())));

    let carrier_a = scope_target(Some(key_a.clone()), None);
    root.emit_scoped(&carrier_a, "ping", vec![json!("a")]);
    let carrier_b = scope_target(Some(key_b), None);
    root.emit_scoped(&carrier_b, "ping", vec![json!("b")]);
    let carrier_none = scope_target(None, None);
    root.emit_scoped(&carrier_none, "ping", vec![json!("none")]);

    assert_eq!(*heard.borrow(), vec!["global", "A", "global", "B", "global"]);
}

#[test]
fn scope_target_preserves_a_base_filter_and_runs_it_before_scope_predicate() {
    let root = ScopedContext::new();
    let key = k();
    let scope = create_scope(&root, key.clone(), Default::default()).unwrap();
    let heard = Rc::new(RefCell::new(Vec::new()));
    let h = heard.clone();
    scope.ctx.on("ping", false, Box::new(move |_, _| h.borrow_mut().push("yes".to_string())));

    // base filter 返回 false（含 receiver 断言：调用一次）
    let receiver_matches = Arc::new(Mutex::new(false));
    let rm = receiver_matches.clone();
    let base_filter: BaseFilter = Arc::new(move || {
        *rm.lock().unwrap() = true;
        false
    });
    let carrier = scope_target(Some(key), Some(base_filter));
    root.emit_scoped(&carrier, "ping", vec![json!("vetoed")]);
    assert_eq!(*heard.borrow(), Vec::<String>::new(), "base filter vetoed all");
    assert!(*receiver_matches.lock().unwrap(), "base filter was called");
}

#[test]
fn scope_target_global_true_listeners_retain_global_semantics() {
    let root = ScopedContext::new();
    let key_a = k();
    let key_b = k();
    let scope_a = create_scope(&root, key_a, Default::default()).unwrap();
    let heard = Rc::new(RefCell::new(Vec::new()));
    let h = heard.clone();
    scope_a.ctx.on("ping", true, Box::new(move |_, _| h.borrow_mut().push("yes".to_string())));

    let carrier_foreign = scope_target(Some(key_b), None);
    root.emit_scoped(&carrier_foreign, "ping", vec![json!("foreign")]);
    let carrier_none = scope_target(None, None);
    root.emit_scoped(&carrier_none, "ping", vec![json!("none")]);
    assert_eq!(*heard.borrow(), vec!["yes", "yes"], "global listener bypasses filter");
}

#[test]
fn scope_target_uses_opaque_carrier_with_separately_tracked_key() {
    let key = k();
    let carrier = scope_target(Some(key.clone()), None);
    assert_eq!(carrier.carrier_key_of(), Some(&key));
    let carrier_none = scope_target(None, None);
    assert_eq!(carrier_none.carrier_key_of(), None);
}

#[test]
fn scope_parent_chain_links_at_mint_walks_to_root_rejects_cycles() {
    let preset = k();
    let agent = k();
    let _ = create_scope(&ScopedContext::new(), preset.clone(), Default::default()).unwrap();
    let _ = create_scope(
        &ScopedContext::new(),
        agent.clone(),
        CreateScopeOptions { parent: Some(preset.clone()) },
    )
    .unwrap();

    assert_eq!(scope_parent_of(&agent), Some(preset.clone()));
    assert_eq!(scope_parent_of(&preset), None);
    assert_eq!(scope_chain_of(Some(&agent)), vec![agent.clone(), preset.clone()]);
    assert_eq!(scope_chain_of(None), Vec::<ScopeKey>::new());

    // bind preset → agent 成环
    let err = bind_scope_parent(preset.clone(), agent.clone()).unwrap_err();
    assert!(err.contains("cycle"), "{err}");
    // self loop
    let err = bind_scope_parent(preset.clone(), preset.clone()).unwrap_err();
    assert!(err.contains("cycle"), "{err}");
}

#[test]
fn scope_parent_chain_relinks_only_through_binding() {
    let preset_a = k();
    let preset_b = k();
    let child = k();
    let agent = k();

    let binding = bind_scope_parent(agent.clone(), preset_a.clone()).unwrap();
    // 再 bind（非原 binding）→ 拒绝
    let err = bind_scope_parent(agent.clone(), preset_b.clone()).unwrap_err();
    assert!(err.contains("already bound"), "{err}");
    // 原 binding rebind
    binding.rebind(preset_b.clone()).unwrap();
    assert_eq!(scope_chain_of(Some(&agent)), vec![agent.clone(), preset_b.clone()]);
    // rebind 仍环检测：把 child 绑到 agent 后，agent 无法 rebind 到 child
    let _ = bind_scope_parent(child.clone(), agent.clone()).unwrap();
    let err = binding.rebind(child.clone()).unwrap_err();
    assert!(err.contains("cycle"), "{err}");
}

#[test]
fn scope_admits_ancestor_tag_never_descendant() {
    let preset = k();
    let agent = k();
    let other = k();
    let _ = bind_scope_parent(agent.clone(), preset.clone()).unwrap();

    let root = ScopedContext::new();
    let scope_preset = create_scope(&root, preset.clone(), Default::default()).unwrap();
    let scope_agent = create_scope(&root, agent.clone(), Default::default()).unwrap();
    let scope_other = create_scope(&root, other.clone(), Default::default()).unwrap();

    let heard = Rc::new(RefCell::new(Vec::new()));
    for (scope, label) in [
        (&scope_preset.ctx, "preset"),
        (&scope_agent.ctx, "agent"),
        (&scope_other.ctx, "other"),
        (&root, "untagged"),
    ] {
        let h = heard.clone();
        let label = label.to_string();
        scope.on("evt", false, Box::new(move |_, _| h.borrow_mut().push(label.clone())));
    }

    // 以 agent key 派发 → 自己 + 祖先 + 无标签（兄弟 other 排除）
    let carrier = scope_target(Some(agent.clone()), None);
    root.emit_scoped(&carrier, "evt", vec![]);
    let mut got = heard.borrow().clone();
    got.sort();
    assert_eq!(got, vec!["agent", "preset", "untagged"]);

    // 以 preset key 派发 → preset + 无标签（agent 在链之下，排除——绝不下行）
    heard.borrow_mut().clear();
    let carrier = scope_target(Some(preset.clone()), None);
    root.emit_scoped(&carrier, "evt", vec![]);
    let mut got = heard.borrow().clone();
    got.sort();
    assert_eq!(got, vec!["preset", "untagged"]);
}

// ---------------------------------------------------------------------------
// store.spec
// ---------------------------------------------------------------------------

#[test]
fn named_entries_owns_duplicates_lookup_order_live_iteration_and_exact_undo() {
    let e = NamedEntries::new(|name| format!("dup: {name}"));
    let undo_a = ins(&e, "a", 1);
    let mut iter = e.values();
    assert_eq!(iter.next(), Some(1)); // 消费第一个
    let _ = ins(&e, "b", 2);
    // live 迭代器续迭代：b
    let rest = iter.collect::<Vec<_>>();
    assert_eq!(rest, vec![2]);
    assert_eq!(e.keys(), vec!["a".to_string(), "b".to_string()]);
    assert_eq!(e.entries(), vec![("a".to_string(), 1), ("b".to_string(), 2)]);
    assert_eq!(e.get("a"), Some(1));
    assert_eq!(e.get("missing"), None);
    assert!(e.has("b"));
    assert!(!e.has("missing"));
    assert!(!e.is_empty());
    let err = match e.insert("a", 3) {
        Err(e) => e,
        Ok(_) => panic!("duplicate insert should fail"),
    };
    assert_eq!(err, "dup: a");
    undo_a();
    ins(&e, "a", 3); // 撤销后可重入
    undo_a(); // 幂等 no-op
    assert_eq!(e.get("a"), Some(3));
    assert_eq!(e.entries(), vec![("b".to_string(), 2), ("a".to_string(), 3)]);
}

#[test]
fn named_entries_starts_fresh_generation_after_drain() {
    let e = NamedEntries::new(|_| "dup".to_string());
    let first = ins(&e, "first", 1);
    let mut iter = e.values();
    assert_eq!(iter.next(), Some(1)); // 已消费
    first(); // 清空 → 换新代
    let _ = ins(&e, "replacement", 2);
    assert_eq!(iter.next(), None, "old iterator detached from new generation");
    assert_eq!(e.values().collect::<Vec<_>>(), vec![2]);
}

#[test]
fn named_entries_entries_live_yields_key_value_pairs() {
    let e = NamedEntries::new(|name| format!("dup: {name}"));
    let _ = ins(&e, "a", 1);
    let mut iter = e.entries_live();
    assert_eq!(iter.next(), Some(("a".to_string(), 1)));
    let _ = ins(&e, "b", 2); // 迭代期间新注册 → 本轮 live 可见
    assert_eq!(iter.next(), Some(("b".to_string(), 2)));
    assert_eq!(iter.next(), None);
}

#[test]
fn anonymous_entries_own_equal_values_independently_with_live_iteration_and_idempotent_undo() {
    let e = AnonymousEntries::new();
    let value = "same";
    let undo_first = e.append(value);
    let mut iter = e.values();
    assert_eq!(iter.next(), Some("same"));
    let undo_second = e.append(value);
    assert_eq!(e.values().collect::<Vec<_>>(), vec!["same", "same"]);

    undo_first();
    undo_first(); // 幂等
    assert_eq!(e.values().collect::<Vec<_>>(), vec!["same"]);
    undo_second();
    assert!(e.is_empty());
}

#[test]
fn anonymous_entries_starts_fresh_generation_after_drain() {
    let e = AnonymousEntries::new();
    let first = e.append(1u8);
    let mut iter = e.values();
    assert_eq!(iter.next(), Some(1));
    first();
    let second = e.append(2u8);
    assert_eq!(iter.next(), None, "detached");
    assert_eq!(e.values().collect::<Vec<_>>(), vec![2]);
    let _ = second;
}

// ---- 测试层：命名 + 匿名条目聚合 ----

struct NamedLayer {
    named: NamedEntries<u8>,
    anon: AnonymousEntries<&'static str>,
}

impl NamedLayer {
    fn new() -> Self {
        NamedLayer {
            named: NamedEntries::new(|n| format!("dup: {n}")),
            anon: AnonymousEntries::new(),
        }
    }
}

impl ScopeLayer for NamedLayer {
    fn is_empty(&self) -> bool {
        self.named.is_empty() && self.anon.is_empty()
    }
}

fn pick_named(layer: &NamedLayer) -> Vec<(String, u8)> {
    layer.named.entries()
}

#[test]
fn scoped_layers_constructs_global_eagerly_reads_non_creating_merge_shadows_in_order() {
    let created = Arc::new(Mutex::new(Vec::new()));
    let cr = created.clone();
    let layers = ScopedLayers::new(
        move |scope: Option<&ScopeKey>| {
            cr.lock().unwrap().push(scope.cloned());
            NamedLayer::new()
        },
        || {},
    );
    let key = k();
    let global = layers.global();
    ins(&global.named, "a", 1);
    ins(&global.named, "shared", 2);

    let created_count = |c: &Arc<Mutex<Vec<Option<ScopeKey>>>>| c.lock().unwrap().len();
    assert_eq!(created_count(&created), 1, "global created eagerly once");
    assert_eq!(
        created.lock().unwrap().iter().filter(|s| s.is_none()).count(),
        1,
        "first create is global"
    );
    assert!(layers.peek(None).is_none(), "peek(undefined) is None");
    assert!(layers.peek(Some(&key)).is_none(), "peek(key) None, no create");
    assert_eq!(created_count(&created), 1, "reads do not create");

    let merged = layers.merge(Some(&key), &pick_named);
    let mut names = merged.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>();
    names.sort();
    assert_eq!(merged.len(), 2);
    assert_eq!(names, vec!["a".to_string(), "shared".to_string()]);
}

#[test]
fn scoped_layers_uses_same_scoped_context_for_visibility_and_reclaims_only_empty_aggregate() {
    let created = Arc::new(Mutex::new(Vec::new()));
    let cr = created.clone();
    let layers = ScopedLayers::new(
        move |scope: Option<&ScopeKey>| {
            cr.lock().unwrap().push(scope.cloned());
            NamedLayer::new()
        },
        || {},
    );
    let key = k();
    let global = layers.global();
    ins(&global.named, "a", 1);
    ins(&global.named, "shared", 1);

    let scope = create_scope(&ScopedContext::new(), key.clone(), Default::default()).unwrap();
    let layer_key = key.clone();
    let scope_for_effect = Some(key.clone());

    // 三个 effect（均 notify:false）：shared:2、c:3、匿名 kept
    let d1 = layers.effect(scope_for_effect.as_ref(), |l| ins(&l.named, "shared", 2), "e1", false);
    let d2 = layers.effect(scope_for_effect.as_ref(), |l| ins(&l.named, "c", 3), "e2", false);
    let d3 = layers.effect(scope_for_effect.as_ref(), |l| l.anon.append("kept"), "e3", false);
    let _ = (&scope, &layer_key);

    // 惰性创建恰好一次 scoped 层
    assert_eq!(created.lock().unwrap().iter().filter(|s| s.is_some()).count(), 1);
    // 全局层 + 一个 scoped 层
    // merge: 全局 a:1/shared:1 被 scoped shared:2 遮蔽 + c:3
    let mut merged = layers.merge(Some(&key), &pick_named);
    merged.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        merged,
        vec![
            ("a".to_string(), 1),
            ("c".to_string(), 3),
            ("shared".to_string(), 2)
        ]
    );

    // 移除 shared → 回落到全局 shared:1；层非空（c + kept）仍在
    d1();
    assert!(layers.peek(Some(&key)).is_some(), "layer not empty yet");
    let mut merged = layers.merge(Some(&key), &pick_named);
    merged.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        merged,
        vec![
            ("a".to_string(), 1),
            ("c".to_string(), 3),
            ("shared".to_string(), 1)
        ]
    );

    // 移除 c → 层仍在（kept）
    d2();
    assert!(layers.peek(Some(&key)).is_some());

    // 移除 kept → 层空 → 回收
    d3();
    assert!(layers.peek(Some(&key)).is_none(), "empty aggregate reclaimed");
}

#[test]
fn scoped_layers_runs_action_notify_undo_notify_in_order() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let ev = events.clone();
    let layers = ScopedLayers::new(
        |_| NamedLayer::new(),
        move || ev.lock().unwrap().push("notify".to_string()),
    );
    let ev = events.clone();
    let disposer = layers.effect(None, |l| {
        let ev2 = ev.clone();
        ev.lock().unwrap().push("action".to_string());
        let undo = ins(&l.named, "x", 1);
        Arc::new(move || {
            ev2.lock().unwrap().push("undo".to_string());
            undo();
        })
    }, "store.order", true);
    assert_eq!(*events.lock().unwrap(), vec!["action".to_string(), "notify".to_string()]);
    disposer();
    assert_eq!(
        *events.lock().unwrap(),
        vec!["action".to_string(), "notify".to_string(), "undo".to_string(), "notify".to_string()]
    );
    disposer(); // Cordis 幂等：再次 dispose no-op
    assert_eq!(
        *events.lock().unwrap(),
        vec!["action".to_string(), "notify".to_string(), "undo".to_string(), "notify".to_string()]
    );
    assert!(layers.global().named.is_empty(), "x removed by undo");
}

#[test]
fn scoped_layers_cleans_up_failed_factories_and_actions_without_discarding_existing_layer() {
    // a) 工厂仅对 scoped 层失败 → 层从未放入 map（global 贪婪创建不失败）
    let fail_factory = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let ff = fail_factory.clone();
    let layers_a = ScopedLayers::new(
        move |selected: Option<&ScopeKey>| -> NamedLayer {
            if selected.is_some() && ff.load(std::sync::atomic::Ordering::SeqCst) {
                panic!("factory failed");
            }
            NamedLayer::new()
        },
        || {},
    );
    let key_a = k();
    let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        layers_a.effect(Some(&key_a), |_l| Arc::new(|| {}), "a", false)
    }));
    assert!(err.is_err(), "factory failed propagates");
    assert!(layers_a.peek(Some(&key_a)).is_none(), "layer never put in map");
    fail_factory.store(false, std::sync::atomic::Ordering::SeqCst);
    // 工厂恢复后同层 effect 成功创建
    let _ok = layers_a.effect(Some(&key_a), |l| ins(&l.named, "later", 1), "a2", false);
    assert!(layers_a.peek(Some(&key_a)).is_some(), "factory recovers");

    // b) action 失败（新建空层）→ 回滚
    let layers_b = ScopedLayers::new(
        |_| NamedLayer::new(),
        || {},
    );
    let key_b = k();
    let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        layers_b.effect(Some(&key_b), |_l| -> Undo {
            panic!("action failed")
        }, "b", false)
    }));
    assert!(err.is_err());
    assert!(
        layers_b.peek(Some(&key_b)).is_none(),
        "empty new layer rolled back"
    );

    // c) 既有层上 action 失败 → 层保留
    let layers_c = ScopedLayers::new(
        |_| NamedLayer::new(),
        || {},
    );
    let key_c = k();
    let _seed = layers_c.effect(Some(&key_c), |l| ins(&l.named, "kept", 1), "seed", false);
    assert!(layers_c.peek(Some(&key_c)).is_some(), "seed layer exists");
    let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        layers_c.effect(Some(&key_c), |_l| -> Undo {
            panic!("second action failed")
        }, "c", false)
    }));
    assert!(err.is_err(), "action failure propagates");
    let kept = layers_c
        .peek(Some(&key_c))
        .and_then(|l| l.named.get("kept"))
        .unwrap_or(0);
    assert_eq!(kept, 1, "existing layer preserved");
}

#[test]
fn scoped_layers_rolls_back_scoped_insertion_when_notification_throws() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let ev = events.clone();
    let first_notify = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let fst = first_notify.clone();
    let action_ev = ev.clone();
    let layers = ScopedLayers::new(
        |_| NamedLayer::new(),
        move || {
            if fst.swap(false, std::sync::atomic::Ordering::SeqCst) {
                ev.lock().unwrap().push("notify".to_string());
                panic!("change failed");
            }
            ev.lock().unwrap().push("notify".to_string());
        },
    );
    let key = k();
    let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        layers.effect(Some(&key), |l| {
            let undo = ins(&l.named, "rollback", 9);
            let ev2 = action_ev.clone();
            Arc::new(move || {
                ev2.lock().unwrap().push("undo".to_string());
                undo();
            })
        }, "r", true)
    }));
    assert!(err.is_err(), "notification failure propagates");
    assert_eq!(
        *events.lock().unwrap(),
        vec!["notify".to_string(), "undo".to_string(), "notify".to_string()],
        "rollback order [notify, undo, notify]"
    );
    assert!(layers.peek(Some(&key)).is_none(), "insertion rolled back");
}

// ---------------------------------------------------------------------------
// invariant.spec
// ---------------------------------------------------------------------------

fn agent_args(agent: &str) -> Vec<Value> {
    vec![json!({ "agent": agent })]
}

fn assert_contains(haystack: &str, needle: &str) {
    assert!(
        haystack.contains(needle),
        "expected {needle:?} in {haystack:?}"
    );
}

#[test]
fn invariant_ignores_ordinary_events_and_requires_carrier_for_scoped() {
    // 普通事件：不检查
    let r = check_scoped_dispatch("ordinary/event", &[json!(1)], false, |_| true);
    assert!(r.is_ok());
    // scope-filtered 无 carrier → fail
    let r = check_scoped_dispatch("agent/error", &agent_args("a"), false, |_| true).unwrap_err();
    assert_contains(&r, "dispatched without a scope carrier");
    // presence-only 无 carrier → fail
    let r = check_scoped_dispatch("session/created", &[], false, |_| true).unwrap_err();
    assert_contains(&r, "dispatched without a scope carrier");
}

#[test]
fn invariant_checks_generated_resolvers_against_carrier_key() {
    // 匹配：carrier key 与载荷 subject 同一
    let r = check_scoped_dispatch("agent/status", &agent_args("the-agent"), true, |s| {
        s == "the-agent"
    });
    assert!(r.is_ok());
    // 错配
    let r = check_scoped_dispatch("agent/status", &agent_args("the-agent"), true, |s| {
        s == "other"
    })
    .unwrap_err();
    assert_contains(&r, "DIFFERENT subject");
    // system-prompt/assemble：args[1].scope
    let args = vec![json!(1), json!({ "scope": "sc" })];
    let r = check_scoped_dispatch("system-prompt/assemble", &args, true, |s| s == "sc");
    assert!(r.is_ok());
    let r = check_scoped_dispatch("system-prompt/assemble", &args, true, |s| s != "sc");
    assert!(r.is_err());
    // presence-only：带合法 carrier 不比较 subject
    let r = check_scoped_dispatch("session/event", &[], true, |_| false);
    assert!(r.is_ok(), "presence-only does not compare subject");
}

#[test]
fn scoped_events_generated_table_is_complete() {
    assert_eq!(SCOPED_EVENTS.len(), 26);
    for (name, resolver) in SCOPED_EVENTS {
        assert_eq!(scoped_subject_resolver_for(name), Some(resolver), "{name}");
    }
    assert_eq!(scoped_subject_resolver_for("not/listed"), None);
    assert_eq!(
        scoped_subject_resolver_for("approval/request"),
        Some(SubjectResolver::AgentAt0)
    );
    assert_eq!(
        scoped_subject_resolver_for("subagent/end"),
        Some(SubjectResolver::PresenceOnly)
    );
}

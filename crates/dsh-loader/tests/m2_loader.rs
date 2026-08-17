//! §5.3 场景 5/7：loader 事务（增删改/热更/替换/回滚/disabled/group/7-case 自处置）。
#![allow(clippy::arc_with_non_send_sync)]

mod common;
use common::*;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use dsh_core::*;
use dsh_loader::*;

/// 插件 A：apply 记录收到的 config。
fn plugin_a(log: &Rc<RefCell<Vec<String>>>) -> Arc<dyn Plugin> {
    let log = log.clone();
    Arc::new(FnPlugin::new("a", &[], move |_ctx, config| {
        push(&log, format!("apply:a:{}", config.get("k").and_then(|v| v.as_i64()).unwrap_or(0)));
        Ok(EffectOutcome::None)
    }))
}

fn plugin_p1(log: &Rc<RefCell<Vec<String>>>) -> Arc<dyn Plugin> {
    let log = log.clone();
    Arc::new(FnPlugin::new("p1", &[], move |_ctx, _cfg| {
        push(&log, "apply:p1");
        Ok(EffectOutcome::None)
    }))
}

fn plugin_p2(log: &Rc<RefCell<Vec<String>>>) -> Arc<dyn Plugin> {
    let log = log.clone();
    Arc::new(FnPlugin::new("p2", &[], move |_ctx, _cfg| {
        push(&log, "apply:p2");
        Ok(EffectOutcome::None)
    }))
}

fn options(id: &str, name: &str, config: Value) -> EntryOptions {
    let mut o = EntryOptions::new(id, name);
    o.config = config;
    o
}

/// 场景 5a：创建入口 → 加载插件；disabled 入口不加载。
#[test]
fn entry_create_loads_plugin() {
    let log = log();
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    loader.register_plugin("a", plugin_a(&log));

    let id = loader.create(options("a", "a", json!({"k": 1}))).unwrap();
    let fid = loader.fiber(&id).expect("fiber attached");
    assert_eq!(cordis.fiber_state(fid), Some(FiberState::Active));
    assert_eq!(snapshot(&log), vec!["apply:a:1"]);
    assert!(!loader.is_disabled(&id));

    // disabled 入口：不启动
    let mut d = options("d", "a", json!({"k": 2}));
    d.disabled = true;
    let did = loader.create(d).unwrap();
    assert!(loader.fiber(&did).is_none());
    assert!(loader.is_disabled(&did));
}

/// 场景 5b：config-only 热更 → fiber 重启 + internal/update 写回。
#[test]
fn entry_update_config_hot_reload() {
    let log = log();
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    loader.register_plugin("a", plugin_a(&log));

    let id = loader.create(options("a", "a", json!({"k": 1}))).unwrap();
    loader.update(&id, options("a", "a", json!({"k": 2}))).unwrap();

    let fid = loader.fiber(&id).unwrap();
    assert_eq!(cordis.fiber_state(fid), Some(FiberState::Active));
    assert_eq!(snapshot(&log), vec!["apply:a:1", "apply:a:2"]);

    // 写回：entry.options.config 已更新，writes 记录 write:
    let snap = loader.entries().into_iter().find(|e| e.id == "a").unwrap();
    assert_eq!(snap.fiber, Some(fid));
    let writes = loader.take_writes();
    assert!(writes.iter().any(|w| w.starts_with("write:a")), "{writes:?}");
    let cfg = loader
        .state
        .borrow()
        .entries
        .get("a")
        .map(|e| e.options.config.clone())
        .unwrap();
    assert_eq!(cfg, json!({"k": 2}));
}

/// 场景 5c：disabled 更新 → 卸载 fiber。
#[test]
fn entry_update_disable_unloads() {
    let log = log();
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    loader.register_plugin("a", plugin_a(&log));

    let id = loader.create(options("a", "a", json!({"k": 1}))).unwrap();
    let fid = loader.fiber(&id).unwrap();

    let mut disabled = options("a", "a", json!({"k": 1}));
    disabled.disabled = true;
    loader.update(&id, disabled).unwrap();

    assert!(loader.fiber(&id).is_none());
    assert!(loader.is_disabled(&id));
    assert_eq!(cordis.fiber_state(fid), Some(FiberState::Disposed));
}

/// 场景 5d：name 替换 → dispose 旧 + start 新。
#[test]
fn entry_replace_name() {
    let log = log();
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    loader.register_plugin("a", plugin_a(&log));
    loader.register_plugin("p1", plugin_p1(&log));

    let id = loader.create(options("e", "a", json!({"k": 1}))).unwrap();
    let old_fid = loader.fiber(&id).unwrap();

    loader.update(&id, options("e", "p1", json!({}))).unwrap();

    let new_fid = loader.fiber(&id).unwrap();
    assert_ne!(old_fid, new_fid);
    assert_eq!(cordis.fiber_state(new_fid), Some(FiberState::Active));
    assert_eq!(cordis.fiber_state(old_fid), Some(FiberState::Disposed));
    assert_eq!(snapshot(&log), vec!["apply:a:1", "apply:p1"]);
}

/// 场景 5e：替换失败 → 回滚旧插件（选项 + 旧实现重新启动）。
#[test]
fn entry_replace_rollback_on_failure() {
    let log = log();
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    loader.register_plugin("a", plugin_a(&log));

    let id = loader.create(options("e", "a", json!({"k": 1}))).unwrap();

    // 替换为未注册插件 → start 失败 → 回滚到 a
    let err = loader.update(&id, options("e", "zzz", json!({}))).unwrap_err();
    assert!(err.to_string().contains("unknown plugin"), "{err}");

    let fid = loader.fiber(&id).unwrap();
    assert_eq!(cordis.fiber_state(fid), Some(FiberState::Active));
    let snap = loader.entries().into_iter().find(|e| e.id == "e").unwrap();
    assert_eq!(snap.name, "a");
    assert_eq!(snapshot(&log), vec!["apply:a:1", "apply:a:1"]);
}

/// 场景 5f：group 嵌套 —— 挂载子入口、卸载时递归卸载。
#[test]
fn group_nested_mount_and_remove() {
    let log = log();
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    loader.register_plugin("p1", plugin_p1(&log));
    loader.register_plugin("p2", plugin_p2(&log));

    let mut g = options("g", "g", json!([{ "id": "c1", "name": "p1" }, { "id": "c2", "name": "p2" }]));
    g.group = true;
    let gid = loader.create(g).unwrap();

    let c1 = loader.fiber("c1").expect("c1 loaded");
    let c2 = loader.fiber("c2").expect("c2 loaded");
    assert_eq!(cordis.fiber_state(c1), Some(FiberState::Active));
    assert_eq!(cordis.fiber_state(c2), Some(FiberState::Active));
    assert_eq!(snapshot(&log), vec!["apply:p1", "apply:p2"]);

    // 组禁用 → 子入口全部卸载
    loader.remove(&gid).unwrap();
    assert!(loader.fiber("c1").is_none());
    assert!(loader.fiber("c2").is_none());
    assert_eq!(cordis.fiber_state(c1), Some(FiberState::Disposed));
    assert_eq!(cordis.fiber_state(c2), Some(FiberState::Disposed));
}

/// 场景 5g：组配置热更 → 移除缺席、创建新增、更新既有。
#[test]
fn group_sync_add_remove_update() {
    let log = log();
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    loader.register_plugin("p1", plugin_p1(&log));
    loader.register_plugin("p2", plugin_p2(&log));

    let mut g = options("g", "g", json!([{ "id": "c1", "name": "p1" }, { "id": "c2", "name": "p2" }]));
    g.group = true;
    loader.create(g).unwrap();
    let c2 = loader.fiber("c2").unwrap();
    assert_eq!(snapshot(&log), vec!["apply:p1", "apply:p2"]);

    // 新配置：c1 保留（同配置 → 无变化）、c2 移除、c3 新增
    let mut g2 = options("g", "g", json!([{ "id": "c1", "name": "p1" }, { "id": "c3", "name": "p1" }]));
    g2.group = true;
    loader.update("g", g2).unwrap();

    assert!(loader.fiber("c1").is_some());
    assert!(loader.fiber("c3").is_some());
    assert!(loader.fiber("c2").is_none());
    assert_eq!(cordis.fiber_state(c2), Some(FiberState::Disposed));
    assert_eq!(snapshot(&log), vec!["apply:p1", "apply:p2", "apply:p1"]);
}

/// 场景 7：7-case 自处置检测 —— 外部 dispose 入口 fiber → 入口被标 disabled 并写回。
#[test]
fn seven_case_external_dispose_marks_disabled() {
    let log = log();
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    loader.register_plugin("a", plugin_a(&log));

    let id = loader.create(options("a", "a", json!({"k": 1}))).unwrap();
    let fid = loader.fiber(&id).unwrap();
    assert!(!loader.is_disabled(&id));

    // 绕过 loader 直接卸载 fiber（等价外部/其他插件 dispose）
    cordis.unload(fid).unwrap();

    // 7-case：非 loader 路径 dispose → 入口标记 disabled + 写回
    assert!(loader.is_disabled(&id));
    let writes = loader.take_writes();
    assert!(writes.iter().any(|w| w.starts_with("disable:a")), "{writes:?}");
}

/// 场景 5h：重复 entry id 报错。
#[test]
fn duplicate_entry_id_fails() {
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    loader.register_plugin("a", plugin_a(&log()));
    loader.create(options("a", "a", json!({}))).unwrap();
    let err = loader.create(options("a", "a", json!({}))).unwrap_err();
    assert!(err.to_string().contains("duplicate"), "{err}");
}

/// M22：group 入口的 Group 插件 fiber 形态——`plugin:Group`、子入口 parent =
/// Group fiber、卸载时 Group disposer 递归 stop 子入口。
#[test]
fn group_plugin_fiber_mounts_children() {
    let log = log();
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    loader.register_plugin("p1", plugin_p1(&log));
    loader.register_plugin("p2", plugin_p2(&log));

    let mut g = options("g", "g", json!([{ "id": "c1", "name": "p1" }, { "id": "c2", "name": "p2" }]));
    g.group = true;
    let gid = loader.create(g).unwrap();

    // Group fiber 存在（group 入口现在有 fiber）
    let gfid = loader.fiber(&gid).expect("group entry has Group fiber");
    // 子入口 fiber 的 parent = Group fiber
    let c1 = loader.fiber("c1").unwrap();
    let c2 = loader.fiber("c2").unwrap();
    let parent_c1 = cordis.with(|rt| rt.fiber(c1).and_then(|f| f.parent));
    let parent_c2 = cordis.with(|rt| rt.fiber(c2).and_then(|f| f.parent));
    assert_eq!(parent_c1, Some(gfid), "c1 parent is Group fiber");
    assert_eq!(parent_c2, Some(gfid), "c2 parent is Group fiber");
    assert_eq!(cordis.fiber_state(gfid), Some(FiberState::Active));

    // 卸载 group → Group fiber 卸载，子入口递归 stop
    loader.remove(&gid).unwrap();
    assert!(loader.fiber("c1").is_none());
    assert!(loader.fiber("c2").is_none());
    assert_eq!(cordis.fiber_state(c1), Some(FiberState::Disposed));
    assert_eq!(cordis.fiber_state(c2), Some(FiberState::Disposed));
    assert_eq!(cordis.fiber_state(gfid), Some(FiberState::Disposed));
}

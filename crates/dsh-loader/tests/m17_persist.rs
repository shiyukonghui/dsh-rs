//! A7 持久化写回（seam 级）：`set_persist` 后 create/update/remove 经 sink 收到
//! 权威入口列表（root 组顺序）；无 sink 时 persist 为 no-op；sink 出错 fail-loud。
#![allow(clippy::arc_with_non_send_sync)]

mod common;
use common::*;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use dsh_core::*;
use dsh_loader::*;

fn options(id: &str, name: &str) -> EntryOptions {
    EntryOptions::new(id, name)
}

/// A7（T5-a）：create 触发 sink，收到含新 entry 的权威列表；无 sink 时正常 no-op。
#[test]
fn create_persists_authoritative_entries() {
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    let seen: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let sink_seen = seen.clone();
    loader.set_persist(Some(Rc::new(move |entries: &[EntryOptions]| {
        let ids: Vec<String> = entries.iter().map(|e| e.id.clone()).collect();
        *sink_seen.borrow_mut() = ids;
        Ok(())
    })));

    loader.register_plugin("a", Arc::new(FnPlugin::noop("a")));
    loader.create(options("a", "a")).unwrap();
    assert_eq!(*seen.borrow(), vec!["a".to_string()]);

    // 无 sink 的 loader：create 正常（no-op persist）
    let cordis2 = Cordis::new();
    let l2 = Loader::new(&cordis2).unwrap();
    l2.register_plugin("x", Arc::new(FnPlugin::noop("x")));
    l2.create(options("x", "x")).unwrap();
}

/// A7（T5-b）：create/update 触发 sink，列表保持 root 声明顺序。
#[test]
fn update_persists_authoritative_entries_in_order() {
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    let seen: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let sink_seen = seen.clone();
    loader.set_persist(Some(Rc::new(move |entries: &[EntryOptions]| {
        let ids: Vec<String> = entries.iter().map(|e| e.id.clone()).collect();
        *sink_seen.borrow_mut() = ids;
        Ok(())
    })));

    loader.register_plugin("a", Arc::new(FnPlugin::noop("a")));
    loader.create(options("a", "a")).unwrap();
    let mut o = options("a", "a");
    o.config = json!({"k": 2});
    loader.update("a", o).unwrap();
    assert_eq!(*seen.borrow(), vec!["a".to_string()]);

    loader.register_plugin("b", Arc::new(FnPlugin::noop("b")));
    loader.create(options("b", "b")).unwrap();
    assert_eq!(*seen.borrow(), vec!["a".to_string(), "b".to_string()]);
}

/// A7（T5-c）：remove 触发 sink 收到移除后的列表。
#[test]
fn remove_persists_authoritative_entries() {
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    let seen: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let sink_seen = seen.clone();
    loader.set_persist(Some(Rc::new(move |entries: &[EntryOptions]| {
        let ids: Vec<String> = entries.iter().map(|e| e.id.clone()).collect();
        *sink_seen.borrow_mut() = ids;
        Ok(())
    })));

    loader.register_plugin("a", Arc::new(FnPlugin::noop("a")));
    loader.register_plugin("b", Arc::new(FnPlugin::noop("b")));
    loader.create(options("a", "a")).unwrap();
    loader.create(options("b", "b")).unwrap();
    loader.remove("a").unwrap();
    assert_eq!(*seen.borrow(), vec!["b".to_string()]);
}

/// A7（T5-d）：sink 返回错误 → write 经 `?` 传播（fail-loud，不静默）。
#[test]
fn persist_failure_propagates() {
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    loader.set_persist(Some(Rc::new(|_entries: &[EntryOptions]| {
        Err("disk full".to_string())
    })));
    loader.register_plugin("a", Arc::new(FnPlugin::noop("a")));
    let err = loader.create(options("a", "a")).unwrap_err();
    assert!(err.to_string().contains("disk full"), "{err}");
}

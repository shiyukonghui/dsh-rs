//! M3：include 文件加载器——读取挂载、patch、写回、手动刷新。
#![allow(clippy::arc_with_non_send_sync)]

mod common;
use common::*;

use std::cell::RefCell;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use dsh_core::*;
use dsh_loader::*;

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_file(name: &str, content: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dsh-m3-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::SeqCst)));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    fs::write(&path, content).unwrap();
    path
}

fn plugin_a(log: &Rc<RefCell<Vec<String>>>) -> Arc<dyn Plugin> {
    let log = log.clone();
    Arc::new(FnPlugin::new("a", &[], move |_ctx, config| {
        push(&log, format!("apply:a:{}", config.get("k").and_then(|v| v.as_i64()).unwrap_or(0)));
        Ok(EffectOutcome::None)
    }))
}

const YAML: &str = "- id: a\n  name: a\n  config:\n    k: 1\n";

/// 读取文件并挂载入口。
#[test]
fn include_read_and_mount() {
    let path = temp_file("entries.yaml", YAML);
    let log = log();
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    loader.register_plugin("a", plugin_a(&log));

    let inc = Include::new(&loader, &path, vec![]);
    inc.load().unwrap();

    let fid = loader.fiber("a").expect("entry mounted");
    assert_eq!(cordis.fiber_state(fid), Some(FiberState::Active));
    assert_eq!(snapshot(&log), vec!["apply:a:1"]);
}

/// patch：覆盖 config；insert 追加。
#[test]
fn include_patch_override_and_insert() {
    let path = temp_file("entries.yaml", YAML);
    let log = log();
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    loader.register_plugin("a", plugin_a(&log));
    loader.register_plugin("b", Arc::new(FnPlugin::noop("b")));

    let override_cfg = Patch {
        id: Some("a".to_string()),
        config: Some(json!({"k": 99})),
        ..Patch::default()
    };
    let insert = Patch {
        insert: Some(vec![EntryOptions::new("b", "b")]),
        ..Patch::default()
    };

    let inc = Include::new(&loader, &path, vec![override_cfg, insert]);
    inc.load().unwrap();

    assert_eq!(snapshot(&log), vec!["apply:a:99"]);
    assert!(loader.fiber("b").is_some());
}

/// 写回：loader 更新后 write_back 把新配置落盘。
#[test]
fn include_write_back_persists() {
    let path = temp_file("entries.json", "[{\"id\":\"a\",\"name\":\"a\",\"config\":{\"k\":1}}]");
    let log = log();
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    loader.register_plugin("a", plugin_a(&log));

    let inc = Include::new(&loader, &path, vec![]);
    inc.load().unwrap();

    // loader 直接更新（config-only 热更）
    loader.update("a", {
        let mut o = EntryOptions::new("a", "a");
        o.config = json!({"k": 2});
        o
    }).unwrap();
    assert_eq!(snapshot(&log), vec!["apply:a:1", "apply:a:2"]);

    inc.write_back().unwrap();
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("\"k\": 2") || text.contains("k: 2"), "file: {text}");
}

/// 手动刷新：改文件 → refresh → 增删入口。
#[test]
fn include_refresh_syncs_tree() {
    let path = temp_file("entries.yaml", YAML);
    let log = log();
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    loader.register_plugin("a", plugin_a(&log));
    loader.register_plugin("b", Arc::new(FnPlugin::noop("b")));

    let inc = Include::new(&loader, &path, vec![]);
    inc.load().unwrap();
    let old_fid = loader.fiber("a").unwrap();

    // 修改文件：a 保留（config 变化 → 同一 fiber 重启）、b 新增
    fs::write(&path, "- id: a\n  name: a\n  config:\n    k: 5\n- id: b\n  name: b\n").unwrap();
    inc.refresh().unwrap();

    let new_fid = loader.fiber("a").unwrap();
    assert_eq!(old_fid, new_fid, "config change restarts the SAME fiber");
    assert!(loader.fiber("b").is_some());
    let last = snapshot(&log).last().cloned().unwrap_or_default();
    assert_eq!(last, "apply:a:5");
}

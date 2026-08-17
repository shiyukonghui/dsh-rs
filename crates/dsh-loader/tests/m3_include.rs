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

/// M33：patch insert 带 id → 向 group 的 config 数组插入（对齐 TS
/// `applyEntryPatches`：目标必须是 group，否则跳过）。
#[test]
fn apply_patches_insert_into_group() {
    let g = options_with_group("g", vec![
        json!({"id": "c1", "name": "a"}),
    ]);
    let data = vec![g];
    let patch = Patch {
        id: Some("g".to_string()),
        insert: Some(vec![EntryOptions::new("c2", "a")]),
        ..Patch::default()
    };
    let out = apply_entry_patches(&data, &[patch]);
    let g_out = out.first().unwrap();
    let children = g_out.config.as_array().unwrap();
    assert_eq!(children.len(), 2, "inserted into group config: {children:?}");
    assert_eq!(children[1].get("id").and_then(|v| v.as_str()), Some("c2"));

    // 非 group 目标：跳过
    let data2 = vec![options("x", "a", json!({}))];
    let patch2 = Patch {
        id: Some("x".to_string()),
        insert: Some(vec![EntryOptions::new("y", "a")]),
        ..Patch::default()
    };
    let out2 = apply_entry_patches(&data2, &[patch2]);
    assert_eq!(out2.len(), 1, "non-group target skipped");
}

/// M33：id patch 命中嵌套 group 子入口（对齐 TS entryMap 含子入口）。
#[test]
fn apply_patches_hits_nested_group_child() {
    let g = options_with_group("g", vec![
        json!({"id": "c1", "name": "a", "config": {"k": 1}}),
    ]);
    let data = vec![g];
    // 命中嵌套 c1 → config 覆盖
    let patch = Patch {
        id: Some("c1".to_string()),
        config: Some(json!({"k": 99})),
        ..Patch::default()
    };
    let out = apply_entry_patches(&data, &[patch]);
    let g_out = out.first().unwrap();
    let children = g_out.config.as_array().unwrap();
    let c1 = &children[0];
    assert_eq!(c1.get("config").and_then(|v| v.get("k")).and_then(|v| v.as_i64()), Some(99));
}

/// M39：`apply_entry_patches_with_warn`——patch 未命中（id 找不到/非 group/
/// name mismatch/缺 id）→ warn sink 收到诊断；命中 → 无警告。结果与静默版一致。
#[test]
fn apply_patches_with_warn_reports_skips() {
    let data = vec![
        options("a", "a", json!({"k": 1})),
        options_with_group("g", vec![json!({"id": "c1", "name": "a"})]),
    ];

    // 各种跳过场景：缺 id、id 找不到、name mismatch、insert 到非 group
    let patches = vec![
        Patch { id: None, config: Some(json!({})), ..Patch::default() },
        Patch { id: Some("nope".into()), config: Some(json!({})), ..Patch::default() },
        Patch { id: Some("a".into()), name: Some("wrong".into()), config: Some(json!({})), ..Patch::default() },
        Patch { id: Some("a".into()), insert: Some(vec![EntryOptions::new("x", "a")]), ..Patch::default() },
        Patch { id: Some("nope".into()), insert: Some(vec![EntryOptions::new("x", "a")]), ..Patch::default() },
    ];
    let mut warns = Vec::new();
    let out = apply_entry_patches_with_warn(&data, &patches, &mut |w| warns.push(w));

    // 结果 = 原数据（所有 patch 都被跳过）
    assert_eq!(out.len(), 2, "{out:?}");
    assert_eq!(
        warns,
        vec![
            "patch: id is required for non-insert patches",
            "patch: entry nope not found",
            "patch: name mismatch for a (expected a, got wrong), skipping",
            "patch insert: entry a is not a group",
            "patch insert: entry nope not found",
        ]
    );
}

/// M39：Include 的 read 收集 patch 警告（take_warns）；命中 patch 无警告。
#[test]
fn include_take_warns_collects_patch_skips() {
    let path = temp_file("warns.yaml", "- id: a\n  name: a\n");
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    loader.register_plugin("a", Arc::new(FnPlugin::noop("a")));

    // 一个命中（config 覆盖）+ 一个未命中（id 不存在）
    let inc = Include::new(&loader, &path, vec![
        Patch { id: Some("a".into()), config: Some(json!({"k": 9})), ..Patch::default() },
        Patch { id: Some("ghost".into()), config: Some(json!({})), ..Patch::default() },
    ]);
    inc.load().unwrap();

    let warns = inc.take_warns();
    assert_eq!(warns, vec!["patch: entry ghost not found"], "{warns:?}");

    // 再次 load：warns 重置（每次 read 重新收集）
    inc.refresh().unwrap();
    assert_eq!(inc.take_warns(), vec!["patch: entry ghost not found"]);
}

fn options(id: &str, name: &str, config: Value) -> EntryOptions {
    let mut o = EntryOptions::new(id, name);
    o.config = config;
    o
}

fn options_with_group(id: &str, children: Vec<Value>) -> EntryOptions {
    let mut o = EntryOptions::new(id, "group");
    o.group = true;
    o.config = Value::Array(children);
    o
}

/// M64：通用 overrides 字段覆盖（对齐 TS `applyEntryPatches` 的
/// `{ id, insert, name, ...overrides }` → `target[key] = value`）。
/// 覆盖 inject / isolate / intercept / disabled_expr，并验证与显式
/// config/disabled/group 合并（serde flatten 收集）。
#[test]
fn apply_patches_overrides_any_entry_field() {
    use std::collections::HashMap;

    let data = vec![options("a", "a", json!({"k": 1}))];

    // 显式 config + overrides 其余键（inject/isolate/intercept 走 flatten map）。
    let mut overrides = HashMap::new();
    overrides.insert("inject".to_string(), json!(["svc1"]));
    overrides.insert("isolate".to_string(), json!({"svc2": true}));
    overrides.insert("intercept".to_string(), json!({"svc3": {"tag": "x"}}));
    overrides.insert("disabled_expr".to_string(), json!("expr"));
    let patches = vec![Patch {
        id: Some("a".into()),
        config: Some(json!({"k": 9})),
        overrides,
        ..Patch::default()
    }];

    let out = apply_entry_patches(&data, &patches);
    assert_eq!(out.len(), 1);
    let a = &out[0];
    assert_eq!(a.config, json!({"k": 9}));
    assert_eq!(a.inject, vec!["svc1"]);
    assert_eq!(a.isolate, HashMap::from([("svc2".to_string(), Value::Bool(true))]));
    assert_eq!(a.intercept, HashMap::from([("svc3".to_string(), json!({"tag": "x"}))]));
    assert_eq!(a.disabled_expr.as_deref(), Some("expr"));
}

/// M64：JSON patch（serde 反序列化）→ overrides 收集——`inject` 等键从
/// flatten map 落到运行时 patch，应用后生效；round-trip 序列化并回同对象。
#[test]
fn patch_deserializes_flatten_overrides_and_roundtrips() {
    use dsh_loader::Patch;

    let json = r#"{"id":"a","inject":["svc"],"isolate":{"svc":true},"config":{"k":2}}"#;
    let patch: Patch = serde_json::from_str(json).unwrap();
    // 显式字段消费 config；inject/isolate 进 overrides。
    assert_eq!(patch.id.as_deref(), Some("a"));
    assert_eq!(patch.config, Some(json!({"k": 2})));
    assert_eq!(patch.overrides.get("inject"), Some(&json!(["svc"])));
    assert_eq!(patch.overrides.get("isolate"), Some(&json!({"svc": true})));

    // round-trip：flatten 并回同一对象（显式键 + overrides 键并回）。
    let out: serde_json::Value = serde_json::to_value(&patch).unwrap();
    let obj = out.as_object().unwrap();
    assert_eq!(obj.get("inject"), Some(&json!(["svc"])));
    assert_eq!(obj.get("isolate"), Some(&json!({"svc": true})));
    assert_eq!(obj.get("config"), Some(&json!({"k": 2})));
    assert_eq!(obj.get("id"), Some(&json!("a")));

    // 应用到入口：overrides 生效。
    let data = vec![options("a", "a", json!({"k": 1}))];
    let res = apply_entry_patches(&data, std::slice::from_ref(&patch));
    assert_eq!(res[0].inject, vec!["svc".to_string()]);
    assert_eq!(res[0].config, json!({"k": 2}));
}

/// M64：overrides 应用到嵌套 group 子入口（递归 patch_update）。
#[test]
fn apply_patches_overrides_nested_group_child() {
    use std::collections::HashMap;

    let data = vec![options_with_group("g", vec![json!({"id": "c1", "name": "c1"})])];
    let mut overrides = HashMap::new();
    overrides.insert("inject".to_string(), json!(["dep"]));
    let patches = vec![Patch {
        id: Some("c1".into()),
        overrides,
        ..Patch::default()
    }];

    let out = apply_entry_patches(&data, &patches);
    let g = &out[0];
    let children = g.config.as_array().unwrap();
    let c1: Value = children[0].clone();
    assert_eq!(c1["inject"], json!(["dep"]), "{c1:?}");
}

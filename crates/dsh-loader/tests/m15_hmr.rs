//! M15：HMR（热重载）——文件 watcher + `Include::refresh` 自动化。
//! 对应 Cordis `cordis-plugin-hmr` 的 `registerConfig(filename, refresh)`：
//! 监听 add/change/unlink → refresh 串行执行；失败记录（hmr/error 语义）。
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

fn tmp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("dsh-m15-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// HMR：首次 poll 不触发（初始快照 = chokidar ready）；内容变化才触发 refresh。
#[test]
fn hmr_first_poll_no_refresh_change_triggers() {
    let dir = tmp_dir("first");
    let path = dir.join("config.yml");
    std::fs::write(&path, "v1").unwrap();

    let log = log();
    let hmr = Hmr::new();
    hmr.register_config(&path, {
        let log = log.clone();
        Rc::new(move || {
            push(&log, "refresh");
            Ok(())
        })
    });

    // 首次 poll：不触发
    let changed = hmr.poll();
    assert!(changed.is_empty(), "{changed:?}");
    assert_eq!(snapshot(&log), Vec::<String>::new());

    // 内容变化 → 触发
    std::fs::write(&path, "v2").unwrap();
    let changed = hmr.poll();
    assert_eq!(changed, vec![path.to_string_lossy().to_string()]);
    assert_eq!(snapshot(&log), vec!["refresh"]);

    // 再次 poll：无变化不触发
    let changed = hmr.poll();
    assert!(changed.is_empty(), "{changed:?}");
    assert_eq!(snapshot(&log), vec!["refresh"]);

    std::fs::remove_dir_all(&dir).ok();
}

/// HMR：文件删除（unlink）触发 refresh；重建（add）也触发。
#[test]
fn hmr_delete_and_recreate_trigger() {
    let dir = tmp_dir("del");
    let path = dir.join("config.yml");
    std::fs::write(&path, "v1").unwrap();

    let log = log();
    let hmr = Hmr::new();
    hmr.register_config(&path, {
        let log = log.clone();
        Rc::new(move || {
            push(&log, "refresh");
            Ok(())
        })
    });
    hmr.poll(); // 建立快照

    // 删除 → unlink 触发
    std::fs::remove_file(&path).unwrap();
    let changed = hmr.poll();
    assert_eq!(changed.len(), 1, "{changed:?}");
    assert_eq!(snapshot(&log), vec!["refresh"]);

    // 重建 → add 触发
    std::fs::write(&path, "v2").unwrap();
    let changed = hmr.poll();
    assert_eq!(changed.len(), 1, "{changed:?}");
    assert_eq!(snapshot(&log), vec!["refresh", "refresh"]);

    std::fs::remove_dir_all(&dir).ok();
}

/// HMR：多个注册文件独立检测；refresh 失败记录到 errors（hmr/error 语义）。
#[test]
fn hmr_multiple_files_and_error_recording() {
    let dir = tmp_dir("multi");
    let a = dir.join("a.yml");
    let b = dir.join("b.yml");
    std::fs::write(&a, "a1").unwrap();
    std::fs::write(&b, "b1").unwrap();

    let log = log();
    let hmr = Hmr::new();
    hmr.register_config(&a, {
        let log = log.clone();
        Rc::new(move || {
            push(&log, "refresh-a");
            Ok(())
        })
    });
    hmr.register_config(&b, {
        Rc::new(move || Err(CordisError::Internal("refresh-b failed".into())))
    });
    hmr.poll(); // 建立快照

    std::fs::write(&a, "a2").unwrap();
    std::fs::write(&b, "b2").unwrap();
    let changed = hmr.poll();
    assert_eq!(changed.len(), 2, "{changed:?}");
    // a 成功、b 失败记录
    assert_eq!(snapshot(&log), vec!["refresh-a"]);
    let errors = hmr.take_errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].1.to_string().contains("refresh-b failed"), "{errors:?}");

    std::fs::remove_dir_all(&dir).ok();
}

/// HMR 集成：修改 include 文件 → poll → Include::refresh → loader 树热更。
#[test]
fn hmr_include_refresh_hot_reload() {
    let dir = tmp_dir("include");
    let path = dir.join("config.yml");
    std::fs::write(
        &path,
        "- id: e1\n  name: a\n  config: { k: 1 }\n",
    )
    .unwrap();

    let log = log();
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    loader.register_plugin("a", plugin_a(&log));
    let include = Include::new(&loader, &path, vec![]);
    include.load().unwrap();
    assert_eq!(snapshot(&log), vec!["apply:a:1"]);
    let old_fid = loader.fiber("e1").unwrap();

    // 注册 HMR：变化 → include.refresh
    let hmr = Hmr::new();
    hmr.register_config(&path, {
        let include = include.clone();
        Rc::new(move || include.refresh())
    });
    hmr.poll(); // 建立快照

    // 修改配置（k=2）→ poll → 热更
    std::fs::write(
        &path,
        "- id: e1\n  name: a\n  config: { k: 2 }\n",
    )
    .unwrap();
    let changed = hmr.poll();
    assert_eq!(changed.len(), 1, "{changed:?}");

    let new_fid = loader.fiber("e1").unwrap();
    assert_eq!(cordis.fiber_state(new_fid), Some(FiberState::Active));
    // config 热更（fiber.update restart）不换 fiber id；以 log 与 config 验证
    assert_eq!(snapshot(&log), vec!["apply:a:1", "apply:a:2"]);
    let cfg = loader
        .state
        .borrow()
        .entries
        .get("e1")
        .map(|e| e.options.config.clone())
        .unwrap();
    assert_eq!(cfg, json!({"k": 2}), "entry config hot-reloaded via HMR");
    assert!(hmr.take_errors().is_empty());
    let _ = old_fid;

    std::fs::remove_dir_all(&dir).ok();
}

/// M35：事件驱动 watcher——`Hmr::watch` 启动后台 notify watcher（mpsc 桥接），
/// 文件变化经事件队列到达，`poll()` 消费事件 → 指纹确认 → refresh。
/// 事件只作唤醒信号：内容未变的事件（如 touch）不触发 refresh。
#[test]
fn hmr_watch_event_driven_triggers_refresh() {
    let dir = tmp_dir("watch");
    let path = dir.join("config.yml");
    std::fs::write(&path, "v1").unwrap();

    let log = log();
    let hmr = Hmr::new();
    hmr.register_config(&path, {
        let log = log.clone();
        Rc::new(move || {
            push(&log, "refresh");
            Ok(())
        })
    });

    // 启动事件驱动 watcher（后台线程 + mpsc；无 watcher 时 poll 退化为轮询）
    let watched = hmr.watch(std::slice::from_ref(&path));
    assert!(watched.is_ok(), "{watched:?}");

    // 首次 poll：建立快照，不触发（与轮询语义一致）
    let changed = hmr.poll();
    assert!(changed.is_empty(), "{changed:?}");
    assert_eq!(snapshot(&log), Vec::<String>::new());

    // 等待 watcher 就绪后改文件 → 事件到达 → poll 消费 → refresh
    std::thread::sleep(std::time::Duration::from_millis(300));
    std::fs::write(&path, "v2").unwrap();

    // 轮询等待事件到达（后台线程异步投递；最多 2s）
    let mut triggered = false;
    for _ in 0..40 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        let changed = hmr.poll();
        if !changed.is_empty() {
            triggered = true;
            break;
        }
    }
    assert!(triggered, "event-driven poll should trigger refresh");
    assert_eq!(snapshot(&log), vec!["refresh"]);

    // 事件消费后再次 poll：无新变化不触发
    let changed = hmr.poll();
    assert!(changed.is_empty(), "{changed:?}");
    assert_eq!(snapshot(&log), vec!["refresh"]);

    hmr.unwatch();
    std::fs::remove_dir_all(&dir).ok();
}

/// M35：事件驱动路径下，未注册路径的变化（事件到达但不在注册表）被忽略。
#[test]
fn hmr_watch_ignores_unregistered_paths() {
    let dir = tmp_dir("watch-ignored");
    let path = dir.join("config.yml");
    let other = dir.join("other.txt");
    std::fs::write(&path, "v1").unwrap();
    std::fs::write(&other, "x").unwrap();

    let log = log();
    let hmr = Hmr::new();
    hmr.register_config(&path, {
        let log = log.clone();
        Rc::new(move || {
            push(&log, "refresh");
            Ok(())
        })
    });
    let watched = hmr.watch(std::slice::from_ref(&path));
    assert!(watched.is_ok(), "{watched:?}");
    hmr.poll(); // 建立快照

    std::thread::sleep(std::time::Duration::from_millis(300));
    // 修改未注册文件（watcher 只监视注册路径，事件根本不会到达）
    std::fs::write(&other, "y").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));
    let changed = hmr.poll();
    assert!(changed.is_empty(), "{changed:?}");
    assert_eq!(snapshot(&log), Vec::<String>::new());

    // 注册路径变化仍正常触发
    std::fs::write(&path, "v2").unwrap();
    let mut triggered = false;
    for _ in 0..40 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        if !hmr.poll().is_empty() {
            triggered = true;
            break;
        }
    }
    assert!(triggered, "registered path change should trigger");
    assert_eq!(snapshot(&log), vec!["refresh"]);

    hmr.unwatch();
    std::fs::remove_dir_all(&dir).ok();
}

/// M38：refresh 失败经 error sink 事件化（对齐 Cordis `hmr/config-update-failed`
/// 的 `ctx.parallel(filename, error)` 语义）——sink 收到 (filename, error)；
/// `take_errors` 查询仍保留（双通道）。
#[test]
fn hmr_error_sink_receives_failures() {
    let dir = tmp_dir("error-sink");
    let path = dir.join("config.yml");
    std::fs::write(&path, "v1").unwrap();

    // sink：记录收到的 (filename, error)（宿主可注入 Cordis `parallel` emit）
    let sink_log = log();
    let hmr = Hmr::new();
    hmr.set_error_sink({
        let sink_log = sink_log.clone();
        Rc::new(move |filename: &str, error: &dsh_core::CordisError| {
            push(&sink_log, format!("sink:{filename}:{error}"));
        })
    });
    hmr.register_config(&path, {
        Rc::new(move || Err(dsh_core::CordisError::Internal("refresh boom".into())))
    });
    hmr.poll(); // 建立快照

    // 内容变化 → refresh 失败 → sink 收到 (filename, error)
    std::fs::write(&path, "v2").unwrap();
    let changed = hmr.poll();
    assert_eq!(changed.len(), 1, "{changed:?}");

    let sink_msgs = snapshot(&sink_log);
    assert_eq!(sink_msgs.len(), 1, "{sink_msgs:?}");
    assert!(
        sink_msgs[0].contains("refresh boom"),
        "sink received error, got: {sink_msgs:?}"
    );
    assert!(
        sink_msgs[0].contains("config.yml"),
        "sink received filename, got: {sink_msgs:?}"
    );

    // take_errors 查询仍可用（双通道）
    let errors = hmr.take_errors();
    assert_eq!(errors.len(), 1, "{errors:?}");

    std::fs::remove_dir_all(&dir).ok();
}

/// M38：error sink 未设置（None）→ 仅记录 errors，不 panic（向后兼容）。
#[test]
fn hmr_without_error_sink_records_only() {
    let dir = tmp_dir("no-sink");
    let path = dir.join("config.yml");
    std::fs::write(&path, "v1").unwrap();

    let hmr = Hmr::new();
    hmr.register_config(&path, {
        Rc::new(move || Err(dsh_core::CordisError::Internal("boom".into())))
    });
    hmr.poll(); // 建立快照

    std::fs::write(&path, "v2").unwrap();
    let changed = hmr.poll();
    assert_eq!(changed.len(), 1, "{changed:?}");
    assert_eq!(hmr.take_errors().len(), 1, "errors recorded without sink");

    std::fs::remove_dir_all(&dir).ok();
}

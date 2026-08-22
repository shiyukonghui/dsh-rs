//! M3b：`dsh_persistence::atomic_write`——settings/credentials 文件写路径的
//! 原子发布基建（temp + sync + rename）。
//!
//! 语义（区别于 jsonl 物化：**可覆盖既有文件**）：
//! - 目标不存在 → 创建；
//! - 目标已存在 -> 原子替换（rename 覆盖，非拒绝——settings 需反复改）；
//! - 由目标父目录派生同前缀 temp；失败清理。
//! - 不留下 `.tmp` 残留（成功/失败均清理）。

use dsh_persistence::fs_atomic::atomic_write;
use std::fs;
use std::path::PathBuf;

fn temp_target(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("dsh-atomic-{tag}-{}", std::process::id()))
}

#[test]
fn creates_new_file_and_cleans_tmp() {
    let target = temp_target("create");
    let _ = fs::remove_file(&target);
    atomic_write(&target, b"hello").expect("write ok");
    assert_eq!(fs::read(&target).unwrap(), b"hello");
    // 无 tmp 残留。
    let stem = target.file_name().unwrap().to_string_lossy().into_owned();
    let parent = target.parent().unwrap();
    let leftover: Vec<_> = fs::read_dir(parent)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with(&stem))
        .collect();
    assert_eq!(leftover.len(), 1, "only the final file, no .tmp");
    fs::remove_file(&target).ok();
}

#[test]
fn overwrites_existing_file() {
    let target = temp_target("overwrite");
    fs::write(&target, b"old").unwrap();
    atomic_write(&target, b"new-content").expect("overwrite ok");
    assert_eq!(fs::read(&target).unwrap(), b"new-content");
    let stem = target.file_name().unwrap().to_string_lossy().into_owned();
    let parent = target.parent().unwrap();
    let leftover: Vec<_> = fs::read_dir(parent)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with(&stem)
            && e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert_eq!(leftover.len(), 0, "no .tmp leftover after overwrite");
    fs::remove_file(&target).ok();
}

#[test]
fn atomic_write_utf8_payload() {
    // 保证 UTF-8 内容整文件原样落盘（settings 字段值逐字）。
    let target = temp_target("utf8");
    let _ = fs::remove_file(&target);
    let content = "a: 1\n__ëy: secret\n".as_bytes();
    atomic_write(&target, content).expect("utf8 write ok");
    assert_eq!(fs::read(&target).unwrap(), content);
    fs::remove_file(&target).ok();
}

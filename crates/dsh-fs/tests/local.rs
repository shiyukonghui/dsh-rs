//! dsh-fs：本地 provider 读写契约（M5-DESIGN §4.1/§4.2）。
//!
//! 参考 `fs-local/src/index.ts`：writeText 原子写（stale guard / createIfAbsent /
//! not-regular-file），读回内容，operation create|update，version 新鲜度；editText
//! 字面替换（唯一性 / 缺失 / 版本守卫）。

use dsh_fs::{
    FsEditRequest, FsErrorCode, FsVersion, FsWriteIntent, LocalFileSystem, ResolveOptions,
};

fn temp_ws() -> (LocalFileSystem, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("dshfs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create ws");
    (LocalFileSystem::new(dir.clone()), dir)
}

fn resolve(fs: &LocalFileSystem, path: &str) -> dsh_fs::FsTarget {
    fs.resolve(path, ResolveOptions { cwd: None }).expect("resolve")
}

#[test]
fn write_create_then_read_roundtrip() {
    let (fs, _dir) = temp_ws();
    let target = resolve(&fs, "hello.txt");
    let out = fs
        .write_text(&target, "hello world\n", None, None)
        .expect("create write");
    assert_eq!(out.operation, "create");
    assert!(out.before.is_none(), "create has no diff basis");
    assert_eq!(out.after, "hello world\n");

    let read = fs.read_text(&target, Default::default()).expect("read");
    assert_eq!(read.content, "hello world\n");
}

#[test]
fn write_overwrite_is_update_with_before() {
    let (fs, _dbir) = temp_ws();
    let target = resolve(&fs, "f.txt");
    fs.write_text(&target, "one\n", None, None).expect("write 1");
    let out = fs.write_text(&target, "two\n", None, None).expect("write 2");
    assert_eq!(out.operation, "update");
    assert_eq!(out.before.as_deref(), Some("one\n"), "before = prior content");
    assert_eq!(out.after, "two\n");
}

#[test]
fn replace_if_version_mismatch_is_stale() {
    let (fs, _dbir) = temp_ws();
    let target = resolve(&fs, "g.txt");
    fs.write_text(&target, "v1\n", None, None).expect("write v1");
    let stale: FsVersion = "bogus-version".into();
    let err = fs
        .write_text(
            &target,
            "v2\n",
            Some(FsWriteIntent::ReplaceIfVersion { version: stale }),
            None,
        )
        .unwrap_err();
    assert_eq!(err.code(), FsErrorCode::FsStaleVersion);
}

#[test]
fn create_if_absent_onto_existing_is_not_observed() {
    let (fs, _dbir) = temp_ws();
    let target = resolve(&fs, "h.txt");
    fs.write_text(&target, "x\n", None, None).expect("write x");
    let err = fs
        .write_text(&target, "y\n", Some(FsWriteIntent::CreateIfAbsent), None)
        .unwrap_err();
    assert_eq!(err.code(), FsErrorCode::FsNotObserved);
}

#[test]
fn edit_literal_unique_swap() {
    let (fs, _dbir) = temp_ws();
    let target = resolve(&fs, "e.txt");
    fs.write_text(&target, "a\nb\n", None, None).expect("seed");
    let req = FsEditRequest {
        old_string: "a".to_string(),
        new_string: "A1".to_string(),
        replace_all: false,
    };
    let out = fs.edit_text(&target, &req, None, None).expect("edit");
    assert_eq!(out.before, "a\nb\n");
    assert_eq!(out.after, "A1\nb\n");
    let read = fs.read_text(&target, Default::default()).expect("read");
    assert_eq!(read.content, "A1\nb\n");
}

#[test]
fn edit_not_found_is_edit_not_found() {
    let (fs, _dbir) = temp_ws();
    let target = resolve(&fs, "e2.txt");
    fs.write_text(&target, "abc\n", None, None).expect("seed");
    let req = FsEditRequest {
        old_string: "zzz".to_string(),
        new_string: "x".to_string(),
        replace_all: false,
    };
    let err = fs.edit_text(&target, &req, None, None).unwrap_err();
    assert_eq!(err.code(), FsErrorCode::FsEditNotFound);
}

#[test]
fn edit_ambiguous_is_ambiguous() {
    let (fs, _dbir) = temp_ws();
    let target = resolve(&fs, "e3.txt");
    fs.write_text(&target, "a\na\n", None, None).expect("seed");
    let req = FsEditRequest {
        old_string: "a".to_string(),
        new_string: "b".to_string(),
        replace_all: false,
    };
    let err = fs.edit_text(&target, &req, None, None).unwrap_err();
    assert_eq!(err.code(), FsErrorCode::FsAmbiguousEdit);
}

#[test]
fn write_to_directory_is_not_regular_file() {
    let (fs, dir) = temp_ws();
    let target = resolve(&fs, "subdir");
    std::fs::create_dir_all(dir.join("subdir")).expect("mkdir");
    let err = fs.write_text(&target, "x\n", None, None).unwrap_err();
    assert_eq!(err.code(), FsErrorCode::FsNotRegularFile);
}

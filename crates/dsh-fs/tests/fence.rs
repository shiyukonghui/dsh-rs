//! dsh-fs：进程内沙箱围栏（M5-DESIGN §4.4）。
//!
//! 参考 `fs-sandbox/src/containment.ts` + `index.ts`：`checkedTarget`（danger 直通 /
//! read-only 拒 / workspace-write is_path_under）、`isPathUnder`（词法快路径 + 身份兜底）。
//! 围栏仅加在 writeText/editText（读路径全放行）；拒绝用 FS_SANDBOX_DENIED。

use dsh_fs::{
    sandbox::{checked_target, is_path_under, SandboxPolicy},
    FsErrorCode, LocalFileSystem, ResolveOptions,
};
use dsh_sandbox::SandboxMode;

fn temp_root() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "dshfsbox-{}-{}",
        std::process::id(),
        std::sync::atomic::AtomicU64::fetch_add(
            &COUNTER,
            1,
            std::sync::atomic::Ordering::Relaxed
        )
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create root");
    dir
}

static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[test]
fn is_path_under_lexical_below_root() {
    let root = temp_root();
    let child = root.join("a").join("b.txt");
    assert!(is_path_under(&child, &root, true), "child under root");
    assert!(is_path_under(&root, &root, true), "root itself");
    // 根前缀边界：root/ab 不是 root/a
    let sibling = root.join("ab");
    let sub = root.join("a");
    assert!(!is_path_under(&sibling, &sub, true), "sibling is not under subdir");
}

#[test]
fn is_path_under_rejects_outside_and_sibling() {
    let root = temp_root();
    let root2 = temp_root();
    assert!(!is_path_under(&root2.join("x"), &root, true), "other root not under");
    // 词法兄弟：同根下的兄弟目录（root/a 下 vs root/ab）不是 from root/ab
    let sub = root.join("a");
    std::fs::create_dir_all(&sub).expect("mkdir a");
    let sibling = root.join("ab.txt");
    assert!(!is_path_under(&sibling, &sub, true), "sibling not under subdir");
}

#[test]
fn read_only_denies_write() {
    let (fs, root) = (LocalFileSystem::new(temp_root()), temp_root());
    let _ = root.join("x");
    let policy = SandboxPolicy { mode: SandboxMode::ReadOnly, workspace_root: None };
    let target = fs.resolve("f.txt", ResolveOptions { cwd: Some(root.clone()) }).expect("resolve");
    let err = checked_target(&target, &policy).unwrap_err();
    assert_eq!(err.code(), FsErrorCode::FsSandboxDenied);
}

#[test]
fn workspace_write_allows_inside_denies_outside() {
    let root = temp_root();
    let fs = LocalFileSystem::new(root.clone());
    let policy = SandboxPolicy {
        mode: SandboxMode::WorkspaceWrite,
        workspace_root: Some(root.clone()),
    };
    let inside = fs.resolve("in.txt", ResolveOptions { cwd: Some(root.clone()) }).expect("resolve");
    checked_target(&inside, &policy).expect("inside workspace allowed");

    // 外部路径：当前工作目录（明显不在 tmpdir/workspace 下，且非 tmpdir 可写根）。
    let outside_dir = std::env::current_dir().expect("cwd");
    let outside = fs.resolve(
        &outside_dir.join("out.should-not-exist.txt").to_string_lossy(),
        ResolveOptions { cwd: Some(root.clone()) },
    )
    .expect("resolve");
    let err = checked_target(&outside, &policy).unwrap_err();
    assert_eq!(err.code(), FsErrorCode::FsSandboxDenied);
}

#[test]
fn danger_full_access_passes_anywhere() {
    let root = temp_root();
    let fs = LocalFileSystem::new(root.clone());
    let policy = SandboxPolicy { mode: SandboxMode::DangerFullAccess, workspace_root: None };
    let target = fs.resolve("any.txt", ResolveOptions { cwd: Some(root.clone()) }).expect("resolve");
    assert!(checked_target(&target, &policy).is_ok(), "danger passes anything");
}

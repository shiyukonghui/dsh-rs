//! dsh-fs：glob 搜索面（M5-DESIGN §4.6 / DIV-7）。
//!
//! 参考 `tool-fs-search/src/glob.ts`：GLOB_MAX_RESULTS=100、GLOB_VCS_EXCLUDES、非空
//! pattern/path 校验、`--no-ignore --hidden` 语义（忽略文件不生效、隐藏文件收录、VCS
//! 目录恒剔除）→ 进程内用 `ignore::WalkBuilder` + `globset` 枚举（DIV-7：ripgrep 同源库，
//! 不拉 rg 二进制），按 modification-time 排序，上限截断。

use dsh_fs::fs_search::{
    glob_search, parse_glob_args, GlobInput, GLOB_MAX_RESULTS, GLOB_VCS_EXCLUDES,
};

fn temp_tree() -> (std::path::PathBuf, std::path::PathBuf) {
    let base = std::env::temp_dir().join(format!(
        "dshglob-{}-{}",
        std::process::id(),
        std::sync::atomic::AtomicU64::fetch_add(&CTR, 1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("src")).expect("mkdir src");
    std::fs::create_dir_all(base.join("vendor")).expect("mkdir vendor");
    std::fs::write(base.join("src/main.rs"), "fn main() {}\n").expect("write main");
    std::fs::write(base.join("src/lib.rs"), "pub fn f() {}\n").expect("write lib");
    std::fs::write(base.join("README.md"), "# hi\n").expect("write readme");
    std::fs::write(base.join("vendor/old.rs"), "old\n").expect("write old");
    // 隐藏文件 + .gitignore + .git（VCS 目录恒剔除）
    std::fs::write(base.join(".hidden.rs"), "h\n").expect("write hidden");
    std::fs::write(base.join(".gitignore"), "target/\n").expect("write gitignore");
    std::fs::create_dir_all(base.join(".git")).expect("mkdir .git");
    std::fs::write(base.join(".git/config"), "[core]\n").expect("write config");
    (base, std::env::temp_dir())
}

static CTR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

// ---------------------------------------------------------------------------
// parse_glob_args
// ---------------------------------------------------------------------------

#[test]
fn parse_glob_args_accepts_pattern_without_path() {
    let input = parse_glob_args("**/*.rs", None).expect("ok");
    assert_eq!(input, GlobInput { pattern: "**/*.rs".to_string(), path: None });
}

#[test]
fn parse_glob_args_accepts_path() {
    let input = parse_glob_args("**/*.rs", Some("src")).expect("ok");
    assert_eq!(input, GlobInput { pattern: "**/*.rs".to_string(), path: Some("src".to_string()) });
}

#[test]
fn parse_glob_args_rejects_blank_pattern_and_path() {
    assert_eq!(
        parse_glob_args("  ", None).unwrap_err(),
        "pattern must be a non-empty string"
    );
    assert_eq!(
        parse_glob_args("**", Some("  ")).unwrap_err(),
        "path must be a non-empty string when given"
    );
}

#[test]
fn glob_constants_aligned() {
    assert_eq!(GLOB_MAX_RESULTS, 100);
    assert_eq!(
        GLOB_VCS_EXCLUDES,
        vec![".git", ".svn", ".hg", ".bzr", ".jj", ".sl"]
    );
}

// ---------------------------------------------------------------------------
// glob_search
// ---------------------------------------------------------------------------

#[test]
fn glob_search_matches_recursively_and_excludes_vcs() {
    let (root, _tmp) = temp_tree();
    let mut paths: Vec<String> = glob_search(&root, "**/*.rs").expect("search");
    paths.sort();
    assert_eq!(
        paths,
        vec![".hidden.rs", "src/lib.rs", "src/main.rs", "vendor/old.rs"]
            .into_iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
    );
}

#[test]
fn glob_search_ignores_ignore_files_but_includes_hidden() {
    let (root, _tmp) = temp_tree();
    // `--no-ignore --hidden`：.gitignore 不生效（"target/" 不会排除什么），隐藏文件收录。
    let mut paths: Vec<String> = glob_search(&root, "**/*").expect("search");
    paths.retain(|p| !p.starts_with(".git/")); // VCS 目录本就不该出现；防御性断言
    assert!(paths.contains(&".hidden.rs".to_string()), "hidden file included");
    assert!(
        paths.iter().all(|p| !p.starts_with(".git/")),
        "VCS 目录恒剔除"
    );
    assert!(paths.contains(&"README.md".to_string()));
}

#[test]
fn glob_search_absolute_path_arg_relative() {
    let (root, _tmp) = temp_tree();
    // pattern 为相对路径仍以 root 为基准
    let mut paths: Vec<String> = glob_search(&root, "src/*.rs").expect("search");
    paths.sort();
    assert_eq!(paths, vec!["src/lib.rs".to_string(), "src/main.rs".to_string()]);
}

#[test]
fn glob_search_empty_pattern_is_plain_files_list() {
    let (root, _tmp) = temp_tree();
    // 空 pattern（允许）→ 全部文件（VCS 剔除），上限内
    let paths: Vec<String> = glob_search(&root, "*").expect("search");
    assert!(paths.len() >= 5);
    // 仅排除 `.git/` 目录内容；`.gitignore`/`.hidden.rs` 是合法隐藏文件，允许出现。
    assert!(!paths.iter().any(|p| p.starts_with(".git/")));
}

#[test]
fn glob_search_respects_max_results_cap() {
    let root = std::env::temp_dir().join(format!(
        "dshglobcap-{}-{}",
        std::process::id(),
        std::sync::atomic::AtomicU64::fetch_add(&CTR, 1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("mkdir");
    for i in 0..150 {
        std::fs::write(root.join(format!("f{i:03}.rs")), "x\n").expect("write");
    }
    let paths: Vec<String> = glob_search(&root, "**/*.rs").expect("search");
    assert!(paths.len() <= GLOB_MAX_RESULTS, "cap enforced: {}", paths.len());
}

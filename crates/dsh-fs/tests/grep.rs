//! dsh-fs：grep 搜索面（M5-DESIGN §4.6 / DIV-7）。
//!
//! 参考 `tool-fs-search/src/grep.ts` + `search-core.ts`：GREP_MAX_MATCHES=250、
//! GREP_MAX_LINE_BYTES=2000、parseGrepArgs 校验（含 include 单正 glob 约束）、
//! previewLine 头部截断、retainGrepMatches、formatGrepMatches / formatGrepOutput、
//! SEARCH_* 词表；引擎进程内 ignore+regex（与 glob 相反：保持默认过滤器）。

use dsh_fs::grep::{
    format_grep_matches, format_grep_output, grep_search, grep_search_in, parse_grep_args,
    preview_line, retain_grep_matches, GrepErrorCode, GrepInput, GrepMatch,
};
use std::sync::atomic::{AtomicU64, Ordering};

static CTR: AtomicU64 = AtomicU64::new(0);

fn temp_dir(name: &str) -> std::path::PathBuf {
    let base = std::env::temp_dir().join(format!(
        "dshgrep-{name}-{}-{}",
        std::process::id(),
        CTR.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).expect("mkdir");
    base
}

// ---------------------------------------------------------------------------
// parse_grep_args
// ---------------------------------------------------------------------------

#[test]
fn parse_grep_args_accepts_plain() {
    let input = parse_grep_args("foo", None, None).expect("ok");
    assert_eq!(
        input,
        GrepInput { pattern: "foo".into(), path: None, include: None }
    );
}

#[test]
fn parse_grep_args_accepts_whitespace_pattern_and_extras() {
    // 空白是合法正则（不同于 glob 的 trim 校验）
    let input = parse_grep_args("foo bar", Some("src"), Some("*.rs")).expect("ok");
    assert_eq!(
        input,
        GrepInput {
            pattern: "foo bar".into(),
            path: Some("src".into()),
            include: Some("*.rs".into())
        }
    );
}

#[test]
fn parse_grep_args_rejects_empty_pattern() {
    assert_eq!(
        parse_grep_args("", None, None).unwrap_err(),
        "pattern must be a non-empty string"
    );
}

#[test]
fn parse_grep_args_rejects_blank_path() {
    assert_eq!(
        parse_grep_args("foo", Some("  "), None).unwrap_err(),
        "path must be a non-empty string when given"
    );
}

#[test]
fn parse_grep_args_include_one_positive_glob() {
    // 空白 include
    assert_eq!(
        parse_grep_args("foo", None, Some("  ")).unwrap_err(),
        "include must be a non-empty glob when given"
    );
    // 否定模式
    assert_eq!(
        parse_grep_args("foo", None, Some("!*.rs")).unwrap_err(),
        "include must be a positive glob filter; negated patterns (\"!…\") are not supported"
    );
    // 顶层逗号列表
    assert_eq!(
        parse_grep_args("foo", None, Some("*.ts,*.js")).unwrap_err(),
        "include must be one glob, not a comma-separated list (use {a,b} alternation instead)"
    );
    // 花括号组内逗号合法
    assert!(parse_grep_args("foo", None, Some("*.{ts,tsx}")).is_ok());
    assert!(parse_grep_args("foo", None, Some("{a,b}")).is_ok());
}

#[test]
fn grep_constants_aligned() {
    assert_eq!(dsh_fs::grep::GREP_MAX_MATCHES, 250);
    assert_eq!(dsh_fs::grep::GREP_MAX_LINE_BYTES, 2000);
}

// ---------------------------------------------------------------------------
// preview_line
// ---------------------------------------------------------------------------

#[test]
fn preview_line_short_unchanged() {
    assert_eq!(preview_line("hello world", 2000), ("hello world".into(), false));
}

#[test]
fn preview_line_truncates_ascii_and_marks() {
    let long = "x".repeat(10);
    assert_eq!(preview_line(&long, 4), ("xxxx".into(), true));
    assert_eq!(
        dsh_fs::grep::preview_line_rendered(&long, 4),
        "xxxx (line truncated)"
    );
}

#[test]
fn preview_line_preserves_utf8_boundary() {
    // 3 字节中文，max_bytes=2 → 容不下一个字符，须退回 0。
    let s = "中中中";
    let (text, truncated) = preview_line(s, 2);
    assert_eq!(text, "", "UTF-8 边界保持：2 字节容纳不下一个 3 字节字符须退回");
    assert!(truncated);
    // max_bytes=3 → 恰一个字符
    let (text, truncated) = preview_line(s, 3);
    assert_eq!(text, "中");
    assert!(truncated);
    // max_bytes=9 → 3 个字符，恰在边界，不截断
    let (text, truncated) = preview_line(s, 9);
    assert_eq!(text, "中中中");
    assert!(!truncated);
}

// ---------------------------------------------------------------------------
// retain_grep_matches
// ---------------------------------------------------------------------------

fn m(path: &str, n: u64, line: &str) -> GrepMatch {
    GrepMatch { path: path.into(), line_number: n, line: line.into() }
}

#[test]
fn retain_under_cap_keeps_all_and_previews() {
    let all = vec![m("a.rs", 1, "hello"), m("a.rs", 2, "world")];
    let retained = retain_grep_matches(&all, 250, 2000);
    assert_eq!(retained.seen, 2);
    assert_eq!(retained.kept(), 2);
    assert!(!retained.truncated());
    assert_eq!(retained.items[0].line, "hello");
}

#[test]
fn retain_over_cap_keeps_head_and_previews() {
    let all: Vec<GrepMatch> =
        (0..5).map(|i| m("a.rs", i + 1, &"y".repeat(10))).collect();
    let retained = retain_grep_matches(&all, 2, 4);
    assert_eq!(retained.seen, 5);
    assert_eq!(retained.kept(), 2);
    assert!(retained.truncated());
    assert_eq!(retained.items[0].line, "yyyy (line truncated)");
}

// ---------------------------------------------------------------------------
// format_grep_matches / format_grep_output
// ---------------------------------------------------------------------------

#[test]
fn format_grep_matches_groups_by_file() {
    let matches = vec![
        m("src/a.rs", 3, "foo"),
        m("src/b.rs", 1, "bar"),
        m("src/a.rs", 7, "baz"),
    ];
    let text = format_grep_matches(&matches);
    assert_eq!(
        text,
        "src/a.rs\nLine 3: foo\nLine 7: baz\n\nsrc/b.rs\nLine 1: bar"
    );
}

#[test]
fn format_grep_output_complete_and_singular() {
    let retained = retain_grep_matches(&[m("a.rs", 1, "hi")], 250, 2000);
    let text = format_grep_output(&retained, None);
    assert_eq!(text, "Found 1 match\n\na.rs\nLine 1: hi");
}

#[test]
fn format_grep_output_truncated_with_spill() {
    let all: Vec<GrepMatch> =
        (1..=5).map(|i| m("a.rs", i, "x")).collect();
    let retained = retain_grep_matches(&all, 2, 2000);
    let text = format_grep_output(
        &retained,
        Some("Full grep result stored at: C:\\spill\\grep-results.txt. Use read"),
    );
    assert!(text.starts_with("Found 2 of 5 matches\n\n"));
    assert!(text.ends_with(
        "(Full grep result stored at: C:\\spill\\grep-results.txt. Use read)"
    ));
}

#[test]
fn format_grep_output_truncated_without_spill() {
    let all: Vec<GrepMatch> = (1..=5).map(|i| m("a.rs", i, "x")).collect();
    let retained = retain_grep_matches(&all, 2, 2000);
    let text = format_grep_output(&retained, None);
    assert!(text.ends_with(
        "(The complete result could not be saved; narrow pattern, path, or include to see more.)"
    ));
}

// ---------------------------------------------------------------------------
// 进程内引擎 grep_search
// ---------------------------------------------------------------------------

#[test]
fn grep_search_finds_matches_with_line_numbers() {
    let root = temp_dir("basic");
    std::fs::write(root.join("a.txt"), "hello world\nnothing\nhello again\n").unwrap();
    std::fs::write(root.join("b.txt"), "just one\n").unwrap();
    let mut matches = grep_search(&root, "hello", None).expect("search");
    matches.sort_by(|a, b| (a.path.clone(), a.line_number).cmp(&(b.path.clone(), b.line_number)));
    assert_eq!(
        matches,
        vec![
            m("a.txt", 1, "hello world"),
            m("a.txt", 3, "hello again"),
        ]
    );
}

#[test]
fn grep_search_respects_include_glob() {
    let root = temp_dir("inc");
    std::fs::write(root.join("a.rs"), "token\n").unwrap();
    std::fs::write(root.join("b.txt"), "token\n").unwrap();
    let matches = grep_search(&root, "token", Some("*.rs")).expect("search");
    assert_eq!(matches, vec![m("a.rs", 1, "token")]);
}

#[test]
fn grep_search_respects_gitignore_and_hides_by_default() {
    let root = temp_dir("ign");
    std::fs::create_dir_all(root.join("ignored")).unwrap();
    std::fs::write(root.join("ignored/x.txt"), "token\n").unwrap();
    std::fs::write(root.join("visible.txt"), "token\n").unwrap();
    std::fs::write(root.join(".hidden.txt"), "token\n").unwrap();
    std::fs::write(root.join(".gitignore"), "ignored/\n").unwrap();
    // 使其成为 git 仓库（require_git 默认 true：非仓库内 .gitignore 不生效，rg 同语义）。
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::write(root.join(".git/config"), "[core]\n").unwrap();
    let matches = grep_search(&root, "token", None).expect("search");
    let mut paths: Vec<String> = matches.iter().map(|m| m.path.clone()).collect();
    paths.sort();
    assert_eq!(paths, vec!["visible.txt"], "隐藏与 gitignore 排除项不出现在结果");
}

#[test]
fn grep_search_supports_regex_syntax() {
    let root = temp_dir("re");
    std::fs::write(root.join("a.txt"), "error: boom\nwarning: meh\n").unwrap();
    let matches = grep_search(&root, r"^error:", None).expect("search");
    assert_eq!(matches, vec![m("a.txt", 1, "error: boom")]);
}

#[test]
fn grep_search_invalid_pattern_is_search_error() {
    let root = temp_dir("bad");
    std::fs::write(root.join("a.txt"), "x\n").unwrap();
    let err = grep_search(&root, "([unclosed", None).unwrap_err();
    assert_eq!(err.code, GrepErrorCode::InvalidPattern);
    assert!(err.message.contains("regex parse error"));
}

#[test]
fn grep_search_in_with_path_subdirectory() {
    let root = temp_dir("patharg");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("top.txt"), "token\n").unwrap();
    std::fs::write(root.join("sub/inner.txt"), "token\n").unwrap();
    let input = parse_grep_args("token", Some("sub"), None).expect("ok");
    let matches = grep_search_in(&root, &input).expect("search");
    assert_eq!(matches, vec![m("inner.txt", 1, "token")], "相对 path 以 root 为基准");
}

#[test]
fn grep_search_no_match_is_empty() {
    let root = temp_dir("none");
    std::fs::write(root.join("a.txt"), "nothing here\n").unwrap();
    let matches = grep_search(&root, "zzz", None).expect("search");
    assert!(matches.is_empty());
}

#[test]
fn retained_matches_wire_format() {
    // GrepMatch 序列化对齐参考 schema：path / lineNumber / line
    let json = serde_json::to_value(m("a.rs", 2, "x")).unwrap();
    assert!(json.get("lineNumber").is_some());
    assert!(json.get("line_number").is_none());
    assert_eq!(json["lineNumber"], serde_json::json!(2));
    assert_eq!(json["path"], serde_json::json!("a.rs"));
}

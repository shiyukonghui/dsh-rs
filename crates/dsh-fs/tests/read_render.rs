//! dsh-fs：tool `read` 的纯渲染面（M5-DESIGN §4.5 工具面第一块）。
//!
//! 逐字参考 `tool-fs/src/read.ts` + `read-render.ts`：
//! * parse_read_args：offset 默认 1，limit 默认 maxLimit，正整数校验，limit ≤ maxLimit。
//! * build_window：带行号窗口 + 每行截断标记 + 字节上限（UTF-8 字节 + 行间 1B）+ 越界报
//!   FS_NOT_FOUND。
//! * format_read_output：OpenCode 信封（`<path>/<type>/<content>`）+ 三态 footer。
//! * lang_from_path：扩展名 → 语言提示（小写、dotfile 无扩展、未知扩扩展无提示）。

use dsh_fs::read_render::{
    build_window, format_read_output, lang_from_path, parse_read_args, FileReadOutcome,
    FileTextLine, ReadWindow, READ_MAX_BYTES, READ_MAX_LINE_LENGTH,
};
use dsh_fs::FsErrorCode;

// ---------------------------------------------------------------------------
// parse_read_args
// ---------------------------------------------------------------------------

#[test]
fn parse_args_defaults_offset_one_limit_to_max() {
    let input = parse_read_args("a.txt", None, None, 2000).expect("defaults");
    assert_eq!(input.file_path, "a.txt");
    assert_eq!(input.offset, 1);
    assert_eq!(input.limit, 2000);
}

#[test]
fn parse_args_accepts_provided_positive() {
    let input = parse_read_args("a.txt", Some(5), Some(10), 2000).expect("provided");
    assert_eq!((input.offset, input.limit), (5, 10));
}

#[test]
fn parse_args_rejects_limit_over_cap() {
    let err = parse_read_args("a.txt", None, Some(2001), 2000).unwrap_err();
    assert_eq!(err, "limit must be less than or equal to 2000");
}

#[test]
fn parse_args_rejects_non_positive() {
    let err = parse_read_args("a.txt", Some(0), None, 2000).unwrap_err();
    assert_eq!(err, "offset must be a positive integer");
    let err = parse_read_args("a.txt", None, Some(-3), 2000).unwrap_err();
    assert_eq!(err, "limit must be a positive integer");
}

#[test]
fn parse_args_rejects_empty_path() {
    let err = parse_read_args("   ", None, None, 2000).unwrap_err();
    assert_eq!(err, "file_path must be a non-empty string");
}

// ---------------------------------------------------------------------------
// build_window
// ---------------------------------------------------------------------------

fn window(offset: usize, limit: usize, max_line_length: usize, max_bytes: usize) -> ReadWindow {
    ReadWindow { offset, limit, max_line_length, max_bytes }
}

#[test]
fn build_window_numbers_lines_1based() {
    let r = build_window("a\nbb\nccc", &window(1, 10, READ_MAX_LINE_LENGTH, READ_MAX_BYTES), "f.txt")
        .expect("ok");
    assert_eq!(r.lines.len(), 3);
    assert_eq!(r.lines[0], FileTextLine { number: 1, text: "a".into() });
    assert_eq!(r.lines[2], FileTextLine { number: 3, text: "ccc".into() });
    assert_eq!(r.total_lines, 3);
    assert!(!r.truncated_by_bytes);
}

#[test]
fn build_window_offset_window_and_skip() {
    let r = build_window("a\nb\nc\nd\ne", &window(3, 2, READ_MAX_LINE_LENGTH, READ_MAX_BYTES), "f")
        .expect("ok");
    assert_eq!(r.total_lines, 5);
    assert_eq!(
        r.lines,
        vec![FileTextLine { number: 3, text: "c".into() }, FileTextLine { number: 4, text: "d".into() }]
    );
}

#[test]
fn build_window_strips_carriage_return() {
    let r = build_window("a\r\nb\r\n", &window(1, 10, READ_MAX_LINE_LENGTH, READ_MAX_BYTES), "f")
        .expect("ok");
    assert_eq!(r.lines[0].text, "a");
    assert_eq!(r.lines[1].text, "b");
    // 尾部换行不产生幽灵空行
    assert_eq!(r.total_lines, 2);
}

#[test]
fn build_window_truncates_overlong_lines() {
    let r =
        build_window(&"x".repeat(50), &window(1, 10, 20, READ_MAX_BYTES), "f").expect("ok");
    assert_eq!(r.lines[0].text, format!("{}... (line truncated to 20 chars)", "x".repeat(20)));
}

#[test]
fn build_window_byte_cap_sets_truncated() {
    // 每行 <maxLineLength 不截断，但字节总量碰顶 → truncated_by_bytes
    let r = build_window("123456\n123456", &window(1, 10, READ_MAX_LINE_LENGTH, 12), "f")
        .expect("ok");
    // 行1=6B 入列；行2=6B + 行间换行1B = 7 → 6+7=13 > 12 → 截断
    assert_eq!(r.lines.len(), 1, "行2 溢出字节上限被截断");
    assert!(r.truncated_by_bytes);
    assert_eq!(r.total_lines, 2, "仍扫描精确总行数");
}

#[test]
fn build_window_offset_beyond_eof_is_found() {
    let err =
        build_window("a\nb", &window(5, 1, READ_MAX_LINE_LENGTH, READ_MAX_BYTES), "f.txt").unwrap_err();
    assert_eq!(err.code(), FsErrorCode::FsNotFound);
    assert_eq!(err.message, "offset 5 is out of range for \"f.txt\" (2 lines)");
}

#[test]
fn build_window_empty_file_offset_one_ok() {
    let r = build_window("", &window(1, 1, READ_MAX_LINE_LENGTH, READ_MAX_BYTES), "f").expect("ok");
    assert_eq!(r.lines.len(), 0);
    assert_eq!(r.total_lines, 0);
}

// ---------------------------------------------------------------------------
// format_read_output
// ---------------------------------------------------------------------------

fn read_outcome(offset: usize, lines: Vec<(usize, &str)>, total: usize, capped: bool) -> FileReadOutcome {
    FileReadOutcome {
        offset,
        lines: lines.into_iter().map(|(n, t)| FileTextLine { number: n, text: t.into() }).collect(),
        total_lines: total,
        truncated_by_bytes: capped,
    }
}

#[test]
fn format_end_of_file_footer() {
    let out = read_outcome(1, vec![(1, "a"), (2, "b")], 2, false);
    let s = format_read_output("foo.txt", &out);
    assert!(s.contains("1: a\n2: b"), "numbered body");
    assert!(s.contains("(End of file - total 2 lines)"));
    assert!(s.contains("<path>foo.txt</path>"));
    assert!(s.contains("<type>file</type>"));
    assert!(s.contains("<content>"));
}

#[test]
fn format_continuation_footer() {
    let out = read_outcome(1, vec![(1, "a"), (2, "b")], 10, false);
    let s = format_read_output("foo.txt", &out);
    assert!(s.contains("(Showing lines 1-2 of 10. Use offset=3 to continue.)"));
}

#[test]
fn format_byte_cap_footer() {
    let out = read_outcome(1, vec![(1, "a"), (2, "b")], 10, true);
    let s = format_read_output("foo.txt", &out);
    assert!(s.contains("(Output capped. Showing lines 1-2. Use offset=3 to continue.)"));
}

#[test]
fn format_empty_lines_footer_only() {
    let out = read_outcome(1, vec![], 0, false);
    let s = format_read_output("foo.txt", &out);
    assert!(!s.contains("1:"));
    assert!(s.contains("(End of file - total 0 lines)"));
}

// ---------------------------------------------------------------------------
// lang_from_path
// ---------------------------------------------------------------------------

#[test]
fn lang_from_path_known_extensions() {
    assert_eq!(lang_from_path("src/main.rs"), Some("rs"));
    assert_eq!(lang_from_path("x.PY"), Some("py")); // 大小写不敏感
    assert_eq!(lang_from_path("a/b/c.json"), Some("json"));
    assert_eq!(lang_from_path("Dockerfile.sh"), Some("sh"));
}

#[test]
fn lang_from_path_unknown_or_missing() {
    assert_eq!(lang_from_path("notes.txt"), None);
    assert_eq!(lang_from_path("README (no dot)"), None);
    assert_eq!(lang_from_path("noext"), None);
    assert_eq!(lang_from_path("directory/"), None);
    assert_eq!(lang_from_path(".gitignore"), None); // dotfile 无扩展
}

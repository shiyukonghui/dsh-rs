//! dsh-fs：str_replace_editor 纯面（M5-DESIGN §4.5，参考 tool-str-replace-editor/src/index.ts）。

use dsh_fs::sr_editor::{
    apply_insert, apply_str_replace, format_file_view, line_numbers_at, match_offsets,
    maybe_truncate, validate_view_range, TRUNCATED_MESSAGE,
};
use dsh_fs::{FsError, FsErrorCode};

// ---------------------------------------------------------------------------
// maybe_truncate
// ---------------------------------------------------------------------------

#[test]
fn truncate_short_unchanged() {
    let text = "hello";
    assert_eq!(maybe_truncate(text, 100), text);
}

#[test]
fn truncate_over_appends_clipped_and_preserves_chars() {
    let text = "abcdef";
    let out = maybe_truncate(text, 3);
    assert!(out.starts_with("abc"));
    assert!(out.ends_with(TRUNCATED_MESSAGE));
    // 中文 char 边界
    let cn = "中文中文";
    let out = maybe_truncate(cn, 3);
    assert!(out.starts_with("中文中"));
}

// ---------------------------------------------------------------------------
// match_offsets / line_numbers_at
// ---------------------------------------------------------------------------

#[test]
fn offsets_find_all_non_overlapping() {
    assert_eq!(match_offsets("aaaa", "aa"), vec![0, 2]);
    assert_eq!(match_offsets("xabcx", "x"), vec![0, 4]);
    assert_eq!(match_offsets("nope", "z"), Vec::<usize>::new());
}

#[test]
fn line_numbers_track_newlines() {
    let content = "one\ntwo\nthree";
    let offsets = match_offsets(content, "e");
    // "one" 的 e(2) → L1；"three" 两个 e → L3、L3
    let lines = line_numbers_at(content, &offsets);
    assert_eq!(lines, vec![1, 3, 3]);
}

// ---------------------------------------------------------------------------
// apply_str_replace
// ---------------------------------------------------------------------------

fn fs_err(err: &FsError) -> String {
    format!("{:?}/{}", err.code, err.message)
}

#[test]
fn str_replace_single_match() {
    let replaced = apply_str_replace("before [x] after", "[x]", "(y)", "a.txt").expect("ok");
    assert_eq!(replaced, "before (y) after");
}

#[test]
fn str_replace_zero_match_is_edit_not_found() {
    let err = apply_str_replace("hello", "zzz", "y", "a.txt").unwrap_err();
    assert_eq!(err.code, FsErrorCode::FsEditNotFound);
    assert_eq!(
        fs_err(&err),
        "FsEditNotFound/No replacement was performed, old_str `zzz` did not appear verbatim in a.txt."
    );
}

#[test]
fn str_replace_multiple_is_ambiguous_with_lines() {
    let err = apply_str_replace("a\nb\na", "a", "x", "a.txt").unwrap_err();
    assert_eq!(err.code, FsErrorCode::FsAmbiguousEdit);
    assert_eq!(
        fs_err(&err),
        "FsAmbiguousEdit/No replacement was performed. Multiple occurrences of old_str `a` in lines [1, 3]. Please ensure it is unique"
    );
}

// ---------------------------------------------------------------------------
// apply_insert
// ---------------------------------------------------------------------------

#[test]
fn insert_at_line_zero_and_mid() {
    assert_eq!(apply_insert("a\nb", 0, "x").expect("ok"), "x\na\nb");
    assert_eq!(apply_insert("a\nb", 1, "x\ny").expect("ok"), "a\nx\ny\nb");
    // 文件末尾（行数 = split 长度）
    assert_eq!(apply_insert("a\nb", 2, "z").expect("ok"), "a\nb\nz");
}

#[test]
fn insert_out_of_range() {
    let err = apply_insert("a\nb", 3, "z").unwrap_err();
    assert_eq!(err, "Invalid `insert_line` parameter: 3. It should be within the range of lines of the file: [0, 2]");
}

// ---------------------------------------------------------------------------
// validate_view_range
// ---------------------------------------------------------------------------

#[test]
fn view_range_validation_messages() {
    assert_eq!(
        validate_view_range((0, 5), 10).unwrap_err(),
        "Invalid `view_range`: [0, 5]. Its first element `0` should be within the range of lines of the file: [1, 10]"
    );
    assert_eq!(
        validate_view_range((1, 11), 10).unwrap_err(),
        "Invalid `view_range`: [1, 11]. Its second element `11` should be smaller than the number of lines in the file: `10`"
    );
    assert_eq!(
        validate_view_range((5, 3), 10).unwrap_err(),
        "Invalid `view_range`: [5, 3]. Its second element `3` should be larger or equal than its first `5`"
    );
    assert_eq!(validate_view_range((2, -1), 4), Ok((2, -1)));
}

// ---------------------------------------------------------------------------
// format_file_view
// ---------------------------------------------------------------------------

#[test]
fn file_view_numbered_full() {
    let content = "one\ntwo\nthree";
    let view = format_file_view("a.txt", content, 16_000, None).expect("ok");
    assert_eq!(
        view,
        "Here's the content of a.txt with line numbers (which has a total of 3 lines):\n     1  one\n     2  two\n     3  three\n"
    );
}

#[test]
fn file_view_range_slice() {
    let content = "l1\nl2\nl3\nl4";
    let view = format_file_view("a.txt", content, 16_000, Some((2, 3))).expect("ok");
    assert_eq!(
        view,
        "Here's the content of a.txt with line numbers (which has a total of 4 lines) with view_range=[2, 3]:\n     2  l2\n     3  l3\n"
    );
}

#[test]
fn file_view_range_to_end() {
    let content = "l1\nl2\nl3";
    let view = format_file_view("a.txt", content, 16_000, Some((2, -1))).expect("ok");
    assert!(view.contains("     2  l2\n     3  l3\n"));
    assert!(view.contains("with view_range=[2, -1]"));
}

#[test]
fn file_view_invalid_range() {
    let err = format_file_view("a.txt", "l1\nl2", 16_000, Some((3, 3))).unwrap_err();
    assert!(err.contains("Its first element `3`"));
}

#[test]
fn file_view_truncated_over_budget() {
    let content = "x\ny\nz";
    let view = format_file_view("a.txt", content, 10, None).expect("ok");
    // 10 字符预算从 prompt 头截断，裁剪后缀附上
    assert!(view.ends_with(TRUNCATED_MESSAGE));
    assert!(view.starts_with("Here's the"));
}

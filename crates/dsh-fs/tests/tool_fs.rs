//! dsh-fs：tool-fs 纯映射面（M5-DESIGN §4.5，参考 tool-fs/src/*.ts）。

use dsh_fs::{FsError, FsErrorCode};
use dsh_fs::tool_fs::{
    format_edit_output, format_image_read_output, format_write_output, image_media_type_for_path,
    parse_edit_args, parse_write_args, remediate_fs_error, ImageRead,
};

// ---------------------------------------------------------------------------
// write
// ---------------------------------------------------------------------------

#[test]
fn parse_write_args_accepts_empty_content() {
    let input = parse_write_args("a.txt", "").expect("ok");
    assert_eq!(input.file_path, "a.txt");
    assert_eq!(input.content, "");
}

#[test]
fn parse_write_args_rejects_blank_file_path() {
    assert_eq!(
        parse_write_args("   ", "x").unwrap_err(),
        "file_path must be a non-empty string"
    );
}

#[test]
fn format_write_output_created_and_updated() {
    assert_eq!(
        format_write_output("src/a.txt", "create"),
        "<path>src/a.txt</path>\n<type>file</type>\n<content>\nCreated file\n</content>"
    );
    assert_eq!(
        format_write_output("src/a.txt", "update"),
        "<path>src/a.txt</path>\n<type>file</type>\n<content>\nUpdated file\n</content>"
    );
}

// ---------------------------------------------------------------------------
// edit
// ---------------------------------------------------------------------------

#[test]
fn parse_edit_args_defaults_replace_all_false() {
    let input = parse_edit_args("a.txt", "x", "y", None).expect("ok");
    assert!(!input.replace_all);
}

#[test]
fn parse_edit_args_validates() {
    assert_eq!(
        parse_edit_args("  ", "x", "y", None).unwrap_err(),
        "file_path must be a non-empty string"
    );
    assert_eq!(
        parse_edit_args("a.txt", "", "y", None).unwrap_err(),
        "old_string must be a non-empty string"
    );
    assert_eq!(
        parse_edit_args("a.txt", "same", "same", None).unwrap_err(),
        "old_string and new_string must differ"
    );
    let input = parse_edit_args("a.txt", "x", "y", Some(true)).expect("ok");
    assert!(input.replace_all);
}

#[test]
fn format_edit_output_single_vs_all() {
    assert_eq!(
        format_edit_output("a.txt", false),
        "The file a.txt has been updated successfully."
    );
    assert_eq!(
        format_edit_output("a.txt", true),
        "The file a.txt has been updated. All occurrences were successfully replaced."
    );
}

// ---------------------------------------------------------------------------
// remediate_fs_error
// ---------------------------------------------------------------------------

#[test]
fn remediate_stale_version_appends_remedy() {
    let error = FsError::new("version mismatch", FsErrorCode::FsStaleVersion);
    let remedied = remediate_fs_error(&error);
    assert_eq!(remedied.message, "version mismatch — re-read the file, then retry");
    assert_eq!(remedied.code, FsErrorCode::FsStaleVersion);
}

#[test]
fn remediate_not_observed_appends_remedy() {
    let error = FsError::new("target not observed", FsErrorCode::FsNotObserved);
    let remedied = remediate_fs_error(&error);
    assert_eq!(remedied.message, "target not observed — read the file, then retry");
    assert_eq!(remedied.code, FsErrorCode::FsNotObserved);
}

#[test]
fn remediate_passthrough_for_others() {
    let error = FsError::new("nope", FsErrorCode::FsNotFound);
    let remedied = remediate_fs_error(&error);
    assert_eq!(remedied.message, "nope");
    assert_eq!(remedied.code, FsErrorCode::FsNotFound);
}

// ---------------------------------------------------------------------------
// read_image
// ---------------------------------------------------------------------------

#[test]
fn image_media_type_by_extension() {
    assert_eq!(image_media_type_for_path("a.png"), Some("image/png"));
    assert_eq!(image_media_type_for_path("a.jpg"), Some("image/jpeg"));
    assert_eq!(image_media_type_for_path("a.jpeg"), Some("image/jpeg"));
    assert_eq!(image_media_type_for_path("a.webp"), Some("image/webp"));
    assert_eq!(image_media_type_for_path("a.gif"), Some("image/gif"));
    // 大小写不敏感
    assert_eq!(image_media_type_for_path("a.PNG"), Some("image/png"));
    // 未知 / 无后缀
    assert_eq!(image_media_type_for_path("a.txt"), None);
    assert_eq!(image_media_type_for_path("noext"), None);
}

#[test]
fn format_image_read_output_plain_envelope() {
    let image = ImageRead {
        media_type: "image/png".into(),
        bytes: 1024,
        width: 100,
        height: 50,
        original_dimensions: None,
    };
    assert_eq!(
        format_image_read_output("img/a.png", &image),
        "<path>img/a.png</path>\n<type>image</type>\n<content>\nimage/png image, 100x50 px, 1024 bytes\n</content>"
    );
}

#[test]
fn format_image_read_output_downscaled_advice() {
    // 原始 2000x1000 → 500x250：x=y=4.00 → 单倍率建议
    let image = ImageRead {
        media_type: "image/jpeg".into(),
        bytes: 2048,
        width: 500,
        height: 250,
        original_dimensions: Some((2000, 1000)),
    };
    let text = format_image_read_output("big.jpg", &image);
    assert!(text.contains(
        " (downscaled from 2000x1000 px; multiply coordinates by 4.00 to locate features in the original file)"
    ));
}

#[test]
fn format_image_read_output_downscaled_xy_advice() {
    // 非等比：2000x600 → 500x300 → x=4.00, y=2.00
    let image = ImageRead {
        media_type: "image/webp".into(),
        bytes: 99,
        width: 500,
        height: 300,
        original_dimensions: Some((2000, 600)),
    };
    let text = format_image_read_output("odd.webp", &image);
    assert!(text.contains("multiply x coordinates by 4.00 and y coordinates by 2.00"));
}

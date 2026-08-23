//! tool-fs 纯映射面（M5-DESIGN §4.5）。
//!
//! 参考 `tool-fs/src/{write,edit,read-image,error}.ts`：参数校验、模型面信封
//! （write 的 `<path>/<type>/<content>`、edit 的确认句、read_image 的 image 信封）、
//! guarded-mutation 失败的错误补救。此层只含纯函数；tool 注册与宿主接线在 step7
//! web.rs（复用 dsh-tools registry）。

use crate::types::{FsError, FsErrorCode};

// ---------------------------------------------------------------------------
// write
// ---------------------------------------------------------------------------

/// 校验后的 write 参数（参考 `parseWriteArgs`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteInput {
    pub file_path: String,
    pub content: String,
}

/// 参考 `parseWriteArgs`：仅 file_path 非空白；content 可为空（写空文件）。
pub fn parse_write_args(file_path: &str, content: &str) -> Result<WriteInput, String> {
    if file_path.trim().is_empty() {
        return Err("file_path must be a non-empty string".into());
    }
    Ok(WriteInput { file_path: file_path.to_string(), content: content.to_string() })
}

/// 参考 `formatWriteOutput`：确认信封；`operation` 选 Created/Updated 措辞，不回显内容。
/// `operation` 为 `FsWriteOutcome::operation`（'create' | 'update'）。
pub fn format_write_output(display_path: &str, operation: &str) -> String {
    let verb = if operation == "create" { "Created" } else { "Updated" };
    format!(
        "<path>{display_path}</path>\n<type>file</type>\n<content>\n{verb} file\n</content>"
    )
}

// ---------------------------------------------------------------------------
// edit
// ---------------------------------------------------------------------------

/// 校验后的 edit 参数（参考 `EditInput`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditInput {
    pub file_path: String,
    pub old_string: String,
    pub new_string: String,
    pub replace_all: bool,
}

/// 参考 `parseEditArgs`：file_path 非空白、old_string 非空、old_string ≠ new_string；
/// replace_all 缺省 false。
pub fn parse_edit_args(
    file_path: &str,
    old_string: &str,
    new_string: &str,
    replace_all: Option<bool>,
) -> Result<EditInput, String> {
    if file_path.trim().is_empty() {
        return Err("file_path must be a non-empty string".into());
    }
    if old_string.is_empty() {
        return Err("old_string must be a non-empty string".into());
    }
    if old_string == new_string {
        return Err("old_string and new_string must differ".into());
    }
    Ok(EditInput {
        file_path: file_path.to_string(),
        old_string: old_string.to_string(),
        new_string: new_string.to_string(),
        replace_all: replace_all.unwrap_or(false),
    })
}

/// 参考 `formatEditOutput`：单匹配 vs 全部替换的确认句。
pub fn format_edit_output(display_path: &str, replace_all: bool) -> String {
    if replace_all {
        format!("The file {display_path} has been updated. All occurrences were successfully replaced.")
    } else {
        format!("The file {display_path} has been updated successfully.")
    }
}

// ---------------------------------------------------------------------------
// guarded-mutation 错误补救
// ---------------------------------------------------------------------------

/// 参考 `remediateFsError`：FS_STALE_VERSION → 「re-read the file, then retry」；
/// FS_NOT_OBSERVED → 「read the file, then retry」；其余原样返回。code 保留。
pub fn remediate_fs_error(error: &FsError) -> FsError {
    let remedy = match error.code {
        FsErrorCode::FsStaleVersion => "re-read the file, then retry",
        FsErrorCode::FsNotObserved => "read the file, then retry",
        _ => return error.clone(),
    };
    FsError::new(format!("{} — {remedy}", error.message), error.code)
}

// ---------------------------------------------------------------------------
// read_image
// ---------------------------------------------------------------------------

/// read_image 接受的后缀 → 媒体类型（参考 `IMAGE_EXTENSIONS`）。
pub fn image_media_type_for_path(file_path: &str) -> Option<&'static str> {
    let ext = std::path::Path::new(file_path)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())?;
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        _ => None,
    }
}

/// image 元数据（参考 `ImageReadValue['image']`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRead {
    pub media_type: String,
    pub bytes: u64,
    pub width: u64,
    pub height: u64,
    /// 归一化前（朝向处理后、存储降采样前）的原始尺寸；仅在存储缩小过时为 Some。
    pub original_dimensions: Option<(u64, u64)>,
}

/// 参考 `formatImageReadOutput`：image 信封；降采样时附原始尺寸 + 坐标倍率建议。
pub fn format_image_read_output(display_path: &str, image: &ImageRead) -> String {
    let mut scaled = String::new();
    if let Some((orig_w, orig_h)) = image.original_dimensions {
        let x = ratio(orig_w, image.width);
        let y = ratio(orig_h, image.height);
        let advice = if x == y {
            format!("multiply coordinates by {x}")
        } else {
            format!("multiply x coordinates by {x} and y coordinates by {y}")
        };
        scaled = format!(
            " (downscaled from {orig_w}x{orig_h} px; {advice} to locate features in the original file)"
        );
    }
    format!(
        "<path>{display_path}</path>\n<type>image</type>\n<content>\n{} image, {}x{} px, {} bytes{scaled}\n</content>",
        image.media_type, image.width, image.height, image.bytes
    )
}

/// 两位小数的倍率（参考 `toFixed(2)`）。
fn ratio(big: u64, small: u64) -> String {
    if small == 0 {
        return "0.00".to_string();
    }
    // 整数除法取两位小数的近似（参考 JS toFixed(2) 的四舍五入）。
    let scaled = big as f64 / small as f64;
    format!("{scaled:.2}")
}

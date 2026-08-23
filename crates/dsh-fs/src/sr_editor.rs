//! str_replace_editor 纯面（M5-DESIGN §4.5）。
//!
//! 参考 `tool-str-replace-editor/src/index.ts`：maxOutputChars=16_000 裁剪 +
//! `<response clipped>` 后缀、view 编号行渲染（含 view_range 校验）、str_replace
//! 唯一性守卫（零匹配 FS_EDIT_NOT_FOUND / 多匹配 FS_AMBIGUOUS_EDIT 带行号）、
//! insert 行插入。此层只含纯函数；命令编排（view/create/str_replace/insert 走
//! ctx.fs）属宿主接线（step7）。

use crate::types::{FsError, FsErrorCode};

/// 参考 `TRUNCATED_MESSAGE`：长输出裁剪后缀。
pub const TRUNCATED_MESSAGE: &str = "<response clipped><NOTE>To save on context only part of this file has been shown to you. You should retry this tool after you have searched inside the file with `grep -n` in order to find the line numbers of what you are looking for.</NOTE>";

/// 参考配置默认 `maxOutputChars`。
pub const DEFAULT_MAX_OUTPUT_CHARS: usize = 16_000;

/// 参考 `maybeTruncate`：超 max_chars 截断并附 `<response clipped>` 后缀。
/// 按 Unicode 标量（char）计数并在字符边界截断（参考按 UTF-16 码元；BMP 平面一致，
/// 星面字符有 +/-1 的轻微分叉，见 DECISIONS 相关条目）。
pub fn maybe_truncate(content: &str, max_chars: usize) -> String {
    let count = content.chars().count();
    if count <= max_chars {
        return content.to_string();
    }
    let head: String = content.chars().take(max_chars).collect();
    format!("{head}{TRUNCATED_MESSAGE}")
}

/// 参考 `matchOffsets`：所有匹配的起始偏移（字节坐标）。
pub fn match_offsets(content: &str, search: &str) -> Vec<usize> {
    let mut offsets = Vec::new();
    let mut offset = 0_usize;
    while let Some(rel) = content[offset..].find(search) {
        let abs = offset + rel;
        offsets.push(abs);
        offset = abs + search.len();
    }
    offsets
}

/// 参考 `lineNumbersAt`：每个偏移对应的 1 起始行号。
pub fn line_numbers_at(content: &str, offsets: &[usize]) -> Vec<u64> {
    let mut line: u64 = 1;
    let mut cursor = 0_usize;
    let bytes = content.as_bytes();
    offsets
        .iter()
        .map(|&offset| {
            while cursor < offset {
                if bytes[cursor] == b'\n' {
                    line += 1;
                }
                cursor += 1;
            }
            line
        })
        .collect()
}

/// 参考 `replaceInFile` 核心：字面唯一替换。零匹配 → FS_EDIT_NOT_FOUND；多匹配 →
/// FS_AMBIGUOUS_EDIT（带命中行号列表）；恰一 → 替换后的完整文本。
pub fn apply_str_replace(
    content: &str,
    old_str: &str,
    new_str: &str,
    display_path: &str,
) -> Result<String, FsError> {
    let offsets = match_offsets(content, old_str);
    let offset = offsets.first().copied();
    let offset = match offset {
        None => {
            return Err(FsError::new(
                format!(
                    "No replacement was performed, old_str `{old_str}` did not appear verbatim in {display_path}."
                ),
                FsErrorCode::FsEditNotFound,
            ))
        }
        Some(o) => o,
    };
    if offsets.len() > 1 {
        let lines = line_numbers_at(content, &offsets);
        let joined = lines
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(FsError::new(
            format!(
                "No replacement was performed. Multiple occurrences of old_str `{old_str}` in lines [{joined}]. Please ensure it is unique"
            ),
            FsErrorCode::FsAmbiguousEdit,
        ));
    }
    let replaced = format!(
        "{}{}{}",
        &content[..offset],
        new_str,
        &content[offset + old_str.len()..]
    );
    Ok(replaced)
}

/// 参考 `insertInFile` 核心：在第 insert_line 行之后插入（0 = 文件头）。
/// 范围校验失败返回普通参数错误文本。
pub fn apply_insert(content: &str, insert_line: usize, new_str: &str) -> Result<String, String> {
    let lines: Vec<&str> = content.split('\n').collect();
    if insert_line > lines.len() {
        return Err(format!(
            "Invalid `insert_line` parameter: {insert_line}. It should be within the range of lines of the file: [0, {}]",
            lines.len()
        ));
    }
    let mut out: Vec<&str> = Vec::with_capacity(lines.len() + new_str.matches('\n').count() + 1);
    out.extend_from_slice(&lines[..insert_line]);
    out.extend(new_str.split('\n'));
    out.extend_from_slice(&lines[insert_line..]);
    Ok(out.join("\n"))
}

/// view_range 校验结果：父层已确认恰两个整数；此处校验范围。
/// 返回 `(initial_line, final_line)`，final_line = -1 表示到文件末尾。
pub fn validate_view_range(
    view_range: (i64, i64),
    line_count: usize,
) -> Result<(i64, i64), String> {
    let (initial, final_line) = view_range;
    if initial < 1 || initial > line_count as i64 {
        return Err(format!(
            "Invalid `view_range`: [{initial}, {final_line}]. Its first element `{initial}` should be within the range of lines of the file: [1, {line_count}]"
        ));
    }
    if final_line > line_count as i64 {
        return Err(format!(
            "Invalid `view_range`: [{initial}, {final_line}]. Its second element `{final_line}` should be smaller than the number of lines in the file: `{line_count}`"
        ));
    }
    if final_line != -1 && final_line < initial {
        return Err(format!(
            "Invalid `view_range`: [{initial}, {final_line}]. Its second element `{final_line}` should be larger or equal than its first `{initial}`"
        ));
    }
    Ok((initial, final_line))
}

/// 参考 `formatFileView`：按行号渲染文件视图（`cat -n` 风格），超预算裁剪。
/// `view_range` = `(start_line, end_line)`，end_line = -1 表示到文件末尾。
pub fn format_file_view(
    path: &str,
    content: &str,
    max_output_chars: usize,
    view_range: Option<(i64, i64)>,
) -> Result<String, String> {
    let all_lines: Vec<&str> = content.split('\n').collect();
    let line_count = all_lines.len();
    let mut lines: &[&str] = &all_lines;
    let mut initial_line: i64 = 1;
    let mut prompt =
        format!("Here's the content of {path} with line numbers (which has a total of {line_count} lines)");
    if let Some(range) = view_range {
        let (initial, final_line) = validate_view_range(range, line_count)?;
        initial_line = initial;
        lines = if final_line == -1 {
            &all_lines[(initial_line - 1) as usize..]
        } else {
            &all_lines[(initial_line - 1) as usize..final_line as usize]
        };
        prompt += &format!(" with view_range=[{initial_line}, {final_line}]");
    }
    let numbered = lines
        .iter()
        .enumerate()
        .map(|(index, line)| format!("{:>6}  {}", initial_line + index as i64, line))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(maybe_truncate(&format!("{prompt}:\n{numbered}\n"), max_output_chars))
}

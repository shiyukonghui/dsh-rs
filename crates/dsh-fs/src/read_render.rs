//! dsh-fs tool `read` 纯渲染面（M5-DESIGN §4.5）。
//!
//! 逐字参考 `tool-fs/src/read.ts` + `read-render.ts`：参数解析（offset 默认 1，limit 默认
//! maxLimit，正整数，limit ≤ cap）、窗口构建（1-based 行号/每行截断/字节上限/越界报
//! FS_NOT_FOUND）、OpenCode 信封渲染（三态 footer）、扩展名语言提示。
//!
//! 本模块纯函数：输入 UTF-8 文本，输出窗口/信封，无 IO、无副作用。

use crate::types::{FsError, FsErrorCode};

/// 参考 `READ_LIMIT`：一次 `read` 的默认与最大行数。
pub const READ_LIMIT: usize = 2000;

/// 参考 `README_MAX_LINE_LENGTH`：单行最大字符数。
pub const READ_MAX_LINE_LENGTH: usize = 2000;

/// 参考 `READ_MAX_BYTES`：选中行的最大字节数。
pub const READ_MAX_BYTES: usize = 50 * 1024;

/// 参考 `ReadInput`（read.ts）：已默认化的参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadInput {
    pub file_path: String,
    pub offset: usize,
    pub limit: usize,
}

/// 参考 `parseReadArgs`：校验 tool 参数并施默认。
pub fn parse_read_args(
    file_path: &str,
    offset: Option<i64>,
    limit: Option<i64>,
    max_limit: usize,
) -> Result<ReadInput, String> {
    if file_path.trim().is_empty() {
        return Err("file_path must be a non-empty string".into());
    }
    let offset = match offset {
        None => 1usize,
        Some(v) => validate_positive(v, "offset")?,
    };
    let limit = match limit {
        None => max_limit,
        Some(v) => validate_positive(v, "limit")?,
    };
    if limit > max_limit {
        return Err(format!("limit must be less than or equal to {max_limit}"));
    }
    Ok(ReadInput { file_path: file_path.to_string(), offset, limit })
}

fn validate_positive(value: i64, name: &str) -> Result<usize, String> {
    if value < 1 {
        return Err(format!("{name} must be a positive integer"));
    }
    Ok(value as usize)
}

/// 参考 `ReadWindow`（read-render.ts）。
#[derive(Debug, Clone, Copy)]
pub struct ReadWindow {
    pub offset: usize,
    pub limit: usize,
    pub max_line_length: usize,
    pub max_bytes: usize,
}

/// 参考 `FileTextLine`：一行（number 为 1-based 行号，text 不含尾部换行）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTextLine {
    pub number: usize,
    pub text: String,
}

/// 参考 `WindowResult`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowResult {
    pub lines: Vec<FileTextLine>,
    pub total_lines: usize,
    pub truncated_by_bytes: bool,
}

fn truncate_line(line: &str, max_line_length: usize) -> String {
    if line.chars().count() > max_line_length {
        let truncated: String = line.chars().take(max_line_length).collect();
        format!("{truncated}... (line truncated to {max_line_length} chars)")
    } else {
        line.to_string()
    }
}

fn line_byte_size(line: &str, current_line_count: usize) -> usize {
    line.len() + if current_line_count > 0 { 1 } else { 0 }
}

#[allow(clippy::too_many_arguments)]
fn consume_line(
    acc: &mut Accumulator,
    raw_line: &str,
    offset: usize,
    limit: usize,
    max_line_length: usize,
    max_bytes: usize,
) {
    acc.total_lines += 1;
    if acc.truncated_by_bytes || acc.total_lines < offset || acc.lines.len() >= limit {
        return;
    }
    let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
    let text = truncate_line(line, max_line_length);
    let bytes = line_byte_size(&text, acc.lines.len());
    if acc.output_bytes + bytes > max_bytes {
        acc.truncated_by_bytes = true;
        return;
    }
    acc.output_bytes += bytes;
    acc.lines.push(FileTextLine { number: acc.total_lines, text });
}

struct Accumulator {
    lines: Vec<FileTextLine>,
    total_lines: usize,
    output_bytes: usize,
    truncated_by_bytes: bool,
}

/// 参考 `buildWindow`：把 UTF-8 文本切成有界窗口，同时扫描精确 totalLines；越界抛
/// FS_NOT_FOUND。
pub fn build_window(
    text: &str,
    request: &ReadWindow,
    display_path: &str,
) -> Result<WindowResult, FsError> {
    let mut acc = Accumulator { lines: vec![], total_lines: 0, output_bytes: 0, truncated_by_bytes: false };
    for line in text.split_terminator('\n') {
        consume_line(
            &mut acc,
            line,
            request.offset,
            request.limit,
            request.max_line_length,
            request.max_bytes,
        );
    }
    if !acc.truncated_by_bytes
        && request.offset > acc.total_lines
        && !(acc.total_lines == 0 && request.offset == 1)
    {
        return Err(FsError::new(
            format!(
                "offset {} is out of range for \"{display_path}\" ({} lines)",
                request.offset, acc.total_lines
            ),
            FsErrorCode::FsNotFound,
        ));
    }
    Ok(WindowResult {
        lines: acc.lines,
        total_lines: acc.total_lines,
        truncated_by_bytes: acc.truncated_by_bytes,
    })
}

/// 参考 `FileReadOutcome`：format_read_output 的输入。
#[derive(Debug, Clone)]
pub struct FileReadOutcome {
    pub offset: usize,
    pub lines: Vec<FileTextLine>,
    pub total_lines: usize,
    pub truncated_by_bytes: bool,
}

/// 参考 `formatReadOutput`：渲染 OpenCode 信封（`<path>/<type>/<content>` + footer）。
pub fn format_read_output(display_path: &str, outcome: &FileReadOutcome) -> String {
    let end_line = outcome.lines.last().map(|l| l.number).unwrap_or_else(|| outcome.offset.saturating_sub(1));
    let footer = if outcome.truncated_by_bytes {
        format!(
            "(Output capped. Showing lines {}-{}. Use offset={} to continue.)",
            outcome.offset, end_line, end_line + 1
        )
    } else if end_line < outcome.total_lines {
        format!(
            "(Showing lines {}-{} of {}. Use offset={} to continue.)",
            outcome.offset, end_line, outcome.total_lines, end_line + 1
        )
    } else {
        format!("(End of file - total {} lines)", outcome.total_lines)
    };
    let body = if outcome.lines.is_empty() {
        footer
    } else {
        let numbered = outcome
            .lines
            .iter()
            .map(|l| format!("{}: {}", l.number, l.text))
            .collect::<Vec<_>>()
            .join("\n");
        format!("{numbered}\n\n{footer}")
    };
    format!("<path>{display_path}</path>\n<type>file</type>\n<content>\n{body}\n</content>")
}

/// 参考 `LANG_BY_EXTENSION`：常见源码/配置/标记扩展名 → 语言提示。
const LANG_BY_EXTENSION: &[(&str, &str)] = &[
    ("ts", "ts"), ("tsx", "tsx"), ("mts", "ts"), ("cts", "ts"),
    ("js", "js"), ("jsx", "jsx"), ("mjs", "js"), ("cjs", "js"),
    ("json", "json"), ("jsonc", "json"),
    ("py", "py"), ("rb", "rb"), ("go", "go"), ("rs", "rs"), ("java", "java"),
    ("c", "c"), ("h", "c"), ("cc", "cpp"), ("cpp", "cpp"), ("hpp", "cpp"), ("cxx", "cpp"),
    ("cs", "cs"), ("kt", "kotlin"), ("swift", "swift"), ("php", "php"),
    ("sh", "sh"), ("bash", "sh"), ("zsh", "sh"),
    ("yaml", "yaml"), ("yml", "yaml"), ("toml", "toml"), ("ini", "ini"),
    ("md", "md"), ("markdown", "md"), ("mdx", "mdx"),
    ("html", "html"), ("htm", "html"), ("css", "css"), ("scss", "scss"), ("less", "less"),
    ("sql", "sql"), ("xml", "xml"), ("lua", "lua"),
];

/// 参考 `langFromPath`：从路径扩展派生语言提示；纯且大小写不敏感；dotfile/未知扩展 →
/// None。
pub fn lang_from_path(path: &str) -> Option<&'static str> {
    let base_start = path.rfind(['/', '\\']).map(|i| i + 1).unwrap_or(0);
    let base = &path[base_start..];
    let dot = base.rfind('.')?;
    if dot == 0 {
        return None; // dotfile 无扩展
    }
    let ext = base[dot + 1..].to_ascii_lowercase();
    LANG_BY_EXTENSION
        .iter()
        .find(|(k, _)| *k == ext)
        .map(|(_, v)| *v)
}

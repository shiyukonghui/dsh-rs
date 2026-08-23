//! dsh-fs 进程内 grep 搜索（M5-DESIGN §4.6 / DIV-7）。
//!
//! 参考 `tool-fs-search/src/grep.ts` + `search-core.ts`：`GREP_MAX_MATCHES`、
//! `GREP_MAX_LINE_BYTES`、`parseGrepArgs` 校验（含 include 单正 glob 约束）、
//! `previewLine` 头部截断（保 UTF-8 边界 + ` (line truncated)` 后缀）、
//! `retainGrepMatches`（保头 max 条 + 逐行预览）、`formatGrepMatches` 按文件分组、
//! `formatGrepOutput` 头/体/尾信封、`SEARCH_*` 错误词表。
//!
//! DIV-7：不走 rg 二进制；用 `ignore` 遍历 + `regex` 在进程内逐行匹配。**与 glob
//! 相反**，grep 参考 argv 只有 `--json --regexp --glob` 而无 `--no-ignore --hidden`，
//! 因此遍历保持默认：隐藏文件忽略、`.gitignore`/`.ignore` 生效。

use crate::types::FsError;
use ignore::overrides::{Override, OverrideBuilder};
use ignore::Match;
use regex::bytes::Regex;
use std::borrow::Cow;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// 参考 `GREP_MAX_MATCHES`：一次 grep 调用内联保留的扁平匹配上限。
pub const GREP_MAX_MATCHES: usize = 250;

/// 参考 `GREP_MAX_LINE_BYTES`：一条匹配行预览的字节预算（UTF-8 边界保留）。
pub const GREP_MAX_LINE_BYTES: usize = 2000;

/// 参考 `SearchErrorCode` 词表（DIV-7 进程内实现；RAW_OUTPUT_OVERFLOW/ABORTED 留给
/// 宿主接线层的捕获预算与取消，本纯层暂不产生）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrepErrorCode {
    InvalidPattern,   // "SEARCH_INVALID_PATTERN"
    Failed,           // "SEARCH_FAILED"
    RawOutputOverflow, // "SEARCH_RAW_OUTPUT_OVERFLOW"
    Aborted,          // "SEARCH_ABORTED"
}

impl GrepErrorCode {
    pub fn wire(&self) -> &'static str {
        match self {
            GrepErrorCode::InvalidPattern => "SEARCH_INVALID_PATTERN",
            GrepErrorCode::Failed => "SEARCH_FAILED",
            GrepErrorCode::RawOutputOverflow => "SEARCH_RAW_OUTPUT_OVERFLOW",
            GrepErrorCode::Aborted => "SEARCH_ABORTED",
        }
    }
}

/// 搜索失败（镜像参考 `SearchError`：message + 稳定 code）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrepError {
    pub message: String,
    pub code: GrepErrorCode,
}

impl GrepError {
    pub fn new(message: impl Into<String>, code: GrepErrorCode) -> Self {
        GrepError { message: message.into(), code }
    }
}

impl std::fmt::Display for GrepError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} [{}]", self.message, self.code.wire())
    }
}

impl std::error::Error for GrepError {}

/// 一条匹配：文件路径（相对搜索根，`/` 分隔）、1 起始行号、行文本。`lineNumber`
/// 序列化对齐参考工具输出 schema（camelCase）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GrepMatch {
    pub path: String,
    pub line_number: u64,
    pub line: String,
}

/// 校验后的 grep 参数（参考 `GrepInput`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrepInput {
    pub pattern: String,
    pub path: Option<String>,
    pub include: Option<String>,
}

/// 参考 `validateInclude`：include 必须是**一个正 glob**——非空白、不以 `!` 开头、
/// 顶层无逗号（花括号组内的逗号合法）。
fn validate_include(include: &str) -> Result<(), String> {
    if include.trim().is_empty() {
        return Err("include must be a non-empty glob when given".into());
    }
    if include.starts_with('!') {
        return Err(
            "include must be a positive glob filter; negated patterns (\"!…\") are not supported".into(),
        );
    }
    let mut brace_depth = 0_u32;
    for ch in include.chars() {
        match ch {
            '{' => brace_depth += 1,
            '}' => brace_depth = brace_depth.saturating_sub(1),
            ',' if brace_depth == 0 => {
                return Err(
                    "include must be one glob, not a comma-separated list (use {a,b} alternation instead)"
                        .into(),
                )
            }
            _ => {}
        }
    }
    Ok(())
}

/// 参考 `parseGrepArgs`：pattern 非空（空白是合法正则）、path 非空白、include 单正 glob。
pub fn parse_grep_args(
    pattern: &str,
    path: Option<&str>,
    include: Option<&str>,
) -> Result<GrepInput, String> {
    if pattern.is_empty() {
        return Err("pattern must be a non-empty string".into());
    }
    if let Some(p) = path {
        if p.trim().is_empty() {
            return Err("path must be a non-empty string when given".into());
        }
    }
    if let Some(i) = include {
        validate_include(i)?;
    }
    Ok(GrepInput {
        pattern: pattern.to_string(),
        path: path.map(|s| s.to_string()),
        include: include.map(|s| s.to_string()),
    })
}

/// 参考 `previewLine`：头部截断保 UTF-8 边界；超预算加 ` (line truncated)` 后缀。
/// 返回 (预览, 是否截断)。
pub fn preview_line(line: &str, max_bytes: usize) -> (String, bool) {
    if line.len() <= max_bytes {
        return (line.to_string(), false);
    }
    let mut end = max_bytes;
    while end > 0 && !line.is_char_boundary(end) {
        end -= 1;
    }
    (line[..end].to_string(), true)
}

/// 已渲染的预览（截断时带 ` (line truncated)` 后缀）。
pub fn preview_line_rendered(line: &str, max_bytes: usize) -> String {
    let (text, truncated) = preview_line(line, max_bytes);
    if truncated {
        format!("{text} (line truncated)")
    } else {
        text
    }
}

/// 保头结果（参考 `dsh-output-retention` 的 `RetainedItems` 语义）。
#[derive(Debug, Clone)]
pub struct RetainedMatches {
    pub items: Vec<GrepMatch>, // 已逐行预览，最多 max_matches 条
    pub seen: usize,           // 完整匹配总数
}

impl RetainedMatches {
    pub fn kept(&self) -> usize {
        self.items.len()
    }
    pub fn truncated(&self) -> bool {
        self.seen > self.items.len()
    }
}

/// 参考 `retainGrepMatches`：逐条预览 + 保头 max_matches。
pub fn retain_grep_matches(
    matches: &[GrepMatch],
    max_matches: usize,
    max_line_bytes: usize,
) -> RetainedMatches {
    let seen = matches.len();
    let items: Vec<GrepMatch> = matches
        .iter()
        .take(max_matches)
        .map(|m| GrepMatch {
            path: m.path.clone(),
            line_number: m.line_number,
            line: preview_line_rendered(&m.line, max_line_bytes),
        })
        .collect();
    RetainedMatches { items, seen }
}

/// 参考 `formatGrepMatches`：按文件分组（首见序），`path` + 每行 `Line N: <text>`。
pub fn format_grep_matches(matches: &[GrepMatch]) -> String {
    let mut by_file: Vec<(String, Vec<&GrepMatch>)> = Vec::new();
    for m in matches {
        match by_file.iter_mut().find(|(p, _)| p == &m.path) {
            Some((_, group)) => group.push(m),
            None => by_file.push((m.path.clone(), vec![m])),
        }
    }
    let sections: Vec<String> = by_file
        .into_iter()
        .map(|(path, group)| {
            let rows: Vec<String> = group
                .iter()
                .map(|m| format!("Line {}: {}", m.line_number, m.line))
                .collect();
            format!("{path}\n{}", rows.join("\n"))
        })
        .collect();
    sections.join("\n\n")
}

fn match_noun(count: usize) -> &'static str {
    if count == 1 {
        "match"
    } else {
        "matches"
    }
}

/// 参考 `formatGrepOutput`：头（Found …）+ 分组体 + 截断尾（spill 定位或无法保存说明）。
/// `spill` = 宿主 spill 层构建的完整 recovery 句（如 `Full grep result stored at:
/// <locator>. <hint>`）；None = 未能保存。
pub fn format_grep_output(retained: &RetainedMatches, spill: Option<&str>) -> String {
    let header = if retained.truncated() {
        format!(
            "Found {} of {} matches",
            retained.kept(),
            retained.seen
        )
    } else {
        format!("Found {} {}", retained.seen, match_noun(retained.seen))
    };
    let body = format_grep_matches(&retained.items);
    if !retained.truncated() {
        return format!("{header}\n\n{body}");
    }
    let recovery = match spill {
        Some(sentence) => sentence.to_string(),
        None => {
            "The complete result could not be saved; narrow pattern, path, or include to see more."
                .to_string()
        }
    };
    format!("{header}\n\n{body}\n\n({recovery})")
}

/// 参考 `toWorkdirRelative` 的显示形式（进程内：统一为相对搜索根的 `/` 分隔路径）。
fn rel_slash(root: &Path, abs: &Path) -> String {
    let rel = abs.strip_prefix(root).unwrap_or(abs);
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// 编译 include 为 Override（对齐参考 `--glob=<include>` 的 Whitlelist 过滤）。
fn include_matcher(include: &str) -> Result<Override, GrepError> {
    let mut b = OverrideBuilder::new(".");
    b.add(include).map_err(|e| {
        GrepError::new(format!("invalid glob include: {e}"), GrepErrorCode::InvalidPattern)
    })?;
    b.build().map_err(|e| {
        GrepError::new(format!("invalid glob include: {e}"), GrepErrorCode::InvalidPattern)
    })
}

/// 进程内 grep：在 root 下按 pattern（ripgrep 正则）匹配文件内容。
///
/// 遍历保持默认（隐藏忽略、gitignore 生效、VCS 剪枝）；`include` 给定则仅搜索
/// 命中 glob 的文件。文件按流式逐行匹配，避免整体载入。返回**完整**匹配列表
/// （统计 `seen` 用），模型面由 `retain_grep_matches` 裁剪预览。
pub fn grep_search(root: &Path, pattern: &str, include: Option<&str>) -> Result<Vec<GrepMatch>, GrepError> {
    let re = Regex::new(pattern).map_err(|e| {
        GrepError::new(
            format!("grep pattern rejected (regex parse error): {e}"),
            GrepErrorCode::InvalidPattern,
        )
    })?;
    let inc = match include {
        Some(i) => Some(include_matcher(i)?),
        None => None,
    };

    let builder = ignore::WalkBuilder::new(root);
    // grep 参考 argv 无 --no-ignore/--hidden：保留全部默认过滤器。
    let walker = builder.build();
    let mut matches: Vec<GrepMatch> = Vec::new();

    for entry in walker {
        let entry = entry.map_err(|e| {
            GrepError::new(
                format!("grep walk error: {e}"),
                GrepErrorCode::Failed,
            )
        })?;
        let ft = entry.file_type();
        let is_file = ft.map(|t| t.is_file()).unwrap_or(false);
        if !is_file {
            continue;
        }
        let abs = entry.path();
        let rel = rel_slash(root, abs);
        // include 过滤：Whitelist 才搜索；未命中/忽略不搜索。
        if let Some(inc) = &inc {
            if !matches!(inc.matched(&rel, false), Match::Whitelist(_)) {
                continue;
            }
        }
        let display = rel.clone();
        grep_file(abs, &display, &re, &mut matches).map_err(|e| {
            GrepError::new(
                format!("grep failed reading {}: {e}", abs.display()),
                GrepErrorCode::Failed,
            )
        })?;
    }
    Ok(matches)
}

/// 逐行流式匹配单个文件，追加到 `out`。非 UTF-8 行显示为占位预览（参考
/// `parseRecord` 的 `(line is not valid UTF-8)`），正则按原始字节匹配，不令搜索失败。
fn grep_file(
    path: &Path,
    display: &str,
    re: &Regex,
    out: &mut Vec<GrepMatch>,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut buf: Vec<u8> = Vec::new();
    let mut line_number: u64 = 0;
    loop {
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf)?;
        if n == 0 {
            break;
        }
        // 去尾随 \n 与 \r\n（对齐参考 `lines.text.replace(/\r?\n$/, '')`）。
        if buf.last() == Some(&b'\n') {
            buf.pop();
        }
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
        line_number += 1;
        if !re.is_match(&buf) {
            continue;
        }
        let line = match String::from_utf8_lossy(&buf) {
            Cow::Borrowed(s) => s.to_string(),
            Cow::Owned(_) => "(line is not valid UTF-8)".to_string(),
        };
        out.push(GrepMatch {
            path: display.to_string(),
            line_number,
            line,
        });
    }
    Ok(())
}

/// 便捷：带 path 的搜索（path 与 root 合并解析，参照参考 `-- <path>`）。
pub fn grep_search_in(root: &Path, input: &GrepInput) -> Result<Vec<GrepMatch>, GrepError> {
    let search_root = match &input.path {
        Some(p) => root.join(p),
        None => root.to_path_buf(),
    };
    grep_search(&search_root, &input.pattern, input.include.as_deref())
}

/// FsError 便捷桥（宿主接线层用）。
pub fn grep_err_to_fs(e: &GrepError) -> FsError {
    FsError::new(e.message.clone(), crate::types::FsErrorCode::FsIoError)
}

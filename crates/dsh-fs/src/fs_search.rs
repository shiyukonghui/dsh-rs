//! dsh-fs 进程内 glob 搜索（M5-DESIGN §4.6 / DIV-7）。
//!
//! 参考 `tool-fs-search/src/glob.ts`：`GLOB_MAX_RESULTS`、`GLOB_VCS_EXCLUDES`、
//! `parseGlobArgs` 校验、`--no-ignore --hidden` 语义（隐藏文件收录、忽略文件不生效、
//! VCS 目录恒剔除、`--sort=modified` 排序、上限截断）。
//!
//! DIV-7：不用 rg 二进制，改用 ripgrep 同源的 `ignore` 遍历 + `ignore::overrides`
//! （rg `--glob` 正是 `Override`）在进程内枚举，语义逐字对齐。

use crate::types::FsError;
use ignore::overrides::{Override, OverrideBuilder};
use ignore::Match;
use std::path::Path;
use std::time::SystemTime;

/// 参考 `GLOB_MAX_RESULTS`：一次 glob 调用内联保留的路径上限。
pub const GLOB_MAX_RESULTS: usize = 100;

/// 参考 `GLOB_VCS_EXCLUDES`：喂给遍历器的 VCS 元数据目录名（遍历时恒剪枝）。
pub const GLOB_VCS_EXCLUDES: &[&str] = &[".git", ".svn", ".hg", ".bzr", ".jj", ".sl"];

/// 参考 `GlobInput`：校验后的 glob 参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobInput {
    pub pattern: String,
    pub path: Option<String>,
}

/// 参考 `parseGlobArgs`：非空 pattern、非空 path（给定时）。
pub fn parse_glob_args(pattern: &str, path: Option<&str>) -> Result<GlobInput, String> {
    if pattern.trim().is_empty() {
        return Err("pattern must be a non-empty string".into());
    }
    if let Some(p) = path {
        if p.trim().is_empty() {
            return Err("path must be a non-empty string when given".into());
        }
    }
    Ok(GlobInput { pattern: pattern.to_string(), path: path.map(|s| s.to_string()) })
}

/// 把喂给 matcher 的相对路径统一为 `/` 分隔（Windows 遍历器用 `\`，override glob
/// 用 `/` 作分隔符）。
fn unify(rel: &Path) -> String {
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// 编译用户 pattern 为 rg `--glob` 同源的 Override（`*.rs` 自动任意深度；带 `/` 的
/// glob 锚定相对 root 的路径；`./` 前缀被 ignore 正常解析）。
fn override_matcher(pattern: &str) -> Result<Override, FsError> {
    let mut b = OverrideBuilder::new(".");
    b.add(pattern).map_err(|e| {
        FsError::new(format!("invalid glob pattern: {e}"), crate::types::FsErrorCode::FsIoError)
    })?;
    b.build().map_err(|e| {
        FsError::new(format!("invalid glob pattern: {e}"), crate::types::FsErrorCode::FsIoError)
    })
}

fn is_vcs_dir(name: &str) -> bool {
    GLOB_VCS_EXCLUDES.contains(&name)
}

/// 参考 `buildGlobCommand` + rg 枚举：在 root 下按 glob 收集文件（隐藏收录、忽略文件
/// 不生效、VCS 剪枝、mtime 排序、上限）。返回相对 root 的 `/` 分隔路径（模型面）。
pub fn glob_search(root: &Path, pattern: &str) -> Result<Vec<String>, FsError> {
    let matcher = override_matcher(pattern)?;

    let mut builder = ignore::WalkBuilder::new(root);
    // `--no-ignore --hidden`：忽略文件不生效、隐藏文件收录。注意 ignore crate 语义是
    // `hidden(yes)` 开启「忽略隐藏文件」；要收录隐藏文件必须 `hidden(false)`（对应 rg `--hidden`）。
    builder
        .hidden(false)     // 不忽略隐藏文件（收录）
        .ignore(false)     // 不读 .gitignore / .ignore
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .parents(false)
        .require_git(false)
        .sort_by_file_name(|a, b| a.cmp(b));
    // VCS 目录剪枝（对齐 GLOB_VCS_EXCLUDES 的双排除语义）。
    builder.filter_entry(|entry| {
        let name = entry.file_name().to_string_lossy();
        !(entry.file_type().map(|t| t.is_dir()).unwrap_or(false) && is_vcs_dir(&name))
    });

    let walker = builder.build();
    let mut matches: Vec<(String, Option<SystemTime>)> = Vec::new();
    for entry in walker {
        let entry = entry.map_err(|e| {
            FsError::new(format!("glob walk error: {e}"), crate::types::FsErrorCode::FsIoError)
        })?;
        let ft = entry.file_type();
        let is_file = ft.map(|t| t.is_file()).unwrap_or(false);
        if !is_file {
            continue;
        }
        let abs = entry.path();
        let rel = abs.strip_prefix(root).unwrap_or(abs).to_path_buf();
        let rel_s = unify(&rel);
        // rg `--files` + `--glob=<p>`：Whitelist = 命中保留，Ignore = 未命中丢弃。
        if matches!(matcher.matched(&rel_s, false), Match::Whitelist(_)) {
            let mt = entry.metadata().ok().and_then(|m| m.modified().ok());
            matches.push((rel_s, mt));
        }
    }
    // `--sort=modified`：mtime 升序（稳定），同刻按路径序。
    matches.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    Ok(matches.into_iter().take(GLOB_MAX_RESULTS).map(|(p, _)| p).collect())
}

/// 便捷：带路径的搜索（path 与 root 合并解析，参照参考的 `-- <path>`）。
pub fn glob_search_in(root: &Path, input: &GlobInput) -> Result<Vec<String>, FsError> {
    let search_root = match &input.path {
        Some(p) => root.join(p),
        None => root.to_path_buf(),
    };
    glob_search(&search_root, &input.pattern)
}

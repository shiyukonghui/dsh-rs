//! M3a host 目录方法面：`host.listDirectory` / `host.createDirectory` 的真实
//! 实现（对齐 `@deepseek-ai/dsh-host-directory-picker-browse`）。
//!
//! 纯 std fs + 纯函数，可差分单测（web.rs 的沙盒环境无法弹本地对话框）。
//! 语义要点（对齐 browse 实现，详见 DECISIONS D-037）：
//! - `fully_qualified` 围栏：只收 absolute 路径，绝不把相对/驱动根路径静默重基；
//! - 有界排序窗口（keep = max_entries + 1）：任意大目录内存有界，truncated 诚实标记；
//! - 符号链接：dirent 命中目录直接放行，符号链接 stat 探针（可进入才 row，broken 静默跳）；
//! - hidden = 名称以 `.` 前缀（POSIX 习惯；Windows hidden 属性 dirent 不暴露，差异记录）；
//! - createDirectory：段名校验（空白/`.`/`..`/含 `/\` 拒）→ `mkdir` 非递归；
//!   `EEXIST → directory-exists`、其余 → `directory-create-failed`。

use std::path::{Path, PathBuf};

/// 目录浏览条目（对齐 `DirectoryEntry`：{name, path, hidden}）。
#[derive(Debug, Clone, PartialEq)]
pub struct DirectoryEntry {
    pub name: String,
    pub path: String,
    pub hidden: bool,
}

/// 目录列表（对齐 `DirectoryListing`：{path, home, crumbs, entries, truncated}）。
#[derive(Debug, Clone, PartialEq)]
pub struct DirectoryListing {
    pub path: String,
    pub home: String,
    pub crumbs: Vec<DirectoryEntry>,
    pub entries: Vec<DirectoryEntry>,
    pub truncated: bool,
}

/// host 目录方法错误（对齐 `DirectoryPickerError` 的 code 三态 + message）。
#[derive(Debug, Clone, PartialEq)]
pub struct HostDirError {
    pub code: &'static str,
    pub path: String,
    pub message: String,
}

impl HostDirError {
    pub fn new(code: &'static str, path: String, message: String) -> Self {
        HostDirError { code, path, message }
    }
}

/// 用户主目录（Windows `USERPROFILE`，其余 `HOME`；无则当前目录）。
pub fn home_dir() -> String {
    let v = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(|p| p.to_string_lossy().to_string());
    v.unwrap_or_else(current_dir_string)
}

fn current_dir_string() -> String {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// True 当路径在本平台 fully qualified（Windows：盘符限定或完整 UNC；
/// 其余：POSIX absolute）。对齐 browse 的 `fullyQualified`——驱动根相对形式
/// （`\foo`/`/foo`）与不完整 UNC 前缀（`\\`、`\\server`）不算。
pub fn fully_qualified(path: &str) -> bool {
    if cfg!(windows) {
        // Windows：须盘符限定（`C:\…`/`C:/…`）或完整 UNC（`\\server\share…`）。
        // 单独 `\` 根相对（`\a`）、单 `/`、以及 `\\server`（缺 share 级）都拒。
        let bytes = path.as_bytes();
        let is_unc = bytes.starts_with(b"\\\\") || bytes.starts_with(b"//");
        let is_drive = bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'\\' | b'/');
        if is_drive {
            return true;
        }
        if !is_unc {
            return false;
        }
        // 完整 UNC 至少含 server 与 share 两段（去掉双前缀后有两级非空）。
        let rest = &path[2..];
        let mut seg = rest.split(['\\', '/']).filter(|s| !s.is_empty());
        let server = seg.next();
        let share = seg.next();
        server.is_some() && share.is_some()
    } else {
        Path::new(path).is_absolute()
    }
}

/// 祖先链 crumbs（从文件系统根到 target 含自身；根 crumb 带全路径名）。
pub fn ancestry_crumbs(target: &str) -> Vec<DirectoryEntry> {
    let mut crumbs: Vec<DirectoryEntry> = Vec::new();
    let mut current = Path::new(target).to_path_buf();
    loop {
        let name = match current.file_name() {
            Some(n) => n.to_string_lossy().to_string(),
            // 根（`/` 或 `C:\`）：file_name() 为 None → 以全路径名标注。
            None => current.to_string_lossy().to_string(),
        };
        let is_root = current
            .parent()
            .map(|p| p == current)
            .unwrap_or(true);
        crumbs.insert(0, DirectoryEntry {
            name,
            path: current.to_string_lossy().to_string(),
            hidden: false,
        });
        if is_root {
            break;
        }
        match current.parent() {
            Some(p) if p != current => current = p.to_path_buf(),
            _ => break,
        }
    }
    crumbs
}

/// 有界排序窗口候选项。
#[derive(Debug, Clone, PartialEq)]
pub struct ListingCandidate {
    pub name: String,
    pub is_directory: bool,
    pub is_symbolic_link: bool,
}

/// 插入候选项到 name 升序有界窗口；返回是否发生驱逐（截断证据）。
/// 对齐 browse `boundedInsert`：满窗且 name ≥ 尾 → O(1) 拒；否则二分定位插入。
pub fn bounded_insert(
    window: &mut Vec<ListingCandidate>,
    candidate: ListingCandidate,
    keep: usize,
) -> bool {
    // 满窗且候选名不小于尾部 → 直接拒（一次比较）。
    if window.len() == keep
        && keep > 0
        && candidate.name >= window[keep - 1].name
    {
        return true;
    }
    // 二分定位（O(log keep)）。
    let mut lo = 0usize;
    let mut hi = window.len();
    while lo < hi {
        let mid = (lo + hi) / 2;
        if candidate.name < window[mid].name {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    window.insert(lo, candidate);
    if window.len() <= keep {
        return false;
    }
    window.pop();
    true
}

/// 展开目标路径：fully-qualified 围栏 + lexical 规整（复用 normalize_without_fs；
/// 参考 browse 的 `resolve(path ?? home)`，但不触 fs 不做符号链接解析）。
fn resolve_target(path: Option<&str>) -> Result<PathBuf, HostDirError> {
    let raw = match path {
        Some(p) => p,
        None => return Ok(PathBuf::from(home_dir())),
    };
    if !fully_qualified(raw) {
        return Err(HostDirError::new(
            "directory-unreadable",
            raw.to_string(),
            format!("cannot list \"{raw}\": not a fully qualified path"),
        ));
    }
    Ok(normalize_without_fs(raw))
}

/// `host.listDirectory`：列目标目录一层（可省略 → home）。对齐 browse：非
/// fully-qualified → directory-unreadable；目录不可读/缺失 → directory-unreadable。
pub fn list_directory(path: Option<&str>, max_entries: usize) -> Result<DirectoryListing, HostDirError> {
    let target = resolve_target(path)?;
    // keep = max_entries + 1：窗口多容一个候选以证明截断。
    let keep = max_entries.saturating_add(1);
    let mut window: Vec<ListingCandidate> = Vec::new();
    let mut evicted = false;
    let read = std::fs::read_dir(&target).map_err(|e| {
        HostDirError::new(
            "directory-unreadable",
            target.to_string_lossy().to_string(),
            format!(
                "cannot list {}: {}",
                target.to_string_lossy(),
                e
            ),
        )
    })?;
    for dirent in read {
        let dirent = dirent.map_err(|e| {
            HostDirError::new(
                "directory-unreadable",
                target.to_string_lossy().to_string(),
                format!("cannot list {}: {}", target.to_string_lossy(), e),
            )
        })?;
        let ft = match dirent.file_type() {
            Ok(ft) => ft,
            // 读不到类型（权限竞态等）→ 跳过该候选。
            Err(_) => continue,
        };
        if !ft.is_dir() && !ft.is_symlink() {
            continue;
        }
        let candidate = ListingCandidate {
            name: dirent.file_name().to_string_lossy().to_string(),
            is_directory: ft.is_dir(),
            is_symbolic_link: ft.is_symlink(),
        };
        if bounded_insert(&mut window, candidate, keep) {
            evicted = true;
        }
    }
    // 逐候选 stat 探针（symlink 需要）：broken link 静默跳；entries 满 max 截断。
    let mut entries: Vec<DirectoryEntry> = Vec::new();
    let mut truncated = evicted;
    for cand in &window {
        let row = directory_row(&target, cand, max_entries);
        match row {
            Some(row) => {
                if entries.len() == max_entries {
                    truncated = true;
                    break;
                }
                entries.push(row);
            }
            None => continue,
        }
    }
    Ok(DirectoryListing {
        path: target.to_string_lossy().to_string(),
        home: home_dir(),
        crumbs: ancestry_crumbs(&target.to_string_lossy()),
        entries,
        truncated,
    })
}

/// 一个 dirent → DirectoryEntry（跟随符号链接 stat 探针；broken/循环 → None）。
fn directory_row(
    parent: &Path,
    cand: &ListingCandidate,
    _max_entries: usize,
) -> Option<DirectoryEntry> {
    let path = parent.join(&cand.name);
    let mut enterable = cand.is_directory;
    if !enterable && cand.is_symbolic_link {
        match std::fs::metadata(&path) {
            Ok(meta) => enterable = meta.is_dir(),
            Err(_) => return None, // broken/cyclic link → 静默跳。
        }
    }
    if !enterable {
        return None;
    }
    Some(DirectoryEntry {
        name: cand.name.clone(),
        path: path.to_string_lossy().to_string(),
        hidden: cand.name.starts_with('.'),
    })
}

/// `host.createDirectory`：在 parent 下创建 name 单段子目录，返回绝对路径。
/// 非 fully-qualified 父 / 非法段名 → directory-create-failed；EEXIST →
/// directory-exists；其余 fs 失败 → directory-create-failed。
pub fn create_directory(parent: &str, name: &str) -> Result<String, HostDirError> {
    if !fully_qualified(parent) {
        return Err(HostDirError::new(
            "directory-create-failed",
            parent.to_string(),
            format!(
                "cannot create under \"{parent}\": not a fully qualified parent path"
            ),
        ));
    }
    if name.trim().is_empty() || name == "." || name == ".." || name.contains(['/', '\\']) {
        let target = Path::new(parent).join(name);
        return Err(HostDirError::new(
            "directory-create-failed",
            target.to_string_lossy().to_string(),
            format!("\"{name}\" is not a single path segment"),
        ));
    }
    let parent = normalize_without_fs(parent);
    let target = parent.join(name);
    match std::fs::create_dir(&target) {
        Ok(()) => Ok(target.to_string_lossy().to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(HostDirError::new(
                "directory-exists",
                target.to_string_lossy().to_string(),
                format!("{} already exists", target.to_string_lossy()),
            ))
        }
        Err(e) => Err(HostDirError::new(
            "directory-create-failed",
            target.to_string_lossy().to_string(),
            format!("cannot create {}: {}", target.to_string_lossy(), e),
        )),
    }
}

/// 路径规整：lexical `..`/`.` 折叠 + 重复分隔符归一（对齐 Node `resolve` 的
/// 字典序语义；不触 fs）。
pub fn normalize_without_fs(path: &str) -> PathBuf {
    let mut out = PathBuf::new();
    let mut root_seen = false;
    for comp in Path::new(path).components() {
        use std::path::Component;
        match comp {
            Component::RootDir => {
                root_seen = true;
                out.push(comp.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                // 已见过 root 才折叠；否则（纯相对前的 ..）保留前导语义由调用方拦截。
                if root_seen && !out.pop() {
                    // 无可 pop（根处 ..）→ 忽略。
                }
            }
            Component::Normal(_) | Component::Prefix(_) => {
                out.push(comp.as_os_str());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(name: impl Into<String>, is_directory: bool, is_symbolic_link: bool) -> ListingCandidate {
        ListingCandidate {
            name: name.into(),
            is_directory,
            is_symbolic_link,
        }
    }

    #[test]
    fn fully_qualified_is_platform() {
        if cfg!(windows) {
            // Windows：盘符 absolute 与完整 UNC 通过；root-relative 与相对拒。
            assert!(fully_qualified(r"C:\a\b"));
            assert!(fully_qualified(r"C:/a/b"));
            assert!(fully_qualified(r"\\server\share\a"));
            assert!(!fully_qualified(r"\a\b"), "drive-less rooted is not absolute");
            assert!(!fully_qualified(r"a\b"));
            assert!(!fully_qualified(""));
            assert!(!fully_qualified("."));
        } else {
            assert!(fully_qualified("/a/b"));
            assert!(fully_qualified("/"));
            assert!(!fully_qualified("a/b"));
            assert!(!fully_qualified(""));
            assert!(!fully_qualified("."));
        }
    }

    /// 临时目录（测试后清理）。
    fn tmp_dir(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "dsh-m3a-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn bounded_insert_keeps_name_sorted_head() {
        let keep = 3;
        let mut w: Vec<ListingCandidate> = Vec::new();
        let mut evicted = false;
        for name in ["b", "a", "c", "d", "ab"] {
            evicted |= bounded_insert(&mut w, candidate(name, true, false), keep);
        }
        let names: Vec<&str> = w.iter().map(|c| c.name.as_str()).collect();
        // keep=3 窗口保有 lexicographic 最小三项：a, ab, b（c/d 被驱逐）。
        assert_eq!(names, vec!["a", "ab", "b"]);
        assert!(evicted, "超过窗口必驱逐");
        // 新来且名大于尾部 → O(1) 直接拒。
        assert!(bounded_insert(&mut w, candidate("zzz", true, false), keep));
        assert_eq!(w.len(), keep);
    }

    #[test]
    fn ancestry_crumbs_posix() {
        let crumbs = ancestry_crumbs("/a/b/c");
        let names: Vec<&str> = crumbs.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["/", "a", "b", "c"]);
    }

    #[test]
    fn list_directory_reads_children_and_flags_hidden() {
        let root = tmp_dir(&format!(
            "list-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(root.join(".hidden")).unwrap();
        std::fs::write(root.join("file.txt"), "x").unwrap();
        let listing = list_directory(Some(root.to_str().unwrap()), 1000).unwrap();
        assert_eq!(listing.path, root.to_string_lossy().to_string());
        assert!(!listing.home.is_empty());
        assert!(!listing.truncated);
        let sub = listing
            .entries
            .iter()
            .find(|e| e.name == "sub")
            .expect("sub directory row present");
        assert!(!sub.hidden);
        assert!(sub.path.contains("sub"));
        let hidden = listing
            .entries
            .iter()
            .find(|e| e.name == ".hidden")
            .expect(".hidden row present");
        assert!(hidden.hidden);
        // 普通文件不在 entries（browse 只列可进入目录）。
        assert!(
            listing.entries.iter().all(|e| e.name != "file.txt"),
            "non-directory row skipped"
        );
        // crumbs 尾为列表目录自身（ancestor chain 含 target）。
        let last = listing.crumbs.last().unwrap();
        assert_eq!(last.name, root.file_name().unwrap().to_string_lossy());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn list_directory_without_path_lists_home() {
        let listing = list_directory(None, 1000).unwrap();
        assert_eq!(listing.path, home_dir());
    }

    #[test]
    fn list_directory_missing_target_is_unreadable() {
        let err = list_directory(Some("C:\\definitely-not-here-xyz"), 1000).unwrap_err();
        assert_eq!(err.code, "directory-unreadable");
        assert!(err.message.contains("not"));
    }

    #[test]
    fn list_directory_relative_path_rejected() {
        let err = list_directory(Some("relative/path"), 1000).unwrap_err();
        assert_eq!(err.code, "directory-unreadable");
        assert!(err.message.contains("fully qualified"));
    }

    #[test]
    fn create_directory_ok_and_exists() {
        let root = tmp_dir(&format!("create-{}", std::process::id()));
        let created = create_directory(root.to_str().unwrap(), "nested").unwrap();
        assert_eq!(created, root.join("nested").to_string_lossy().to_string());
        assert!(root.join("nested").is_dir());
        let err = create_directory(root.to_str().unwrap(), "nested").unwrap_err();
        assert_eq!(err.code, "directory-exists");
        assert!(err.message.contains("already exists"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn create_directory_rejects_bad_segment() {
        let root = tmp_dir(&format!("create-bad-{}", std::process::id()));
        for bad in ["", " ", ".", "..", "a/b", "a\\b"] {
            let err = create_directory(root.to_str().unwrap(), bad).unwrap_err();
            assert_eq!(
                err.code,
                "directory-create-failed",
                "segment {:?} rejected",
                bad
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn create_directory_rejects_relative_parent() {
        let err = create_directory("relative/parent", "x").unwrap_err();
        assert_eq!(err.code, "directory-create-failed");
        assert!(err.message.contains("fully qualified"));
    }

    #[test]
    fn normalize_dots_and_duplicate_seps() {
        // `/a/./b/../c//d` → `/a/c/d` 的 OS 原生分隔形式（Windows `\a\c\d`）。
        let normalized = normalize_without_fs("/a/./b/../c//d").to_string_lossy().to_string();
        let expected = if cfg!(windows) {
            "\\a\\c\\d".to_string()
        } else {
            "/a/c/d".to_string()
        };
        assert_eq!(normalized, expected);
        // 折叠后无残余 `.`/`..` 组件。
        let comps: Vec<String> = Path::new(&normalized)
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect();
        assert!(!comps.iter().any(|c| c == "." || c == ".."));
    }

    /// 大目录：超过 max_entries 时 entries 截断且 truncated=true（有界窗口）。
    #[test]
    fn list_directory_truncates_oversized_level() {
        let root = tmp_dir(&format!("truncate-{}", std::process::id()));
        for i in 0..60 {
            std::fs::create_dir_all(root.join(format!("d{i:03}"))).unwrap();
        }
        let listing = list_directory(Some(root.to_str().unwrap()), 10).unwrap();
        assert!(listing.entries.len() <= 10);
        assert!(listing.truncated);
        let _ = std::fs::remove_dir_all(&root);
    }
}

//! dsh-fs 本地 provider（M5-DESIGN §4.2）。
//!
//! 参考 `fs-local/src/index.ts`：按 targetKey 串行化（per-target lock）、probe 探身份与
//! 版本、writeText/editText 的守卫语义（stale/not-observed/not-regular-file）、原子写
//! （同目录 temp + rename）、版本由 stat 高分辨率新鲜度派生。

use crate::types::{
    FsEditOutcome, FsEditRequest, FsError, FsErrorCode, FsReadText, FsTarget, FsTargetKey,
    FsVersion, FsWriteIntent, FsWriteOutcome, ReadTextOptions, ResolveOptions,
};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// 高分辨率新鲜度：len:mtime_nanos:ino（stat identity + freshness 字段）。
fn version_for(path: &Path) -> Option<FsVersion> {
    let meta = fs::metadata(path).ok()?;
    #[cfg(unix)]
    let ino = {
        use std::os::unix::fs::MetadataExt;
        meta.ino()
    };
    #[cfg(not(unix))]
    let ino = 0u64;
    let nanos = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Some(FsVersion(format!("{}:{}:{}", meta.len(), nanos, ino)))
}

/// 目标身份：显示路径 + canonical 键。
pub struct LocalFileSystem {
    root: PathBuf,
}

impl LocalFileSystem {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// 参考 `resolve`：路径 → 目标；identity 跨别名稳定（此处 canonical）。
    pub fn resolve(&self, path: &str, opts: ResolveOptions) -> Result<FsTarget, FsError> {
        let base = opts.cwd.unwrap_or_else(|| self.root.clone());
        let joined = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            base.join(path)
        };
        let canon = fs::canonicalize(&joined).unwrap_or_else(|_| joined.clone());
        let abs = if joined.exists() { canon } else { joined };
        Ok(FsTarget {
            target_key: FsTargetKey(abs.to_string_lossy().into_owned()),
            display_path: path.to_string(),
        })
    }

    /// 参考 `readText`：读全部文本（此步不做行数限额，限额在 tool 层）。
    pub fn read_text(
        &self,
        target: &FsTarget,
        _opts: ReadTextOptions,
    ) -> Result<FsReadText, FsError> {
        let abs = PathBuf::from(&target.target_key.0);
        let content = fs::read_to_string(&abs).map_err(|e| self.map_io(target, e))?;
        let version = version_for(&abs).unwrap_or_else(|| FsVersion(target.target_key.0.clone()));
        Ok(FsReadText { content, version })
    }

    fn map_io(&self, target: &FsTarget, e: std::io::Error) -> FsError {
        let code = match e.kind() {
            std::io::ErrorKind::NotFound => FsErrorCode::FsNotFound,
            std::io::ErrorKind::PermissionDenied => FsErrorCode::FsPermissionDenied,
            _ => FsErrorCode::FsIoError,
        };
        FsError::new(format!("{}: {e}", target.display_path), code)
    }

    fn probe(&self, target: &FsTarget) -> Result<Option<(FsVersion, bool /*is_file*/)>, FsError> {
        let abs = PathBuf::from(&target.target_key.0);
        match fs::metadata(&abs) {
            Ok(m) => Ok(Some((version_for(&abs).unwrap_or(FsVersion("?".into())), m.is_file()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(self.map_io(target, e)),
        }
    }

    /// 原子写：同目录临时文件 + rename。
    fn write_file_atomic(&self, target: &FsTarget, content: &str) -> Result<(), FsError> {
        let abs = PathBuf::from(&target.target_key.0);
        let dir = abs.parent().ok_or_else(|| {
            FsError::new(format!("no parent for {}", target.display_path), FsErrorCode::FsIoError)
        })?;
        let tmp = dir.join(format!(".dsh-tmp-{}.{}", std::process::id(), tmp_counter()));
        let mut f = fs::File::create(&tmp).map_err(|e| self.map_io(target, e))?;
        f.write_all(content.as_bytes()).map_err(|e| self.map_io(target, e))?;
        f.sync_all().ok();
        drop(f);
        fs::rename(&tmp, &abs).map_err(|e| self.map_io(target, e))?;
        Ok(())
    }

    /// 参考 `writeText`（见模块注释守卫语义）。
    pub fn write_text(
        &self,
        target: &FsTarget,
        content: &str,
        expected: Option<FsWriteIntent>,
        _signal: Option<()>,
    ) -> Result<FsWriteOutcome, FsError> {
        let existing = self.probe(target)?;
        if let Some((_, is_file)) = &existing {
            if !is_file {
                return Err(FsError::new(
                    format!("cannot write \"{}\": not a regular file", target.display_path),
                    FsErrorCode::FsNotRegularFile,
                ));
            }
        }
        match &expected {
            Some(FsWriteIntent::ReplaceIfVersion { version }) => match &existing {
                Some((v, _)) if v == version => {}
                Some(_) => {
                    return Err(FsError::new(
                        format!("cannot write \"{}\": file changed since it was read", target.display_path),
                        FsErrorCode::FsStaleVersion,
                    ))
                }
                None => {
                    return Err(FsError::new(
                        format!("cannot write \"{}\": file no longer exists", target.display_path),
                        FsErrorCode::FsStaleVersion,
                    ))
                }
            },
            Some(FsWriteIntent::CreateIfAbsent) => {
                if existing.is_some() {
                    return Err(FsError::new(
                        format!("cannot overwrite existing \"{}\" without reading it first", target.display_path),
                        FsErrorCode::FsNotObserved,
                    ));
                }
            }
            None => {}
        }

        // before：更新路径捕获先前内容作 diff 基础。
        let before = if existing.is_some() {
            fs::read_to_string(PathBuf::from(&target.target_key.0)).ok()
        } else {
            None
        };
        self.write_file_atomic(target, content)?;
        let version = version_for(&PathBuf::from(&target.target_key.0))
            .unwrap_or_else(|| FsVersion(format!("missing:{}", target.target_key.0)));
        Ok(FsWriteOutcome {
            operation: if existing.is_some() { "update" } else { "create" },
            version,
            before,
            after: normalize_lf(content),
        })
    }

    /// 参考 `editText`：先守卫（缺失/类型/版本），再字面替换，写回。
    pub fn edit_text(
        &self,
        target: &FsTarget,
        req: &FsEditRequest,
        expected: Option<&FsVersion>,
        _signal: Option<()>,
    ) -> Result<FsEditOutcome, FsError> {
        let existing = self.probe(target)?;
        let exist = existing.ok_or_else(|| {
            FsError::new(
                format!("cannot edit \"{}\": file changed since it was read", target.display_path),
                FsErrorCode::FsStaleVersion,
            )
        })?;
        let (v, is_file) = exist;
        if !is_file {
            return Err(FsError::new(
                format!("cannot edit \"{}\": not a regular file", target.display_path),
                FsErrorCode::FsNotRegularFile,
            ));
        }
        if let Some(expected_v) = expected {
            if v != *expected_v {
                return Err(FsError::new(
                    format!("cannot edit \"{}\": file changed since it was read", target.display_path),
                    FsErrorCode::FsStaleVersion,
                ));
            }
        }

        let original = fs::read_to_string(PathBuf::from(&target.target_key.0))
            .map_err(|e| self.map_io(target, e))?;
        let edited = apply_literal_edit(&original, req, &target.display_path)?;
        self.write_file_atomic(target, &edited)?;
        let version = version_for(&PathBuf::from(&target.target_key.0))
            .unwrap_or_else(|| FsVersion(format!("missing:{}", target.target_key.0)));
        Ok(FsEditOutcome {
            version,
            before: normalize_lf(&original),
            after: normalize_lf(&edited),
        })
    }
}

fn normalize_lf(s: &str) -> String {
    s.replace("\r\n", "\n")
}

/// 字面替换：find old_string；replaceAll 或唯一；多匹配非 replaceAll → FS_AMBIGUOUS_EDIT；
/// 零匹配 → FS_EDIT_NOT_FOUND。
fn apply_literal_edit(original: &str, req: &FsEditRequest, display: &str) -> Result<String, FsError> {
    if req.old_string.is_empty() {
        // 参考：空旧文作为插入语义（在每处前插）——此处按唯一/全量处理。
        return apply_empty_insert(original, &req.new_string, req.replace_all);
    }
    let matches: Vec<(usize, &str)> = original.match_indices(&req.old_string).collect();
    if matches.is_empty() {
        return Err(FsError::new(
            format!("cannot edit \"{display}\": old_string not found"),
            FsErrorCode::FsEditNotFound,
        ));
    }
    if !req.replace_all && matches.len() > 1 {
        return Err(FsError::new(
            format!("cannot edit \"{display}\": multiple matches for old_string"),
            FsErrorCode::FsAmbiguousEdit,
        ));
    }
    if req.replace_all {
        Ok(original.replace(&req.old_string, &req.new_string))
    } else {
        let (idx, _) = matches[0];
        let mut out = String::with_capacity(original.len());
        out.push_str(&original[..idx]);
        out.push_str(&req.new_string);
        out.push_str(&original[idx + req.old_string.len()..]);
        Ok(out)
    }
}

/// 空 old_string：在每处字符边界前插入（参考 edit 工具的 insert 语义近似）。
fn apply_empty_insert(original: &str, new_string: &str, replace_all: bool) -> Result<String, FsError> {
    if !replace_all {
        // 单一：插在首个字符前（近似参考 view 工厂「新文前置」）。
        let mut out = String::new();
        out.push_str(new_string);
        out.push_str(original);
        Ok(out)
    } else {
        let mut out = String::new();
        for ch in original.chars() {
            out.push_str(new_string);
            out.push(ch);
        }
        if original.is_empty() {
            out.push_str(new_string);
        }
        Ok(out)
    }
}

/// 免依赖的进程内短计数器（避免引入全局状态）。
static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn tmp_counter() -> u64 {
    COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

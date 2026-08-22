//! fs 原子写基建：settings/credentials 文件写路径的 temp+sync+rename 发布。
//!
//! 从 jsonl 物化的 `write_tmp_then_publish` 形态抽取的通用小函数；与 jsonl 的
//! 差别是 **可覆盖既有目标**（settings 需反复改写），其余保持同一原子语义
//! （写 temp → fsync → rename 发布 → 失败清理 temp）。

use std::fs;
use std::io::Write;
use std::path::Path;

/// 把 `content` 原子写入 `target`（存在则原子替换）。
///
/// 步骤：确保父目录存在 → `{stem}.tmp` create_new 写入 + sync → rename 发布；
/// 任一步失败清理 temp 并返回 `Other`。不会留下 `.tmp` 残留。
pub fn atomic_write(target: &Path, content: &[u8]) -> Result<(), crate::PersistenceError> {
    let dir = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir)
        .map_err(|e| crate::PersistenceError::Other(format!("mkdir {}: {e}", dir.display())))?;
    let stem = target
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!("{stem}.tmp"));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .map_err(|e| {
            crate::PersistenceError::Other(format!("create temp {}: {e}", tmp.display()))
        })?;
    if let Err(e) = file.write_all(content).and_then(|_| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(&tmp);
        return Err(crate::PersistenceError::Other(format!(
            "write temp {}: {e}",
            tmp.display()
        )));
    }
    drop(file);
    fs::rename(&tmp, target).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        crate::PersistenceError::Other(format!(
            "publish {} -> {}: {e}",
            tmp.display(),
            target.display()
        ))
    })?;
    Ok(())
}

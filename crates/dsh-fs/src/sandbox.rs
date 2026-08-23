//! dsh-fs 进程内沙箱围栏（M5-DESIGN §4.4）。
//!
//! 参考 `fs-sandbox/src/index.ts` + `containment.ts`（逐字语义）：
//! - `SandboxPolicy` 镜像 `SandboxExecutionPolicy`（mode + workspace_root）。
//! - `checked_target(target, policy)`：danger 直通；read-only → FS_SANDBOX_DENIED；
//!   workspace-write → is_path_under(任一 writable_roots) 否则拒。围栏只在写路径调用
//!   （writeText/editText），读路径全放行。
//! - `is_path_under(path, root, case_sensitive)`：词法快路径（==root 或
//!   starts_with(root+sep)）→ true；否则沿祖先做文件系统身份比对（Windows 8.3/大小写
//!   别名兜底）。

use crate::types::{FsError, FsErrorCode, FsTarget};
use dsh_sandbox::{SandboxMode, writable_roots};
use std::path::{Path, PathBuf};

/// 镜像 `SandboxExecutionPolicy`：一次执行的沙箱策略事实。
#[derive(Debug, Clone)]
pub struct SandboxPolicy {
    pub mode: SandboxMode,
    /// workspace-write 时的工作区根（其余模式可为 None）。
    pub workspace_root: Option<PathBuf>,
}

impl SandboxPolicy {
    /// 默认（read-only）。
    pub fn read_only() -> Self {
        Self { mode: SandboxMode::ReadOnly, workspace_root: None }
    }

    pub fn workspace_write(root: PathBuf) -> Self {
        Self { mode: SandboxMode::WorkspaceWrite, workspace_root: Some(root) }
    }

    pub fn danger() -> Self {
        Self { mode: SandboxMode::DangerFullAccess, workspace_root: None }
    }
}

/// 大小写不敏感判断（Windows 约定）：参考 `process.platform !== 'win32'`。
fn is_case_sensitive() -> bool {
    #[cfg(windows)]
    {
        false
    }
    #[cfg(not(windows))]
    {
        true
    }
}

fn comparable(path: &str, case_sensitive: bool) -> String {
    // 规整 Windows 扩展长路径前缀 `\\?\`（`C:\...` 与 `\\?\C:\...` 同一文件）。
    let path = path
        .strip_prefix("\\\\?\\")
        .map(str::to_string)
        .unwrap_or_else(|| path.to_string());
    if case_sensitive {
        path
    } else {
        path.to_lowercase()
    }
}

fn is_lexically_under(path: &str, root: &str, case_sensitive: bool) -> bool {
    let target = comparable(path, case_sensitive);
    let root = comparable(root, case_sensitive);
    if target == root {
        return true;
    }
    let prefix = if root.ends_with('/') || root.ends_with('\\') {
        root
    } else {
        format!("{root}{}", std::path::MAIN_SEPARATOR)
    };
    target.starts_with(&prefix)
}

/// 参考 `isPathUnder`：词法快路径 → 身份兜底（祖先 dev+ino 等同 root）。
pub fn is_path_under(path: &Path, root: &Path, case_sensitive: bool) -> bool {
    let path_s = path.to_string_lossy();
    let root_s = root.to_string_lossy();
    if is_lexically_under(&path_s, &root_s, case_sensitive) {
        return true;
    }
    // 身份兜底：root 存在才可比；沿 path 祖先上溯比对 dev+ino。
    let root_meta = std::fs::metadata(root).ok();
    if root_meta.is_none() {
        return false;
    }
    let root_id = identity(&root_meta.unwrap());
    let root_id = match root_id {
        Some(id) => id,
        None => return false,
    };
    let mut ancestor = path.to_path_buf();
    loop {
        if let Ok(meta) = std::fs::metadata(&ancestor) {
            if identity(&meta) == Some(root_id) {
                return true;
            }
        }
        let parent = ancestor.parent();
        match parent {
            Some(p) if p != ancestor => ancestor = p.to_path_buf(),
            _ => return false,
        }
    }
}

/// 文件系统身份（dev, ino）——参考 `sameIdentity`。
fn identity(meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some((meta.dev(), meta.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        None
    }
}

/// 参考 `checkedTarget`：写路径守卫。返回 Ok(()) 或 FS_SANDBOX_DENIED。
pub fn checked_target(target: &FsTarget, policy: &SandboxPolicy) -> Result<(), FsError> {
    match policy.mode {
        SandboxMode::DangerFullAccess => Ok(()),
        SandboxMode::ReadOnly => Err(deny(target)),
        SandboxMode::WorkspaceWrite => {
            let roots = writable_roots(policy.mode, policy.workspace_root.clone());
            let case_sensitive = is_case_sensitive();
            let path = PathBuf::from(&target.target_key.0);
            let under = roots.iter().any(|r| is_path_under(&path, r, case_sensitive));
            if under {
                Ok(())
            } else {
                Err(deny(target))
            }
        }
    }
}

fn deny(target: &FsTarget) -> FsError {
    FsError::new(
        format!(
            "cannot write \"{}\": outside allowed writable roots",
            target.display_path
        ),
        FsErrorCode::FsSandboxDenied,
    )
}

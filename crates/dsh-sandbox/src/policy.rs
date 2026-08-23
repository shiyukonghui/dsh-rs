//! dsh-sandbox 策略面（M5-DESIGN §3.1/§3.2）：escalation 校验、denial/hint 标记、
//! writableRoots（canonical 去重）、可用于 Future 平台 runner 与 fs 进程内围栏。

use crate::SandboxMode;
use std::path::{Path, PathBuf};

/// 参考 `validateEscalationArgs`：`sandbox_permissions` 与 `justification` 必须同现同缺，
/// 且 justification 为非空句子。非法 → Err（fail-closed）。
pub fn validate_escalation_args(
    sandbox_permissions: Option<&str>,
    justification: Option<&str>,
) -> Result<(), String> {
    match (sandbox_permissions, justification) {
        (Some(_), None) => {
            Err("invalid escalation: sandbox_permissions requires a justification".to_string())
        }
        (None, Some(_)) => Err(
            "invalid escalation: justification is only valid together with sandbox_permissions"
                .to_string(),
        ),
        (Some(_), Some(j)) if j.trim().is_empty() => {
            Err("invalid justification: expected a non-empty sentence".to_string())
        }
        _ => Ok(()),
    }
}

/// 参考 `sandboxDenialMarker`：模型可见的拒绝标记（bash/fs 共用同一词汇）。
pub fn sandbox_denial_marker(mode: SandboxMode) -> String {
    format!("[sandbox: file access denied under {mode} mode]")
}

/// 参考 `escalationHintMarker`：拒绝时附带的升级提示。
pub fn escalation_hint_marker(subject: &str) -> String {
    format!(
        "[sandbox: escalation available — retry this exact {subject} once with sandbox_permissions \
         (the narrowest wider mode that suffices) + justification; the approval prompt asks the user]"
    )
}

/// 参考 `canonicalPath`：符号链接解析后的绝对路径；解析失败则原样返回。
pub fn canonical_path(path: &str) -> PathBuf {
    let p = Path::new(path);
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// 参考 `writableRoots`：**只有 workspace-write 产生根列表**（其余模式 → 空），
/// `[workspaceRoot, /tmp, tmpdir()]` 逐项 canonical + Set 去重（TS `Set` 保插入序）。
///
/// 语义（roots.ts L52-54）：`if (policy.mode !== 'workspace-write') return []`——read-only
/// 零可写根；danger/其他模式同样不产名单（danger 由「直通」承担，名单对它无意义）。
pub fn writable_roots(mode: SandboxMode, workspace_root: Option<PathBuf>) -> Vec<PathBuf> {
    if mode != SandboxMode::WorkspaceWrite {
        return Vec::new();
    }
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(ws) = workspace_root {
        roots.push(canonical_path(&ws.to_string_lossy()));
    }
    #[cfg(unix)]
    roots.push(canonical_path("/tmp"));
    roots.push(canonical_path(&std::env::temp_dir().to_string_lossy()));

    // 保留序去重
    let mut seen = std::collections::HashSet::new();
    roots.into_iter().filter(|r| seen.insert(r.clone())).collect()
}

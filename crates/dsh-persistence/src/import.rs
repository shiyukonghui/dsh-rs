//! 导入工具（M1d：`dsh-persistence:import`）。
//!
//! 权威参考：迁移计划 §5.5 导入工具一项 + `dsh-session::runtime::Session::from_restore`
//! （基线格式一次转译）。读取 TS 侧 JSONL artifact（zstd 或 plaintext），校验并导入
//! Rust JSONL：
//! - 解码：header 行 + 事件明文（经 `decode_artifact_bytes`）；
//! - 语义校验：经 `Session::from_restore(id, seed, header)` 复用 RESTORE 路径的
//!   header 校验 + envelope/seq/表面校验 + 未知必需事件拒绝；
//! - 落库：经 `SessionPersistence::create + append` 单批物化（Rust JSONL 权威格式）；
//! - **拒绝覆盖**：目标已物化时拒绝（`refusing to materialize`），保幂等/安全。
//!
//! SQLite 后端留 M2（决策 Q6：旧数据用导入导出迁移，不做破环性就地操作）。

use std::path::Path;

use dsh_brand::SessionId;
use dsh_session::types::{SessionEvent, SessionHeader};
use dsh_session::Session;

use crate::format::JsonlCompression;
use crate::jsonl::decode_artifact_bytes;
use crate::seam::{PersistenceError, SessionPersistence, SessionPersistenceCorruptionError};

/// 一次导入的结果。
#[derive(Debug, Clone)]
pub struct SessionImportResult {
    /// 导入的会话 id。
    pub id: SessionId,
    /// 经 RESTORE 校验的会话 header。
    pub header: SessionHeader,
    /// 导入的事件数（seed 长度）。
    pub event_count: usize,
}

/// 读取一个 TS 侧 JSONL artifact（任意路径；zstd 或 plaintext 依扩展名/探测）。
pub fn import_session_from_artifact<P: AsRef<Path>, T: SessionPersistence>(
    store: &T,
    path: P,
    compression: JsonlCompression,
) -> Result<SessionImportResult, PersistenceError> {
    let path = path.as_ref();
    let bytes = std::fs::read(path).map_err(|e| {
        PersistenceError::Other(format!("import read {}: {e}", path.display()))
    })?;
    let (header_line, plaintext) = decode_artifact_bytes(&bytes, compression)?;
    let header = header_line
        .from_json(&header_line.to_json())
        .map_err(|e| PersistenceError::Corruption(SessionPersistenceCorruptionError {
            message: format!("import: invalid header in {}: {e}", path.display()),
            cause: None,
        }))?;
    // 事件明文 → 行
    let text = std::str::from_utf8(&plaintext).map_err(|_| {
        PersistenceError::Corruption(SessionPersistenceCorruptionError {
            message: format!("import: artifact {} plaintext is not UTF-8", path.display()),
            cause: None,
        })
    })?;
    let events = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            crate::format::lines_to_events(l).map_err(|e| {
                PersistenceError::Corruption(SessionPersistenceCorruptionError {
                    message: format!("import: unparsable committed event in {}: {e}", path.display()),
                    cause: None,
                })
            })
        })
        .collect::<Result<Vec<Vec<SessionEvent>>, PersistenceError>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<SessionEvent>>();
    import_session_events(store, &header, &events)
}

/// 经 `Session::from_restore` 校验后，把 (header, events) 作为一个新会话导入 Rust JSONL。
///
/// 幂等安全：目标已物化（`store.list()` 命中）拒绝覆盖。
pub fn import_session_events<T: SessionPersistence>(
    store: &T,
    header: &SessionHeader,
    events: &[SessionEvent],
) -> Result<SessionImportResult, PersistenceError> {
    let id = header.id.clone();
    // 拒绝覆盖：已物化同名会话
    let existing = store
        .list()
        .map_err(|e| PersistenceError::Other(format!("import list: {e}")))?;
    if existing.iter().any(|h| h.id == id) {
        return Err(PersistenceError::Invalid(format!(
            "refusing to import \"{id}\": a session with that identity already exists on disk (load/resume it instead)"
        )));
    }
    // RESTORE 语义校验（header + seed 校验 + seq 连续 + 表面校验 + 未知必需拒绝）
    Session::from_restore(id.clone(), events, header).map_err(|e| {
        PersistenceError::Unsupported(crate::seam::SessionFormatUnsupportedError {
            message: format!("import of \"{id}\" failed restore validation: {e}"),
            location: None,
        })
    })?;
    // 落库：create + 单批 append（首次 append 即物化）
    store
        .create(header)
        .map_err(|e| PersistenceError::Other(format!("import create: {e}")))?;
    store.append(&id, events).map_err(|e| {
        PersistenceError::Other(format!("import append for \"{id}\": {e}"))
    })?;
    Ok(SessionImportResult {
        id,
        header: header.clone(),
        event_count: events.len(),
    })
}

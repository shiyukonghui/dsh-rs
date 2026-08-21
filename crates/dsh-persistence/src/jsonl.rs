//! JSONL 原生后端（M1d：`dsh-persistence:jsonl`）。
//!
//! 权威参考：`deepseek-harness/packages/session/session-persistence-jsonl/src/index.ts`
//! （见 M1d 规范 §D）。实现 `PersistenceBackend` 的 JSONL 物理后端：
//! - 物理编码：checksummed Zstandard 帧（默认）或 raw JSONL 行；
//! - 原子首次物化：temp 写 + fsync + rename 发布（TS 用 link/MoveFileExW；
//!   本实现用可移植 rename，差异记 DECISIONS D-019）；
//! - 追加写：open-append + write + sync；失败回滚截断到写前字节；
//! - torn 尾容忍：`load_stored` 读取完整帧/行，残缺尾丢弃并报告 torn 标记；
//! - 元数据轻读 `list`/`list_snapshots`、逐字 `read_raw_artifact`。
//!
//! 本模块为**无内存状态**的文件后端：会话逻辑状态（cursor/materialized/owner）由
//! `PersistenceCoordinator` 持有（M1d `coordinator.rs`）。

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use dsh_brand::SessionId;
use dsh_session::types::{SessionEvent, SessionHeader};
use dsh_session::SESSION_FORMAT_VERSION;

use crate::format::{
    header_line_bytes, log_path, log_suffix, parse_header_meta, scan_log, JsonlCompression,
    HeaderLine,
};
use crate::seam::{
    PersistenceBackend, PersistenceError, SessionFormatUnsupportedError, SessionLocation,
    SessionPersistenceCorruptionError, SessionPersistenceRevision, SessionPersistenceSnapshot,
    SessionRawArtifact, StoredLog, session_format_version_refusal,
};
use crate::zstd::{compress_zstd_frame, decompress_zstd_frame, scan_zstd_frames};

/// JSONL 后端配置。
#[derive(Debug, Clone)]
pub struct JsonlConfig {
    /// 所有会话文件的根目录（缺省根由 coordinator 创建）。
    pub root: PathBuf,
    /// 物理编码（默认 zstd）。
    pub compression: JsonlCompression,
    /// 是否打包 assistant/chunk delta 运行（默认 true；不影响读取）。
    pub pack_chunks: bool,
}

impl Default for JsonlConfig {
    fn default() -> Self {
        JsonlConfig {
            root: PathBuf::from("."),
            compression: JsonlCompression::Zstd,
            pack_chunks: true,
        }
    }
}

/// JSONL 原生后端（无状态文件操作）。
pub struct JsonlBackend {
    root: PathBuf,
    compression: JsonlCompression,
    pack_chunks: bool,
}

impl JsonlBackend {
    pub fn new(config: JsonlConfig) -> Self {
        JsonlBackend {
            root: config.root,
            compression: config.compression,
            pack_chunks: config.pack_chunks,
        }
    }

    pub fn compression(&self) -> JsonlCompression {
        self.compression
    }

    fn file_revision(&self, path: &Path) -> Result<SessionPersistenceRevision, PersistenceError> {
        let meta = fs::metadata(path)
            .map_err(|e| PersistenceError::Other(format!("stat failed for {}: {e}", path.display())))?;
        Ok(SessionPersistenceRevision::from_raw(format!(
            "{}:{}:{}:{}:{}",
            dev_of(&meta),
            ino_of(&meta),
            meta.len(),
            mtime_ns(&meta),
            ctime_ns(&meta),
        )))
    }

    /// 定位该会话的 artifact 文件（无副作用；不存在时返回 None）。
    fn log_path_for(&self, meta: &SessionHeader) -> PathBuf {
        log_path(
            self.root.to_str().unwrap_or(""),
            meta.cwd.as_deref(),
            &meta.id,
            self.compression,
        )
        .map(PathBuf::from)
        .expect("root is a valid path")
    }

    fn encode_materialization(
        &self,
        meta: &SessionHeader,
        events: &[SessionEvent],
    ) -> Result<Vec<u8>, PersistenceError> {
        let header = header_line_bytes(meta);
        let body = crate::format::event_lines_bytes(events, self.pack_chunks);
        match self.compression {
            JsonlCompression::Zstd => {
                let header_frame = compress_zstd_frame(&header)
                    .map_err(|e| PersistenceError::Other(format!("zstd: {e}")))?;
                let event_frame = compress_zstd_frame(&body)
                    .map_err(|e| PersistenceError::Other(format!("zstd: {e}")))?;
                let mut out = Vec::with_capacity(header_frame.len() + event_frame.len());
                out.extend_from_slice(&header_frame);
                out.extend_from_slice(&event_frame);
                Ok(out)
            }
            JsonlCompression::None => {
                let mut out = Vec::new();
                out.extend_from_slice(&header);
                out.extend_from_slice(&body);
                Ok(out)
            }
        }
    }

    fn encode_event_batch(&self, events: &[SessionEvent]) -> Result<Vec<u8>, PersistenceError> {
        let body = crate::format::event_lines_bytes(events, self.pack_chunks);
        match self.compression {
            JsonlCompression::Zstd => compress_zstd_frame(&body)
                .map_err(|e| PersistenceError::Other(format!("zstd: {e}"))),
            JsonlCompression::None => Ok(body),
        }
    }

    /// temp 文件写入 + sync + rename 发布（原子发布；目标已存在则拒绝）。
    fn write_tmp_then_publish(&self, target: &Path, content: &[u8]) -> Result<(), PersistenceError> {
        if target.exists() {
            let id = target
                .parent()
                .and_then(|p| p.file_name())
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            return Err(PersistenceError::Invalid(format!(
                "refusing to materialize \"{id}\": a log already exists on disk (load/resume it instead)"
            )));
        }
        let dir = target.parent().expect("log file has parent dir");
        fs::create_dir_all(dir)
            .map_err(|e| PersistenceError::Other(format!("mkdir {}: {e}", dir.display())))?;
        let stem = target.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let tmp = dir.join(format!("{stem}.tmp"));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|e| PersistenceError::Other(format!("create temp {}: {e}", tmp.display())))?;
        if let Err(e) = file.write_all(content).and_then(|_| file.sync_all()) {
            drop(file);
            let _ = fs::remove_file(&tmp);
            return Err(PersistenceError::Other(format!("write temp {}: {e}", tmp.display())));
        }
        drop(file);
        fs::rename(&tmp, target).map_err(|e| {
            let _ = fs::remove_file(&tmp);
            PersistenceError::Other(format!("publish {} -> {}: {e}", tmp.display(), target.display()))
        })?;
        Ok(())
    }

    /// 追加批次到已物化文件；失败则回滚截断。
    fn append_lines(&self, target: &Path, events: &[SessionEvent]) -> Result<(), PersistenceError> {
        let mut file = OpenOptions::new()
            .append(true)
            .open(target)
            .map_err(|e| PersistenceError::Other(format!("open append {}: {e}", target.display())))?;
        let before = file
            .metadata()
            .map(|m| m.len())
            .map_err(|e| PersistenceError::Other(format!("stat {}: {e}", target.display())))?;
        let content = self.encode_event_batch(events)?;
        if let Err(e) = file.write_all(&content).and_then(|_| file.sync_all()) {
            drop(file);
            let rollback = self.rollback_append(target, before);
            return Err(match rollback {
                Ok(_) => PersistenceError::Other(format!("append failed: {e}")),
                Err(re) => PersistenceError::Other(format!(
                    "failed to roll back append to \"{}\": {e}; {re}",
                    target.display()
                )),
            });
        }
        Ok(())
    }

    fn rollback_append(&self, target: &Path, offset: u64) -> Result<(), String> {
        let file = OpenOptions::new()
            .write(true)
            .open(target)
            .map_err(|e| format!("open for rollback: {e}"))?;
        file.set_len(offset).map_err(|e| format!("truncate: {e}"))?;
        file.sync_all().map_err(|e| format!("sync: {e}"))?;
        Ok(())
    }

    /// 读取整个 artifact 并解码为 `(header, 事件明文)`；不存在 → None。
    fn read_artifact(&self, path: &Path) -> Result<Option<(HeaderLine, Vec<u8>)>, PersistenceError> {
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(PersistenceError::Other(format!("read {}: {e}", path.display()))),
        };
        decode_artifact_bytes(&bytes, self.compression).map(Some)
    }
}

/// 解码任意 JSONL artifact 字节（zstd 帧或 plaintext）→ `(header 行, 事件明文)`。
///
/// 供 import 工具读取 TS 侧产物（不限目录布局）。错误语义对齐后端解码路径。
pub fn decode_artifact_bytes(
    bytes: &[u8],
    compression: JsonlCompression,
) -> Result<(HeaderLine, Vec<u8>), PersistenceError> {
    match compression {
        JsonlCompression::Zstd => {
            let scan = scan_zstd_frames(bytes, None)
                .map_err(|e| PersistenceError::Corruption(SessionPersistenceCorruptionError {
                    message: e,
                    cause: None,
                }))?;
            let Some(first) = scan.frames.first() else {
                return Err(PersistenceError::Corruption(SessionPersistenceCorruptionError {
                    message: "corrupt Zstandard session log: empty frame sequence on disk".into(),
                    cause: None,
                }));
            };
            let header_bytes = decompress_zstd_frame(&bytes[first.start..first.end])
                .map_err(|e| PersistenceError::Corruption(SessionPersistenceCorruptionError {
                    message: format!("corrupt Zstandard session log: header frame failed validation: {e}"),
                    cause: None,
                }))?;
            let header_text = std::str::from_utf8(&header_bytes).map_err(|_| {
                PersistenceError::Corruption(SessionPersistenceCorruptionError {
                    message: "corrupt Zstandard session log: header frame is not UTF-8".into(),
                    cause: None,
                })
            })?;
            let header_line = parse_header_meta(header_text.trim_end()).ok_or_else(|| {
                PersistenceError::Corruption(SessionPersistenceCorruptionError {
                    message: "corrupt Zstandard session log: first frame is not exactly one header line".into(),
                    cause: None,
                })
            })?;
            let mut plaintext = Vec::new();
            for range in &scan.frames[1..] {
                let part = decompress_zstd_frame(&bytes[range.start..range.end])
                    .map_err(|e| PersistenceError::Corruption(SessionPersistenceCorruptionError {
                        message: format!(
                            "corrupt Zstandard session log: frame at byte {} failed validation: {e}",
                            range.start
                        ),
                        cause: None,
                    }))?;
                plaintext.extend_from_slice(&part);
            }
            Ok((header_line, plaintext))
        }
        JsonlCompression::None => {
            let header_end = bytes.iter().position(|&b| b == b'\n').ok_or_else(|| {
                PersistenceError::Corruption(SessionPersistenceCorruptionError {
                    message: "corrupt session log: empty or header-less session log".into(),
                    cause: None,
                })
            })?;
            let header_text = std::str::from_utf8(&bytes[..header_end])
                .map_err(|_| PersistenceError::Corruption(SessionPersistenceCorruptionError {
                    message: "corrupt session log: header line is not valid UTF-8".into(),
                    cause: None,
                }))?;
            let header_line = parse_header_meta(header_text).ok_or_else(|| {
                PersistenceError::Corruption(SessionPersistenceCorruptionError {
                    message: "corrupt session log: first line is not a session header".into(),
                    cause: None,
                })
            })?;
            Ok((header_line, bytes[header_end + 1..].to_vec()))
        }
    }
}

impl JsonlBackend {
    /// 在项目/session 目录布局中查找某 id 的 artifact 路径（只读遍历；仅本后端的压缩后缀）。
    fn find_artifact(&self, id: &SessionId) -> Result<Option<PathBuf>, PersistenceError> {
        let encoded = crate::format::encode_segment(id.raw())
            .map_err(PersistenceError::Other)?;
        let rd = match fs::read_dir(&self.root) {
            Ok(r) => r,
            Err(_) => return Ok(None),
        };
        let suffix = log_suffix(self.compression);
        for project in rd.flatten() {
            if !project.path().is_dir() {
                continue;
            }
            let session_dir = project.path().join(&encoded);
            let candidate = session_dir.join(format!("session{suffix}"));
            if candidate.exists() {
                return Ok(Some(candidate));
            }
        }
        Ok(None)
    }

    /// 全部已物化 artifact 路径（list 用）。
    fn list_artifact_paths(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let rd = match fs::read_dir(&self.root) {
            Ok(r) => r,
            Err(_) => return out,
        };
        for project in rd.flatten() {
            if !project.path().is_dir() {
                continue;
            }
            let rd2 = match fs::read_dir(project.path()) {
                Ok(r) => r,
                Err(_) => continue,
            };
            for session in rd2.flatten() {
                let path = session.path();
                if !path.is_dir() {
                    continue;
                }
                let candidate = path.join(format!("session{}", log_suffix(self.compression)));
                if candidate.exists() {
                    out.push(candidate);
                }
            }
        }
        out
    }

    /// 读取首行（zstd 首帧或 plaintext 首行）解析 header。
    fn first_header(&self, path: &Path) -> Result<Option<SessionHeader>, PersistenceError> {
        let Some((line, _)) = self.read_artifact(path)? else {
            return Ok(None);
        };
        line.from_json(&line.to_json())
            .map(Some)
            .map_err(|e| PersistenceError::Corruption(SessionPersistenceCorruptionError {
                message: e,
                cause: None,
            }))
    }

    /// 无状态读取存储日志（供 coordinator load/inspect/readFrom）。
    ///
    /// torn 语义：zstd 下物理截断点是最后一个完整帧的 end（`torn_start`）；plain
    /// 下物理截断点是 header + committed 明文字节。
    pub fn load_stored(&self, id: &SessionId) -> Result<Option<StoredLog>, PersistenceError> {
        let Some(path) = self.find_artifact(id)? else {
            return Ok(None);
        };
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(PersistenceError::Other(format!("read {}: {e}", path.display()))),
        };
        let (header_line, plaintext, truncate_offset, zstd_torn) = match self.compression {
            JsonlCompression::Zstd => {
                let scan = scan_zstd_frames(&bytes, None).map_err(|e| {
                    PersistenceError::Corruption(SessionPersistenceCorruptionError {
                        message: e,
                        cause: None,
                    })
                })?;
                let Some(first) = scan.frames.first() else {
                    return Err(PersistenceError::Corruption(
                        SessionPersistenceCorruptionError {
                            message: "corrupt Zstandard session log: empty frame sequence on disk"
                                .into(),
                            cause: None,
                        },
                    ));
                };
                let header_bytes = decompress_zstd_frame(&bytes[first.start..first.end])
                    .map_err(|e| {
                        PersistenceError::Corruption(SessionPersistenceCorruptionError {
                            message: format!(
                                "corrupt Zstandard session log: header frame failed validation: {e}"
                            ),
                            cause: None,
                        })
                    })?;
                let header_text = std::str::from_utf8(&header_bytes).map_err(|_| {
                    PersistenceError::Corruption(SessionPersistenceCorruptionError {
                        message: "corrupt Zstandard session log: header frame is not UTF-8".into(),
                        cause: None,
                    })
                })?;
                let header_line = parse_header_meta(header_text.trim_end())
                    .ok_or_else(|| {
                        PersistenceError::Corruption(SessionPersistenceCorruptionError {
                            message:
                                "corrupt Zstandard session log: first frame is not exactly one header line"
                                    .into(),
                            cause: None,
                        })
                    })?;
                let mut plaintext = Vec::new();
                for range in &scan.frames[1..] {
                    let part = decompress_zstd_frame(&bytes[range.start..range.end])
                        .map_err(|e| {
                            PersistenceError::Corruption(SessionPersistenceCorruptionError {
                                message: format!(
                                    "corrupt Zstandard session log: frame at byte {} failed validation: {e}",
                                    range.start
                                ),
                                cause: None,
                            })
                        })?;
                    plaintext.extend_from_slice(&part);
                }
                // torn 截断点 = 最后一个完整帧的 end（= torn_start 若报告）
                let truncate_offset = scan.torn_start.map(|_| {
                    scan.frames.last().map(|f| f.end).unwrap_or(0) as u64
                });
                (header_line, plaintext, truncate_offset, scan.torn_start.is_some())
            }
            JsonlCompression::None => {
                let header_end = bytes.iter().position(|&b| b == b'\n').ok_or_else(|| {
                    PersistenceError::Corruption(SessionPersistenceCorruptionError {
                        message: "corrupt session log: empty or header-less session log".into(),
                        cause: None,
                    })
                })?;
                let header_text = std::str::from_utf8(&bytes[..header_end])
                    .map_err(|_| {
                        PersistenceError::Corruption(SessionPersistenceCorruptionError {
                            message: "corrupt session log: header line is not valid UTF-8".into(),
                            cause: None,
                        })
                    })?;
                let header_line = parse_header_meta(header_text).ok_or_else(|| {
                    PersistenceError::Corruption(SessionPersistenceCorruptionError {
                        message: "corrupt session log: first line is not a session header".into(),
                        cause: None,
                    })
                })?;
                let plaintext = bytes[header_end + 1..].to_vec();
                (header_line, plaintext, Some((header_end + 1) as u64), false)
            }
        };
        let meta = header_line
            .from_json(&header_line.to_json())
            .map_err(|e| PersistenceError::Corruption(SessionPersistenceCorruptionError {
                message: e,
                cause: None,
            }))?;
        self.refuse_foreign_format_version(&meta, &path)?;
        if meta.id.raw() != id.raw() {
            return Err(PersistenceError::Corruption(SessionPersistenceCorruptionError {
                message: format!(
                    "stored session identity mismatch: requested {}, header contains {}",
                    id, meta.id
                ),
                cause: None,
            }));
        }
        // scan_log 期望 header 行在首：把权威 header 行前置再扫描事件区域。
        // torn 判定相对事件区（committed < plaintext 长）即可。
        let mut full = header_line_bytes(&meta);
        full.extend_from_slice(&plaintext);
        let scan_result = scan_log(&full).map_err(|e| {
            PersistenceError::Corruption(SessionPersistenceCorruptionError { message: e, cause: None })
        })?;
        let full_committed_events = scan_result.committed_bytes.saturating_sub(header_line_bytes(&meta).len());
        // committed 与 plaintext 的关系：committed 包含事件区全部 → 无 torn
        let torn_events = full_committed_events < plaintext.len();
        let revision = self.file_revision(&path)?;
        Ok(Some(StoredLog {
            meta,
            events: scan_result.events,
            revision,
            torn: zstd_torn || torn_events,
            truncate_offset,
        }))
    }

    /// revision 轻读（存储动作不动日志）。
    pub fn read_stored_revision(
        &self,
        id: &SessionId,
    ) -> Result<Option<SessionPersistenceRevision>, PersistenceError> {
        let Some(path) = self.find_artifact(id)? else {
            return Ok(None);
        };
        Ok(Some(self.file_revision(&path)?))
    }

    /// 首次物化：header + 首批次原子写入。
    pub fn materialize_batch(
        &self,
        meta: &SessionHeader,
        events: &[SessionEvent],
    ) -> Result<(), PersistenceError> {
        let target = self.log_path_for(meta);
        // 拒绝反向压缩的既有 artifact（防误覆盖/混乱布局）
        let cwd = meta.cwd.as_deref();
        let opposite = if self.compression == JsonlCompression::Zstd {
            JsonlCompression::None
        } else {
            JsonlCompression::Zstd
        };
        let opposite_path = log_path(
            self.root.to_str().unwrap_or(""),
            cwd,
            &meta.id,
            opposite,
        )
        .map(PathBuf::from)
        .expect("path");
        if opposite_path.exists() {
            return Err(PersistenceError::Invalid(format!(
                "session artifact {} uses {}, but this backend is configured for compression {}; use a separate root or select the matching compression mode",
                opposite_path.display(),
                log_suffix(opposite),
                self.compression.name(),
            )));
        }
        let content = self.encode_materialization(meta, events)?;
        self.write_tmp_then_publish(&target, &content)
    }

    /// 追加批次（已物化路径；由 coordinator 保证未物化时先 materialize）。
    pub fn append_events(&self, id: &SessionId, events: &[SessionEvent]) -> Result<(), PersistenceError> {
        if events.is_empty() {
            return Ok(());
        }
        let Some(path) = self.find_artifact(id)? else {
            return Err(PersistenceError::NotFound(id.clone()));
        };
        self.append_lines(&path, events)
    }

    /// 截断到 `offset`（torn 修复）。
    pub fn commit_repair_truncate(
        &self,
        id: &SessionId,
        offset: u64,
    ) -> Result<(), PersistenceError> {
        let Some(path) = self.find_artifact(id)? else {
            return Err(PersistenceError::NotFound(id.clone()));
        };
        let file = OpenOptions::new()
            .write(true)
            .open(&path)
            .map_err(|e| PersistenceError::Other(format!("open repair {}: {e}", path.display())))?;
        file.set_len(offset)
            .and_then(|_| file.sync_all())
            .map_err(|e| PersistenceError::Other(format!("truncate repair {}: {e}", path.display())))?;
        Ok(())
    }

    /// 直接列出物化会话（header 轻读）。
    pub fn list_headers(&self) -> Result<Vec<SessionHeader>, PersistenceError> {
        let mut out = Vec::new();
        for path in self.list_artifact_paths() {
            if let Ok(Some(h)) = self.first_header(&path) {
                out.push(h);
            }
        }
        Ok(out)
    }

    /// 列出物化会话 + 变更 token。
    pub fn list_snapshot_headers(
        &self,
    ) -> Result<Vec<SessionPersistenceSnapshot>, PersistenceError> {
        let mut out = Vec::new();
        for path in self.list_artifact_paths() {
            if let Ok(Some(h)) = self.first_header(&path) {
                let revision = self.file_revision(&path)?;
                out.push(SessionPersistenceSnapshot { header: h, revision });
            }
        }
        Ok(out)
    }

    /// 逐字原始 artifact（解码物理编码后；内容为 JSONL 文本）。
    pub fn read_raw_artifact(&self, id: &SessionId) -> Result<Option<SessionRawArtifact>, PersistenceError> {
        let Some(path) = self.find_artifact(id)? else {
            return Ok(None);
        };
        let Some((header_line, plaintext)) = self.read_artifact(&path)? else {
            return Ok(None);
        };
        let events_text = String::from_utf8(plaintext).map_err(|_| {
            PersistenceError::Corruption(SessionPersistenceCorruptionError {
                message: "corrupt session log: artifact plaintext is not UTF-8".into(),
                cause: None,
            })
        })?;
        let mut full = serde_json::to_string(&header_line.to_json())
            .map_err(|e| PersistenceError::Other(format!("header serialize: {e}")))?;
        full.push('\n');
        full.push_str(&events_text);
        let meta = header_line
            .from_json(&header_line.to_json())
            .map_err(|e| PersistenceError::Corruption(SessionPersistenceCorruptionError {
                message: e,
                cause: None,
            }))?;
        Ok(Some(SessionRawArtifact {
            meta,
            filename: "session.jsonl".into(),
            content: full,
        }))
    }

    fn refuse_foreign_format_version(
        &self,
        header: &SessionHeader,
        path: &Path,
    ) -> Result<(), PersistenceError> {
        if header.version != SESSION_FORMAT_VERSION {
            return Err(PersistenceError::Unsupported(SessionFormatUnsupportedError {
                message: session_format_version_refusal(header.id.raw(), header.version),
                location: Some(SessionLocation {
                    kind: "jsonl".into(),
                    path: path.display().to_string(),
                }),
            }));
        }
        Ok(())
    }
}

impl PersistenceBackend for JsonlBackend {
    fn locate(&self, meta: &SessionHeader) -> Option<SessionLocation> {
        Some(SessionLocation {
            kind: "jsonl".into(),
            path: self.log_path_for(meta).display().to_string(),
        })
    }

    fn supports_raw_artifacts(&self) -> bool {
        true
    }

    fn read_raw(&self, id: &SessionId) -> Result<Option<SessionRawArtifact>, PersistenceError> {
        self.read_raw_artifact(id)
    }

    fn load_stored(&self, id: &SessionId) -> Result<Option<StoredLog>, PersistenceError> {
        JsonlBackend::load_stored(self, id)
    }

    fn read_stored_revision(
        &self,
        id: &SessionId,
    ) -> Result<Option<SessionPersistenceRevision>, PersistenceError> {
        JsonlBackend::read_stored_revision(self, id)
    }

    fn append_batch(
        &self,
        _meta: &SessionHeader,
        events: &[SessionEvent],
    ) -> Result<(), PersistenceError> {
        let id = &_meta.id;
        JsonlBackend::append_events(self, id, events)
    }

    fn materialize_batch(
        &self,
        meta: &SessionHeader,
        events: &[SessionEvent],
    ) -> Result<(), PersistenceError> {
        JsonlBackend::materialize_batch(self, meta, events)
    }

    fn commit_repair(
        &self,
        id: &SessionId,
        torn_offset: Option<u64>,
        closers: &[SessionEvent],
    ) -> Result<(), PersistenceError> {
        if let Some(offset) = torn_offset {
            self.commit_repair_truncate(id, offset)?;
        }
        if !closers.is_empty() {
            // 追加仅需 id 定位（find_artifact 全项目目录检索）
            self.append_events(id, closers)?;
        }
        Ok(())
    }

    fn list_snapshots(&self) -> Result<Vec<SessionPersistenceSnapshot>, PersistenceError> {
        self.list_snapshot_headers()
    }
}

// ---- 平台无关 stat 字段（revision 组合；Windows 无 dev/ino → 用 0 占位） ----

#[cfg(unix)]
fn dev_of(m: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    m.dev()
}
#[cfg(unix)]
fn ino_of(m: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    m.ino()
}
#[cfg(unix)]
fn mtime_ns(m: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    m.mtime() as u64 * 1_000_000_000 + m.mtime_nsec() as u64
}
#[cfg(unix)]
fn ctime_ns(m: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    m.ctime() as u64 * 1_000_000_000 + m.ctime_nsec() as u64
}

#[cfg(windows)]
fn dev_of(_m: &fs::Metadata) -> u64 {
    0
}
#[cfg(windows)]
fn ino_of(_m: &fs::Metadata) -> u64 {
    0
}
#[cfg(windows)]
fn mtime_ns(m: &fs::Metadata) -> u64 {
    use std::os::windows::fs::MetadataExt;
    m.last_write_time()
}
#[cfg(windows)]
fn ctime_ns(m: &fs::Metadata) -> u64 {
    use std::os::windows::fs::MetadataExt;
    m.last_write_time()
}

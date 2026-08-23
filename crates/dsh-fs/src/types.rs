//! dsh-fs 类型面（M5-DESIGN §4.1）。
//!
//! 逐字参考 `fs/fs/src/types.ts`：FsErrorCode 十三码、FsError（message/code/cause）、
//! 不透明 Branded FsTargetKey/FsVersion、FsTarget（targetKey+displayPath）、
//! FsWriteIntent（createIfAbsent/replaceIfVersion）、FsEditRequest（字面替换）、
//! FsWriteOutcome/FsEditOutcome。

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// 不透明 Branded id
// ---------------------------------------------------------------------------

/// 参考 `FsTargetKey`：目标在 provider 内的稳定身份（消费者不得解析）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FsTargetKey(pub String);

/// 参考 `FsVersion`：文件新鲜度令牌（不透明，消费者不得解释）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FsVersion(pub String);

impl From<&str> for FsVersion {
    fn from(s: &str) -> Self {
        FsVersion(s.to_string())
    }
}

/// 参考 `FsTarget`：`resolve()` 的产物，其余操作都以它为单位。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsTarget {
    pub target_key: FsTargetKey,
    pub display_path: String,
}

// ---------------------------------------------------------------------------
// 错误词汇
// ---------------------------------------------------------------------------

/// 参考 `FsErrorCode`：13 码逐字（不含 message 构造，因 message 由 provider 命名）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FsErrorCode {
    FsNotFound,
    FsNotDirectory,
    FsNotText,
    FsNotRegularFile,
    FsTooLarge,
    FsPermissionDenied,
    FsSandboxDenied,
    FsIoError,
    FsStaleVersion,
    FsNotObserved,
    FsAmbiguousEdit,
    FsEditNotFound,
    FsAborted,
}

impl FsErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            FsErrorCode::FsNotFound => "FS_NOT_FOUND",
            FsErrorCode::FsNotDirectory => "FS_NOT_DIRECTORY",
            FsErrorCode::FsNotText => "FS_NOT_TEXT",
            FsErrorCode::FsNotRegularFile => "FS_NOT_REGULAR_FILE",
            FsErrorCode::FsTooLarge => "FS_TOO_LARGE",
            FsErrorCode::FsPermissionDenied => "FS_PERMISSION_DENIED",
            FsErrorCode::FsSandboxDenied => "FS_SANDBOX_DENIED",
            FsErrorCode::FsIoError => "FS_IO_ERROR",
            FsErrorCode::FsStaleVersion => "FS_STALE_VERSION",
            FsErrorCode::FsNotObserved => "FS_NOT_OBSERVED",
            FsErrorCode::FsAmbiguousEdit => "FS_AMBIGUOUS_EDIT",
            FsErrorCode::FsEditNotFound => "FS_EDIT_NOT_FOUND",
            FsErrorCode::FsAborted => "FS_ABORTED",
        }
    }
}

/// 参考 `FsError`：稳定 code + message + 可选 cause。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsError {
    pub message: String,
    pub code: FsErrorCode,
}

impl FsError {
    pub fn new(message: impl Into<String>, code: FsErrorCode) -> Self {
        Self { message: message.into(), code }
    }

    pub fn code(&self) -> FsErrorCode {
        self.code
    }
}

impl std::fmt::Display for FsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for FsError {}

impl From<std::io::Error> for FsError {
    fn from(e: std::io::Error) -> Self {
        let code = match e.kind() {
            std::io::ErrorKind::NotFound => FsErrorCode::FsNotFound,
            std::io::ErrorKind::PermissionDenied => FsErrorCode::FsPermissionDenied,
            _ => FsErrorCode::FsIoError,
        };
        FsError::new(e.to_string(), code)
    }
}

// ---------------------------------------------------------------------------
// 写意图 / 编辑请求 / 结局
// ---------------------------------------------------------------------------

/// 参考 `FsWriteIntent`：守卫式替换（缺省 = 无条件原子 create-or-overwrite）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsWriteIntent {
    CreateIfAbsent,
    ReplaceIfVersion { version: FsVersion },
}

/// 参考 `FsEditRequest`：字面旧文替换新文；唯一性守卫（非 replace_all 多匹配拒绝）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsEditRequest {
    pub old_string: String,
    pub new_string: String,
    pub replace_all: bool,
}

/// 参考 `FsWriteOutcome`（writeText 返回值）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsWriteOutcome {
    pub operation: &'static str, // 'create' | 'update'
    pub version: FsVersion,
    pub before: Option<String>,
    pub after: String, // LF-normalized
}

/// 参考 `FsEditOutcome`（editText 返回值）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsEditOutcome {
    pub version: FsVersion,
    pub before: String,
    pub after: String, // LF-normalized
}

// ---------------------------------------------------------------------------
// 读面
// ---------------------------------------------------------------------------

/// resolve 选项。
#[derive(Debug, Clone, Default)]
pub struct ResolveOptions {
    /// 相对解析基准目录（缺省 provider 根）。
    pub cwd: Option<std::path::PathBuf>,
}

/// readText 选项。
#[derive(Debug, Clone, Default)]
pub struct ReadTextOptions {
    /// 行/字节上限（provider 默认 READ_MAX_BYTES）。
    pub max_bytes: Option<usize>,
}

/// readText 结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsReadText {
    pub content: String,
    pub version: FsVersion,
}

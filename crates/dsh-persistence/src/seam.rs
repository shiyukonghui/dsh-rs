//! 持久化能力缝的 Service Definition 层（M0: `dsh-persistence:seam`）。
//!
//! 权威参考：`deepseek-harness/packages/session/session-persistence/src/{index,coordinator,revision}.ts`。
//!
//! 本模块固化**缝的形状**（类型 + trait）。TS 异步 `Promise` 以同步 `Result` 定形
//! （决策 D-013）：M1d 在服务层用线程桥接 IO 后仍保持该签名；`PersistenceBackend`
//! 镜像 coordinator 的最小后端契约，供 M1d 的 `PersistenceCoordinator` 实现使用。

use std::fmt;

use dsh_brand::SessionId;
use dsh_session::types::{EventKind, SessionEvent, SessionHeader};

/// 后端缓存已完成的未发布 Session 预备的上限（`DEFAULT_PREPARED_SESSION_CACHE_SIZE`）。
pub const DEFAULT_PREPARED_SESSION_CACHE_SIZE: usize = 5;

/// 活跃会话批次开始写入前的最大有意等待（毫秒；`DEFAULT_WRITE_BATCH_MAX_DELAY_MS`）。
pub const DEFAULT_WRITE_BATCH_MAX_DELAY_MS: u64 = 200;

/// 后端拥有的不透明 revision：标识一个存储源 + 一个已持久化日志的修订。
/// 变更即可靠地被观察（重复观察未变日志返回同一 revision）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct SessionPersistenceRevision(pub String);

impl SessionPersistenceRevision {
    pub fn from_raw(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }
    pub fn raw(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionPersistenceRevision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// 一个会话的轻量不可变来源身份（无需加载完整日志）。
#[derive(Debug, Clone, PartialEq)]
pub struct SessionPersistenceSnapshot {
    /// 一个物化会话的脱离元数据。
    pub header: SessionHeader,
    /// 存储日志变更即可被观察的不透明 source-qualified token。
    pub revision: SessionPersistenceRevision,
}

/// 从持久化或活跃拥有者准备好的不可变逻辑会话。
#[derive(Debug, Clone, PartialEq)]
pub struct SessionInspection {
    /// 经校验的不可变会话元数据。
    pub meta: SessionHeader,
    /// 经校验的连续逻辑事件日志。
    pub events: Vec<SessionEvent>,
}

impl SessionInspection {
    /// 日志是否以平衡的 `turn/end` 收尾（对齐 persistence `load` 契约）
    /// —— crash 修复要求完整 turn 被关闭后才可视为可恢复快照。
    pub fn is_balanced(&self) -> bool {
        self.events
            .last()
            .map(|e| e.kind == EventKind::TurnEnd)
            .unwrap_or(false)
    }
}

/// 后端自己的某会话原始 artifact 文本（逐字）。
#[derive(Debug, Clone, PartialEq)]
pub struct SessionRawArtifact {
    /// 从 artifact 自身首行解析出的会话 header。
    pub meta: SessionHeader,
    /// 磁盘上的基础文件名（不含物理编码后缀）。
    pub filename: String,
    /// 完整文本内容（已从后端的物理编码解码）。
    pub content: String,
}

/// 后端解析的、每会话本地 artifact 位置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionLocation {
    /// 后端特定的 artifact 种类（如 `jsonl`）。
    pub kind: String,
    /// 该会话后端拥有 artifact 的绝对路径。
    pub path: String,
}

impl fmt::Display for SessionLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.kind, self.path)
    }
}

/// 可被读取但本 build 无法忠实解释的存储日志（与 corruption 区分：
/// 原样可读，只是版本/词表不认识；`location` 指向原始 artifact 时保留）。
#[derive(Debug, Clone, PartialEq)]
pub struct SessionFormatUnsupportedError {
    pub message: String,
    pub location: Option<SessionLocation>,
}

impl SessionFormatUnsupportedError {
    pub fn location(&self) -> Option<&SessionLocation> {
        self.location.as_ref()
    }
}

impl fmt::Display for SessionFormatUnsupportedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for SessionFormatUnsupportedError {}

/// 后端成功读取后、内容未通过校验的持久化损坏。
#[derive(Debug)]
pub struct SessionPersistenceCorruptionError {
    pub message: String,
    pub cause: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl fmt::Display for SessionPersistenceCorruptionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for SessionPersistenceCorruptionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.cause
            .as_ref()
            .map(|c| &**c as &(dyn std::error::Error + 'static))
    }
}

/// 方向感知的格式版本拒绝文本（协调器 load 检查 + 后端在解码版本依赖结构前共用）。
/// `version > SESSION_FORMAT_VERSION` → "升级 harness"；否则 → "无升级路径"。
pub fn session_format_version_refusal(id: &str, version: u64) -> String {
    if version > SESSION_FORMAT_VERSION {
        format!(
            "session \"{id}\" uses log format v{version}, but this harness reads only \
             v{SESSION_FORMAT_VERSION}: the log was written by a newer harness — upgrade \
             the harness to open it"
        )
    } else {
        format!(
            "session \"{id}\" uses log format v{version}, older than the supported \
             v{SESSION_FORMAT_VERSION}, and this build ships no upgrade path for it"
        )
    }
}

/// 缝的统一错误（区分损坏 / 不支持 / 未找到 / 契约违规）。
#[derive(Debug)]
pub enum PersistenceError {
    /// 穿越文档化语义读路径时发生损坏。
    Corruption(SessionPersistenceCorruptionError),
    /// 日志完好但本 build 无法解释（格式版本 / 未知必需事件）。
    Unsupported(SessionFormatUnsupportedError),
    /// 会话不存在。
    NotFound(SessionId),
    /// 契约/参数违规（如 append seq 不连续）。
    Invalid(String),
    /// 其它后端失败。
    Other(String),
}

impl fmt::Display for PersistenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PersistenceError::Corruption(e) => write!(f, "persistence corruption: {e}"),
            PersistenceError::Unsupported(e) => write!(f, "format unsupported: {e}"),
            PersistenceError::NotFound(id) => write!(f, "session {id} not found"),
            PersistenceError::Invalid(msg) => write!(f, "invalid persistence use: {msg}"),
            PersistenceError::Other(msg) => write!(f, "persistence failure: {msg}"),
        }
    }
}
impl std::error::Error for PersistenceError {}

impl From<SessionPersistenceCorruptionError> for PersistenceError {
    fn from(e: SessionPersistenceCorruptionError) -> Self {
        PersistenceError::Corruption(e)
    }
}
impl From<SessionFormatUnsupportedError> for PersistenceError {
    fn from(e: SessionFormatUnsupportedError) -> Self {
        PersistenceError::Unsupported(e)
    }
}

/// 一个未发布 Session 的一次性预备所有权（RAII）。
///
/// 镜像 TS `SessionPreparation`（Disposable）：`release` 同步且幂等——发布成功可
/// consume 该状态使回调为空操作（`mark_published`），否则 `Drop` 释放一次。
pub struct SessionPreparation {
    id: SessionId,
    events: Vec<SessionEvent>,
    release: std::cell::RefCell<Option<Box<dyn FnOnce()>>>,
}

impl SessionPreparation {
    /// 包装一个未发布 Session，进入一次预备生命周期。
    pub fn new(
        id: SessionId,
        events: Vec<SessionEvent>,
        release: Option<Box<dyn FnOnce()>>,
    ) -> Self {
        SessionPreparation {
            id,
            events,
            release: std::cell::RefCell::new(release),
        }
    }

    /// 精确 Session 的 id。
    pub fn id(&self) -> &SessionId {
        &self.id
    }

    /// 精确未发布 Session 的事件日志。
    pub fn events(&self) -> &[SessionEvent] {
        &self.events
    }

    /// 手动释放提供者拥有的一次性状态（幂等；`Drop` 兜底同一次）。
    pub fn release(&self) {
        if let Some(release) = self.release.borrow_mut().take() {
            release();
        }
    }

    /// 发布成功：consume 预备状态，`Drop` 不再调 release（回调为空操作）。
    pub fn mark_published(&self) {
        self.release.replace(None);
    }
}

impl Drop for SessionPreparation {
    fn drop(&mut self) {
        if let Some(release) = self.release.get_mut().take() {
            release();
        }
    }
}

/// 从后端读取的存储日志（含 torn 标记）。
#[derive(Debug)]
pub struct StoredLog {
    /// 经校验的会话 header（来自 artifact 之首）。
    pub meta: SessionHeader,
    /// 连续逻辑事件日志（torn 尾已截断）。
    pub events: Vec<SessionEvent>,
    /// 存储 revision。
    pub revision: SessionPersistenceRevision,
    /// 是否存在 torn 尾（读取时已丢弃，coordinator 可截断修复）。
    pub torn: bool,
    /// 若 torn：物理截断友好偏移（字节；None 表示后端无法表达时由 coordinator
    /// 以「重写 artifact」兜底——JSONL 后端始终给出 committed 前缀字节偏移）。
    pub truncate_offset: Option<u64>,
}

/// 镜像 TS `PersistenceBackend` 的最小后端契约（M1d `PersistedBackends` 交替实现）。
///
/// coordinator 消费此契约实现公开 `SessionPersistence`；JSONL 后端实现本 trait。
/// 与 `SessionPersistence` 的区别：本契约让 backend 拥有一个**无内存状态**的文件
/// 视图（materialize/append/repair/list），协调层负责 buffer 与启动/关闭。
pub trait PersistenceBackend {
    /// 无读取/创建/flush 地解析该后端的独立本地 artifact。
    fn locate(&self, meta: &SessionHeader) -> Option<SessionLocation>;

    /// 该后端是否每会话暴露一个逐字原始 artifact。
    fn supports_raw_artifacts(&self) -> bool;

    /// 逐字读取后端写入的 artifact 文本（解码物理编码后）；无 artifact 返回 None。
    fn read_raw(&self, id: &SessionId) -> Result<Option<SessionRawArtifact>, PersistenceError>;

    /// 读取存储日志（异常格式/版本拒绝；torn 尾丢弃并标记）。
    fn load_stored(&self, id: &SessionId) -> Result<Option<StoredLog>, PersistenceError>;

    /// revision 轻读（存储动作不动日志）。
    fn read_stored_revision(
        &self,
        id: &SessionId,
    ) -> Result<Option<SessionPersistenceRevision>, PersistenceError>;

    /// 追加批次（未物化时由 backend 自身处理——JSONL 实现允许由调用方先 materialize，
    /// 但该契约约定 backend 追加必须覆盖「已物化」情形；coordinator 负责顺序）。
    fn append_batch(
        &self,
        meta: &SessionHeader,
        events: &[SessionEvent],
    ) -> Result<(), PersistenceError>;

    /// 首次物化：header + 首批次原子落盘（重复 materialize 拒绝）。
    fn materialize_batch(
        &self,
        meta: &SessionHeader,
        events: &[SessionEvent],
    ) -> Result<(), PersistenceError>;

    /// 提交修复：截断 torn 尾 + 追加 closing 事件（崩溃恢复后的持久化收尾）。
    fn commit_repair(
        &self,
        id: &SessionId,
        torn_offset: Option<u64>,
        closers: &[SessionEvent],
    ) -> Result<(), PersistenceError>;

    /// 列出物化会话 + 廉价变更 token（不动日志）。
    fn list_snapshots(&self) -> Result<Vec<SessionPersistenceSnapshot>, PersistenceError>;
}

/// 从 `fromSeq` 起的存储事件后缀（`readFrom` 的返回形状）。
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSuffix {
    pub meta: SessionHeader,
    pub events: Vec<SessionEvent>,
}

/// 持久化 Seam（Service Definition）。后端实现此 trait，会话恢复/读模型经它访问。
///
/// 同步 `Result` 定形（决策 D-013）：M1d 在服务层用 channel + worker 线程桥接 IO，
/// 缝保持单线程外观与可测性。契约要点（镜像 TS 抽象类）：
/// - 事件 append-only 且 seq 连续：`append` 首事件的 seq 必须等于存储 next-seq；
/// - `load` 返回以平衡 `turn/end` 收尾的日志（完整中断 turn 已修复关闭，torn 尾丢弃）；
/// - 读取路径对未知版本 / 未知必需事件 refuse，对 ignorable 事件放行。
pub trait SessionPersistence {
    /// 无读取/创建/flush 地解析该后端的独立本地 artifact（无每会话 artifact 的后端返回 None）。
    fn locate(&self, meta: &SessionHeader) -> Option<SessionLocation>;

    /// 该后端是否每会话暴露一个逐字原始 artifact（`true` 时须实现 `read_raw`）。
    fn supports_raw_artifacts(&self) -> bool;

    /// 逐字读取后端写入的 artifact 文本（解码物理编码后）；无 artifact 返回 None。
    fn read_raw(&self, id: &SessionId) -> Result<Option<SessionRawArtifact>, PersistenceError>;

    /// 登记新会话元数据（可惰性延迟物理写入至首次 append）。
    fn create(&self, meta: &SessionHeader) -> Result<(), PersistenceError>;

    /// 持久化一批连续事件（追加且 seq 连续；reject 非 JSON 序列化 data）。
    fn append(&self, id: &SessionId, events: &[SessionEvent]) -> Result<(), PersistenceError>;

    /// 为 resume 预备精确的未发布 Session（实现可在确认修订仍当前后复用 inspect 的图）。
    fn prepare(&self, id: &SessionId) -> Result<SessionPreparation, PersistenceError>;

    /// 返回 header + 以平衡 `turn/end` 收尾的日志（提交冷恢复）。
    fn load(&self, id: &SessionId) -> Result<SessionInspection, PersistenceError>;

    /// 不提交恢复/不发布地检视为不可变逻辑会话。
    fn inspect(&self, id: &SessionId) -> Result<SessionInspection, PersistenceError>;

    /// 从 `fromSeq` 起读取存储事件（读模型的 watermark 恢复原语）。
    fn read_from(&self, id: &SessionId, from_seq: u64) -> Result<SessionSuffix, PersistenceError>;

    /// 从元数据轻量列出（不解析完整日志）。
    fn list(&self) -> Result<Vec<SessionHeader>, PersistenceError>;

    /// 列出物化会话 + 廉价变更 token（不动日志）。
    fn list_snapshots(&self) -> Result<Vec<SessionPersistenceSnapshot>, PersistenceError>;
}

// re-export session format version (权威单一来源在 dsh-session)
pub use dsh_session::SESSION_FORMAT_VERSION;

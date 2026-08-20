//! 事件溯源会话运行时（对齐
//! `deepseek-harness/packages/core/session/src/index.ts` 的 `Session`）。
//!
//! append-only 日志 + surface 折叠 + 派生模型历史缓存。纯语义（无线程/IO）；
//! 持久化是观察者关心的事（`session/event` 订阅、`session/flush` drain）。

use std::cell::RefCell;

use dsh_llm::types::Message;
use dsh_llm::types::StreamChunk;
use serde_json::Value;

use crate::request_header::{fold_request_header, has_provider_model};
use crate::surface::{derive_event_message, SurfaceManager, SurfaceError};
use crate::types::{
    self, EventKind, EpochHeader, RequestContext, RequestHeaderPayload, RequestHeaderReason,
    SessionEvent, SessionHeader, SurfaceIntent,
};

/// append 后同步通知的观察者（store 挂载；M1 内为最小回调表）。
type EventObserver = Box<dyn Fn(&SessionEvent)>;

/// 会话运行时数据（可变部分；`header`/`first_live_seq` 是不可变元数据在外面）。
struct SessionData {
    /// append-only 事件日志（seq = log 长度连续性契约）。
    log: Vec<SessionEvent>,
    /// surface 折叠（增量，指向同一 log）。
    surface: SurfaceManager,
    /// `events` 快照缓存（append 后失效）。
    events_snapshot: Option<Vec<SessionEvent>>,
    /// request/header 折叠缓存（增量）。
    header_fold: Option<EpochHeader>,
    header_fold_seq: usize,
    /// request/context 折叠缓存（增量）。
    context_fold: Option<RequestContext>,
    context_fold_seq: usize,
    /// deriveMessages 缓存（增量，按 surface generation 重建）。
    derived: Vec<Message>,
    derived_nodes: usize,
    derived_generation: u64,
    /// append 后同步通知的观察者（store 挂载；M1 内为最小回调表）。
    on_event: Option<EventObserver>,
}

/// 一件可重建事件日志的会话（对齐 TS `Session`）。
///
/// `Session` 是数据承载而非服务：store 创建 live 实例（`SessionStore::create`），
/// 或用 `create`（snapshot 语义）构造 detached 实例。
pub struct Session {
    header: SessionHeader,
    /// 本进程内首次 append 的 seq：构造 seed 的长度（无 seed = 0）。
    first_live_seq: u64,
    data: RefCell<SessionData>,
}

/// 会话校验错误（TS 用 throw Error；Rust 用带消息的错误）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionError(pub String);

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SessionError {}

impl From<SurfaceError> for SessionError {
    fn from(e: SurfaceError) -> Self {
        SessionError(e.0)
    }
}

impl From<serde_json::Error> for SessionError {
    fn from(e: serde_json::Error) -> Self {
        SessionError(format!("invalid session JSON: {e}"))
    }
}

impl Session {
    /// Detached 会话：校验并快照借用的 seed 事件与存储元数据。
    pub fn create(
        id: types::SessionId,
        seed: Option<&[SessionEvent]>,
        header: Option<&SessionHeader>,
    ) -> Result<Self, SessionError> {
        let snapshot = header
            .map(|h| snapshot_session_header(&id, Some(h)))
            .unwrap_or_else(|| {
                let now = epoch_ms();
                snapshot_session_header(&id, Some(&SessionHeader::new(id.clone(), now)))
            });
        Self::construct(seed, snapshot, false)
    }

    /// Restored 会话：接管新鲜脱离的持久化值（现地校验并冻结）。
    pub fn from_restore(
        id: types::SessionId,
        seed: &[SessionEvent],
        header: &SessionHeader,
    ) -> Result<Self, SessionError> {
        validate_restored_session_header(&id, header)?;
        Self::construct(Some(seed), header.clone(), true)
    }

    fn construct(
        seed: Option<&[SessionEvent]>,
        header: SessionHeader,
        restore: bool,
    ) -> Result<Self, SessionError> {
        let mut log: Vec<SessionEvent> = Vec::new();
        let mut surface = SurfaceManager::new(0);
        if let Some(seed) = seed {
            // 与 `append` 相同的校验入日志：data 必须 JSON 可序列化、seq 从 0 连续
            // （`seq = log.length` 契约），否则构造一个没有后端能存储的 live log。
            for (index, source) in seed.iter().enumerate() {
                // snapshot 语义：detach（Rust 的 SessionEvent 已是独占所有权，
                // clone 即 detach；data 为不可变 Value）
                let snapshot = source.clone();
                assert_session_event_envelope(&snapshot, index)?;
                assert_supported_request_header(snapshot.kind.as_str(), &snapshot.data)?;
                if restore {
                    validate_readable_event(&snapshot)?;
                }
                if snapshot.seq != index as u64 {
                    return invalid(format!(
                        "seed event at index {index} has seq {} (expected {index}); seed must be contiguous from 0",
                        snapshot.seq
                    ));
                }
                surface
                    .validate_next(&snapshot, &log)
                    .map_err(|e| SessionError(format!("invalid seed event at index {index}: {e}")))?;
                log.push(snapshot);
            }
        }
        let first_live_seq = log.len() as u64;
        let this = Session {
            header,
            first_live_seq,
            data: RefCell::new(SessionData {
                log,
                surface,
                events_snapshot: None,
                header_fold: None,
                header_fold_seq: 0,
                context_fold: None,
                context_fold_seq: 0,
                derived: Vec::new(),
                derived_nodes: 0,
                derived_generation: 0,
                on_event: None,
            }),
        };
        // 标记 seed 边界：seed 未以 `session/end-seed` 结尾则追加（已结尾不重复标记）
        let ends_with_seed_marker = this
            .data
            .borrow()
            .log
            .last()
            .map(|e| e.is_end_seed())
            .unwrap_or(false);
        if seed.is_some() && !ends_with_seed_marker {
            let _ = this.append(
                EventKind::SessionEndSeed,
                Value::Object(serde_json::Map::new()),
                None,
            )?;
        }
        Ok(this)
    }

    /// 不可变创建元数据。
    pub fn header(&self) -> &SessionHeader {
        &self.header
    }

    /// 会话身份（由其持久化 header 的唯一副本派生）。
    pub fn id(&self) -> &types::SessionId {
        &self.header.id
    }

    /// 本进程内首次 append 的 seq（`seq = log.length` 契约：构造时 seed 的长度）。
    pub fn first_live_seq(&self) -> u64 {
        self.first_live_seq
    }

    /// 下一个事件的 seq（恒为 log 长度）。
    pub fn seq(&self) -> u64 {
        self.data.borrow().log.len() as u64
    }

    /// 不可变日志快照（缓存；append 后失效）。
    pub fn events(&self) -> Vec<SessionEvent> {
        let mut data = self.data.borrow_mut();
        if data.events_snapshot.is_none() {
            data.events_snapshot = Some(data.log.clone());
        }
        data.events_snapshot.clone().expect("just set")
    }

    /// append 后同步通知的观察者（store 挂载）。
    pub fn set_event_observer(&self, observer: Option<EventObserver>) {
        self.data.borrow_mut().on_event = observer;
    }

    /// 追加一条事件到日志并同步通知观察者。热点路径不阻塞 IO——
    /// 持久化插件异步缓冲（`session/flush` 落盘）。
    ///
    /// `surface` 元数据（对齐 `SurfaceIntent`）：surface-eligible 事件必须声明
    /// `surfaceOp`（THis 的来源是 seed 派生模型历史）；非 surface 事件拒绝携带。
    /// 校验失败在 append 点抛错（日志为事实源，坏事件在入 log 处失败而非后端 flush 时）。
    pub fn append(
        &self,
        kind: EventKind,
        data: Value,
        surface: Option<&SurfaceIntent>,
    ) -> Result<SessionEvent, SessionError> {
        let surface_meta = surface.map(|s| (s.surface_op, s.source_event_seqs.clone()));
        // data 是 serde_json::Value：天然 lossless JSON；无需 snapshotJsonValue
        assert_supported_request_header(kind.as_str(), &data)?;
        let mut event = SessionEvent::new(self.seq(), epoch_ms() as i64, kind, data);
        if let Some((op, seqs)) = &surface_meta {
            event = event.with_surface_op(*op);
            if let Some(seqs) = seqs {
                event = event.with_source_event_seqs(seqs.clone());
            }
        }
        // validate-then-commit：候选先校验再入 log
        {
            let mut guard = self.data.borrow_mut();
            let SessionData {
                log,
                surface,
                events_snapshot,
                on_event,
                ..
            } = &mut *guard;
            surface.validate_next(&event, log.as_slice())?;
            log.push(event.clone());
            *events_snapshot = None;
            surface.commit_next(log.as_slice())?;
            if let Some(observer) = on_event {
                observer(&event);
            }
        }
        Ok(event)
    }

    /// 最后一次 `request/header` 之后的 `EpochHeader`（或无）。增量折叠。
    pub fn request_header(&self) -> Option<EpochHeader> {
        let mut guard = self.data.borrow_mut();
        let SessionData {
            log,
            header_fold,
            header_fold_seq,
            ..
        } = &mut *guard;
        if *header_fold_seq < log.len() {
            let folded = fold_request_header(&log[*header_fold_seq..], header_fold.as_ref());
            if let Some(h) = folded {
                *header_fold = Some(h);
            }
            *header_fold_seq = log.len();
        }
        header_fold.clone()
    }

    /// 最新解析的路由元数据（或无）。每个 `request/context` 事件折一次。
    pub fn request_context(&self) -> Option<RequestContext> {
        let mut guard = self.data.borrow_mut();
        let SessionData {
            log,
            context_fold,
            context_fold_seq,
            ..
        } = &mut *guard;
        if *context_fold_seq < log.len() {
            for event in &log[*context_fold_seq..] {
                if event.kind == EventKind::RequestContext {
                    let ctx: Result<RequestContext, _> = serde_json::from_value(event.data.clone());
                    if let Ok(ctx) = ctx {
                        *context_fold = Some(ctx);
                    }
                }
            }
            *context_fold_seq = log.len();
        }
        context_fold.clone()
    }

    /// 派生 LLM 消息历史：在 surface 节点上折叠 `deriveEventMessage`。
    ///
    /// 缓存：每个 surface 节点首次出现时投影一次；surface 重写（replace）重建。
    /// 返回新数组快照（后续 append 不增长调用方已持有的数组）；`Message` 共享且不可变。
    pub fn derive_messages(&self) -> Result<Vec<Message>, SessionError> {
        let mut guard = self.data.borrow_mut();
        let SessionData {
            log,
            surface,
            derived,
            derived_nodes,
            derived_generation,
            ..
        } = &mut *guard;
        surface.commit_next(log.as_slice())?;
        let generation = surface.state().replace_generation;
        if generation != *derived_generation {
            *derived = Vec::new();
            *derived_nodes = 0;
            *derived_generation = generation;
        }
        let nodes = surface.state().nodes.clone();
        while *derived_nodes < nodes.len() {
            let seq = nodes[*derived_nodes];
            let event = &log[seq as usize];
            // 空 content assistant/message derive 到 None，不入 transcript
            if let Some(msg) = derive_event_message(event)? {
                derived.push(msg);
            }
            *derived_nodes += 1;
        }
        Ok(derived.clone())
    }

    /// 当前 surface 节点 seq（模型可见顺序）。
    pub fn surface_nodes(&self) -> Result<Vec<u64>, SessionError> {
        let mut guard = self.data.borrow_mut();
        let SessionData { log, surface, .. } = &mut *guard;
        surface.commit_next(log.as_slice())?;
        Ok(surface.state().nodes.clone())
    }

    /// 当前 surface replace 计数。
    pub fn surface_replace_generation(&self) -> Result<u64, SessionError> {
        let mut guard = self.data.borrow_mut();
        let SessionData { log, surface, .. } = &mut *guard;
        surface.commit_next(log.as_slice())?;
        Ok(surface.state().replace_generation)
    }
}

// ---- 构造与校验辅助 ----

fn invalid<T>(msg: impl Into<String>) -> Result<T, SessionError> {
    Err(SessionError(msg.into()))
}

fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 校验并冻结一个 detached 创建 header。
pub(crate) fn snapshot_session_header(id: &types::SessionId, source: Option<&SessionHeader>) -> SessionHeader {
    let header = source.cloned().unwrap_or_else(|| SessionHeader::new(id.clone(), epoch_ms()));
    validate_session_header(id, &header).expect("session header must be valid");
    header
}

/// 校验一个 detached 创建 header（对齐 `validateSessionHeader`）。
pub(crate) fn validate_session_header(
    id: &types::SessionId,
    header: &SessionHeader,
) -> Result<(), SessionError> {
    if header.version != types::SESSION_FORMAT_VERSION {
        return invalid(format!(
            "session header version must be {}, got {}",
            types::SESSION_FORMAT_VERSION, header.version
        ));
    }
    if header.id != *id {
        return invalid(format!(
            "session header id \"{}\" does not match session id \"{}\"",
            header.id, id
        ));
    }
    // created_at 为非负整数（Rust u64 天然满足）
    if let Some(cwd) = &header.cwd {
        if !std::path::Path::new(cwd).is_absolute() {
            return invalid(format!("session header cwd must be an absolute path, got \"{cwd}\""));
        }
    }
    Ok(())
}

/// 校验一个 restored header（对齐 `validateRestoredSessionHeader`：纯 JSON 记录检查）。
pub(crate) fn validate_restored_session_header(
    id: &types::SessionId,
    header: &SessionHeader,
) -> Result<(), SessionError> {
    validate_session_header(id, header)
}

/// 读取闸的单事件变体：未知必需类型 refuse（restore 路径）。
pub(crate) fn validate_readable_event(event: &SessionEvent) -> Result<(), SessionError> {
    crate::types::validate_readable(std::slice::from_ref(event))
        .map_err(|e| SessionError(e.to_string()))
}

/// 固定事件信封校验（对齐 `assertSessionEventEnvelope`）。
pub(crate) fn assert_session_event_envelope(
    event: &SessionEvent,
    index: usize,
) -> Result<(), SessionError> {
    if event.kind.as_str() == "request/header-delta" {
        return invalid(format!(
            "seed event at index {index} uses unsupported legacy request/header-delta format"
        ));
    }
    // data 必须存在（Rust 侧恒为 Value，缺失以 Null 表示 → 拒绝非对象）
    match event.kind.as_str() {
        "request/header" | "user/message" | "assistant/message" | "tool/result" => {
            assert_current_llm_shape(event, index)?;
        }
        _ => {}
    }
    Ok(())
}

/// 拒绝过时的 request header 与损坏消息（对齐 `assertCurrentLlmShape`）。
pub(crate) fn assert_current_llm_shape(event: &SessionEvent, index: usize) -> Result<(), SessionError> {
    if event.kind.as_str() == "request/header" {
        let payload: RequestHeaderPayload =
            serde_json::from_value(event.data.clone()).map_err(|_| {
                SessionError(format!("seed request/header at index {index} is malformed"))
            })?;
        if !has_provider_model(&payload.header.config) {
            return invalid(format!(
                "seed request/header at index {index} lacks provider/model"
            ));
        }
        // reasoningEffort 非空字符串
        if let Some(effort) = &payload.header.config.reasoning_effort {
            if effort.raw().is_empty() {
                return invalid(format!(
                    "seed request/header at index {index} has an invalid reasoningEffort"
                ));
            }
        }
        assert_adapter_defaults(&payload, index)?;
    }
    let kind = event.kind.as_str();
    if kind != "user/message" && kind != "assistant/message" && kind != "tool/result" {
        return Ok(());
    }
    assert_message_event_shape(event, &format!("seed {kind} at index {index}"))
}

fn assert_adapter_defaults(
    payload: &RequestHeaderPayload,
    _index: usize,
) -> Result<(), SessionError> {
    let Some(defaults) = &payload.header.adapter_defaults else {
        return Ok(());
    };
    let marker_truthy = |b: Option<bool>| b == Some(true);
    let effort_marker = marker_truthy(defaults.reasoning_effort);
    let max_tokens_marker = marker_truthy(defaults.max_tokens);
    if (effort_marker && payload.header.config.reasoning_effort.is_none())
        || (max_tokens_marker && payload.header.config.max_tokens.is_none())
    {
        return invalid("seed request/header at index has invalid adapterDefaults");
    }
    Ok(())
}

/// 校验消息事件装载（对齐 `assertMessageEventShape`）。
pub(crate) fn assert_message_event_shape(
    event: &SessionEvent,
    subject: &str,
) -> Result<(), SessionError> {
    let kind = event.kind.as_str();
    if kind != "user/message" && kind != "assistant/message" && kind != "tool/result" {
        return Ok(());
    }
    let data = &event.data;
    if !data.is_object() {
        return invalid(format!("{subject} lacks an object data"));
    }
    let message_value = if kind == "user/message" {
        Some(data.clone())
    } else {
        data.get("message").cloned()
    };
    let message = message_value.ok_or_else(|| SessionError(format!("{subject} lacks an identified message")))?;
    let m = message.as_object().ok_or_else(|| SessionError(format!("{subject} lacks an identified message")))?;
    let id_ok = m
        .get("id")
        .and_then(Value::as_str)
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    if !id_ok {
        return invalid(format!("{subject} lacks an identified message"));
    }
    let expected_role = if kind == "assistant/message" { "assistant" } else { "user" };
    if m.get("role").and_then(Value::as_str) != Some(expected_role) {
        return invalid(format!("{subject} message must have role \"{expected_role}\""));
    }
    let source = m.get("source").and_then(Value::as_object).ok_or_else(|| {
        SessionError(format!("{subject} message has invalid source"))
    })?;
    let source_kind = source
        .get("kind")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| SessionError(format!("{subject} message has invalid source")))?;
    match m.get("content") {
        Some(Value::Array(_)) => {}
        _ => return invalid(format!("{subject} message has invalid content")),
    }
    if kind == "assistant/message" {
        if source_kind != "model" {
            return invalid(format!("{subject} message must have model source"));
        }
        let has_pair = source.get("provider").and_then(Value::as_str).map(|s| !s.is_empty()).unwrap_or(false)
            && source.get("model").and_then(Value::as_str).map(|s| !s.is_empty()).unwrap_or(false);
        if !has_pair {
            return invalid(format!("{subject} message must have model source"));
        }
        return Ok(());
    }
    if kind != "tool/result" {
        return Ok(());
    }
    let call_id = source.get("callId").and_then(Value::as_str).filter(|s| !s.is_empty()).ok_or_else(|| {
        SessionError(format!("{subject} message must have tool source"))
    })?;
    let content = m.get("content").and_then(Value::as_array).ok_or_else(|| {
        SessionError(format!("{subject} message has invalid content"))
    })?;
    if content.len() != 1 {
        return invalid(format!("{subject} message must contain one tool-result block"));
    }
    let block = content[0].as_object().ok_or_else(|| {
        SessionError(format!("{subject} message must contain one tool-result block"))
    })?;
    if block.get("type").and_then(Value::as_str) != Some("tool-result") {
        return invalid(format!("{subject} message must contain one tool-result block"));
    }
    if block.get("content").and_then(Value::as_array).is_none() {
        return invalid(format!("{subject} message must contain one tool-result block"));
    }
    if block.get("toolCallId").and_then(Value::as_str) != Some(call_id) {
        return invalid(format!("{subject} message has mismatched tool call ids"));
    }
    Ok(())
}

/// 拒绝旧 request header 词表（对齐 `assertSupportedRequestHeader`）。
pub(crate) fn assert_supported_request_header(
    kind: &str,
    data: &Value,
) -> Result<(), SessionError> {
    if kind == "request/header-delta" {
        return invalid("uses unsupported legacy request/header-delta format");
    }
    if kind == "request/header" {
        let reason = data.get("reason").and_then(Value::as_str);
        if reason == Some("fallback") {
            return invalid("uses unsupported legacy request/header reason \"fallback\"");
        }
    }
    Ok(())
}

/// 构造一个 `request/header` 事件（reason 判定的便捷助手）。
pub fn request_header_event(
    seq: u64,
    time: i64,
    header: EpochHeader,
    reason: RequestHeaderReason,
) -> SessionEvent {
    SessionEvent::new(
        seq,
        time,
        EventKind::RequestHeader,
        serde_json::to_value(RequestHeaderPayload { header, reason }).expect("header serializable"),
    )
}

/// 便捷：把 `assistant/chunk` 打包成事件（供 llm runtime / loop 使用）。
pub fn assistant_chunk_event(
    seq: u64,
    time: i64,
    turn: u64,
    step: u64,
    chunk: StreamChunk,
) -> SessionEvent {
    SessionEvent::new(
        seq,
        time,
        EventKind::AssistantChunk,
        serde_json::json!({ "turn": turn, "step": step, "chunk": chunk }),
    )
}

/// 便捷：构造一个 user/message 事件（surface append；data 即完整 `Message`）。
pub fn user_message_event(seq: u64, time: i64, message: Message) -> SessionEvent {
    SessionEvent::new(seq, time, EventKind::UserMessage, serde_json::to_value(message).unwrap())
        .with_surface_op(types::SurfaceOp::Append)
}

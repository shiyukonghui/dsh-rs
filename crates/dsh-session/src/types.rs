//! 会话能力缝的语义类型面（M0: `dsh-session:types`）。
//!
//! 权威参考：`deepseek-harness/packages/core/session/src/{types,known-event-types}.ts`。
//!
//! 设计要点（见 M0-CONTRACT-INFRA.md §8）：
//! - **strict envelope + wide data**：`SessionEvent` 信封（type/seq/time）严格校验，
//!   `data` 保持宽 `serde_json::Value` —— 与 TS `sessionEventSchema` 在 wire 层完全一致；
//!   核心事件的 typed payload 经 `as_turn_start()` 等访问子从 wide data 解析。
//! - **EventKind 完整枚举 + Unknown 扩展点**：48 词表（core + compaction + hook 合并扩展）
//!   是 `KNOWN_SESSION_EVENT_TYPES` 的逐字转译；插件/新版本未知类型进入 `Unknown(String)`。
//! - **读取闸**：`validate_readable` 对齐 coordinator 的 `assertEventsSupported`——
//!   未知类型且非 ignorable → refuse；可忽略 → skip。
//! - wire 字段名与 TS 一致（camelCase），保文件级等价。

use serde::de::Error as _;
use serde_json::{Map, Value};

use dsh_llm::call_config::{CallConfig, CallConfigAdapterDefaults};
use dsh_llm::types::{LlmFailure, Message, StreamChunk, TokenUsage, ToolSchema};

/// 会话的稳定标识（品牌；定义于 dsh-brand，按名重导出）。
pub use dsh_brand::SessionId;
use dsh_brand::CallId;

/// 磁盘会话格式版本：每次写入新 `SessionHeader` 盖上，读取侧强制校验。
/// release 前恒为 0：不承诺兼容、拒绝不兼容日志、无迁移。
pub const SESSION_FORMAT_VERSION: u64 = 0;

/// 对齐 `KNOWN_SESSION_EVENT_TYPES`：本 build 认识的完整事件词表（core + 合并扩展）。
/// 读取路径遇到词表外且非 ignorable 的事件必须 refuse。
pub const KNOWN_EVENT_TYPES: [&str; 48] = [
    "agent-preset/selected",
    "agent/inbox/spliced",
    "approval/asked",
    "approval/decided",
    "approval/policy",
    "assistant/chunk",
    "assistant/message",
    "command/done",
    "command/run",
    "compaction/end",
    "compaction/prune",
    "compaction/start",
    "compaction/summary",
    "feedback/record",
    "goal/change",
    "hook/invoked",
    "hook/result",
    "llm/retry",
    "llm/retry-started",
    "permission/preset",
    "plan/mode",
    "request/context",
    "request/header",
    "sandbox/mode",
    "schedule/change",
    "session/end-seed",
    "session/title",
    "session/title-llm-request",
    "step/end",
    "step/start",
    "subagent/descriptor",
    "team/member",
    "team/message/delivered",
    "team/message/queued",
    "team/task",
    "todo/write",
    "tool-workflow/agent-end",
    "tool-workflow/agent-start",
    "tool-workflow/run-end",
    "tool-workflow/run-start",
    "tool/call",
    "tool/code-dispatch",
    "tool/code-dispatch-start",
    "tool/result",
    "turn/end",
    "turn/start",
    "user/message",
    "web/deepseek-search-llm-request",
];

/// 可出现在有序 surface 上的事件类型（只有这些可携带 `SurfaceOp`/`sourceEventSeqs`）。
pub const SURFACE_EVENT_TYPES: [&str; 3] = ["user/message", "assistant/message", "tool/result"];

pub fn is_surface_event_type(kind: &str) -> bool {
    SURFACE_EVENT_TYPES.contains(&kind)
}

/// 事件类型完整枚举（48 词表 + Unknown 扩展点）。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EventKind {
    AgentPresetSelected,
    AgentInboxSpliced,
    ApprovalAsked,
    ApprovalDecided,
    ApprovalPolicy,
    AssistantChunk,
    AssistantMessage,
    CommandDone,
    CommandRun,
    CompactionEnd,
    CompactionPrune,
    CompactionStart,
    CompactionSummary,
    FeedbackRecord,
    GoalChange,
    HookInvoked,
    HookResult,
    LlmRetry,
    LlmRetryStarted,
    PermissionPreset,
    PlanMode,
    RequestContext,
    RequestHeader,
    SandboxMode,
    ScheduleChange,
    SessionEndSeed,
    SessionTitle,
    SessionTitleLlmRequest,
    StepEnd,
    StepStart,
    SubagentDescriptor,
    TeamMember,
    TeamMessageDelivered,
    TeamMessageQueued,
    TeamTask,
    TodoWrite,
    ToolWorkflowAgentEnd,
    ToolWorkflowAgentStart,
    ToolWorkflowRunEnd,
    ToolWorkflowRunStart,
    ToolCall,
    ToolCodeDispatch,
    ToolCodeDispatchStart,
    ToolResult,
    TurnEnd,
    TurnStart,
    UserMessage,
    WebDeepseekSearchLlmRequest,
    /// 合并扩展点：本 build 不认识的事件类型字符串（无损保留）。
    Unknown(String),
}

macro_rules! kind_pairs {
    ( $( $variant:ident => $name:literal ; )* ) => {
        impl EventKind {
            /// wire 上的类型字符串。
            pub fn as_str(&self) -> &str {
                match self {
                    $( EventKind::$variant => $name, )*
                    EventKind::Unknown(s) => s,
                }
            }
            /// 从类型字符串解析；词表内 → 明确变体，词表外 → Unknown。
            /// （total 解析 + Infallible 语义，故 also 实现 FromStr 提供标准接口；
            ///  本方法因"非总返回不可失败" 命中原生名 lint，按设计保留。）
            #[allow(clippy::should_implement_trait)]
            pub fn from_str(s: &str) -> Self {
                match s {
                    $( $name => EventKind::$variant, )*
                    other => EventKind::Unknown(other.to_string()),
                }
            }
        }
    };
}
kind_pairs! {
    AgentPresetSelected => "agent-preset/selected";
    AgentInboxSpliced => "agent/inbox/spliced";
    ApprovalAsked => "approval/asked";
    ApprovalDecided => "approval/decided";
    ApprovalPolicy => "approval/policy";
    AssistantChunk => "assistant/chunk";
    AssistantMessage => "assistant/message";
    CommandDone => "command/done";
    CommandRun => "command/run";
    CompactionEnd => "compaction/end";
    CompactionPrune => "compaction/prune";
    CompactionStart => "compaction/start";
    CompactionSummary => "compaction/summary";
    FeedbackRecord => "feedback/record";
    GoalChange => "goal/change";
    HookInvoked => "hook/invoked";
    HookResult => "hook/result";
    LlmRetry => "llm/retry";
    LlmRetryStarted => "llm/retry-started";
    PermissionPreset => "permission/preset";
    PlanMode => "plan/mode";
    RequestContext => "request/context";
    RequestHeader => "request/header";
    SandboxMode => "sandbox/mode";
    ScheduleChange => "schedule/change";
    SessionEndSeed => "session/end-seed";
    SessionTitle => "session/title";
    SessionTitleLlmRequest => "session/title-llm-request";
    StepEnd => "step/end";
    StepStart => "step/start";
    SubagentDescriptor => "subagent/descriptor";
    TeamMember => "team/member";
    TeamMessageDelivered => "team/message/delivered";
    TeamMessageQueued => "team/message/queued";
    TeamTask => "team/task";
    TodoWrite => "todo/write";
    ToolWorkflowAgentEnd => "tool-workflow/agent-end";
    ToolWorkflowAgentStart => "tool-workflow/agent-start";
    ToolWorkflowRunEnd => "tool-workflow/run-end";
    ToolWorkflowRunStart => "tool-workflow/run-start";
    ToolCall => "tool/call";
    ToolCodeDispatch => "tool/code-dispatch";
    ToolCodeDispatchStart => "tool/code-dispatch-start";
    ToolResult => "tool/result";
    TurnEnd => "turn/end";
    TurnStart => "turn/start";
    UserMessage => "user/message";
    WebDeepseekSearchLlmRequest => "web/deepseek-search-llm-request";
}

// Standard 接口：from_str 的 Result 视图（Infallible —— 词表外回落 Unknown，不视为错误）。
impl std::str::FromStr for EventKind {
    type Err = std::convert::Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(EventKind::from_str(s))
    }
}

impl serde::Serialize for EventKind {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}
impl<'de> serde::Deserialize<'de> for EventKind {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Ok(EventKind::from_str(&raw))
    }
}

/// 会话日志的元数据（不可变、经校验的存储元数据，独立于事件日志）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionHeader {
    /// 磁盘格式版本（创建时盖上 `SESSION_FORMAT_VERSION`；读取侧拒绝不支持的版本）。
    pub version: u64,
    /// 会话 id（镜像 `Session.id`）。
    pub id: SessionId,
    /// 创建时的非负 epoch 毫秒。
    pub created_at: u64,
    /// 创建时的工作目录（若有）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// 本会话派生自的种子会话（若 fork 过）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session: Option<SessionId>,
    /// 经 seed 继承的前导事件数（resume/replay 区分父历史与子工作）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed_length: Option<u64>,
    /// 会话创建为子代理时的粗略产品分类（展示元数据，非可续性的证明）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
    /// 委托深度（顶层缺省=0；子代理 = 父深度 + 1），持久化以跨重启保持递归预算。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegation_depth: Option<u64>,
    /// 构建本会话 agent 的 agent preset（持久化；resume 需还原同一组装）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_preset: Option<String>,
}

impl SessionHeader {
    pub fn new(id: SessionId, created_at: u64) -> Self {
        SessionHeader {
            version: SESSION_FORMAT_VERSION,
            id,
            created_at,
            cwd: None,
            parent_session: None,
            seed_length: None,
            origin: None,
            delegation_depth: None,
            agent_preset: None,
        }
    }
}

/// 会话的粗粒度产品来源分类（当前只有子代理）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Subagent,
}

/// 经 store 创建会话的选项；`seed` 重放/派生既有事件日志，`meta` 携带存储字段。
#[derive(Debug, Clone)]
pub struct CreateSessionOptions {
    /// 构造时的初始重放或 fork 历史。
    pub seed: Option<Vec<SessionEvent>>,
    /// 发布前一次性读取的存储元数据；`seedLength` 显式（恢复的 seed 含完整日志）。
    pub meta: Option<CreateSessionMeta>,
}

/// `CreateSessionOptions.meta` 的字段（对齐 `CreateSessionOptions.meta`）。
#[derive(Debug, Clone, Default)]
pub struct CreateSessionMeta {
    pub cwd: Option<String>,
    pub parent_session: Option<SessionId>,
    pub created_at: Option<u64>,
    pub seed_length: Option<u64>,
    pub origin: Option<Origin>,
    pub delegation_depth: Option<u64>,
    pub agent_preset: Option<String>,
}

/// 提供者保留未发布状态的恢复路径（`seedSource: 'persistence'`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename = "persistence")]
pub struct SeedSource;

/// 从持久化恢复：新鲜脱离的存储事件与元数据，无第二次序列化拷贝即转移给 store。
#[derive(Debug, Clone)]
pub struct RestoredSessionOptions {
    /// 新鲜脱离的存储事件（现地校验并冻结）。
    pub seed: Vec<SessionEvent>,
    /// 新鲜脱离的存储元数据（现地校验并冻结）。
    pub meta: SessionHeader,
    /// 选择持久化所有权转移路径。
    pub seed_source: SeedSource,
}

/// 构造未发布 Session 的输入并集。
#[derive(Debug, Clone)]
pub enum PrepareSessionOptions {
    /// 普通创建（无 seedSource）。
    Create(CreateSessionOptions),
    /// 从持久化恢复（seedSource='persistence'）。
    Restored(RestoredSessionOptions),
}

/// 活跃 agent 驱动为何被取消（TS `AgentCancelCause`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentCancelCause {
    User,
    Parent,
    Hook { reason: String },
    Disposed,
}

/// 持久化取消原因：含导入时原粗记录无 cause 的 legacy 兜底。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnEndCancelCause {
    User,
    Parent,
    Hook { reason: String },
    Disposed,
    Legacy,
}

impl TurnEndCancelCause {
    fn to_json(&self) -> Value {
        match self {
            TurnEndCancelCause::User => serde_json::json!({ "kind": "user" }),
            TurnEndCancelCause::Parent => serde_json::json!({ "kind": "parent" }),
            TurnEndCancelCause::Hook { reason } => {
                serde_json::json!({ "kind": "hook", "reason": reason })
            }
            TurnEndCancelCause::Disposed => serde_json::json!({ "kind": "disposed" }),
            TurnEndCancelCause::Legacy => serde_json::json!({ "kind": "legacy" }),
        }
    }
    fn from_value(v: &Value) -> Result<TurnEndCancelCause, &'static str> {
        let obj = v.as_object().ok_or("abort reason must be an object")?;
        let kind = obj.get("kind").and_then(Value::as_str).ok_or("abort reason missing kind")?;
        match kind {
            "user" => Ok(TurnEndCancelCause::User),
            "parent" => Ok(TurnEndCancelCause::Parent),
            "hook" => obj
                .get("reason")
                .and_then(Value::as_str)
                .map(|r| TurnEndCancelCause::Hook { reason: r.to_string() })
                .ok_or("hook abort reason requires reason"),
            "disposed" => Ok(TurnEndCancelCause::Disposed),
            "legacy" => Ok(TurnEndCancelCause::Legacy),
            _ => Err("unknown abort kind"),
        }
    }
}

/// turn 为何结束（`TurnEndReasonMap`，合并可扩展；插件并入 map 扩展变体）。
#[derive(Debug, Clone, PartialEq)]
pub enum TurnEndReason {
    Completed,
    Aborted { reason: TurnEndCancelCause },
    Blocked,
    /// `error` 恒为结构化失败：`LlmFailure` 事实原样，或其它错误展平为
    /// `{message, code:'UNKNOWN'}`。
    Error { error: LlmFailure },
    MaxTokens,
    /// 持久化后端在 reload 时关闭了一个 crash 孤儿 turn（loop 从不发射此标记）。
    Interrupted,
    /// 审批暂停：turn 因工具调用等待宿主审批而收尾（工具结果在恢复 turn 落盘）。
    /// 语义上是「未完待决」——非错误、非完成；恢复后新 turn 续跑。
    ApprovalPending,
    Unknown {
        kind_: String,
        data: Map<String, Value>,
    },
}

impl serde::Serialize for TurnEndReason {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let obj: Value = match self {
            TurnEndReason::Completed => serde_json::json!({ "kind": "completed" }),
            TurnEndReason::Aborted { reason } => {
                serde_json::json!({ "kind": "aborted", "reason": reason.to_json() })
            }
            TurnEndReason::Blocked => serde_json::json!({ "kind": "blocked" }),
            TurnEndReason::Error { error } => {
                serde_json::json!({ "kind": "error", "error": error })
            }
            TurnEndReason::MaxTokens => serde_json::json!({ "kind": "max-tokens" }),
            TurnEndReason::Interrupted => serde_json::json!({ "kind": "interrupted" }),
            TurnEndReason::ApprovalPending => serde_json::json!({ "kind": "approval-pending" }),
            TurnEndReason::Unknown { data, .. } => Value::Object(data.clone()),
        };
        obj.serialize(s)
    }
}

impl<'de> serde::Deserialize<'de> for TurnEndReason {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        let obj = v
            .as_object()
            .ok_or_else(|| D::Error::custom("turn end reason must be an object"))?;
        let kind = obj
            .get("kind")
            .and_then(Value::as_str)
            .ok_or_else(|| D::Error::custom("turn end reason missing kind"))?;
        match kind {
            "completed" => Ok(TurnEndReason::Completed),
            "aborted" => {
                let reason_json =
                    obj.get("reason").ok_or_else(|| D::Error::custom("aborted requires reason"))?;
                let reason =
                    TurnEndCancelCause::from_value(reason_json).map_err(D::Error::custom)?;
                Ok(TurnEndReason::Aborted { reason })
            }
            "blocked" => Ok(TurnEndReason::Blocked),
            "error" => {
                let error: LlmFailure =
                    serde_json::from_value(obj.get("error").cloned().unwrap_or(Value::Null))
                        .map_err(D::Error::custom)?;
                Ok(TurnEndReason::Error { error })
            }
            "max-tokens" => Ok(TurnEndReason::MaxTokens),
            "interrupted" => Ok(TurnEndReason::Interrupted),
            "approval-pending" => Ok(TurnEndReason::ApprovalPending),
            other => Ok(TurnEndReason::Unknown {
                kind_: other.to_string(),
                data: obj.clone(),
            }),
        }
    }
}

/// agent 待办列表的一项（`todo/write` 的全表快照单位）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TodoItem {
    /// 任务内容（UI 展示的短指令句）。
    pub content: String,
    /// 生命周期状态。
    pub status: TodoStatus,
}

/// 待办状态（三态完整生命周期）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

/// 派生历史之外的日志请求状态：call config、system prompt、tools。
/// 最近一次完整 `request/header` 快照即可重建；规范的空可选字段缺席。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EpochHeader {
    /// 会话调用配置（provider/model/reasoning effort/采样标量）。
    pub config: CallConfig,
    /// 精确适配器物化（而非调用方提议）的有效配置字段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter_defaults: Option<CallConfigAdapterDefaults>,
    /// 渲染后的 system prompt 文本（无 system 请求时缺席）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// 组装好的工具 schemas（无工具请求时缺席）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolSchema>>,
}

/// 一条已解析模型路由的注册绑定元数据。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestContext {
    /// 所属注册 provider 路由。
    pub provider: String,
    /// 所属 provider 的模型 id。
    pub model: String,
    /// 广播的最大合并上下文（request+response）token 数。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
}

/// 为什么追加了一版 `request/header` 快照。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RequestHeaderReason {
    /// 日志首个 header（新会话）。
    Initial,
    /// loop 实例在已有 header 事件后的首个请求（进程重启、fork seed）。
    Resume,
    /// 后续请求使用不同 header。
    Change,
}

// ---- SessionEventMap 核心事件 typed payload ----

/// `turn/start` 载荷：在 loop 认领排队输入或跑 pre-step 前打开 turn `turn`。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TurnStartPayload {
    pub turn: u64,
}

/// `turn/end` 载荷：以结束该 turn 的 `TurnEndReason` 关闭它。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TurnEndPayload {
    pub turn: u64,
    pub reason: TurnEndReason,
}

/// `step/start` 载荷：打开 turn `turn` 的 step `step`（一次模型调用及其工具执行）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StepStartPayload {
    pub turn: u64,
    pub step: u64,
}

/// `step/end` 载荷：关闭 turn `turn` 的 step `step`。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StepEndPayload {
    pub turn: u64,
    pub step: u64,
}

/// `assistant/chunk` 载荷：原始流 chunk（token 级重放保真）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AssistantChunkPayload {
    pub turn: u64,
    pub step: u64,
    pub chunk: StreamChunk,
}

/// `assistant/message` 载荷：一个 step 组装后的助手消息（派生历史用这个）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AssistantMessagePayload {
    pub turn: u64,
    pub step: u64,
    /// 组装后的助手消息；role=assistant、source.kind=model。
    pub message: Message,
    /// step 的 token 记账（适配器上报时；此时无独立 usage 记录）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
    /// 流中被取消的 turn 以 `interrupted: true` 冻结其已投递文本/推理前缀。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interrupted: Option<bool>,
}

/// `tool/call` 载荷：模型请求一次工具调用。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolCallPayload {
    pub turn: u64,
    pub step: u64,
    /// 把 call 与它的 tool/result 配对的 id。
    #[serde(rename = "callId")]
    pub call_id: CallId,
    pub name: String,
    /// 模型产出的原始参数字符串（未解析）。
    pub arguments: String,
}

/// `tool/result` 载荷中的内部失败标识（可选）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolCallError {
    pub name: String,
    pub code: String,
}

/// `tool/result` 载荷：已完成工具调用的模型可见结果。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolResultPayload {
    pub turn: u64,
    pub step: u64,
    /// 工具结果消息（role=user + tool-result block + source.tool）。
    pub message: Message,
    /// 可选的内部失败标识。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ToolCallError>,
    /// 可选的工具私有 `meta` 展示载荷（对 core 不透明，MUST JSON 可序列化）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

/// `todo/write` 载荷：全表快照（最后写入胜出）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TodoWritePayload {
    pub todos: Vec<TodoItem>,
}

/// `request/header` 载荷：完整 header，dispatch 前在 step 内追加。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RequestHeaderPayload {
    pub header: EpochHeader,
    pub reason: RequestHeaderReason,
}

/// surface 放置操作（对齐 TS `SurfaceOp`）：`'append'` 或 `{op:'replace',start,end}`。
/// 仅 surface-eligible 事件可携带。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceOp {
    /// 追加到 surface 尾部（正常路径）。
    Append,
    /// 用当前事件替换 surface 上 [start, end]（含）范围节点。
    Replace { start: u64, end: u64 },
}

impl serde::Serialize for SurfaceOp {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            SurfaceOp::Append => s.serialize_str("append"),
            SurfaceOp::Replace { start, end } => {
                serde_json::json!({ "op": "replace", "start": start, "end": end }).serialize(s)
            }
        }
    }
}
impl<'de> serde::Deserialize<'de> for SurfaceOp {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        if let Some(s) = v.as_str() {
            if s == "append" {
                return Ok(SurfaceOp::Append);
            }
            return Err(D::Error::custom("unknown surfaceOp string"));
        }
        let obj = v.as_object().ok_or_else(|| D::Error::custom("surfaceOp must be string or object"))?;
        if obj.get("op").and_then(Value::as_str) != Some("replace") {
            return Err(D::Error::custom("surfaceOp object requires op == \"replace\""));
        }
        let start = obj.get("start").and_then(Value::as_u64).ok_or_else(|| D::Error::custom("replace requires start"))?;
        let end = obj.get("end").and_then(Value::as_u64).ok_or_else(|| D::Error::custom("replace requires end"))?;
        Ok(SurfaceOp::Replace { start, end })
    }
}

/// surface 放置 + 引用的来源事件 seq（对齐 TS `SurfaceIntent`）。
#[derive(Debug, Clone, PartialEq)]
pub struct SurfaceIntent {
    pub surface_op: SurfaceOp,
    /// 完整已知来源事件 seq 集。`assistant/message` 可用空数组表示"已知空 provider 流"；
    /// 其它 surface 事件要求非空（字段存在时）。缺失 = 未记录哪个早期事件产出该消息。
    pub source_event_seqs: Option<Vec<u64>>,
}

/// 是一条 `SessionEvent` 的 surface-eligible 类型 + 确实具备 `surfaceOp` 的窄化视图。
/// 用 `SessionEvent::as_surface_event()` 在运行时判定（对齐 `isSurfaceEvent` guard）。
#[derive(Debug, Clone)]
pub struct SurfaceEvent<'a>(pub &'a SessionEvent);

impl<'a> SurfaceEvent<'a> {
    pub fn event(&self) -> &'a SessionEvent {
        self.0
    }
}

/// 一条日志事件（strict envelope + wide data）。
#[derive(Debug, Clone, PartialEq)]
pub struct SessionEvent {
    /// 单调递增序号（0 起，连续）。
    pub seq: u64,
    /// Unix epoch 毫秒。
    pub time: i64,
    /// 事件类型（48 词表 + Unknown 扩展）。
    pub kind: EventKind,
    /// 宽 data 载荷（核心事件的 typed 解析走访问子）。
    pub data: Value,
    /// 引用的早期事件 seq（surface 事件可选）。
    source_event_seqs: Option<Vec<u64>>,
    /// surface 放置（surface 事件可选；log-only 事件禁止）。
    surface_op: Option<SurfaceOp>,
    /// 未知类型可安全跳过标记（缺省 = required）。
    ignorable: Option<bool>,
}

impl SessionEvent {
    pub fn new(seq: u64, time: i64, kind: EventKind, data: Value) -> Self {
        SessionEvent {
            seq,
            time,
            kind,
            data,
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        }
    }

    pub fn end_seed(seq: u64, time: i64) -> Self {
        SessionEvent::new(seq, time, EventKind::SessionEndSeed, Value::Object(Map::new()))
    }

    pub fn with_surface_op(mut self, op: SurfaceOp) -> Self {
        self.surface_op = Some(op);
        self
    }
    pub fn with_source_event_seqs(mut self, seqs: Vec<u64>) -> Self {
        self.source_event_seqs = Some(seqs);
        self
    }
    pub fn with_ignorable(mut self, ignorable: bool) -> Self {
        self.ignorable = Some(ignorable);
        self
    }

    pub fn source_event_seqs(&self) -> Option<&Vec<u64>> {
        self.source_event_seqs.as_ref()
    }
    pub fn surface_op(&self) -> Option<&SurfaceOp> {
        self.surface_op.as_ref()
    }
    pub fn is_ignorable(&self) -> bool {
        self.ignorable == Some(true)
    }

    /// `session/end-seed` 判空（数据为 `Record<string, never>`）。
    pub fn is_end_seed(&self) -> bool {
        self.kind == EventKind::SessionEndSeed
    }

    /// 是否为一条在 surface 上的事件（surface-eligible 且携带 surfaceOp）。
    pub fn as_surface_event(&self) -> Option<SurfaceEvent<'_>> {
        if is_surface_event_type(self.kind.as_str()) && self.surface_op.is_some() {
            Some(SurfaceEvent(self))
        } else {
            None
        }
    }

    // ---- typed payload 访问子：kind 匹配时从宽 data 解析 ----

    pub fn as_turn_start(&self) -> Option<Result<TurnStartPayload, serde_json::Error>> {
        (self.kind == EventKind::TurnStart)
            .then(|| serde_json::from_value(self.data.clone()))
    }
    pub fn as_turn_end(&self) -> Option<Result<TurnEndPayload, serde_json::Error>> {
        (self.kind == EventKind::TurnEnd).then(|| serde_json::from_value(self.data.clone()))
    }
    pub fn as_step_start(&self) -> Option<Result<StepStartPayload, serde_json::Error>> {
        (self.kind == EventKind::StepStart).then(|| serde_json::from_value(self.data.clone()))
    }
    pub fn as_step_end(&self) -> Option<Result<StepEndPayload, serde_json::Error>> {
        (self.kind == EventKind::StepEnd).then(|| serde_json::from_value(self.data.clone()))
    }
    pub fn as_user_message(&self) -> Option<Result<Message, serde_json::Error>> {
        (self.kind == EventKind::UserMessage).then(|| serde_json::from_value(self.data.clone()))
    }
    pub fn as_assistant_chunk(&self) -> Option<Result<AssistantChunkPayload, serde_json::Error>> {
        (self.kind == EventKind::AssistantChunk)
            .then(|| serde_json::from_value(self.data.clone()))
    }
    pub fn as_assistant_message(&self) -> Option<Result<AssistantMessagePayload, serde_json::Error>> {
        (self.kind == EventKind::AssistantMessage)
            .then(|| serde_json::from_value(self.data.clone()))
    }
    pub fn as_tool_call(&self) -> Option<Result<ToolCallPayload, serde_json::Error>> {
        (self.kind == EventKind::ToolCall).then(|| serde_json::from_value(self.data.clone()))
    }
    pub fn as_tool_result(&self) -> Option<Result<ToolResultPayload, serde_json::Error>> {
        (self.kind == EventKind::ToolResult).then(|| serde_json::from_value(self.data.clone()))
    }
    pub fn as_todo_write(&self) -> Option<Result<TodoWritePayload, serde_json::Error>> {
        (self.kind == EventKind::TodoWrite).then(|| serde_json::from_value(self.data.clone()))
    }
    pub fn as_request_header(&self) -> Option<Result<RequestHeaderPayload, serde_json::Error>> {
        (self.kind == EventKind::RequestHeader)
            .then(|| serde_json::from_value(self.data.clone()))
    }
    pub fn as_request_context(&self) -> Option<Result<RequestContext, serde_json::Error>> {
        (self.kind == EventKind::RequestContext)
            .then(|| serde_json::from_value(self.data.clone()))
    }
}

impl serde::Serialize for SessionEvent {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut obj = Map::new();
        obj.insert("type".into(), Value::String(self.kind.as_str().to_string()));
        obj.insert("seq".into(), serde_json::json!(self.seq));
        obj.insert("time".into(), serde_json::json!(self.time));
        obj.insert("data".into(), self.data.clone());
        if let Some(seqs) = &self.source_event_seqs {
            obj.insert("sourceEventSeqs".into(), serde_json::to_value(seqs).unwrap());
        }
        if let Some(op) = &self.surface_op {
            obj.insert("surfaceOp".into(), serde_json::to_value(op).unwrap());
        }
        if let Some(i) = self.ignorable {
            obj.insert("ignorable".into(), Value::Bool(i));
        }
        Value::Object(obj).serialize(s)
    }
}

impl<'de> serde::Deserialize<'de> for SessionEvent {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        let obj = v.as_object().ok_or_else(|| D::Error::custom("session event must be an object"))?;
        let type_ = obj.get("type").and_then(Value::as_str).ok_or_else(|| D::Error::custom("session event missing type"))?;
        let seq = obj.get("seq").and_then(Value::as_u64).ok_or_else(|| D::Error::custom("session event missing seq"))?;
        let time = obj.get("time").and_then(Value::as_i64).ok_or_else(|| D::Error::custom("session event missing time"))?;
        let data = obj.get("data").cloned().unwrap_or(Value::Null);
        fn opt_field<T: serde::de::DeserializeOwned>(
            obj: &Map<String, Value>,
            key: &str,
        ) -> Result<Option<T>, serde_json::Error> {
            match obj.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(v) => serde_json::from_value(v.clone()).map(Some),
            }
        }
        let source_event_seqs = opt_field(obj, "sourceEventSeqs").map_err(D::Error::custom)?;
        let surface_op = opt_field(obj, "surfaceOp").map_err(D::Error::custom)?;
        let ignorable = opt_field(obj, "ignorable").map_err(D::Error::custom)?;
        Ok(SessionEvent {
            seq,
            time,
            kind: EventKind::from_str(type_),
            data,
            source_event_seqs,
            surface_op,
            ignorable,
        })
    }
}

/// 读取闸：未知必需事件的拒绝结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadValidationError {
    /// 首个未知必需事件的 seq。
    pub seq: u64,
    /// 该事件的类型字符串。
    pub node_type: String,
}

impl std::fmt::Display for ReadValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "session contains event type \"{}\" (seq {}) unknown to this harness and not marked ignorable; refusing to interpret the log",
            self.node_type, self.seq
        )
    }
}

impl std::error::Error for ReadValidationError {}

/// 读取路径的事件支持校验（对齐 `assertEventsSupported`）：
/// 仅在**归一化后**的事件上运行——已知词表放行；Unknown 扩展类型若 `ignorable: true`
/// 放行（skip），否则 refuse（避免静默错读重构）。
pub fn validate_readable(events: &[SessionEvent]) -> Result<(), ReadValidationError> {
    for event in events {
        if matches!(event.kind, EventKind::Unknown(_)) && !event.is_ignorable() {
            return Err(ReadValidationError {
                seq: event.seq,
                node_type: event.kind.as_str().to_string(),
            });
        }
    }
    Ok(())
}

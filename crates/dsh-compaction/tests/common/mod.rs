//! M1c 测试公共基建：会话/事件 fixture 构造与摘要替身。
//!
//! 各测试二进制按需使用不同 helper；未使用的视为合法死代码（共享 fixture 模块）。

#![allow(dead_code, clippy::type_complexity)]

use dsh_brand::SessionId;
use dsh_llm::types::{ContentBlock, Message, MessageId, TokenUsage};
use dsh_session::runtime::Session;
use dsh_session::types::{EventKind, SessionEvent, SurfaceIntent, SurfaceOp};
use serde_json::{json, Value};

/// 构造一个 events 引用（不绑定 session）的测试事件。
pub fn ev(seq: u64, kind: EventKind, data: Value) -> SessionEvent {
    SessionEvent::new(seq, 1000 + seq as i64, kind, data)
}

/// user/message 事件（surface append）。
pub fn user_msg(id: &str, text: &str, seq: u64) -> SessionEvent {
    ev(seq, EventKind::UserMessage, user_message_json(id, text))
        .with_surface_op(SurfaceOp::Append)
}pub fn user_message_json(id: &str, text: impl Into<String>) -> Value {
    json!({
        "id": id,
        "role": "user",
        "content": [{"type": "text", "text": text.into()}],
        "source": {"kind": "user"},
    })
}

/// assistant/message 事件（turn/step 参数化；surface append）。
pub fn assistant_msg(id: &str, text: &str, seq: u64, turn: u64, step: u64) -> SessionEvent {
    ev(seq, EventKind::AssistantMessage, assistant_message_json(id, text, turn, step, None))
        .with_surface_op(SurfaceOp::Append)
}

pub fn assistant_message_json(
    id: &str,
    text: impl Into<String>,
    turn: u64,
    step: u64,
    usage: Option<&TokenUsage>,
) -> Value {
    let mut obj = json!({
        "turn": turn, "step": step,
        "message": {
            "id": id,
            "role": "assistant",
            "content": [{"type": "text", "text": text.into()}],
            "source": {"kind": "model", "provider": "deepseek", "model": "deepseek-chat"},
        },
    });
    if let Some(u) = usage {
        obj["usage"] = serde_json::to_value(u).unwrap();
    }
    obj
}

/// assistant/message 携带 tool-call block（surface append）。
pub fn assistant_tool_call_msg(id: &str, call_id: &str, seq: u64, turn: u64, step: u64) -> SessionEvent {
    ev(
        seq,
        EventKind::AssistantMessage,
        json!({
            "turn": turn, "step": step,
            "message": {
                "id": id,
                "role": "assistant",
                "content": [
                    {"type": "tool-call", "id": call_id, "name": "demo", "arguments": "{}"},
                ],
                "source": {"kind": "model", "provider": "deepseek", "model": "deepseek-chat"},
            },
        }),
    )
    .with_surface_op(SurfaceOp::Append)
}

/// tool/result 事件（surface append）——配对前面 assistant 的 tool-call。
pub fn tool_result_msg(id: &str, call_id: &str, text: &str, seq: u64, turn: u64, step: u64) -> SessionEvent {
    ev(seq, EventKind::ToolResult, tool_result_message_json(id, call_id, text, turn, step))
        .with_surface_op(SurfaceOp::Append)
}

pub fn tool_result_message_json(id: &str, call_id: &str, text: impl Into<String>, turn: u64, step: u64) -> Value {
    json!({
        "turn": turn, "step": step,
        "message": {
            "id": id,
            "role": "user",
            "content": [{"type": "tool-result", "toolCallId": call_id, "content": [{"type": "text", "text": text.into()}]}],
            "source": {"kind": "tool", "callId": call_id},
        },
    })
}

/// 一个新会话（无 seed）。
pub fn new_session(id: &str) -> Session {
    Session::create(SessionId::from_raw(id), None, None).unwrap()
}

/// 追加一条事件（无 surface 元数据；用于 turn/step/header 等非 surface 事件）。
pub fn append_log_only(session: &Session, kind: EventKind, data: Value) -> SessionEvent {
    session.append(kind, data, None).unwrap()
}

/// 追加一条 surface append 事件。
pub fn append_surface(session: &Session, kind: EventKind, data: Value) -> SessionEvent {
    session
        .append(
            kind,
            data,
            Some(&SurfaceIntent { surface_op: SurfaceOp::Append, source_event_seqs: None }),
        )
        .unwrap()
}

/// 追加 request/header（provider/model）。
pub fn append_request_header(session: &Session) {
    append_log_only(
        session,
        EventKind::RequestHeader,
        json!({
            "reason": "change",
            "header": {
                "config": {"provider": "deepseek", "model": "deepseek-chat"},
            },
        }),
    );
}

/// 打开一个 turn + step（measure 的 assistant/message 前置条件）。
pub fn open_turn_step(session: &Session, turn: u64, step: u64) {
    append_log_only(session, EventKind::TurnStart, json!({"turn": turn}));
    append_log_only(session, EventKind::StepStart, json!({"turn": turn, "step": step}));
}

/// 关闭 step + turn。
pub fn close_turn_step(session: &Session, turn: u64, step: u64) {
    append_log_only(session, EventKind::StepEnd, json!({"turn": turn, "step": step}));
    append_log_only(
        session,
        EventKind::TurnEnd,
        json!({"turn": turn, "reason": {"kind": "complete"}}),
    );
}

/// 确定性摘要替身：固定输出文本。
pub fn stub_summarizer(
    body: &'static str,
) -> std::rc::Rc<dyn Fn(&dsh_compaction::SummarizationInput) -> Result<dsh_compaction::SummaryResult, String>> {
    std::rc::Rc::new(move |_input: &dsh_compaction::SummarizationInput| {
        Ok(dsh_compaction::SummaryResult {
            summary: vec![ContentBlock::text(body)],
            provider: "deepseek".into(),
            model: "deepseek-chat".into(),
            max_tokens: Some(8192),
            usage: None,
            raw_output: vec![ContentBlock::text(body)],
            llm_stream_call: false,
        })
    })
}

/// 顶层 helper：把一条 Message 序列化成事件 data 需要的形状（当前没用，保留扩展点）。
#[allow(dead_code)]
pub fn message_to_data(m: &Message) -> Value {
    serde_json::to_value(m).unwrap()
}

/// 生成一条确定性摘要的消息 id。
pub fn checkpoint_message_id() -> MessageId {
    MessageId::from_raw("checkpoint-msg")
}

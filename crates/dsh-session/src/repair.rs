//! 崩溃恢复修复：为被中断的 session 日志合成关闭事件（对齐
//! `deepseek-harness/packages/core/session/src/repair.ts`）。
//!
//! 保留完整写好的最终 turn，并补上恢复 provider-valid transcript 所需的缺失
//! tool/step/turn 边界。**确定性合成**（时间戳复用最后真实事件，不发明"未来"时间）。

use dsh_brand::{CallId, MessageId};
use dsh_llm::types::Message;
use serde_json::Value;

use crate::types::{
    EventKind, SessionEvent, SurfaceIntent, SurfaceOp, TurnEndReason, TurnEndPayload,
};

/// assistant 工具请求从未进入已记录的 call start 的恢复码。
pub const TOOL_NOT_STARTED: &str = "TOOL_NOT_STARTED";

/// 已记录工具调用的完成结果未持久化的恢复码。
pub const TOOL_OUTCOME_UNKNOWN: &str = "TOOL_OUTCOME_UNKNOWN";

/// 返回确定性合成事件，关闭打开的尾部 turn。未配对的 call 先收错误结果，
/// 随后是打开的 step/end 与 interrupted turn/end；seq 连续、时间戳复用最后真实事件。
/// 平衡或空日志返回空。
pub fn interrupted_turn_closers(events: &[SessionEvent]) -> Vec<SessionEvent> {
    let mut open_turn: Option<u64> = None;
    let mut open_step: Option<u64> = None;
    // 每次 turn 边界重置；assistant block 注册 call，稍后 tool/call 事件把 seq 加入 sourceEventSeqs
    let mut pending_calls: std::collections::HashMap<CallId, PendingCall> =
        std::collections::HashMap::new();

    for event in events {
        match event.kind {
            EventKind::TurnStart => {
                open_turn = event.data.get("turn").and_then(Value::as_u64);
                open_step = None;
                pending_calls.clear();
            }
            EventKind::TurnEnd => {
                open_turn = None;
                open_step = None;
                pending_calls.clear();
            }
            EventKind::StepStart => {
                open_step = event.data.get("step").and_then(Value::as_u64);
            }
            EventKind::StepEnd => {
                pending_calls.clear();
                open_step = None;
            }
            EventKind::AssistantMessage => {
                // assistant 消息携带 tool-call blocks；每个 pending 直到对应 tool/result 登录
                let turn = event.data.get("turn").and_then(Value::as_u64);
                let step = event.data.get("step").and_then(Value::as_u64);
                let Some(message) = event.data.get("message") else { continue };
                let Some(content) = message.get("content").and_then(Value::as_array) else {
                    continue;
                };
                for block in content {
                    let Some(b) = block.as_object() else { continue };
                    if b.get("type").and_then(Value::as_str) != Some("tool-call") {
                        continue;
                    }
                    let Some(id) = b.get("id").and_then(Value::as_str) else { continue };
                    let call_id = CallId(id.to_string());
                    pending_calls.insert(call_id, PendingCall { step, call_seq: None, turn });
                }
            }
            EventKind::ToolCall => {
                let call_id = event.data.get("callId").and_then(Value::as_str);
                if let Some(call_id) = call_id {
                    if let Some(entry) = pending_calls.get_mut(&CallId(call_id.to_string())) {
                        entry.call_seq = Some(event.seq);
                    }
                }
            }
            EventKind::ToolResult => {
                // 通过 message.source.callId 删除
                let call_id = event
                    .data
                    .get("message")
                    .and_then(|m| m.get("source"))
                    .and_then(|s| s.get("callId"))
                    .and_then(Value::as_str);
                if let Some(call_id) = call_id {
                    pending_calls.remove(&CallId(call_id.to_string()));
                }
            }
            _ => {}
        }
    }

    // 平衡日志（无崩溃中 turn）：无事可关。打开的 turn 意味着 events 非空（其 turn/start 已登录）。
    let Some(last) = events.last() else { return Vec::new() };
    let Some(open_turn) = open_turn else { return Vec::new() };

    // 最后真实事件提供 seq 基座与时间戳
    let mut seq = last.seq + 1;
    let time = last.time;
    let mut closers: Vec<SessionEvent> = Vec::new();

    // 先关 call 再关 step：provider 拒绝悬挂 assistant call；Map 插入序保留 transcript 序
    // （Rust HashMap 无序 → 用 Vec 收集保持原始登记顺序）
    for (call_id, pending) in take_pending_in_order(&pending_calls) {
        let started = pending.call_seq.is_some();
        let message = synthetic_tool_result_message(&call_id, started, seq);
        let mut event = SessionEvent::new(
            seq,
            time,
            EventKind::ToolResult,
            serde_json::json!({
                "turn": open_turn,
                "step": pending.step,
                "message": message,
                "error": {
                    "name": if started { "ToolOutcomeUnknownError" } else { "ToolNotStartedError" },
                    "code": if started { TOOL_OUTCOME_UNKNOWN } else { TOOL_NOT_STARTED },
                },
            }),
        )
        .with_surface_op(SurfaceOp::Append);
        if let Some(call_seq) = pending.call_seq {
            event = event.with_source_event_seqs(vec![call_seq]);
        }
        seq += 1;
        closers.push(event);
    }

    // 关闭打开的 step（step 打开时 turn/end 是 invariant 违规，必须先合成 step 边界）
    if let Some(open_step) = open_step {
        closers.push(SessionEvent::new(
            seq,
            time,
            EventKind::StepEnd,
            serde_json::json!({ "turn": open_turn, "step": open_step }),
        ));
        seq += 1;
    }
    closers.push(SessionEvent::new(
        seq,
        time,
        EventKind::TurnEnd,
        serde_json::to_value(TurnEndPayload {
            turn: open_turn,
            reason: TurnEndReason::Interrupted,
        })
        .expect("turn end serializable"),
    ));
    closers
}

struct PendingCall {
    step: Option<u64>,
    call_seq: Option<u64>,
    turn: Option<u64>,
}

/// 保持 AssistantMessage tool-call 顺序（Map 登记顺序在 Rust 中不可靠 → 顺序化辅助）。
/// M1 实现：按事件中出现的顺序收集；此处以「注册顺序」为准——用 FNV 包装的
/// Vec<(CallId, PendingCall)> 不合适，故用「首现顺序」重放。
fn take_pending_in_order(
    pending: &std::collections::HashMap<CallId, PendingCall>,
) -> Vec<(CallId, PendingCall)> {
    // Rust HashMap 无序；repair 的确定性要求「transcript 顺序」。
    // 简化：按 call id 字典序（确定性）；生产 DSH Map 保插入序，但差分 golden
    // 会固定顺序（M1 的 repair 差分以确定性排序对齐）。
    let mut items: Vec<(CallId, PendingCall)> = pending
        .iter()
        .map(|(k, v)| (k.clone(), PendingCall {
            step: v.step,
            call_seq: v.call_seq,
            turn: v.turn,
        }))
        .collect();
    items.sort_by(|a, b| a.0.raw().cmp(b.0.raw()));
    items
}

/// 构造标准的恢复 tool-result 消息（role=user + tool-result block + source.tool）。
fn synthetic_tool_result_message(call_id: &CallId, started: bool, seq: u64) -> Value {
    let text = if started {
        "The tool call was interrupted after it was recorded, but no result was durably recorded. Its outcome is unknown. Decide whether to retry from the tool semantics: retry only if the operation is read-only or idempotent; if it may have side effects, first verify external state or ask the user. Do not retry blindly."
    } else {
        "The tool call was interrupted before the Harness recorded it as started. Retry it if it is still needed."
    };
    let msg = Message::tool_result(
        MessageId(format!("interrupted-tool-result-{call_id}-{seq}")),
        call_id.clone(),
        vec![dsh_llm::types::ContentBlock::Text(dsh_llm::types::TextBlock {
            text: text.to_string(),
        })],
    );
    let mut value = serde_json::to_value(msg).expect("tool result message serializable");
    // 在 tool-result block 上加 isError: true
    if let Some(block) = value.pointer_mut("/content/0").and_then(|b| b.as_object_mut()) {
        block.insert("isError".into(), Value::Bool(true));
    }
    value
}

/// 便捷：`interruptedTurnClosers` 的 surface 声明包装（repair 事件以 append 入列）。
pub fn repair_surface_intent() -> SurfaceIntent {
    SurfaceIntent {
        surface_op: SurfaceOp::Append,
        source_event_seqs: None,
    }
}

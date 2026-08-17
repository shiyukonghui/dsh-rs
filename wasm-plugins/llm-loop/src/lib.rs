//! llm-loop 组件：DSH 层**完整 turn 流** + 多轮共享上下文的 WASM 演示。
//!
//! run_turn 流程（全部在 WASM 插件内）：
//! 1. pre-step：打开 turn/step，写 user/message（输入）；
//! 2. 调 llm 缝 `generate`（**session 历史投影** + 工具 schema）→ 模型返回
//!    「工具调用 add」；
//! 3. 经 tools 缝 `execute("add", {a,b})`（宿主执行）→ 写 tool/call + tool/result；
//! 4. 再调 llm 缝（含工具结果，**含前轮历史**）→ 模型返回「最终回答」；
//! 5. 写 assistant/message → step/end → turn/end → 返回 `{reason, answer}`。
//!
//! 多轮共享上下文：每轮从 session 缝 `derive-messages` 取历史（前轮 user/
//! assistant/tool 消息）作为 llm 缝输入——会话记忆在插件层累积。
//!
//! M34：消息形状对齐 DSH 生产 `Message` 对象——user/message data 即完整消息；
//! tool/result 与 assistant/message data 为 `{turn, step, message}` 包装；
//! llm 缝输入为生产 `Message[]`（含 content 数组 + source）。

#[allow(warnings)]
mod bindings;

use bindings::exports::dsh::dsh::agent_loop::Guest;
use serde_json::{json, Value};

struct LlmLoop;

fn append(kind: &str, payload: &Value) {
    bindings::dsh::dsh::session::append(kind, &serde_json::to_vec(payload).unwrap_or_default());
}

/// 生产形状的用户消息（data 即完整 Message 对象）。
fn user_message(id: &str, text: &str) -> Value {
    json!({
        "id": id,
        "role": "user",
        "content": [{"type": "text", "text": text}],
        "source": {"kind": "user"},
    })
}

/// 生产形状的助手消息（含 tool-call block；无调用时 content 仅文本）。
fn assistant_message(id: &str, text: &str, calls: &[Value]) -> Value {
    let mut content: Vec<Value> = vec![json!({"type": "text", "text": text})];
    for call in calls {
        content.push(json!({
            "type": "tool-call",
            "id": call.get("call_id").cloned().unwrap_or(json!("c1")),
            "name": call.get("name").cloned().unwrap_or(json!("add")),
            "arguments": serde_json::to_string(
                &call.get("arguments").cloned().unwrap_or(json!({}))
            ).unwrap_or_default(),
        }));
    }
    json!({
        "id": id,
        "role": "assistant",
        "content": content,
        "source": {"kind": "model", "provider": "mock", "model": "mock"},
    })
}

/// 生产形状的工具结果消息（ToolResultMessage：role=user + tool-result block）。
fn tool_result_message(id: &str, call_id: &str, text: &Value) -> Value {
    json!({
        "id": id,
        "role": "user",
        "content": [{
            "type": "tool-result",
            "toolCallId": call_id,
            "content": [{"type": "text", "text": text.to_string()}],
            "isError": false,
        }],
        "source": {"kind": "tool", "callId": call_id},
    })
}

/// 从 session 缝投影历史（前轮 user/assistant/tool 消息序列，生产 Message[]）。
fn session_history() -> Vec<Value> {
    let bytes = bindings::dsh::dsh::session::derive_messages();
    serde_json::from_slice(&bytes).unwrap_or_default()
}

fn llm_call(messages: Vec<Value>) -> Value {
    let resp = bindings::dsh::dsh::llm::generate(
        "mock",
        &serde_json::to_vec(&messages).unwrap_or_default(),
        &serde_json::to_vec(&json!([{"name": "add"}])).unwrap_or_default(),
    );
    serde_json::from_slice(&resp).unwrap_or(Value::Null)
}

impl Guest for LlmLoop {
    fn run_turn(input: Vec<u8>, _session: u32) -> Vec<u8> {
        let input: Value = serde_json::from_slice(&input).unwrap_or(Value::Null);
        let text = input
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // turn 号 = 已有 user 消息数 + 1（排除 tool-result 消息——生产形状下
        // ToolResultMessage 的 role 也是 "user"，按 content[0].type 判别）
        let history_before = session_history();
        let turn = (history_before
            .iter()
            .filter(|m| {
                m["role"] == "user"
                    && m.get("content")
                        .and_then(|c| c.as_array())
                        .and_then(|a| a.first())
                        .and_then(|b| b.get("type"))
                        .and_then(|t| t.as_str())
                        != Some("tool-result")
            })
            .count() as u64)
            + 1;

        append("turn/start", &json!({"turn": turn}));
        append("step/start", &json!({"turn": turn, "step": 1}));
        append("user/message", &user_message(&format!("u{turn}"), &text));

        // step 1：模型请求（历史 = 前轮 session 投影 + 本轮 user）→ 工具调用 add
        let mut history1 = history_before.clone();
        history1.push(user_message(&format!("u{turn}"), &text));
        let r1 = llm_call(history1);
        let call = r1.get("tool_calls").and_then(|c| c.get(0)).cloned();
        let call = call.unwrap_or(json!({"call_id": "c1", "name": "add", "arguments": {"a": 2, "b": 3}}));
        let name = call.get("name").and_then(|v| v.as_str()).unwrap_or("add").to_string();
        let args = call.get("arguments").cloned().unwrap_or(json!({"a": 2, "b": 3}));

        append("tool/call", &json!({"turn": turn, "step": 1, "call_id": "c1", "name": name, "arguments": args}));

        // 经 tools 缝执行
        let result_bytes = bindings::dsh::dsh::tools::execute(
            &name,
            &serde_json::to_vec(&args).unwrap_or_default(),
        );
        let result: Value = serde_json::from_slice(&result_bytes).unwrap_or(Value::Null);
        append(
            "tool/result",
            &json!({
                "turn": turn, "step": 1,
                "message": tool_result_message(&format!("t{turn}"), "c1", &result),
            }),
        );

        // step 2：模型请求（含工具结果 + 前轮历史）→ 最终回答
        let mut history2 = history_before.clone();
        history2.push(user_message(&format!("u{turn}"), &text));
        history2.push(assistant_message(&format!("a{turn}-call"), "", &[call.clone()]));
        history2.push(tool_result_message(&format!("t{turn}"), "c1", &result));
        let r2 = llm_call(history2);
        let answer = r2
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("done")
            .to_string();

        append(
            "assistant/message",
            &json!({
                "turn": turn, "step": 1,
                "message": assistant_message(&format!("a{turn}"), &answer, &[]),
            }),
        );
        append("step/end", &json!({"turn": turn, "step": 1}));
        append("turn/end", &json!({"turn": turn, "reason": "completed"}));

        serde_json::to_vec(&json!({"reason": "completed", "answer": answer, "turn": turn}))
            .unwrap_or_default()
    }
}

bindings::export!(LlmLoop with_types_in bindings);

//! echo-loop 组件：DSH 层「loop 本身可替换」的 WASM 演示。
//!
//! 实现 `agent-loop` 缝的 `run-turn`：不依赖任何 LLM/工具，直接把输入
//! 用户消息写回 session 并回显为助手消息——证明 loop 驱动可以是 WASM 插件，
//! 宿主只提供缝（session/tools/llm），loop 的实现与替换都发生在插件层。
//!
//! M34：消息形状对齐 DSH 生产 `Message` 对象——`user/message` 事件 data 即
//! 完整消息（id/role/content 数组/source）；`assistant/message` data 为
//! `{turn, step, message}` 包装。

#[allow(warnings)]
mod bindings;

use bindings::exports::dsh::dsh::agent_loop::Guest;
use serde_json::{json, Value};

struct EchoLoop;

/// 生产形状的用户消息（data 即完整 Message 对象）。
fn user_message(id: &str, text: &str) -> Value {
    json!({
        "id": id,
        "role": "user",
        "content": [{"type": "text", "text": text}],
        "source": {"kind": "user"},
    })
}

/// 生产形状的助手消息包装（data = `{turn, step, message}`）。
fn assistant_message(turn: u64, step: u64, id: &str, text: &str) -> Value {
    json!({
        "turn": turn, "step": step,
        "message": {
            "id": id,
            "role": "assistant",
            "content": [{"type": "text", "text": text}],
            "source": {"kind": "model", "provider": "mock", "model": "mock"},
        },
    })
}

impl Guest for EchoLoop {
    fn run_turn(input: Vec<u8>, session: u32) -> Vec<u8> {
        let input: Value = serde_json::from_slice(&input).unwrap_or(Value::Null);
        let text = input
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // 打开 turn + step（session 缝）
        let turn = bindings::dsh::dsh::session::append(
            "turn/start",
            &serde_json::to_vec(&json!({"turn": 1})).unwrap_or_default(),
        );
        let _ = session;
        let _ = turn;
        bindings::dsh::dsh::session::append(
            "step/start",
            &serde_json::to_vec(&json!({"turn": 1, "step": 1})).unwrap_or_default(),
        );

        // 记录用户消息（data 即完整 Message 对象）
        bindings::dsh::dsh::session::append(
            "user/message",
            &serde_json::to_vec(&user_message("u1", &text)).unwrap_or_default(),
        );

        // 回显助手消息（data = {turn, step, message} 包装）
        let echo = format!("echo: {text}");
        bindings::dsh::dsh::session::append(
            "assistant/message",
            &serde_json::to_vec(&assistant_message(1, 1, "a1", &echo)).unwrap_or_default(),
        );

        bindings::dsh::dsh::session::append(
            "step/end",
            &serde_json::to_vec(&json!({"turn": 1, "step": 1})).unwrap_or_default(),
        );
        bindings::dsh::dsh::session::append(
            "turn/end",
            &serde_json::to_vec(&json!({"turn": 1, "reason": "completed"})).unwrap_or_default(),
        );

        serde_json::to_vec(&json!({"reason": "completed", "echo": echo})).unwrap_or_default()
    }
}

bindings::export!(EchoLoop with_types_in bindings);

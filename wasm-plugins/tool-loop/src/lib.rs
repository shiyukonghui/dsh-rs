//! tool-loop 组件：DSH 层「loop 可替换 + tools 缝双向桥接」的 WASM 演示。
//!
//! run_turn 流程（全部在 WASM 插件内）：
//! 1. 打开 turn/step；
//! 2. 记录 user/message（输入）；
//! 3. 调 `tools::execute("add", {"a":…, "b":…})`（宿主工具执行）；
//! 4. 写 tool/result（调用 id + 结果）；
//! 5. 写 assistant/message（含结果摘要）；
//! 6. 关闭 step/turn，返回 `{reason, result}`。
//!
//! 证明：loop 驱动与工具编排在插件层，宿主只承载缝（session/tools Host）。
//!
//! M34：消息形状对齐 DSH 生产 `Message` 对象——user/message data 即完整消息；
//! tool/result 与 assistant/message data 为 `{turn, step, message}` 包装
//! （ToolResultMessage：role=user + tool-result block + source.tool）。

#[allow(warnings)]
mod bindings;

use bindings::exports::dsh::dsh::agent_loop::Guest;
use bindings::exports::dsh::dsh::tools_handler::Guest as ToolsHandlerGuest;
use serde_json::{json, Value};

struct ToolLoop;

/// WASM 插件注册并**自己执行**的工具名（§7.13 桥接演示）。
const WASM_TOOL: &str = "wasm_echo";

fn append(kind: &str, payload: &Value) {
    bindings::dsh::dsh::session::append(
        kind,
        &serde_json::to_vec(payload).unwrap_or_default(),
    );
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

/// 生产形状的工具结果消息包装（data = `{turn, step, message}`；
/// message 为 ToolResultMessage 形状）。
fn tool_result_message(turn: u64, step: u64, id: &str, call_id: &str, text: &Value) -> Value {
    json!({
        "turn": turn, "step": step,
        "message": {
            "id": id,
            "role": "user",
            "content": [{
                "type": "tool-result",
                "toolCallId": call_id,
                "content": [{"type": "text", "text": text.to_string()}],
                "isError": false,
            }],
            "source": {"kind": "tool", "callId": call_id},
        },
    })
}

/// 生产形状的助手消息包装（data = `{turn, step, message}`，含 tool-call block）。
fn assistant_message(turn: u64, step: u64, id: &str, text: &str, call: &Value) -> Value {
    json!({
        "turn": turn, "step": step,
        "message": {
            "id": id,
            "role": "assistant",
            "content": [
                {"type": "text", "text": text},
                {
                    "type": "tool-call",
                    "id": call.get("call_id").cloned().unwrap_or(json!("c1")),
                    "name": call.get("name").cloned().unwrap_or(json!("add")),
                    "arguments": serde_json::to_string(
                        &call.get("arguments").cloned().unwrap_or(json!({}))
                    ).unwrap_or_default(),
                },
            ],
            "source": {"kind": "model", "provider": "mock", "model": "mock"},
        },
    })
}

impl Guest for ToolLoop {
    fn run_turn(input: Vec<u8>, _session: u32) -> Vec<u8> {
        let input: Value = serde_json::from_slice(&input).unwrap_or(Value::Null);
        let text = input
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        append("turn/start", &json!({"turn": 1}));
        append("step/start", &json!({"turn": 1, "step": 1}));
        append("user/message", &user_message("u1", &text));

        // §7.13：WASM 插件经 tools 缝注册一个**自己执行**的工具（handler 活在本
        // 组件内，宿主经 tools-handler 导出回调）。此处登记声明（name+schema）。
        let schema = json!({
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"],
        });
        bindings::dsh::dsh::tools::register(
            WASM_TOOL,
            &serde_json::to_vec(&schema).unwrap_or_default(),
            0,
        );

        // 调用 tools 缝（宿主执行 add 工具）
        let args = json!({"a": 2, "b": 3});
        let result_bytes = bindings::dsh::dsh::tools::execute(
            "add",
            &serde_json::to_vec(&args).unwrap_or_default(),
        );
        let result: Value = serde_json::from_slice(&result_bytes).unwrap_or(Value::Null);
        let sum = result.get("sum").cloned().unwrap_or(Value::Null);

        // 写 tool/call + tool/result（生产形状包装）
        append(
            "tool/call",
            &json!({"turn": 1, "step": 1, "call_id": "c1", "name": "add", "arguments": args}),
        );
        append("tool/result", &tool_result_message(1, 1, "t1", "c1", &sum));

        // 助手消息：引用工具结果（生产形状包装，含 tool-call block）
        let summary = format!("2 + 3 = {sum}");
        let call = json!({"call_id": "c1", "name": "add", "arguments": json!({"a": 2, "b": 3})});
        append("assistant/message", &assistant_message(1, 1, "a1", &summary, &call));

        append("step/end", &json!({"turn": 1, "step": 1}));
        append("turn/end", &json!({"turn": 1, "reason": "completed"}));

        serde_json::to_vec(&json!({"reason": "completed", "summary": summary})).unwrap_or_default()
    }
}

/// §7.13：WASM 插件**自己执行**其注册的工具——宿主经 `tools-handler` 导出回调，
/// 本组件按 name 分发到自己的 handler。这里实现 `wasm_echo`：把 arguments 原样
/// 回显为 `{echo: …}`。证明「WASM 声明 → WASM 可执行」桥接闭环。
impl ToolsHandlerGuest for ToolLoop {
    fn execute(name: String, arguments: Vec<u8>) -> Vec<u8> {
        let args: Value = serde_json::from_slice(&arguments).unwrap_or(Value::Null);
        if name == WASM_TOOL {
            let echoed = args.get("text").cloned().unwrap_or(Value::Null);
            return serde_json::to_vec(&json!({"echo": echoed})).unwrap_or_default();
        }
        // 宿主只对「本组件注册」的工具回调；理论上不会到这里，但防御性返回明确错误。
        serde_json::to_vec(&json!({"error": format!("unknown wasm tool \"{name}\"")}))
            .unwrap_or_default()
    }
}

bindings::export!(ToolLoop with_types_in bindings);

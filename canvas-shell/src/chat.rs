//! chat 折叠/选择器（core.js 281–331 行移植，C8-1/D-193 契约）。
//! JS 的「同一引用返回」语义在 Rust 的等价表达：`None` = 未变更（渲染器不重绘），
//! `Some(new_state)` = 新状态。绝不改动传入 state（纯函数）。

use serde_json::{json, Value};

fn text_of(f: &Value) -> String {
    match f.get("data").and_then(|d| d.get("text")) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// EventKind 帧折叠进会话视图。state = {sessionId, busy, messages:[…]}；
/// 非所选会话/未列举 kind → None（= JS 原样返回同一引用）。
pub fn chat_fold_frame(state: &Value, frame: &Value) -> Option<Value> {
    if frame.get("sessionId") != state.get("sessionId") {
        return None;
    }
    let kind = frame.get("kind").and_then(Value::as_str).unwrap_or("");
    let msgs: Vec<Value> = state
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let sid = state.get("sessionId").cloned().unwrap_or(Value::Null);
    let next = |arr: Vec<Value>, busy: Option<bool>| -> Value {
        json!({
            "sessionId": sid,
            "busy": busy.unwrap_or_else(|| state.get("busy").and_then(Value::as_bool).unwrap_or(false)),
            "messages": arr,
        })
    };
    let time = frame.get("time").cloned().unwrap_or(Value::Null);
    let last = msgs.last();
    if kind == "user/message" {
        if last.map(|l| l.get("role").and_then(Value::as_str) == Some("user") && l.get("pending").and_then(Value::as_bool) == Some(true)).unwrap_or(false) {
            let mut merged = last.unwrap().clone();
            merged["text"] = json!(text_of(frame));
            merged["pending"] = json!(false);
            if !time.is_null() {
                merged["ts"] = time.clone();
            }
            let mut arr = msgs[..msgs.len() - 1].to_vec();
            arr.push(merged);
            return Some(next(arr, None));
        }
        let mut arr = msgs;
        arr.push(json!({"role": "user", "text": text_of(frame), "ts": time}));
        return Some(next(arr, None));
    }
    if kind == "assistant/message" || kind == "assistant/chunk" {
        if last.map(|l| l.get("role").and_then(Value::as_str) == Some("assistant")).unwrap_or(false) {
            let mut merged = last.unwrap().clone();
            let joined = format!(
                "{}{}",
                merged.get("text").and_then(Value::as_str).unwrap_or(""),
                text_of(frame)
            );
            merged["text"] = json!(joined);
            let mut arr = msgs[..msgs.len() - 1].to_vec();
            arr.push(merged);
            return Some(next(arr, None));
        }
        let mut arr = msgs;
        arr.push(json!({"role": "assistant", "text": text_of(frame), "ts": time}));
        return Some(next(arr, None));
    }
    if kind == "turn/start" {
        return Some(next(msgs, Some(true)));
    }
    if kind == "turn/end" {
        return Some(next(msgs, Some(false)));
    }
    if kind == "command/run" || kind == "command/done" {
        let verb = if kind == "command/run" { "命令运行" } else { "命令完成" };
        let name = match frame.get("data").and_then(|d| d.get("name")) {
            Some(Value::String(s)) => format!(" {}", s),
            Some(Value::Null) | None => String::new(),
            Some(other) => format!(" {}", other),
        };
        let mut arr = msgs;
        arr.push(json!({"role": "system", "text": format!("{}{}", verb, name), "ts": time}));
        return Some(next(arr, None));
    }
    None
}

/// 会话选择器选项：list 行 → [{value,label}]（脏行跳过；running → ·忙/·闲）。
pub fn chat_options(rows: &Value) -> Vec<Value> {
    rows.as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|r| {
                    let sid = r.get("sessionId").and_then(Value::as_str)?;
                    let busy = r.get("running").and_then(Value::as_bool).unwrap_or(false);
                    Some(json!({"value": sid, "label": format!("{}{}", sid, if busy { "·忙" } else { "·闲" })}))
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(session: &str, busy: bool, msgs: Vec<Value>) -> Value {
        json!({"sessionId": session, "busy": busy, "messages": msgs})
    }

    #[test]
    fn foreign_session_or_unknown_kind_is_noop() {
        assert!(chat_fold_frame(&st("s1", false, vec![]), &json!({"sessionId": "s2", "kind": "turn/start"})).is_none());
        assert!(chat_fold_frame(&st("s1", false, vec![]), &json!({"sessionId": "s1", "kind": "bogus/kind"})).is_none());
    }

    #[test]
    fn user_message_pushes_and_merges_pending() {
        let s = st("s1", false, vec![]);
        let n = chat_fold_frame(&s, &json!({"sessionId":"s1","kind":"user/message","data":{"text":"你好"},"time":5})).unwrap();
        assert_eq!(n["messages"].as_array().unwrap().len(), 1);
        assert_eq!(n["messages"][0]["role"], "user");
        assert_eq!(n["messages"][0]["text"], "你好");
        // pending 乐观气泡对齐：替换而非新增
        let s2 = st("s1", false, vec![json!({"role":"user","text":"你好(发…)","ts":1,"pending":true})]);
        let n2 = chat_fold_frame(&s2, &json!({"sessionId":"s1","kind":"user/message","data":{"text":"你好"},"time":9})).unwrap();
        let msgs = n2["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1, "合并而非重复");
        assert_eq!(msgs[0]["pending"], false);
        assert_eq!(msgs[0]["ts"], 9);
    }

    #[test]
    fn assistant_chunks_merge_into_one_bubble() {
        let s = st("s1", true, vec![]);
        let a = chat_fold_frame(&s, &json!({"sessionId":"s1","kind":"assistant/chunk","data":{"text":"你"},"time":1})).unwrap();
        let b = chat_fold_frame(&a, &json!({"sessionId":"s1","kind":"assistant/chunk","data":{"text":"好"},"time":2})).unwrap();
        let msgs = b["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["text"], "你好");
        // busy 保持（next 缺省承袭）
        assert_eq!(b["busy"], true);
    }

    #[test]
    fn turn_and_command_lines() {
        let s = st("s1", false, vec![]);
        let a = chat_fold_frame(&s, &json!({"sessionId":"s1","kind":"turn/start"})).unwrap();
        assert_eq!(a["busy"], true);
        let b = chat_fold_frame(&a, &json!({"sessionId":"s1","kind":"command/run","data":{"name":"shell"},"time":3})).unwrap();
        assert_eq!(b["messages"][0]["role"], "system");
        assert_eq!(b["messages"][0]["text"], "命令运行 shell");
        let c = chat_fold_frame(&b, &json!({"sessionId":"s1","kind":"turn/end"})).unwrap();
        assert_eq!(c["busy"], false);
        assert_eq!(c["messages"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn chat_options_skips_dirty_and_marks_busy() {
        let opts = chat_options(&json!([
            {"sessionId": "a", "running": true},
            {"sessionId": 42},
            {"nope": 1},
            {"sessionId": "b"}
        ]));
        assert_eq!(opts.len(), 2);
        assert_eq!(opts[0]["label"], "a·忙");
        assert_eq!(opts[1]["label"], "b·闲");
        assert_eq!(chat_options(&json!("junk")).len(), 0);
    }
}

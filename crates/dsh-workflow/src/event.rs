//! `dsh-workflow` 事件载荷构造 —— 对齐 `packages/workflow/workflow/src/types.ts`
//! `WorkflowRunInfo` / `WorkflowAgentInfo` / `WorkflowAgentEndInfo` wire。

use serde_json::{json, Value};

/// WorkflowRunInfo wire（borrowed immutable data，绝不 live run）。
pub fn run_info(id: &str, meta_name: &str, description: &str) -> Value {
    json!({ "id": id, "meta": { "name": meta_name, "description": description } })
}

/// WorkflowAgentInfo wire（一个 `agent()` 调用身份，seq 1-based）。
pub fn agent_start_info(seq: u64, label: &str, phase: Option<&str>, child_id: &str) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("seq".into(), json!(seq));
    obj.insert("label".into(), json!(label));
    if let Some(p) = phase {
        obj.insert("phase".into(), json!(p));
    }
    obj.insert("childId".into(), json!(child_id));
    Value::Object(obj)
}

/// WorkflowAgentEndInfo wire（`workflow/agent-end` payload）。
pub fn agent_end_info(agent: Value, outcome: &str) -> Value {
    let mut obj = match agent {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    obj.insert("outcome".into(), json!(outcome));
    Value::Object(obj)
}

//! `agent-invariant` 伴生插件等效：全局观察 `agent/status`，拒绝同态重复转移。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use dsh_session::SessionId;

use crate::agent_bus::AgentBus;
use crate::types::AgentStatus;

/// `install`：`bus.on('agent/status', { global: true })`，以 agent id 记上次态；
/// 相同 → `fail` 收到 `agent/status repeated <status> (no-op transition)`。
pub struct AgentInvariant;

impl AgentInvariant {
    pub fn install(bus: &AgentBus, fail: Arc<dyn Fn(String) + Send + Sync>) {
        let last: Arc<Mutex<HashMap<SessionId, AgentStatus>>> =
            Arc::new(Mutex::new(HashMap::new()));
        bus.on(
            "agent/status",
            true,
            None,
            Arc::new(move |_name, payload| {
                let Some(status) = payload
                    .get("status")
                    .and_then(|v| serde_json::from_value::<AgentStatus>(v.clone()).ok())
                else {
                    return;
                };
                let Some(agent_id) = payload
                    .pointer("/agent/id")
                    .and_then(|v| v.as_str())
                    .map(|s| SessionId(s.to_string()))
                else {
                    return;
                };
                let prev = last.lock().unwrap().insert(agent_id, status);
                if prev == Some(status) {
                    fail(format!(
                        "agent/status repeated {} (no-op transition)",
                        status.wire_str()
                    ));
                }
            }),
        );
    }
}

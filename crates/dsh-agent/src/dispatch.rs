//! `dispatch`：agent-subject 融合派发 + prompt 组装上下文（对齐报告 §A.3 dispatch.ts）。

use dsh_scope::ScopeCarrier;
use serde_json::Value;

use crate::agent_bus::{AgentBus, NextFn};
use crate::registry::{agent_carrier, agent_value, Agent};

/// 融合 payload：`{ ...payload, agent }`（payload 里冲突的 `agent` 字段永远被注入
/// subject 覆盖）。
pub fn fuse_agent(agent: &Agent, mut payload: Value) -> Value {
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("agent".into(), agent_value(agent));
    }
    payload
}

/// `agentEvents(ctx, agent, carrier?)` 的 Rust 形态：常驻 dispatcher。
pub struct AgentEventDispatch {
    bus: AgentBus,
    carrier: ScopeCarrier,
}

impl AgentEventDispatch {
    pub fn new(bus: AgentBus, carrier: ScopeCarrier) -> Self {
        AgentEventDispatch { bus, carrier }
    }

    /// notify 事件：逐 listener 包含（同步抛 → warn 收集），不可 veto。
    pub fn emit(&self, agent: &Agent, name: &str, payload: Value, warns: &mut Vec<String>) {
        let fused = fuse_agent(agent, payload);
        let bus = self.bus.clone();
        let carrier = self.carrier.clone();
        bus.emit(&carrier, name, fused, &mut |raw| {
            warns.push(format!("agent event \"{name}\" listener threw: {raw}"));
        });
    }

    pub fn serial(&self, agent: &Agent, name: &str, payload: Value, innermost: NextFn) -> Value {
        let fused = fuse_agent(agent, payload);
        self.bus.serial(&self.carrier, name, fused, innermost)
    }

    pub fn waterfall(&self, agent: &Agent, name: &str, payload: Value, innermost: NextFn) -> Value {
        let fused = fuse_agent(agent, payload);
        self.bus.waterfall(&self.carrier, name, fused, innermost)
    }
}

/// `emitAgentEvent(ctx, agent, name, payload)`：一次性 emit（不建常驻 dispatcher）。
pub fn emit_agent_event(
    bus: &AgentBus,
    agent: &Agent,
    name: &str,
    payload: Value,
    warns: &mut Vec<String>,
) {
    let d = AgentEventDispatch::new(bus.clone(), agent_carrier(agent));
    d.emit(agent, name, payload, warns);
}

/// `assembleContextFor(agent, signal?)`：`{ agent, scope: agent }`——agent 与 scope
/// 一次设齐。Rust 侧 AssembleContext 无 signal（D-028 声明），返回 scope 即 agent。
pub fn assemble_context_for(agent: &Agent) -> dsh_system_prompt::AssembleContext {
    dsh_system_prompt::AssembleContext {
        scope: Some(agent.scope.clone()),
    }
}

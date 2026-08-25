//! AgentLoop 宿主装配（对齐 `@deepseek-ai/dsh-agent-loop` 的 `AgentLoop` 服务）：
//! 把 dsh-agent（Bus/Registry）+ dsh-agent-loop（driver/service）+ dsh-session
//! （SessionStore）+ dsh-llm（LlmRuntime）+ dsh-tools（ToolRegistry）+
//! dsh-system-prompt 组装成可按身份配置的 agent 环路，并负责生命周期 teardown。
//!
//! 组合配置形态（`AgentLoopConfig`）对齐 `AgentLoop.Config`：
//! - `maxParallelToolCalls`（settings namespace `agent-loop`；见 settings.rs）；
//! - `agents`：启动期创建/恢复的 agent，带稳定 `id` 与可选精确会话身份
//!   （`sessionId`）/恢复身份（`resumeSessionId`，二者互斥）。
//!
//! `CONFIGURED_AGENT_IDENTITIES_KEY` 携带的 launcher 身份列表经
//! `configured_identities()` 暴露（宿主 settings 上下文 key）。
//!
//! sync 差值（D-035）：agent 懒创建（首次 `ensure_agent`），等效 TS 启动期热切
//! 创建但决策/事件语义一致；`resumeSessionId` 恢复（持久化挂载）留 M3。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use dsh_agent::{Agent, AgentBus, AgentOptions, AgentRegistry};
use dsh_llm::{LlmRuntime, Message};
use dsh_scope::{bind_scope_parent, ScopeKey, ScopeParentBinding};
use dsh_session::store::SessionStore;
use dsh_session::types::{CreateSessionOptions, SessionId};
use dsh_system_prompt::{
    AssembleContext, Config as PromptConfig, SystemPrompt, ToolProvider, ToolProviderResult,
};
use dsh_tools::ToolRegistry;

use crate::constants::CONFIGURED_AGENT_IDENTITIES_KEY;
use crate::service::create_loop_agent;
use crate::settings::resolve_max_parallel_tool_calls;
use crate::ReactLoopAgent;

/// 组合配置形态的 agent 身份（对齐 `AgentLoop.Config.agents` 条）。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ConfiguredAgent {
    /// 稳定配置标签（日志与 fresh combined-id 前缀）。
    pub id: String,
    /// 可选精确会话身份（remount 恢复其物化历史；首用新建）。
    #[serde(default, rename = "sessionId", skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, rename = "maxTokens", skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    /// 全新会话的工作区（informational；M2 宿主不落盘）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// 恢复的持久化会话（与 `sessionId` 互斥）。
    #[serde(default, rename = "resumeSessionId", skip_serializing_if = "Option::is_none")]
    pub resume_session_id: Option<String>,
}

impl ConfiguredAgent {
    /// 会话身份匹配规则（`run_rust_loop` / `register_session_agent` 共用）：
    /// 精确 `sessionId` ▸ `resumeSessionId` ▸ 约定身份 `agent-{id}`。
    pub fn matches_session(&self, session_id: &str) -> bool {
        self.session_id.as_deref() == Some(session_id)
            || self.resume_session_id.as_deref() == Some(session_id)
            || format!("agent-{}", self.id) == session_id
    }
}

/// `AgentLoop.Config` 组合形态（`maxParallelToolCalls` + `agents`）。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct AgentLoopConfig {
    #[serde(default, rename = "maxParallelToolCalls", skip_serializing_if = "Option::is_none")]
    pub max_parallel_tool_calls: Option<u64>,
    #[serde(default)]
    pub agents: Vec<ConfiguredAgent>,
}

impl AgentLoopConfig {
    /// 解析 `maxParallelToolCalls`（缺省常量；非法 → Err，逐字消息见 settings.rs）。
    pub fn resolved_max_parallel_tool_calls(&self) -> Result<u64, String> {
        resolve_max_parallel_tool_calls(self.max_parallel_tool_calls)
    }

    /// 加载期校验：设置 + 配置身份冲突（对齐 `validateConfiguredAgents` 逐字）。
    pub fn validate(&self) -> Result<(), String> {
        self.resolved_max_parallel_tool_calls()?;
        validate_configured_agents(&self.agents)
    }
}

/// 配置身份（`CONFIGURED_AGENT_IDENTITIES_KEY` 的 launcher 身份形态）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ConfiguredAgentIdentity {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// 拒绝自包含身份冲突（对齐 `validateConfiguredAgents` 逐字消息）。
pub fn validate_configured_agents(agents: &[ConfiguredAgent]) -> Result<(), String> {
    let mut exact = HashMap::new();
    for agent in agents {
        let has_resume = agent
            .resume_session_id
            .as_deref()
            .is_some_and(|s| !s.is_empty());
        if agent.session_id.is_some() && has_resume {
            return Err(format!(
                "agent \"{}\": sessionId and resumeSessionId are mutually exclusive",
                agent.id
            ));
        }
        let exact_identity = if has_resume {
            agent.resume_session_id.as_deref()
        } else {
            agent.session_id.as_deref()
        };
        let Some(exact_identity) = exact_identity else {
            continue;
        };
        if let Some(first) = exact.get(exact_identity) {
            return Err(format!(
                "agents \"{first}\" and \"{}\" use duplicate exact session identity \"{exact_identity}\"",
                agent.id
            ));
        }
        exact.insert(exact_identity.to_string(), agent.id.clone());
    }
    Ok(())
}

/// AgentLoop 服务的 Rust 宿主。
///
/// `&self` 方法经内部 `RefCell` 可变（单线程；`SessionStore`/`LlmRuntime`/`ToolRegistry`
/// 都由 Rc 共享）。`store` 可由宿主注入共享（web 侧与 SessionHost 同店，使 Rust loop
/// 事件直接落前端读模型 + 下链 + 持久化）。
pub struct AgentLoopHost {
    pub config: AgentLoopConfig,
    /// 共享 session store（`with_store` 注入；否则宿主自建）。
    pub store: Rc<SessionStore>,
    pub bus: AgentBus,
    pub registry: Rc<AgentRegistry>,
    pub llm: Rc<LlmRuntime>,
    pub tools: Rc<ToolRegistry>,
    pub prompt: Rc<SystemPrompt>,
    agents: RefCell<HashMap<String, Rc<ReactLoopAgent>>>,
    /// 运行时注册的配置 agent（D-101：`session.create`/`fork` 铸新会话时按需挂接）。
    /// 与 `config.agents`（静态配置）并列做会话→agent 发现；`config` 保持装配期
    /// 校验语义不变（validate 只在 with_store 一次）。
    runtime_agents: RefCell<Vec<ConfiguredAgent>>,
    /// 宿主持有的 disposer（工具注册/守卫等；teardown 时按序执行）。
    disposers: RefCell<Vec<Rc<dyn Fn()>>>,
    /// P4：每 agent 的 standing 父绑定（agent scope → preset standing scope）。
    /// select 回调 join/rebind；随 agent 生命周期（host 持有）。
    joins: RefCell<HashMap<String, ScopeParentBinding>>,
}

impl AgentLoopHost {
    /// 自建 store 的宿主（用于独立测试/命令行路径）。
    pub fn new(config: AgentLoopConfig, llm: Rc<LlmRuntime>, tools: Rc<ToolRegistry>) -> Result<Rc<Self>, String> {
        Self::with_store(config, llm, tools, Rc::new(SessionStore::new()))
    }

    /// 登记一个宿主 disposer（teardown 时按序执行；如工具注册/守卫的 disposer）。
    pub fn add_disposer(&self, disposer: Rc<dyn Fn()>) {
        self.disposers.borrow_mut().push(disposer);
    }

    /// 与外部 SessionStore 共享的宿主（web 集成：事件直接落前端读模型）。
    pub fn with_store(
        config: AgentLoopConfig,
        llm: Rc<LlmRuntime>,
        tools: Rc<ToolRegistry>,
        store: Rc<SessionStore>,
    ) -> Result<Rc<Self>, String> {
        config.validate()?;
        let prompt = Rc::new(
            SystemPrompt::new(&PromptConfig::default(), Rc::new(|| {}))
                .map_err(|e| e.to_string())?,
        );
        // M6W（D-093，真实端点 agent 冒烟发现）：把 ToolRegistry 注册为 system-prompt
        // 工具 provider——否则 `assembly.tools` 恒空 → `GenerateOptions.tools=None` →
        // 真实请求不带 `tools` 参数，模型**看不到任何工具定义**、无法发起 tool call
        // （此前只有 mock-适配器驱动「能执行已发出的 tool/call」，从未真发 tools）。
        // 一次性（with_store 每 host 恰一次）：provider 按组装 scope 投影 registry
        // （`ctx.scope`），受 restrict/作用域过滤，与 dsh-tools 注册语义一致。
        {
            let tools = tools.clone();
            let provider: ToolProvider = Rc::new(move |ctx: &AssembleContext| ToolProviderResult {
                schemas: tools.schemas(ctx.scope.as_ref()),
                known_names: Some(tools.known_names(ctx.scope.as_ref())),
            });
            prompt.tools(None, provider);
        }
        let bus = AgentBus::new();
        let registry = Rc::new(AgentRegistry::new(bus.clone()));
        Ok(Rc::new(AgentLoopHost {
            config,
            store,
            bus,
            registry,
            llm,
            tools,
            prompt,
            agents: RefCell::new(HashMap::new()),
            runtime_agents: RefCell::new(Vec::new()),
            disposers: RefCell::new(Vec::new()),
            joins: RefCell::new(HashMap::new()),
        }))
    }

    /// 当前配置身份列表（`CONFIGURED_AGENT_IDENTITIES_KEY` 的宿主侧值）。
    pub fn configured_identities(&self) -> Vec<ConfiguredAgentIdentity> {
        self.config
            .agents
            .iter()
            .map(|a| ConfiguredAgentIdentity {
                id: a.id.clone(),
                session_id: a.session_id.clone().or_else(|| a.resume_session_id.clone()),
            })
            .collect()
    }

    /// 按会话身份解析一个配置 agent（D-101）：静态 `config.agents` 优先，其次
    /// 运行时 `runtime_agents`。静态配置在前，运行时注册不会遮蔽装配期身份。
    pub fn configured_for_session(&self, session_id: &str) -> Option<ConfiguredAgent> {
        self.config
            .agents
            .iter()
            .find(|a| a.matches_session(session_id))
            .cloned()
            .or_else(|| {
                self.runtime_agents
                    .borrow()
                    .iter()
                    .find(|a| a.matches_session(session_id))
                    .cloned()
            })
    }

    /// 运行时给一个会话注册 agent（幂等；D-101）。会话已被任一（静态或运行时）
    /// agent 命中 → 返回其已装配 agent，不重复登记；否则 `ensure_agent` 装配后
    /// 记入 runtime_agents（装配失败不入册，保持幂等）。
    pub fn register_session_agent(
        &self,
        configured: ConfiguredAgent,
    ) -> Result<Rc<ReactLoopAgent>, String> {
        let session_key = configured
            .session_id
            .clone()
            .or_else(|| configured.resume_session_id.clone())
            .unwrap_or_else(|| format!("agent-{}", configured.id));
        if let Some(existing) = self.configured_for_session(&session_key) {
            return self.ensure_agent(&existing);
        }
        let agent = self.ensure_agent(&configured)?;
        self.runtime_agents.borrow_mut().push(configured);
        Ok(agent)
    }

    /// 已装配的 agent（按配置 id）；未知 → None。
    pub fn agent(&self, id: &str) -> Option<Rc<ReactLoopAgent>> {
        self.agents.borrow().get(id).cloned()
    }

    /// 装配（或取已装配的）一个配置 agent：mint 会话 → Agent → driver。
    /// 幂等：同一 id 已装配则原样返回。
    pub fn ensure_agent(&self, configured: &ConfiguredAgent) -> Result<Rc<ReactLoopAgent>, String> {
        if let Some(existing) = self.agent(&configured.id) {
            return Ok(existing);
        }
        let session_id_str = configured
            .session_id
            .clone()
            .or_else(|| configured.resume_session_id.clone())
            .unwrap_or_else(|| format!("agent-{}", configured.id));
        let sid = SessionId::from_raw(session_id_str.clone());
        let session = match self.store.get(&sid) {
            // 已在 store（如 web 预置的 "default"）→ 复用（续接/挂载既有会话）。
            Some(existing) => existing,
            None => self
                .store
                .create(
                    Some(sid),
                    &CreateSessionOptions { seed: None, meta: None },
                )
                .map_err(|e| {
                    format!(
                        "host: create session for agent \"{}\": {}",
                        configured.id, e.0
                    )
                })?,
        };
        let scope = ScopeKey::new();
        let agent = Rc::new(
            Agent::new(
                SessionId::from_raw(session_id_str.clone()),
                session,
                AgentOptions {
                    provider: configured.provider.clone(),
                    model: configured.model.clone(),
                    max_tokens: configured.max_tokens,
                },
                self.bus.clone(),
                scope,
            )
            .map_err(|e| e.to_string())?,
        );

        let max_parallel = self.config.resolved_max_parallel_tool_calls()? as usize;
        let driver = create_loop_agent(
            agent.clone(),
            self.registry.clone(),
            self.prompt.clone(),
            self.llm.clone(),
            self.tools.clone(),
            max_parallel,
        );
        let id = configured.id.clone();
        self.agents.borrow_mut().insert(id, driver.clone());
        Ok(driver)
    }

    /// 按配置 id 提交一条 user 消息（等价 `driver.followup`）。
    /// 未知 id → Err（fail loud）。
    pub fn followup(&self, id: &str, message: Message) -> Result<(), String> {
        let driver = self
            .agent(id)
            .ok_or_else(|| format!("host: no configured agent \"{id}\""))?;
        driver.followup(message).map_err(|e| e.to_string())
    }

    /// D-106：**审批恢复裸踢**（等价 `driver.kick_resume`）——不追加消息，唤醒 driver
    /// 重跑暂停的 pending 调用；无待决审批或非 Idle → Err（fail loud）。
    /// 未知 id → Err。
    pub fn kick(&self, id: &str) -> Result<(), String> {
        let driver = self
            .agent(id)
            .ok_or_else(|| format!("host: no configured agent \"{id}\""))?;
        driver.kick_resume().map_err(|e| e.to_string())
    }

    /// D-106：当前待决审批调用（只读，宿主 decide RPC 感知 / 返回面）。
    pub fn pending_calls(&self, id: &str) -> Result<Vec<super::agent::PendingCall>, String> {
        let driver = self
            .agent(id)
            .ok_or_else(|| format!("host: no configured agent \"{id}\""))?;
        Ok(driver.pending_calls())
    }

    /// P4（直通 accept）：把 agent 作用域链到 preset 的 standing scope（join）。
    /// - 首次 = `bind_scope_parent(agent.scope → standing)`，绑定存入宿主 `joins`；
    /// - 再次（换 preset）= 原绑定 `rebind`（沿用装配期 scope）；
    /// - agent 必须已装配（懒装配会话须先 `ensure_agent`/`register_session_agent`）；
    ///   未知 id → fail loud（不静默）。
    ///
    /// 生效即时性：loop 每 turn 以 `AssembleContext{scope: agent.scope}` 组装（走
    /// `scope_chain_of` 父链），故 join 后**下一 turn 的 assemble 即含 standing 视图**，
    /// 无需重建 host/loop。
    pub fn join_standing(&self, agent_id: &str, standing: &ScopeKey) -> Result<(), String> {
        let agent = self
            .agent(agent_id)
            .ok_or_else(|| format!("host: no agent \"{agent_id}\""))?;
        let scope = agent.agent.scope.clone();
        let mut joins = self.joins.borrow_mut();
        if let Some(binding) = joins.get(agent_id) {
            binding
                .rebind(standing.clone())
                .map_err(|e| format!("host: rebind {}: {e}", agent_id))
        } else {
            let binding = bind_scope_parent(scope, standing.clone())
                .map_err(|e| format!("host: join {}: {e}", agent_id))?;
            joins.insert(agent_id.to_string(), binding);
            Ok(())
        }
    }

    /// 会话事件（前端历史读模型；未知 → 空）。
    pub fn events(&self, session_id: &str) -> Vec<dsh_session::types::SessionEvent> {
        let sid = SessionId::from_raw(session_id.to_string());
        self.store
            .get(&sid)
            .map(|s| s.events())
            .unwrap_or_default()
    }

    /// 生命周期 teardown：detach 全部登记 agent + 执行宿主 disposer、清空装配表。
    pub fn teardown(&self) {
        let disposers = std::mem::take(&mut *self.disposers.borrow_mut());
        for d in disposers {
            d();
        }
        self.agents.borrow_mut().clear();
    }
}

/// 便捷：把设置/身份 key 暴露给宿主（web RPC settings.describe 用）。
pub fn configured_agent_identities_key() -> &'static str {
    CONFIGURED_AGENT_IDENTITIES_KEY
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_scope::scope_chain_of;

    fn host() -> Rc<AgentLoopHost> {
        let config = AgentLoopConfig {
            max_parallel_tool_calls: None,
            agents: vec![ConfiguredAgent {
                id: "main".into(),
                provider: Some("mock".into()),
                model: Some("mock-model".into()),
                session_id: Some("default".into()),
                max_tokens: None,
                cwd: None,
                resume_session_id: None,
            }],
        };
        AgentLoopHost::with_store(
            config,
            Rc::new(dsh_llm::LlmRuntime::new()),
            Rc::new(dsh_tools::ToolRegistry::new(dsh_tools::ToolExecutionMode::Native)),
            Rc::new(SessionStore::new()),
        )
        .unwrap()
    }

    /// P4 join_standing：链上 standing scope；换 preset = rebind（agent scope 不变、
    /// 父换）；未知 agent → fail loud。
    #[test]
    fn join_standing_links_scope_and_rebounds() {
        let h = host();
        h.ensure_agent(&h.config.agents[0]).unwrap();
        let a = h.agent("main").unwrap();
        let standing1 = ScopeKey::new();
        let standing2 = ScopeKey::new();

        h.join_standing("main", &standing1).unwrap();
        let chain = scope_chain_of(Some(&a.agent.scope));
        assert!(chain.contains(&standing1), "joined scope present: {chain:?}");
        assert!(!chain.contains(&standing2));

        // 换 preset → rebind。
        h.join_standing("main", &standing2).unwrap();
        let chain2 = scope_chain_of(Some(&a.agent.scope));
        assert!(chain2.contains(&standing2), "rebound scope present: {chain2:?}");
        assert!(!chain2.contains(&standing1));

        // 未知 agent → fail loud。
        assert!(h.join_standing("nope", &standing1).is_err());
    }
}

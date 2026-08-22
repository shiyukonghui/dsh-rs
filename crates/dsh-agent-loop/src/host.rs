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
use dsh_scope::ScopeKey;
use dsh_session::store::SessionStore;
use dsh_session::types::{CreateSessionOptions, SessionId};
use dsh_system_prompt::{Config as PromptConfig, SystemPrompt};
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
    /// 宿主持有的 disposer（工具注册/守卫等；teardown 时按序执行）。
    disposers: RefCell<Vec<Rc<dyn Fn()>>>,
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
            disposers: RefCell::new(Vec::new()),
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

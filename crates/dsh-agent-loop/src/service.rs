//! AgentLoop 服务装配：把真实服务（system-prompt / llm / tools）接成 driver 的
//! `LoopDeps`，并在 llm 派发点守卫 invariant（对齐 `index.ts` / §2.8；sync/宿主差值
//! 见 D-033）。`preparedCall.stream ?? llm.stream` 的选择归位为：prepared 流优先，
//! 超回退到 `deps.stream`（同样带 invariant 守卫）。

// 跨 crate 泥合 seam（Rc<dyn Fn>）与 LlmError 大错值是设计选择，见 D-032/033。
#![allow(clippy::type_complexity)]
#![allow(clippy::result_large_err)]

use std::cell::RefCell;
use std::rc::Rc;

use dsh_agent::{Agent, AgentRegistry};
use dsh_llm::{
    CallConfig, GenerateOptions, LlmError, LlmRuntime, Message, PreparedLlmCall, StreamChunk,
};
use dsh_system_prompt::{
    join_context_sections, render_context_sections, AssembleContext, PromptAssembly, SystemPrompt,
};
use dsh_tools::ToolRegistry;

use crate::agent::{LoopDeps, ReactLoopAgent, ToolExecCtx, ToolExecOutcome};
use crate::invariant::{check_loop_request, AgentLoopRequest};
use crate::runtime_context::RuntimeContextProjection;
use crate::tool_calls::execute_tool_calls;

pub use crate::runtime_context::{CLEARED as RUNTIME_CONTEXT_CLEARED, SOURCE as RUNTIME_CONTEXT_SOURCE};

fn invariant_error(message: String) -> LlmError {
    LlmError::new(message, "UNKNOWN")
}

/// 为既有 agent 装配真实 `LoopDeps`（宿主服务已实例化；M2g 接入 boot 的组合点）。
///
/// - `assemble` → `SystemPrompt::assemble`；
/// - `prepare_call` → `LlmRuntime::prepare_call`，且把 invariant 守卫包装进 prepared 流；
/// - `stream`（fallback）→ invariant + `LlmRuntime::stream` 物化；
/// - `project_context` → `RuntimeContextProjection`（session 日志权威重派生）；
/// - `tool_exec` → `execute_tool_calls`（绑定 session/tools/scope/agent）。
#[allow(clippy::too_many_arguments)]
pub fn build_loop_deps(
    agent: &Rc<Agent>,
    prompt: Rc<SystemPrompt>,
    llm: Rc<LlmRuntime>,
    tools: Rc<ToolRegistry>,
    max_parallel_tool_calls: usize,
) -> LoopDeps {
    let session = agent.session.clone();
    let agent_id: Option<String> = Some(agent.id.raw().to_string());
    let scope = agent.scope.clone();

    let assemble: Rc<dyn Fn(&AssembleContext) -> Result<PromptAssembly, String>> = {
        let prompt = prompt.clone();
        Rc::new(move |ctx: &AssembleContext| {
            let p = prompt.clone();
            p.assemble(ctx)
        })
    };

    let prepare_call: Rc<dyn Fn(CallConfig) -> Result<PreparedLlmCall, LlmError>> = {
        let llm = llm.clone();
        let session = session.clone();
        Rc::new(move |config: CallConfig| {
            let mut prepared = llm.prepare_call(&config)?;
            if let Some(mut inner) = prepared.stream.take() {
                let session = session.clone();
                prepared.stream = Some(Box::new(move |opts: GenerateOptions| {
                    check_loop_request(&AgentLoopRequest(opts.clone()), Some(&session))
                        .map_err(invariant_error)?;
                    inner(opts)
                }));
            }
            Ok(prepared)
        })
    };

    let stream: Rc<dyn Fn(&GenerateOptions) -> Result<Vec<StreamChunk>, LlmError>> = {
        let llm = llm.clone();
        let session = session.clone();
        Rc::new(move |request: &GenerateOptions| {
            check_loop_request(&AgentLoopRequest(request.clone()), Some(&session))
                .map_err(invariant_error)?;
            Ok(llm.stream(request.clone()).collect())
        })
    };

    let projection = Rc::new(RefCell::new(RuntimeContextProjection::new()));
    let project_context: Rc<dyn Fn(&PromptAssembly) -> Option<Message>> = {
        let session = session.clone();
        let projection = projection.clone();
        Rc::new(move |assembly: &PromptAssembly| {
            let sections = render_context_sections(assembly).unwrap_or_default();
            let current = join_context_sections(&sections);
            projection.borrow_mut().project(&session, &current, &sections)
        })
    };

    let tool_exec: Rc<dyn Fn(&ToolExecCtx) -> ToolExecOutcome> = {
        let session = session.clone();
        let tools = tools.clone();
        let agent_id = agent_id.clone();
        Rc::new(move |ctx: &ToolExecCtx| {
            let mut context: Vec<Message> = Vec::new();
            let mut accept = |m: Message| context.push(m);
            let concluded = execute_tool_calls(
                &session,
                &tools,
                Some(&scope),
                agent_id.as_deref(),
                max_parallel_tool_calls,
                ctx.turn,
                ctx.step,
                ctx.tool_calls,
                &ctx.resume,
                &mut accept,
            )
            .unwrap_or(false);
            // 直通路径永不暂停审批（pending 由宿主包装层注入）；保留 pending 字段供宿主复用。
            ToolExecOutcome {
                concluded,
                context,
                pending: Vec::new(),
            }
        })
    };

    LoopDeps {
        assemble,
        prepare_call,
        stream,
        project_context,
        tool_exec,
    }
}

/// 便捷：装配真实 deps 并把 agent 交给驱动。
pub fn create_loop_agent(
    agent: Rc<Agent>,
    registry: Rc<AgentRegistry>,
    prompt: Rc<SystemPrompt>,
    llm: Rc<LlmRuntime>,
    tools: Rc<ToolRegistry>,
    max_parallel_tool_calls: usize,
) -> Rc<ReactLoopAgent> {
    let deps = build_loop_deps(&agent, prompt, llm, tools, max_parallel_tool_calls);
    ReactLoopAgent::new(agent, registry, deps)
}

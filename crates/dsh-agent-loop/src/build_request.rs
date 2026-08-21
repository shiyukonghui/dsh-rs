//! buildRequest 纯核心 + requestProposal（对齐 `agent.ts#buildRequest` 逐行语义）。
//!
//! 是本包「请求可由 log 前缀逐字节重建」THEOREM 的构造侧：request/header 唯一锚点
//! (initial/resume/change)、request/context 增量去重、适配器填充暗省的重解析剥离、
//! 空 system/tools 不写、loop 标记。信号（AbortSignal）不入 Rust 面（sync 纪律）。

use std::rc::Rc;

use dsh_agent::AgentOptions;
use dsh_llm::types::{Message, ToolSchema};
use dsh_llm::{CallConfig, GenerateOptions, LlmError, PreparedLlmCall};
use dsh_session::{
    canonical_header, header_equals, request_header_reason_str, EventKind, RequestContext,
    RequestHeaderReason, Session,
};
use serde_json::{json, Value};

use crate::invariant::AgentLoopRequest;

/// `requestProposal(header)`：从最后一次请求头导出**下一次**请求提案。
/// 适配器填充（`adapterDefaults` 标记）的 `reasoningEffort`/`maxTokens` 会被删除，
/// 以便每步按 provider/default 重新解析。
pub fn request_proposal(header: &dsh_session::EpochHeader) -> CallConfig {
    let mut proposal = header.config.clone();
    if let Some(adapter) = &header.adapter_defaults {
        if adapter.reasoning_effort == Some(true) {
            proposal.reasoning_effort = None;
        }
        if adapter.max_tokens == Some(true) {
            proposal.max_tokens = None;
        }
    }
    proposal
}

/// `buildRequest` 的产物 + 需要写回实例的状态。
pub struct BuiltRequest {
    /// loop 标记的最终请求（冻结面 = Rust 不可变值）。
    pub request: AgentLoopRequest,
    /// prepareCall 成功时的绑定派发（含 adapterDefaults/context/retryPolicy）。
    pub prepared_call: Option<PreparedLlmCall>,
    /// 本步的 canonical 请求头（含 adapterDefaults/system/tools）。
    pub header: dsh_session::EpochHeader,
    /// 写回实例：首次请求/header 记录后为 true（后续稳定 prompt 不重复记录）。
    pub request_header_logged: bool,
}

/// 构建一步模型请求（对齐 `agent.ts#buildRequest`）。
///
/// - `propose`：`agent/request` 水岭（默认恒等）。M2e-2 起接作用域总线；本层收
///   纯函数以保持可测（sync 纪律；`agent/request` 的 `{turn,step,signal}` 在 M2e-2
///   由驱动线程化）。
/// - `prepare_call`：`llm.prepareCall`。`NO_ADAPTER` 降级为透传 config（允许
///   llm/stream 插件短路未注册路由）；其它错误传播。
/// - `boundary_messages`：派生消息（`session.deriveMessages()`），请求的唯一消息源。
#[allow(clippy::too_many_arguments)] // 1:1 对齐 buildRequest 输入面（8 参数，拒绝包壳失真）
pub fn build_request(
    session: &Rc<Session>,
    options: &AgentOptions,
    request_header_logged: bool,
    tools: &[ToolSchema],
    system: &str,
    boundary_messages: Vec<Message>,
    propose: &dyn Fn(CallConfig) -> Result<CallConfig, String>,
    prepare_call: &dyn Fn(CallConfig) -> Result<PreparedLlmCall, LlmError>,
) -> Result<BuiltRequest, String> {
    let persisted_header = session.request_header();
    let persisted_config = persisted_header.as_ref().map(|h| h.config.clone());
    let route_provider = options.provider.clone().unwrap_or_default();
    let route_model = options.model.clone().unwrap_or_default();

    // 显式 effort 仅当其属于那条确切的模型（provider+model 同 + 非适配器填充）时恢复。
    let reasoning_effort = match (&persisted_config, &persisted_header) {
        (Some(pc), Some(ph))
            if pc.provider == route_provider
                && pc.model == route_model
                && ph
                    .adapter_defaults
                    .as_ref()
                    .is_none_or(|a| a.reasoning_effort != Some(true)) =>
        {
            pc.reasoning_effort.clone()
        }
        _ => None,
    };
    let max_tokens = options.max_tokens;

    let seed_config = if request_header_logged {
        request_proposal(persisted_header.as_ref().expect("requestHeaderLogged implies a folded header"))
    } else {
        CallConfig {
            provider: route_provider,
            model: route_model,
            reasoning_effort,
            max_tokens,
            ..Default::default()
        }
    };

    let proposed_config = propose(seed_config)?;
    if proposed_config.provider.is_empty() || proposed_config.model.is_empty() {
        return Err(format!(
            "agent \"{}\" has no provider/model: set AgentOptions.provider and AgentOptions.model or supply both via the agent/request waterfall",
            session.id()
        ));
    }

    let (config, prepared_call) = match prepare_call(proposed_config.clone()) {
        Ok(pc) => (pc.config.clone(), Some(pc)),
        Err(e) if e.code == "NO_ADAPTER" => (proposed_config, None),
        Err(e) => return Err(e.message.clone()),
    };

    let header = canonical_header(&dsh_session::EpochHeader {
        config: config.clone(),
        adapter_defaults: prepared_call.as_ref().map(|p| p.adapter_defaults.clone()),
        system: if system.is_empty() {
            None
        } else {
            Some(system.to_string())
        },
        tools: if tools.is_empty() {
            None
        } else {
            Some(tools.to_vec())
        },
    });

    let baseline = session.request_header();
    let mut logged = request_header_logged;
    if !logged {
        let reason = match baseline {
            None => RequestHeaderReason::Initial,
            Some(_) => RequestHeaderReason::Resume,
        };
        append_header(session, &header, reason)?;
        logged = true;
    } else if baseline.is_none() || !header_equals(&baseline.expect("Some above"), &header) {
        append_header(session, &header, RequestHeaderReason::Change)?;
    }

    let context_window = prepared_call
        .as_ref()
        .and_then(|p| p.context.as_ref())
        .map(|c| c.context_window);
    let request_context = RequestContext {
        provider: config.provider.clone(),
        model: config.model.clone(),
        context_window,
    };
    let prior = session.request_context();
    let context_changed = match &prior {
        None => true,
        Some(p) => {
            p.provider != request_context.provider
                || p.model != request_context.model
                || p.context_window != request_context.context_window
        }
    };
    if context_changed {
        session
            .append(
                EventKind::RequestContext,
                serde_json::to_value(&request_context).unwrap_or(Value::Null),
                None,
            )
            .map_err(|e| e.0.clone())?;
    }

    let request = GenerateOptions {
        provider: config.provider,
        model: config.model,
        reasoning_effort: config.reasoning_effort,
        messages: boundary_messages,
        system: header.system.clone(),
        tools: header.tools.clone(),
        temperature: config.temperature,
        max_tokens: config.max_tokens,
        stop: config.stop,
        session_id: Some(session.id().clone()),
        purpose: None,
    };

    Ok(BuiltRequest {
        request: AgentLoopRequest(request),
        prepared_call,
        header,
        request_header_logged: logged,
    })
}

fn append_header(
    session: &Rc<Session>,
    header: &dsh_session::EpochHeader,
    reason: RequestHeaderReason,
) -> Result<dsh_session::SessionEvent, String> {
    let data = json!({
        "header": serde_json::to_value(header).unwrap_or(Value::Null),
        "reason": request_header_reason_str(&reason),
    });
    session
        .append(EventKind::RequestHeader, data, None)
        .map_err(|e| e.0.clone())
}

//! tool-calls 调度（对齐 `tool-calls.ts`；sync 差值见 D-033）。
//!
//! 把一个 assistant step 的模型调用按其并发模式调度：独占调用形成屏障，并行调用
//! 使用有界滚动池（sync 下顺序执行，但**分类与模型顺序提交**语义不变）。调度器写
//! `tool/call` + `tool/result` 持久事件；`concluded` 来自任一结果的 `concludes_turn`。
//! sync 差值：无并发 `inFlight` 池（顺序执行、天然模型顺序）；Abort 排水不可达
//! （ToolSignal 每次全新，无中流抢占——D-033）。

// 跨 crate 泥合 seam 与调度签名（9 参、Rc<dyn Fn>、结果大 Err）是设计选择，见 D-032/033。
#![allow(clippy::type_complexity)]
#![allow(clippy::result_large_err)]
#![allow(clippy::too_many_arguments)]

use std::rc::Rc;

use dsh_llm::{ContentBlock, Message, MessageId, ToolCallBlock, ToolResultBlock};
use dsh_scope::ScopeKey;
use dsh_session::{
    EventKind, Session, SurfaceIntent, SurfaceOp, ToolCallError, ToolCallPayload,
    ToolResultPayload,
};
use dsh_tools::{
    ToolExecutionClass, ToolExecutionInput, ToolExecutionResult, ToolRegistry,
    TOOL_ABORTED_BEFORE_DISPATCH,
};
use serde_json::{json, Value};

/// 一次已解析好参数的、待调度的模型调用。
struct PlannedCall {
    block: ToolCallBlock,
    input: ToolExecutionInput,
}

/// 一次调度组的结果（含排水式取消）。
struct GroupOutcome {
    consumed: usize,
    aborted: bool,
    concluded: bool,
}

/// 解析模型参数：空串 → `{}`；非法 JSON → 原样字符串（不崩溃）。
fn parse_arguments(raw: &str) -> Value {
    if raw.is_empty() {
        return json!({});
    }
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
}

/// 调度一个 assistant step 的全部模型工具调用（模型顺序）。
///
/// 返回 `concluded`（任一提交 result 携带 `concludesTurn`）。`accept_context` 接受
/// 已提交结果延后到全部调用提交之后的 next-step 上下文（模型顺序）。
pub fn execute_tool_calls(
    session: &Rc<Session>,
    tools: &ToolRegistry,
    scope: Option<&ScopeKey>,
    agent: Option<&str>,
    _max_parallel_tool_calls: usize,
    turn: u64,
    step: u64,
    tool_calls: &[ToolCallBlock],
    accept_context: &mut dyn FnMut(Message),
) -> Result<bool, String> {
    let planned: Vec<PlannedCall> = tool_calls
        .iter()
        .map(|block| PlannedCall {
            block: block.clone(),
            input: ToolExecutionInput::new(
                block.id.raw(),
                block.name.clone(),
                parse_arguments(&block.arguments),
                agent.map(ToString::to_string),
            ),
        })
        .collect();
    let mut next = 0;
    let mut concluded = false;
    while next < planned.len() {        let first = &planned[next];
        let mode = tools.execution_mode(&first.input, scope);
        // 并行 = 剩余全部（有界滚动池）；独占 = 单 call 屏障。
        let group = if mode == ToolExecutionClass::Parallel {
            &planned[next..]
        } else {
            &planned[next..next + 1]
        };
        let outcome = run_group(session, tools, scope, turn, step, group, accept_context)?;
        next += outcome.consumed;
        concluded |= outcome.concluded;
        if outcome.aborted {
            for call in planned[next..].iter() {
                append_skipped_tool_call(session, turn, step, &call.block);
            }
            return Ok(concluded);
        }
    }
    Ok(concluded)
}

/// 运行一个独占屏障或并行池。结果与上下文按**模型顺序**提交；同步顺序执行即天然有序。
fn run_group(
    session: &Rc<Session>,
    tools: &ToolRegistry,
    scope: Option<&ScopeKey>,
    turn: u64,
    step: u64,
    group: &[PlannedCall],
    accept_context: &mut dyn FnMut(Message),
) -> Result<GroupOutcome, String> {
    let mut consumed = 0;
    let mut concluded = false;
    for (idx, call) in group.iter().enumerate() {
        // 并行池：后续 call 转 exclusive → 新屏障，留给调用方下一轮。
        if idx > 0 && tools.execution_mode(&call.input, scope) != ToolExecutionClass::Parallel {
            break;
        }
        let call_seq = append_tool_call(session, turn, step, &call.block);
        let result = tools.execute(&call.input, scope);
        append_tool_result(session, turn, step, &call.block, &result, call_seq)?;
        for ctx in &result.additional_contexts {
            let msg: Message =
                serde_json::from_value(ctx.clone()).map_err(|e| format!("invalid tool additional context: {e}"))?;
            accept_context(msg);
        }
        concluded |= result.concludes_turn;
        consumed += 1;
        // sync 下不可达（ToolSignal 每次全新）；保留排水式取消语义以对齐：
        // 已启动的当前调用提交其 aborted 结果，未启动的补合成 skipped。
        if is_aborted_before_dispatch(&result) {
            for rest in group[consumed..].iter() {
                append_skipped_tool_call(session, turn, step, &rest.block);
            }
            return Ok(GroupOutcome {
                consumed: group.len(),
                aborted: true,
                concluded,
            });
        }
    }
    Ok(GroupOutcome {
        consumed,
        aborted: false,
        concluded,
    })
}

fn is_aborted_before_dispatch(result: &ToolExecutionResult) -> bool {
    result
        .error
        .as_ref()
        .and_then(|e| e.info.as_ref())
        .map(|d| d.code == TOOL_ABORTED_BEFORE_DISPATCH)
        .unwrap_or(false)
}

/// 解析取消后跳过的模型调用：写 call + 合成的错误结果（对偶于 TS `appendSkippedToolCall`）。
fn append_skipped_tool_call(session: &Rc<Session>, turn: u64, step: u64, block: &ToolCallBlock) {
    let call_seq = append_tool_call(session, turn, step, block);
    let payload = ToolResultPayload {
        turn,
        step,
        message: Message::tool_result(
            MessageId::from_raw(format!("tool-result-{turn}-{step}-{}", block.id.raw())),
            block.id.clone(),
            vec![ContentBlock::ToolResult(ToolResultBlock {
                tool_call_id: block.id.clone(),
                content: vec![ContentBlock::text("Error: tool call aborted before dispatch")],
                is_error: Some(true),
            })],
        ),
        error: Some(ToolCallError {
            name: "AbortError".into(),
            code: TOOL_ABORTED_BEFORE_DISPATCH.into(),
        }),
        meta: None,
    };
    let _ = session.append(
        EventKind::ToolResult,
        serde_json::to_value(&payload).unwrap_or(Value::Null),
        Some(&SurfaceIntent {
            surface_op: SurfaceOp::Append,
            source_event_seqs: Some(vec![call_seq]),
        }),
    );
}

/// 追加已启动 call，返回其结果必须引用的事件 seq。
fn append_tool_call(session: &Rc<Session>, turn: u64, step: u64, block: &ToolCallBlock) -> u64 {
    let payload = ToolCallPayload {
        turn,
        step,
        call_id: block.id.clone(),
        name: block.name.clone(),
        arguments: block.arguments.clone(),
    };
    session
        .append(
            EventKind::ToolCall,
            serde_json::to_value(&payload).unwrap_or(Value::Null),
            None,
        )
        .unwrap()
        .seq
}

/// 追加模型顺序结果并链接到其 call 事件。
fn append_tool_result(
    session: &Rc<Session>,
    turn: u64,
    step: u64,
    block: &ToolCallBlock,
    result: &ToolExecutionResult,
    call_seq: u64,
) -> Result<(), String> {
    let error = result
        .error
        .as_ref()
        .and_then(|e| e.info.as_ref())
        .map(|d| ToolCallError {
            name: d.name.clone(),
            code: d.code.clone(),
        });
    let message = Message::tool_result(
        MessageId::from_raw(format!("tool-result-{turn}-{step}-{}", block.id.raw())),
        block.id.clone(),
        vec![ContentBlock::ToolResult(ToolResultBlock {
            tool_call_id: block.id.clone(),
            content: result.content.clone(),
            is_error: Some(result.is_error),
        })],
    );
    let payload = ToolResultPayload {
        turn,
        step,
        message,
        error,
        meta: None,
    };
    session
        .append(
            EventKind::ToolResult,
            serde_json::to_value(&payload).map_err(|e| e.to_string())?,
            Some(&SurfaceIntent {
                surface_op: SurfaceOp::Append,
                source_event_seqs: Some(vec![call_seq]),
            }),
        )
        .map(|_| ())
        .map_err(|e| e.0)
}

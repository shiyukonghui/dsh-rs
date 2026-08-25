//! D-106/段 B：执行层审批策略（宿主侧；loop 只提供机制，策略在此）。
//!
//! plan 模式 active 时 mutation 工具（D-b 用户确认清单）的 execute 经异步审批门
//! 强制走 拒绝/ask（loop 段 A 的 pending 机制 + `ApprovalPending` 暂停）：
//! - 正常 step：无决策 → 落 `tool/call` + `approval/asked` + pending（turn 以
//!   approval-pending 暂停，driver Idle 停车）；
//! - 恢复 step（`ctx.resume` 非空）：`approval/decided{allowedOnce}` → 执行该调用
//!   （只追 `tool/result`、复用 call seq）；`{rejected}` → 合成拒绝 result（不执行）。
//!
//! 硬纪律：不伪造批准来源（无 decided → 拒绝/停留，绝不冒充放行）；read 系工具与
//! plan 非激活时的全部工具 → 直通（与既有行为逐位一致）。

use std::rc::Rc;

use dsh_agent_loop::{
    append_pending_rejection, emit_pending_calls, execute_tool_calls, PendingCall, ToolExecCtx,
    ToolExecFactory, ToolExecOutcome,
};
use dsh_llm::{Message, ToolCallBlock};
use dsh_scope::ScopeKey;
use dsh_session::{EventKind, Session};
use dsh_tools::ToolRegistry;
use serde_json::{json, Value};

/// `approval/decided` 的 decision 值（对齐 harness 一次性语义）。
pub const DECISION_ALLOWED_ONCE: &str = "allowedOnce";
pub const DECISION_REJECTED: &str = "rejected";

/// D-b mutation 工具集（用户确认清单）。read 系不在此列 → 直通。
pub fn mutation_tool_set() -> &'static [&'static str] {
    &[
        "write",
        "edit",
        "terminal_open",
        "terminal_send",
        "terminal_signal",
        "bash",
        "str_replace_editor",
        "run_code",
        "schedule_create",
        "schedule_delete",
        "job_kill",
    ]
}

fn is_mutation(name: &str) -> bool {
    mutation_tool_set().contains(&name)
}

/// 宿主注入 tool_exec 的工厂（`AgentLoopHost::set_tool_exec_factory`）。
/// 按每个 driver 绑定事实产出包装；与服务直通路径（`pending` 恒空）逐位一致，仅当
/// 会话 plan 折叠 active 且调用为 mutation 时才走审批门。
pub fn approval_tool_exec_factory() -> Rc<ToolExecFactory> {
    Rc::new(move |session, tools, scope, agent, max_parallel| {
        let session = session.clone();
        let tools = tools.clone();
        let scope = scope.clone();
        let agent = agent.clone();
        Rc::new(move |ctx: &ToolExecCtx| -> ToolExecOutcome {
            approval_tool_exec(&session, &tools, scope.as_ref(), agent.as_deref(), max_parallel, ctx)
        })
    })
}

fn approval_tool_exec(
    session: &Rc<Session>,
    tools: &Rc<ToolRegistry>,
    scope: Option<&ScopeKey>,
    agent: Option<&str>,
    max_parallel: usize,
    ctx: &ToolExecCtx,
) -> ToolExecOutcome {
    // 恢复路径优先：处理上一步暂停的 pending（决策已下）。
    if !ctx.resume.is_empty() {
        return resume_path(session, tools, scope, agent, max_parallel, ctx);
    }
    // 正常路径：plan 非激活 → 直通（保持既有行为）。
    let plan_active = dsh_plan::fold_plan_mode(&session.events());
    if !plan_active {
        return run_calls(session, tools, scope, agent, max_parallel, ctx.turn, ctx.step, ctx.tool_calls, &[], Vec::new());
    }
    // plan 激活：拆分 run（直通）与 pending（审批门）。
    let mut run: Vec<ToolCallBlock> = Vec::new();
    let mut pending_blocks: Vec<ToolCallBlock> = Vec::new();
    for block in ctx.tool_calls {
        if is_mutation(&block.name) {
            pending_blocks.push(block.clone());
        } else {
            run.push(block.clone());
        }
    }
    let pending = emit_pending_calls(session, ctx.turn, ctx.step, &pending_blocks);
    for p in &pending {
        let _ = session.append(
            EventKind::ApprovalAsked,
            json!({
                "tool": p.block.name,
                "toolCallId": p.block.id.raw(),
                "agent": agent,
                "reason": format!(
                    "tool \"{}\" mutates state and requires approval while plan mode is active",
                    p.block.name
                ),
            }),
            None,
        );
    }
    run_calls(session, tools, scope, agent, max_parallel, ctx.turn, ctx.step, &run, &[], pending)
}

/// 恢复路径：按 `fold_decided` 分派——allowedOnce 执行（只追 result）、rejected 合成
/// 拒绝、未决（防御，decide 是先决）保持 pending 并重发 asked。
fn resume_path(
    session: &Rc<Session>,
    tools: &Rc<ToolRegistry>,
    scope: Option<&ScopeKey>,
    agent: Option<&str>,
    max_parallel: usize,
    ctx: &ToolExecCtx,
) -> ToolExecOutcome {
    let mut exec: Vec<PendingCall> = Vec::new();
    let mut still_pending: Vec<PendingCall> = Vec::new();
    for p in ctx.resume.iter() {
        match fold_decided(session, p.block.id.raw()).as_deref() {
            Some(DECISION_ALLOWED_ONCE) => exec.push(p.clone()),
            Some(DECISION_REJECTED) => {
                let _ = append_pending_rejection(
                    session,
                    ctx.turn,
                    ctx.step,
                    p,
                    &format!("the user rejected tool \"{}\"", p.block.name),
                );
            }
            _ => {
                let _ = session.append(
                    EventKind::ApprovalAsked,
                    json!({
                        "tool": p.block.name,
                        "toolCallId": p.block.id.raw(),
                        "agent": agent,
                        "reason": "approval still undecided (resume without decision is refused)".to_string(),
                    }),
                    None,
                );
                still_pending.push(p.clone());
            }
        }
    }
    let mut context: Vec<Message> = Vec::new();
    let concluded = if exec.is_empty() {
        false
    } else {
        let mut accept = |m: Message| context.push(m);
        execute_tool_calls(session, tools, scope, agent, max_parallel, ctx.turn, ctx.step, &[], &exec, &mut accept)
            .unwrap_or(false)
    };
    ToolExecOutcome {
        concluded,
        context,
        pending: still_pending,
    }
}

/// 调用 `execute_tool_calls` 并包装 outcome（pending 由调用方给定）。
#[allow(clippy::too_many_arguments)]
fn run_calls(
    session: &Rc<Session>,
    tools: &Rc<ToolRegistry>,
    scope: Option<&ScopeKey>,
    agent: Option<&str>,
    max_parallel: usize,
    turn: u64,
    step: u64,
    calls: &[ToolCallBlock],
    resume: &[PendingCall],
    pending: Vec<PendingCall>,
) -> ToolExecOutcome {
    let mut context: Vec<Message> = Vec::new();
    let mut accept = |m: Message| context.push(m);
    let concluded = execute_tool_calls(session, tools, scope, agent, max_parallel, turn, step, calls, resume, &mut accept)
        .unwrap_or(false);
    ToolExecOutcome {
        concluded,
        context,
        pending,
    }
}

/// 该 call 最近的 `approval/decided` 决策（无/未知 → None）。
fn fold_decided(session: &Rc<Session>, call_id: &str) -> Option<String> {
    session
        .events()
        .into_iter()
        .rev()
        .find(|e| {
            e.kind == EventKind::ApprovalDecided
                && e.data.get("toolCallId").and_then(Value::as_str) == Some(call_id)
        })
        .and_then(|e| e.data.get("decision").and_then(Value::as_str).map(str::to_string))
        .filter(|d| d.as_str() == DECISION_ALLOWED_ONCE || d.as_str() == DECISION_REJECTED)
}

/// 宿主审批决定（`session.approval.decide` RPC 后端）：写 `approval/decided` 到
/// default 会话 → 裸踢恢复。返回「仍待决」调用数（None → 已全清）。
pub fn decide(boot: &crate::Boot, call_id: &str, decision: &str) -> Result<usize, String> {
    const AGENT: &str = "default";
    let host = boot
        .agent_loop
        .as_ref()
        .ok_or_else(|| "no Rust AgentLoopHost assembled in this boot".to_string())?;
    let pending = host.pending_calls(AGENT)?;
    let _tool = pending
        .iter()
        .find(|p| p.block.id.raw() == call_id)
        .map(|p| p.block.name.clone())
        .ok_or_else(|| format!("no pending approval for toolCallId \"{call_id}\""))?;
    let session = host
        .store
        .get(&dsh_session::types::SessionId::from_raw("default".to_string()))
        .ok_or_else(|| "default session missing".to_string())?;
    session
        .append(
            EventKind::ApprovalDecided,
            json!({
                "toolCallId": call_id,
                "decision": decision,
            }),
            None,
        )
        .map_err(|e| e.0)?;
    host.kick(AGENT)?;
    Ok(host.pending_calls(AGENT)?.len())
}

/// D-106/S1：宿主 plan-mode 入口/出口（`session.plan.mode` RPC 后端）。
/// 用户侧动作：进入与离开都**无前置**——宿主 leave 是 GUI 用户显式动作，不要求
/// plan heading；模型 `exit_plan_mode` 保持 dsh_plan 三重前置不变。落 `plan/mode`
/// 事件（standing 折叠段随事件注入/撤下）+ `approval/policy` 诚实宣告
/// （作用域 `mutation`，工具集即 D-b 清单）。返回当前 plan-active 值。
pub fn set_plan_mode(
    boot: &crate::Boot,
    active: bool,
    message: Option<&str>,
) -> Result<bool, String> {
    let sid = boot
        .plan_session
        .as_ref()
        .map(|ps| ps.borrow().clone())
        .unwrap_or_else(|| "default".to_string());
    let host = boot
        .agent_loop
        .as_ref()
        .ok_or_else(|| "no Rust AgentLoopHost assembled in this boot".to_string())?;
    let session = host
        .store
        .get(&dsh_session::types::SessionId::from_raw(sid.clone()))
        .ok_or_else(|| format!("session \"{sid}\" missing"))?;
    let mut mode = serde_json::Map::new();
    mode.insert("active".to_string(), json!(active));
    if active {
        if let Some(m) = message {
            mode.insert("message".to_string(), json!(m));
        }
    }
    session
        .append(EventKind::PlanMode, Value::Object(mode), None)
        .map_err(|e| e.0)?;
    session
        .append(
            EventKind::ApprovalPolicy,
            json!({
                "active": active,
                "scope": "mutation",
                "tools": mutation_tool_set(),
            }),
            None,
        )
        .map_err(|e| e.0)?;
    Ok(active)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_llm::{CallId, ContentBlock};
    use dsh_tools::{define_tool, DefineToolOptions, ToolExecutionMode};

    fn session() -> Rc<Session> {
        Rc::new(dsh_session::Session::create(
            dsh_session::types::SessionId::from_raw("s0"),
            None,
            None,
        )
        .unwrap())
    }

    fn registry() -> Rc<ToolRegistry> {
        let r = Rc::new(ToolRegistry::new(ToolExecutionMode::Native));
        for name in ["bash", "read"] {
            let def = define_tool(DefineToolOptions {
                name: name.to_string(),
                description: format!("{name} a value"),
                parameters: json!({}),
                output_schema: json!({ "type": "json" }),
                render: Rc::new(|_, v| vec![ContentBlock::text(v.to_string())]),
                execute: Rc::new(|_, _| Ok(json!({"ran": true}))),
                ..Default::default()
            })
            .unwrap();
            r.register_global(Rc::new(def)).unwrap();
        }
        r
    }

    fn block(id: &str, name: &str) -> ToolCallBlock {
        ToolCallBlock {
            id: CallId::from_raw(id),
            name: name.into(),
            arguments: "{}".into(),
        }
    }

    fn enter_plan(s: &Rc<Session>) {
        s.append(
            EventKind::PlanMode,
            json!({ "active": true }),
            None,
        )
        .unwrap();
    }

    fn ask(s: &Rc<Session>, call_id: &str) {
        s.append(EventKind::ApprovalAsked, json!({ "toolCallId": call_id }), None)
            .unwrap();
    }

    fn decide(s: &Rc<Session>, call_id: &str, decision: &str) {
        s.append(
            EventKind::ApprovalDecided,
            json!({ "toolCallId": call_id, "decision": decision }),
            None,
        )
        .unwrap();
    }

    fn ctx(turn: u64, step: u64, calls: &[ToolCallBlock]) -> ToolExecCtx<'_> {
        ToolExecCtx {
            turn,
            step,
            tool_calls: calls,
            resume: Vec::new(),
        }
    }

    fn asked_call_ids(s: &Rc<Session>) -> Vec<String> {
        s.events()
            .into_iter()
            .filter(|e| e.kind == EventKind::ApprovalAsked)
            .map(|e| e.data["toolCallId"].as_str().unwrap_or("?").to_string())
            .collect()
    }

    #[test]
    fn plan_inactive_is_passthrough_no_pending_no_asked() {
        let s = session();
        let tools = registry();
        let out = approval_tool_exec(
            &s,
            &tools,
            None,
            Some("agent-1"),
            8,
            &ctx(1, 1, &[block("c1", "bash")]),
        );
        assert!(out.pending.is_empty(), "plan 非激活 → 直通");
        assert!(asked_call_ids(&s).is_empty());
    }

    #[test]
    fn plan_active_mutation_pauses_with_asked_and_pending() {
        let s = session();
        enter_plan(&s);
        let tools = registry();
        let out = approval_tool_exec(
            &s,
            &tools,
            None,
            Some("agent-1"),
            8,
            &ctx(1, 1, &[block("c1", "bash"), block("c2", "read")]),
        );
        // bash 是 mutation → pending；read 不是 → 不拦。
        assert_eq!(out.pending.len(), 1, "只 pending mutation");
        assert_eq!(out.pending[0].block.id, CallId::from_raw("c1"));
        assert_eq!(out.pending[0].block.name, "bash");
        assert_eq!(asked_call_ids(&s), vec!["c1".to_string()]);
        // tool/call：bash(pending) + read(直通，仍执行) = 2；tool/result：仅 read = 1。
        let calls = s
            .events()
            .into_iter()
            .filter(|e| e.kind == EventKind::ToolCall)
            .count();
        assert_eq!(calls, 2);
        let results: Vec<_> = s
            .events()
            .into_iter()
            .filter(|e| e.kind == EventKind::ToolResult)
            .collect();
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].data["message"]["content"][0]["toolCallId"],
            json!("c2"),
            "直通 read 正常执行"
        );
    }

    #[test]
    fn resume_allowed_once_executes_tool_result_only() {
        let s = session();
        // 先模拟暂停步已落 call + asked。
        let pending = emit_pending_calls(&s, 1, 1, &[block("c1", "bash")]);
        ask(&s, "c1");
        // 用户允许（一次性）。
        decide(&s, "c1", DECISION_ALLOWED_ONCE);
        let resume_ctx = ToolExecCtx {
            turn: 2,
            step: 1,
            tool_calls: &[],
            resume: pending,
        };
        let tools2 = registry();
        let out = approval_tool_exec(&s, &tools2, None, Some("agent-1"), 8, &resume_ctx);
        assert!(out.pending.is_empty(), "allowedOnce → 全部放行");
        // 只追 result，不重复 call。
        let calls = s
            .events()
            .into_iter()
            .filter(|e| e.kind == EventKind::ToolCall)
            .count();
        assert_eq!(calls, 1, "恢复不重复 tool/call");
        let results: Vec<_> = s
            .events()
            .into_iter()
            .filter(|e| e.kind == EventKind::ToolResult)
            .collect();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].data["error"], Value::Null, "工具真跑且成功");
    }

    #[test]
    fn resume_rejected_synthesizes_rejection_without_execute() {
        let s = session();
        let tools = registry();
        let pending = emit_pending_calls(&s, 1, 1, &[block("c1", "bash")]);
        ask(&s, "c1");
        decide(&s, "c1", DECISION_REJECTED);
        let resume_ctx = ToolExecCtx {
            turn: 2,
            step: 1,
            tool_calls: &[],
            resume: pending,
        };
        let out = approval_tool_exec(&s, &tools, None, Some("agent-1"), 8, &resume_ctx);
        assert!(out.pending.is_empty());
        let results: Vec<_> = s
            .events()
            .into_iter()
            .filter(|e| e.kind == EventKind::ToolResult)
            .collect();
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].data["error"]["code"],
            json!(dsh_agent_loop::CODE_TOOL_REJECTED)
        );
        assert!(results[0].data["message"]["content"][0]["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .contains("rejected"));
    }

    #[test]
    fn mutation_set_covers_db_list_and_exempts_reads() {
        for name in ["write", "edit", "terminal_open", "terminal_send", "terminal_signal", "bash",
                     "str_replace_editor", "run_code", "schedule_create", "schedule_delete", "job_kill"] {
            assert!(is_mutation(name), "{name} 应属 mutation");
        }
        for name in ["read", "read_image", "glob", "grep", "terminal_read", "terminal_list",
                     "job_list", "job_output", "schedule_list", "todo_write", "goal/create"] {
            assert!(!is_mutation(name), "{name} 应豁免");
        }
    }
}

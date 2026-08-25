//! M4h 补实：subagent 真实进程内驱动（in-process provider 的 web 落地点）。
//!
//! 权威参考：`deepseek-harness/packages/subagent/subagent/src/{list-children,
//! in-process-driver,spawn-in-process,fork-in-process}.ts` 与 `apiproxy/.../subagents.ts`。
//!
//! 关键事实（D-048 已承、M4i 验收补齐/验收 #5）：
//! - 子代理 = 一个**会话自身**（`dsh-session` store 里的真实 Session，带 header
//!   `origin=Subagent` + `parentSession` + `delegationDepth`）；
//! - 身份 = 会话日志里的 `subagent/descriptor` 事件（`rollup` 后经 subagent 投影折叠）；
//! - `list` 是**只读枚举**：不激活 Agent、不 consult provider，直接读 store + 折叠描述符；
//! - `prompt` 才走活驱动：把消息投进 child 的 agent-loop（`AgentLoopHost` followup），
//!   返回真实 `messageId`（本仓同步单线程，followup 即时驱动一轮——fake-loop 即真实
//!    Rust loop 装配 mock adapter 的测试模板）；
//! - `interrupt` fire-and-return 收据（`interrupt_receipt`）。
//!
//! 本模块只依赖 `SessionHost`（store 权威）+ `AgentLoopHost`（可选：未装配时
//! `prompt` fail loud，`list/history` 仍可只读枚举——诚实降级，不伪装成功）。

use crate::session_host::SessionHost;
use dsh_agent_loop::AgentLoopHost;
use dsh_session::types::{CreateSessionMeta, CreateSessionOptions, EventKind, Origin};
use dsh_subagent::{
    category_child, diagnostic_row, fold_descriptor_from_events, interrupt_receipt, prompt_gate,
    resolve_child_depth, snapshot_descriptor, ChildEntry, Descriptor, DescriptorInput,
    PromptAddress, PromptError,
};
use serde_json::{json, Value};
use std::rc::Rc;
use std::sync::Arc;

/// 子代理运行时错误（web 侧转 wire：bad-request / internal 两档）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubagentError {
    /// 前置校验失败（缺参 / 模式不符 / 深度越界 / 源不存在）。
    BadRequest(String),
    /// 运行时失败（宿主未装配 / 会话缺失 / 驱动失败）。
    Internal(String),
}

impl SubagentError {
    pub fn message(&self) -> &str {
        match self {
            SubagentError::BadRequest(m) | SubagentError::Internal(m) => m,
        }
    }
}

/// 一次 spawn 的结果：child 会话 id。
pub type SpawnResult = Result<String, SubagentError>;

/// spawn 选项（对齐 `spawn-in-process` 的组合）。
#[derive(Debug, Clone, Default)]
pub struct SpawnOptions {
    pub mode: SpawnMode,
    pub provider: String,
    pub label: Option<String>,
    pub max_depth: Option<u64>,
    /// continuable 的 agent provider/model（缺省沿用父 agent 值）。
    pub agent_provider: Option<String>,
    pub agent_model: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpawnMode {
    #[default]
    OneShot,
    Continuable,
}

/// mint 唯一 child 会话 id（`sa-<n>`；与 `s{n}` 命名空间分开）。
fn mint_child_id(host: &Arc<SessionHost>) -> String {
    let mut n = 1u64;
    loop {
        let candidate = format!("sa-{n}");
        if !host.is_live(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// 解析父会话 delegationDepth（无会话 → 0）。
fn parent_depth(host: &Arc<SessionHost>, parent: &str) -> u64 {
    host.session(parent)
        .ok()
        .map(|s| s.header().delegation_depth.unwrap_or(0))
        .unwrap_or(0)
}

/// 构造 child 会话已经通过深度预算校验后的 header meta。
fn child_meta(parent: &str, depth: u64) -> CreateSessionMeta {
    CreateSessionMeta {
        parent_session: Some(dsh_brand::SessionId::from_raw(parent.to_string())),
        origin: Some(Origin::Subagent),
        delegation_depth: Some(depth),
        ..Default::default()
    }
}

/// 把 child 会话 id 与描述符写进 store（spawn/fork 共用；描述符事件落日志）。
fn persist_child(
    host: &Arc<SessionHost>,
    parent: &str,
    opts: &SpawnOptions,
    child_id: &str,
) -> Result<(), SubagentError> {
    // 深度预算：childDepth = max(header, runtime) + 1（无 runtime 标注 → 仅 header）。
    let depth = resolve_child_depth(Some(parent_depth(host, parent)), None)
        .map_err(|e| SubagentError::BadRequest(e.to_string()))?;
    if let Some(max) = opts.max_depth {
        if depth > max {
            return Err(SubagentError::BadRequest(format!(
                "subagent depth {depth} exceeds maxDepth {max}"
            )));
        }
    }
    // 创建 child 会话（header origin=subagent + parent + depth）。
    let sid = dsh_brand::SessionId::from_raw(child_id.to_string());
    host.store
        .create(
            Some(sid),
            &CreateSessionOptions {
                seed: None,
                meta: Some(child_meta(parent, depth)),
            },
        )
        .map_err(|e| SubagentError::Internal(e.0.clone()))?;
    // 落描述符事件（durable 身份；subagent 投影 last-wins 折叠）。
    let input = match opts.mode {
        SpawnMode::OneShot => DescriptorInput::OneShot {
            mode: "one-shot".into(),
            provider: opts.provider.clone(),
            label: opts.label.clone(),
        },
        SpawnMode::Continuable => DescriptorInput::Continuable {
            mode: "continuable".into(),
            provider: opts.provider.clone(),
            label: opts
                .label
                .clone()
                .unwrap_or_else(|| "subagent".to_string()),
            agent_provider: opts.agent_provider.clone(),
            agent_model: opts.agent_model.clone(),
            persona: None,
            tool_filter: None,
        },
    };
    let desc = snapshot_descriptor(&input).map_err(SubagentError::Internal)?;
    let data = serde_json::to_value(&desc).unwrap_or(Value::Null);
    host.session(child_id)
        .and_then(|s| {
            s.append(EventKind::SubagentDescriptor, data, None)
                .map_err(|e| e.0)
        })
        .map_err(SubagentError::Internal)?;
    Ok(())
}

/// in-process spawn：mint child 会话 + 描述符；返回 child id。
/// 对齐 `spawn-in-process`（fresh session；不继承父上下文）。
pub fn spawn_child(
    host: &Arc<SessionHost>,
    parent: &str,
    opts: &SpawnOptions,
) -> SpawnResult {
    if parent.is_empty() {
        return Err(SubagentError::BadRequest("spawn requires parentSessionId".into()));
    }
    if opts.provider.is_empty() {
        return Err(SubagentError::BadRequest("subagent provider must be non-empty".into()));
    }
    if opts.mode == SpawnMode::Continuable && opts.label.as_deref().is_none_or(str::is_empty) {
        return Err(SubagentError::BadRequest(
            "continuable spawn requires a label".into(),
        ));
    }
    let child_id = mint_child_id(host);
    persist_child(host, parent, opts, &child_id)?;
    Ok(child_id)
}

/// in-process fork：从源会话 seed 派生 child（继承父上下文），再落描述符。
/// 对齐 `fork-in-process`（`inheritsParentContext=true`）。
pub fn fork_child(
    host: &Arc<SessionHost>,
    parent: &str,
    source: &str,
    opts: &SpawnOptions,
) -> SpawnResult {
    if parent.is_empty() {
        return Err(SubagentError::BadRequest("fork requires parentSessionId".into()));
    }
    if source.is_empty() {
        return Err(SubagentError::BadRequest("fork requires sourceSessionId".into()));
    }
    let depth = resolve_child_depth(Some(parent_depth(host, parent)), None)
        .map_err(|e| SubagentError::BadRequest(e.to_string()))?;
    let child_id = mint_child_id(host);
    // seed = 源会话的既有事件（fork 语义：继承父上下文）。
    let seed = host.events(source);
    let sid = dsh_brand::SessionId::from_raw(child_id.to_string());
    let mut meta = child_meta(parent, depth);
    meta.seed_length = Some(seed.len() as u64);
    host.store
        .create(
            Some(sid),
            &CreateSessionOptions {
                seed: Some(seed),
                meta: Some(meta),
            },
        )
        .map_err(|e| SubagentError::Internal(e.0.clone()))?;
    // 描述符事件（fork 的 durable 身份）。
    let input = DescriptorInput::Continuable {
        mode: "continuable".into(),
        provider: opts.provider.clone(),
        label: opts
            .label
            .clone()
            .unwrap_or_else(|| "fork".to_string()),
        agent_provider: opts.agent_provider.clone(),
        agent_model: opts.agent_model.clone(),
        persona: None,
        tool_filter: None,
    };
    let desc = snapshot_descriptor(&input).map_err(SubagentError::Internal)?;
    let data = serde_json::to_value(&desc).unwrap_or(Value::Null);
    host.session(&child_id)
        .and_then(|s| {
            s.append(EventKind::SubagentDescriptor, data, None)
                .map_err(|e| e.0)
        })
        .map_err(SubagentError::Internal)?;
    Ok(child_id)
}

/// 只读枚举 parent 的直接子代理（真实目录；不激活 Agent）。
///
/// - child 会话：header `origin=Subagent` 且 `parentSession == parent`；
/// - 身份：折叠该 child 日志的 `subagent/descriptor`（last-wins；不可信 → 无值）；
/// - 有身份 → child 行（mode/label/activity=inactive 因本仓纯 store 无运行态标记）；
/// - 有身份但 mode 异常 → diagnostic「corrupt」；
/// - parent 存在 → parentAvailable=true。
pub fn list_children(
    host: &Arc<SessionHost>,
    parent: &str,
) -> (Vec<ChildEntry>, bool) {
    let parent_available = host.is_live(parent);
    let parent_sid = dsh_brand::SessionId::from_raw(parent.to_string());
    let mut rows = Vec::new();
    for session in host.store.list() {
        let h = session.header();
        if h.origin != Some(Origin::Subagent) || h.parent_session.as_ref() != Some(&parent_sid) {
            continue;
        }
        // 折叠描述符：把事件帧喂 fold_descriptor_from_events（含 type+data）。
        let frames: Vec<Value> = session
            .events()
            .iter()
            .map(|e| json!({ "type": e.kind.as_str(), "data": e.data }))
            .collect();
        match fold_descriptor_from_events(&frames) {
            Ok(Some(Descriptor::OneShot { label, .. })) => {
                rows.push(category_child(
                    h.id.raw(),
                    "one-shot",
                    "inactive",
                    false,
                    label,
                ));
            }
            Ok(Some(Descriptor::Continuable { label, .. })) => {
                rows.push(category_child(
                    h.id.raw(),
                    "continuable",
                    "inactive",
                    false,
                    Some(label.clone()),
                ));
            }
            _ => {
                rows.push(diagnostic_row(h.id.raw(), "corrupt"));
            }
        }
    }
    // createdAt 排序（稳定枚举）。
    rows.sort_by_key(|r| r.id.clone());
    (rows, parent_available)
}

/// 读 child 会话事件（分页 read；不激活 Agent）。
/// 返回 `(events, has_more)`；事件是 strict-envelope 形式的 wire 行。
pub fn history(
    host: &Arc<SessionHost>,
    child: &str,
    before_seq: Option<u64>,
    max: Option<usize>,
) -> (Vec<Value>, bool) {
    let all: Vec<Value> = host
        .events(child)
        .iter()
        .filter(|e| before_seq.is_none_or(|b| e.seq < b))
        .map(|e| json!({ "event": serde_json::to_value(e).unwrap_or(Value::Null) }))
        .collect();
    let cap = max.unwrap_or(all.len()).max(1);
    let has_more = all.len() > cap;
    let page = all.into_iter().take(cap).collect();
    (page, has_more)
}

/// prompt 投递：gate（continuable）+ 经 AgentLoopHost followup 驱动 child 一轮，
/// 返回真实 messageId。
///
/// fail loud：未装配 agent-loop / 非 continuable / child 不可投递 → 错误（绝不伪装
/// 成功）。同步单线程：followup 即时驱动到本轮结束（fake-loop 即 mock adapter 驱动
/// 真实 Rust loop 的测试模板）。
pub fn prompt(
    host: &Arc<SessionHost>,
    agent_loop: &Option<Rc<AgentLoopHost>>,
    parent: &str,
    child: &str,
    content: &str,
) -> Result<String, SubagentError> {
    let addr = PromptAddress {
        parent_session_id: parent.to_string(),
        child_session_id: child.to_string(),
        mode: "continuable".to_string(),
    };
    if let Err(PromptError::NotContinuable) = prompt_gate(&addr) {
        return Err(SubagentError::BadRequest(
            "subagent.prompt requires a continuable child".into(),
        ));
    }
    let loop_host = agent_loop
        .as_ref()
        .ok_or_else(|| SubagentError::Internal("no Rust AgentLoopHost assembled".into()))?;
    // 从 child 会话的 durable 描述符解析 agent provider/model（fail loud：无描述符 /
    // 结构坏 → 明确错误，绝不伪装）。
    let sid = dsh_brand::SessionId::from_raw(child.to_string());
    let session = host
        .store
        .get(&sid)
        .ok_or_else(|| SubagentError::Internal(format!("child session \"{child}\" not live")))?;
    let frames: Vec<Value> = session
        .events()
        .iter()
        .map(|e| json!({ "type": e.kind.as_str(), "data": e.data }))
        .collect();
    let (agent_provider, agent_model): (String, String) =
        match fold_descriptor_from_events(&frames) {
            Ok(Some(Descriptor::Continuable {
                agent_provider,
                agent_model,
                provider,
                ..
            })) => (
                agent_provider.clone().unwrap_or_else(|| provider.clone()),
                agent_model
                    .clone()
                    .unwrap_or_else(|| "default".to_string()),
            ),
            Ok(Some(Descriptor::OneShot { .. })) => {
                return Err(SubagentError::BadRequest(
                    "subagent.prompt requires a continuable child".into(),
                ));
            }
            _ => {
                return Err(SubagentError::Internal(format!(
                    "child session \"{child}\" has no valid subagent/descriptor"
                )));
            }        };
    // child agent：与 SessionHost 共享 store（web 集成时同一个 store），装配 child
    // 会话的配置 agent（session_id = child；provider/model 来自描述符）→ followup
    // 驱动一轮（同步单线程；fake-loop = mock adapter 驱动真实 Rust loop）。
    let configured = dsh_agent_loop::ConfiguredAgent {
        id: format!("sa-agent-{child}"),
        provider: Some(agent_provider.clone()),
        model: Some(agent_model.clone()),
        session_id: Some(child.to_string()),
        max_tokens: None,
        cwd: None,
        resume_session_id: None,
    };
    loop_host.ensure_agent(&configured).map_err(SubagentError::Internal)?;
    // messageId 与投递的消息一致（真实 id；遵循本仓 MessageId 构造惯例）。
    let message_id = format!("pmsg-{child}:{}", session.seq() + 1);
    let message = dsh_llm::Message::user(
        dsh_llm::MessageId::from_raw(message_id.clone()),
        vec![dsh_llm::ContentBlock::text(content)],
    );
    loop_host
        .followup(&configured.id, message)
        .map_err(SubagentError::Internal)?;
    Ok(message_id)
}

/// interrupt 收据（fire-and-return：调用即返，不等待 child 关停）。
pub fn interrupt(parent: &str, child: &str) -> bool {
    interrupt_receipt(&dsh_subagent::InterruptAddress {
        parent_session_id: parent.to_string(),
        child_session_id: child.to_string(),
        mode: "continuable".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host_with_default() -> Arc<SessionHost> {
        let host = SessionHost::in_memory();
        let _ = host.session("default");
        host
    }

    #[test]
    fn spawn_child_mints_subagent_session_with_descriptor() {
        let host = host_with_default();
        let opts = SpawnOptions {
            mode: SpawnMode::Continuable,
            provider: "mock".into(),
            label: Some("audit".into()),
            ..Default::default()
        };
        let child = spawn_child(&host, "default", &opts).expect("spawn ok");
        assert!(child.starts_with("sa-"), "child id namespace");
        assert!(host.is_live(&child));
        // header：origin=Subagent + parentSession=default + delegationDepth=1。
        let s = host.session(&child).expect("child live");
        let h = s.header();
        assert_eq!(
            h.origin,
            Some(Origin::Subagent),
            "child session marked origin=subagent"
        );
        assert_eq!(
            h.parent_session.as_ref().map(|i| i.raw()),
            Some("default")
        );
        assert_eq!(h.delegation_depth, Some(1));
        // 描述符事件落日志（subagent 投影可折叠）。
        let frames: Vec<Value> = s
            .events()
            .iter()
            .map(|e| json!({ "type": e.kind.as_str(), "data": e.data }))
            .collect();
        let desc = fold_descriptor_from_events(&frames).ok().flatten();
        assert!(matches!(desc, Some(Descriptor::Continuable { label, .. }) if label == "audit"));
    }

    #[test]
    fn spawn_one_shot_rejects_missing_provider_or_label_rules() {
        let host = host_with_default();
        // 空 provider → BadRequest
        let opts = SpawnOptions {
            mode: SpawnMode::OneShot,
            provider: String::new(),
            label: None,
            ..Default::default()
        };
        assert_eq!(
            spawn_child(&host, "default", &opts),
            Err(SubagentError::BadRequest(
                "subagent provider must be non-empty".into()
            ))
        );
        // continuable 缺 label → BadRequest
        let opts = SpawnOptions {
            mode: SpawnMode::Continuable,
            provider: "mock".into(),
            label: None,
            ..Default::default()
        };
        assert!(matches!(
            spawn_child(&host, "default", &opts),
            Err(SubagentError::BadRequest(_))
        ));
    }

    #[test]
    fn spawn_respects_max_depth() {
        let host = host_with_default();
        let opts = SpawnOptions {
            mode: SpawnMode::OneShot,
            provider: "mock".into(),
            label: None,
            max_depth: Some(0),
            ..Default::default()
        };
        // childDepth=1 > maxDepth=0 → BadRequest。
        assert!(matches!(
            spawn_child(&host, "default", &opts),
            Err(SubagentError::BadRequest(msg)) if msg.contains("maxDepth")
        ));
    }

    #[test]
    fn list_only_returns_direct_subagent_children() {
        let host = host_with_default();
        // 造两个 child（continuable + one-shot）+ 一个普通 sessions（s1）。
        let _ = host.session("s1");
        let o1 = SpawnOptions {
            mode: SpawnMode::Continuable,
            provider: "mock".into(),
            label: Some("alpha".into()),
            ..Default::default()
        };
        let o2 = SpawnOptions {
            mode: SpawnMode::OneShot,
            provider: "mock".into(),
            label: Some("beta".into()),
            ..Default::default()
        };
        let c1 = spawn_child(&host, "default", &o1).expect("spawn1");
        let c2 = spawn_child(&host, "default", &o2).expect("spawn2");
        let (rows, parent_available) = list_children(&host, "default");
        assert!(parent_available, "parent live → available");
        assert_eq!(rows.len(), 2, "两 child 行，普通 s1 被排除");
        let kinds: Vec<_> = rows.iter().map(|r| r.mode.as_str()).collect();
        assert!(kinds.contains(&"continuable"));
        assert!(kinds.contains(&"one-shot"));
        // 从非父会话列 → 空且 parentAvailable=false
        let (rows2, avail2) = list_children(&host, "nope");
        assert!(rows2.is_empty());
        assert!(!avail2);
        assert!(host.is_live(&c1) && host.is_live(&c2));
    }

    #[test]
    fn history_pages_and_reports_has_more() {
        let host = host_with_default();
        let opts = SpawnOptions {
            mode: SpawnMode::OneShot,
            provider: "mock".into(),
            label: None,
            ..Default::default()
        };
        let c = spawn_child(&host, "default", &opts).expect("spawn");
        // 再落一条 turn/end 使 child 日志 > 1 条（分页语义可验证）。
        let _ = host
            .session(&c)
            .and_then(|s| {
                s.append(
                    EventKind::TurnEnd,
                    json!({ "turn": 1, "reason": "completed" }),
                    None,
                )
                .map_err(|e| e.0)
            })
            .expect("append turn/end");
        // 分页 size=1 → has_more=true。
        let (page, has_more) = history(&host, &c, None, Some(1));
        assert_eq!(page.len(), 1);
        assert!(has_more, "总事件>1 → has_more");
        // beforeSeq 过滤：strict < beforeSeq。max_seq=1 → 不含 seq0(descriptor) 之外；
        // beforeSeq=0 → 空（无 seq < 0）。
        let max_seq = host.events(&c).iter().map(|e| e.seq).max().unwrap_or(0);
        let (page2, _) = history(&host, &c, Some(max_seq), None);
        assert_eq!(page2.len(), 1, "beforeSeq=当前最大 seq → 只剩更早事件");
        let (page3, _) = history(&host, &c, Some(0), None);
        assert!(page3.is_empty(), "beforeSeq=0 → 无 seq<0");
    }

    #[test]
    fn prompt_requires_continuable_and_agent_loop() {
        let host = host_with_default();
        // 无 agent-loop → Internal（fail loud，不伪装成功）。
        let err = prompt(&host, &None, "default", "sa-x", "hi");
        assert!(matches!(err, Err(SubagentError::Internal(msg)) if msg.contains("AgentLoopHost")));
        // 非 continuable mode（由调用方传 mode=one-shot）→ BadRequest。
        let addr = PromptAddress {
            parent_session_id: "default".into(),
            child_session_id: "sa-x".into(),
            mode: "one-shot".into(),
        };
        assert_eq!(
            prompt_gate(&addr),
            Err(PromptError::NotContinuable)
        );
    }

    #[test]
    fn interrupt_is_fire_and_return_receipt() {
        assert!(interrupt("default", "sa-x"));
    }
}


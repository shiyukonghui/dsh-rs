//! M2e-3：RuntimeContextProjection 测试——retained 三态、CLEARED、快照去重、
//! surface-replacement 后无 retained、form Snapshot 携带。

use std::sync::Arc;

use dsh_agent_loop::{RuntimeContextProjection, RUNTIME_CONTEXT_CLEARED};
use dsh_llm::{
    ContextForm, ContentBlock, Message, MessageId, MessageSource, PluginMessageSource, Role,
};
use dsh_session::{
    types::SurfaceIntent, EventKind, Session, SessionId, SurfaceOp,
};
use serde_json::json;

pub const SOURCE: &str = "@deepseek-ai/dsh-system-prompt";

fn session() -> Arc<Session> {
    Arc::new(Session::create(SessionId::from_raw("s0"), None, None).unwrap())
}

fn section(name: &str, text: &str) -> dsh_llm::ContextSnapshotSection {
    dsh_llm::ContextSnapshotSection {
        name: name.into(),
        text: text.into(),
    }
}

fn owned_message(id: &str, text: &str, sections: &[dsh_llm::ContextSnapshotSection]) -> Message {
    let source = if sections.is_empty() {
        PluginMessageSource::new(SOURCE)
    } else {
        PluginMessageSource::new(SOURCE)
            .with_form(ContextForm::Snapshot {
                sections: sections.to_vec(),
            })
    };
    Message {
        id: MessageId::from_raw(id),
        role: Role::User,
        content: vec![ContentBlock::text(text)],
        source: MessageSource::Plugin(source),
    }
}

fn append_user(s: &Arc<Session>, msg: Message, replace: Option<(u64, u64)>) -> u64 {
    let surface = match replace {
        Some((start, end)) => SurfaceIntent {
            surface_op: SurfaceOp::Replace { start, end },
            source_event_seqs: Some(vec![start]),
        },
        None => SurfaceIntent {
            surface_op: SurfaceOp::Append,
            source_event_seqs: None,
        },
    };
    s.append(
        EventKind::UserMessage,
        serde_json::to_value(msg).unwrap(),
        Some(&surface),
    )
    .unwrap()
    .seq
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[test]
fn never_retained_and_empty_current_projects_nothing() {
    let s = session();
    let mut p = RuntimeContextProjection::new();
    assert_eq!(p.debug_retained(), json!("never"));
    assert!(p.project(&s, "", &[]).is_none());
    // 依旧 never（未写出任何东西）
    assert_eq!(p.debug_retained(), json!("never"));
}

#[test]
fn first_change_writes_snapshot_message() {
    let s = session();
    let mut p = RuntimeContextProjection::new();
    let sections = vec![section("files", "a=1")];
    let Some(msg) = p.project(&s, "Current runtime context.\n\na=1", &sections) else {
        panic!("expected a snapshot message");
    };
    assert_eq!(msg.role, Role::User);
    assert_eq!(msg.content[0], ContentBlock::text("Current runtime context.\n\na=1"));
    let MessageSource::Plugin(ps) = &msg.source else {
        panic!("plugin source expected");
    };
    assert_eq!(ps.plugin, SOURCE);
    assert!(matches!(ps.form, Some(ContextForm::Snapshot { .. })));
    // 新快照应被会话接受
    let seq = append_user(&s, msg, None);

    // retained 已建立；同一内容不再投影
    assert!(p.project(&s, "Current runtime context.\n\na=1", &sections).is_none());
    // 日志权威地保留它
    assert_eq!(p.debug_retained(), json!({ "seq": seq, "text": "Current runtime context.\n\na=1" }));
}

#[test]
fn changed_snapshot_writes_new_message_and_updates_retained() {
    let s = session();
    let mut p = RuntimeContextProjection::new();
    let first = p.project(&s, "ctx1", &[section("s", "x")]).unwrap();
    append_user(&s, first, None);
    let second = p.project(&s, "ctx2", &[section("s", "y")]).unwrap();
    assert_eq!(second.content[0], ContentBlock::text("ctx2"));
    let MessageSource::Plugin(ps) = &second.source else {
        panic!();
    };
    let Some(ContextForm::Snapshot { sections }) = &ps.form else {
        panic!("snapshot form expected on second");
    };
    assert_eq!(sections[0].name, "s");
    assert_eq!(sections[0].text, "y");
    append_user(&s, second, None);
}

#[test]
fn cleared_when_context_empties() {
    let s = session();
    let mut p = RuntimeContextProjection::new();
    let snapshot = p.project(&s, "ctxA", &[section("s", "x")]).unwrap();
    append_user(&s, snapshot, None);
    // 上下文变空 → 写 CLEARED（保留历史快照不再适用）
    let cleared = p.project(&s, "", &[]).unwrap();
    assert_eq!(cleared.content[0], ContentBlock::text(RUNTIME_CONTEXT_CLEARED));
    append_user(&s, cleared, None);
    // 再次空 → 无投影
    assert!(p.project(&s, "", &[]).is_none());
    // 重新有值 → 新快照
    let again = p.project(&s, "ctxB", &[section("s", "z")]).unwrap();
    assert_eq!(again.content[0], ContentBlock::text("ctxB"));
}

#[test]
fn owned_user_messages_ignored_by_projection() {
    // 任意普通 user message 不参与 retained 推导。
    let s = session();
    append_user(&s, Message::user(MessageId::from_raw("u0"), vec![ContentBlock::text("hello")]), None);
    let mut p = RuntimeContextProjection::new();
    assert!(p.project(&s, "", &[]).is_none());
    assert_eq!(p.debug_retained(), json!("never"));
}

#[test]
fn reconcile_reflects_replacement_removing_snapshot() {
    let s = session();
    let mut p = RuntimeContextProjection::new();
    let snapshot = p.project(&s, "ctxA", &[section("s", "a")]).unwrap();
    let seq = append_user(&s, snapshot, None);
    // 触发 reconcile：日志权威地保留它
    assert!(p.project(&s, "ctxA", &[section("s", "a")]).is_none());
    assert_eq!(p.debug_retained(), json!({ "seq": seq, "text": "ctxA" }));

    // 一个替换事件覆盖该快照节点（内容不同但同样来自插件 SOURCE 无 sections → CLEARED）；
    // 这里用真实的 CLEARED 替换（与 TS surface-replacement 一致）。
    let cleared = owned_message("replacement", RUNTIME_CONTEXT_CLEARED, &[]);
    let repl_seq = append_user(&s, cleared, Some((seq, seq)));

    // reconcile 后：retained 仍是日志中的最后一个 owned 消息（CLEARED 自身）
    let _ = p.project(&s, "", &[]);
    assert_eq!(
        p.debug_retained(),
        json!({ "seq": repl_seq, "text": RUNTIME_CONTEXT_CLEARED })
    );
    // 已是最新 → 无重复投影
    assert!(p.project(&s, "", &[]).is_none());
}

#[test]
fn source_constant_and_clear_text_match_reference() {
    assert_eq!(SOURCE, "@deepseek-ai/dsh-system-prompt");
    // join_context_sections 空主体时无前缀，因此 current 为空串是对齐契约。
    assert_eq!(
        RUNTIME_CONTEXT_CLEARED,
        "Current runtime context: none. Earlier runtime-context snapshots no longer apply."
    );
}

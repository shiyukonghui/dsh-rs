//! runtime-context 投影（对齐 `runtime-context.ts`；sync 差值见 D-033）。
//!
//! 跟踪最后一个 retained 的 dynamic runtime-context 快照，不拥有其提交。每 turn 的
//! pre-step 把最新渲染的 context 作为 `user/message`（source `@deepseek-ai/dsh-system-prompt`
//! 插件）投影进历史尾。sync 差值：无事件订阅——retained 每次 `project` 前从 session 日志
//! 权威重派生（日志即权威，见 THEOREM），等价 TS 构造扫描 + 事件跟随的最终状态。

use std::cell::Cell;
use std::sync::Arc;

use dsh_llm::{
    ContentBlock, ContextForm, ContextSnapshotSection, Message, MessageId, MessageSource,
    PluginMessageSource, Role,
};
use dsh_session::{EventKind, Session};
use serde_json::Value;

/// system-prompt 插件的 source 标记。
pub const SOURCE: &str = "@deepseek-ai/dsh-system-prompt";
/// 清空标记文本（保留历史快照不再适用）。
pub const CLEARED: &str =
    "Current runtime context: none. Earlier runtime-context snapshots no longer apply.";

fn is_owned(msg: &Message) -> bool {
    matches!(&msg.source, MessageSource::Plugin(p) if p.plugin == SOURCE)
}

/// 单文本块且仅一个内容块 → 文本；否则 None。
fn text_of(msg: &Message) -> Option<String> {
    if msg.content.len() == 1 {
        if let ContentBlock::Text(t) = &msg.content[0] {
            return Some(t.text().to_string());
        }
    }
    None
}

/// `RuntimeContextProjection`：`None` = 从未存在快照；`Some(None)` = 无 retained；
/// `Some(Some((seq, text)))` = 当前 retained。
pub struct RuntimeContextProjection {
    retained: Option<Option<(u64, Option<String>)>>,
    id_seq: Cell<u64>,
}

impl Default for RuntimeContextProjection {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeContextProjection {
    pub fn new() -> Self {
        RuntimeContextProjection {
            retained: None,
            id_seq: Cell::new(0),
        }
    }

    /// 从 session 日志权威重派生 retained（后向找最后一个 owned 且仍在 surface 的
    /// user/message；owned 存在但不在 surface → 无 retained）。
    pub fn reconcile(&mut self, session: &Arc<Session>) {
        let surface: Vec<u64> = session.surface_nodes().unwrap_or_default();
        let mut found_owned = false;
        for event in session.events().into_iter().rev() {
            if event.kind != EventKind::UserMessage {
                continue;
            }
            let Ok(msg) = serde_json::from_value::<Message>(event.data.clone()) else {
                continue;
            };
            if !is_owned(&msg) {
                continue;
            }
            found_owned = true;
            if surface.contains(&event.seq) {
                self.retained = Some(Some((event.seq, text_of(&msg))));
                return;
            }
        }
        self.retained = if found_owned { Some(None) } else { None };
    }

    /// 仅当 retained 值变化时创建一个未提交快照。
    ///
    /// `current` 为完整渲染的动态 context（含前缀）；`sections` 为构成该快照的具名贡献。
    pub fn project(
        &mut self,
        session: &Arc<Session>,
        current: &str,
        sections: &[ContextSnapshotSection],
    ) -> Option<Message> {
        self.reconcile(session);
        if self.retained.is_none() && current.is_empty() {
            return None;
        }
        let snapshot = if current.is_empty() {
            CLEARED.to_string()
        } else {
            current.to_string()
        };
        let retained_text = self
            .retained
            .as_ref()
            .and_then(|r| r.as_ref())
            .and_then(|(_, t)| t.clone())
            .unwrap_or_default();
        if retained_text == snapshot {
            return None;
        }
        let id_seq = self.id_seq.get();
        self.id_seq.set(id_seq + 1);
        let source = MessageSource::Plugin(if sections.is_empty() {
            PluginMessageSource::new(SOURCE)
        } else {
            PluginMessageSource::new(SOURCE)
                .with_form(ContextForm::Snapshot { sections: sections.to_vec() })
        });
        Some(Message {
            id: MessageId::from_raw(format!("runtime-context-{id_seq}")),
            role: Role::User,
            content: vec![ContentBlock::text(snapshot)],
            source,
        })
    }
}

/// `RuntimeContextProjection` 的公开 getter（测试锚定用）。
impl RuntimeContextProjection {
    pub fn retained(&self) -> Option<Option<(u64, Option<String>)>> {
        self.retained.clone()
    }
    pub fn debug_retained(&self) -> Value {
        match &self.retained {
            None => Value::String("never".into()),
            Some(None) => Value::String("none".into()),
            Some(Some((seq, text))) => serde_json::json!({ "seq": seq, "text": text }),
        }
    }
}

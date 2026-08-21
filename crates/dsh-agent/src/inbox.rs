//! Inbox：pending 消息双队列的 **durable 投影**（对齐报告 §A.3 Inbox）。
//!
//! 设计要点：
//! - durable 先提交：`session.append('agent/inbox/spliced', splice)` 先于投影变更；
//!   `0 删 0 插` 不写事件。
//! - splice 坐标用 f64 算术按 JS `Array.prototype.splice` 语义复刻（`Math.trunc`/
//!   NaN→0/负坐标/上界截断）。
//! - 构造时重放 `header.seedLength` 之后的持久 splice（错误包装逐字）。

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use dsh_llm::{Message, MessageId};
use dsh_session::{EventKind, Session};

use crate::types::{InboxSpliceOutcome, InboxTarget};

// ---------------------------------------------------------------------------
// Wire + 通知
// ---------------------------------------------------------------------------

/// `agent/inbox/spliced` 的 durable wire 形状（可选字段缺省即省略）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxSpliceRecord {
    pub target: InboxTarget,
    /// 标准化后的坐标。
    pub start: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed_count: Option<u64>,
    pub inserted: Vec<Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<InboxSpliceOutcome>,
}

/// 便捷构造（`inbox_splice` 对齐 TS 侧 splice 记录的请求形态；removedCount 仅
/// 当 >0 时出现在 wire 上——与 fold 的「removedCount 缺省 = 纯插入」口径一致）。
pub fn inbox_splice(
    target: InboxTarget,
    start: u64,
    removed_count: u64,
    inserted: Vec<Message>,
    outcome: Option<InboxSpliceOutcome>,
) -> InboxSpliceRecord {
    InboxSpliceRecord {
        target,
        start,
        removed_count: (removed_count > 0).then_some(removed_count),
        inserted,
        outcome,
    }
}

/// live 通知（对齐 TS Inbox notifications 三素：inserted/discarded/claimed）。
#[derive(Debug, Clone, PartialEq)]
pub enum InboxNotification {
    Claimed { message: Message, turn: u64 },
    Discarded { message: Message },
    Inserted { message: Message },
}

/// 通知接收器（M2d-2 将在此发射 agent/inbox/inserted 等 live 事件）。
pub type InboxNotify = Rc<dyn Fn(&InboxNotification)>;

/// 一次 mutate 的投影结果（被移除的消息）。
#[derive(Debug, Default, Clone)]
pub struct Mutation {
    pub removed: Vec<Message>,
}

/// `claim(target, turn)` 的结果：next-step 全取 +（按需）队首 1 条 turn。
#[derive(Debug, Clone)]
pub struct ClaimResult {
    next_steps: Vec<Message>,
    next_turn_front: Option<Message>,
}

impl ClaimResult {
    pub fn next_steps(&self) -> &[Message] {
        &self.next_steps
    }
    pub fn next_turn_front(&self) -> Option<Message> {
        self.next_turn_front.clone()
    }
}

// ---------------------------------------------------------------------------
// Inbox
// ---------------------------------------------------------------------------

struct InboxInner {
    session: Rc<Session>,
    next_turn: Vec<Message>,
    next_step: Vec<Message>,
    notify: InboxNotify,
}

pub struct Inbox {
    inner: Rc<RefCell<InboxInner>>,
}

impl Inbox {
    /// 用会话日志重建投影。仅重放 `seed_length` 之后的持久 splice。
    pub fn new(session: Rc<Session>) -> Result<Self, String> {
        Self::with_notify(session, Rc::new(|_| {}))
    }

    pub fn with_notify(session: Rc<Session>, notify: InboxNotify) -> Result<Self, String> {
        let seed_length = session.header().seed_length.unwrap_or(0) as usize;
        let mut inner = InboxInner {
            session,
            next_turn: Vec::new(),
            next_step: Vec::new(),
            notify,
        };
        let events = inner.session.events();
        for event in events.iter().skip(seed_length) {
            if event.kind != EventKind::AgentInboxSpliced {
                continue;
            }
            let rec = serde_json::from_value::<InboxSpliceRecord>(event.data.clone())
                .map_err(|_| {
                    format!("invalid persisted inbox splice at session seq {}", event.seq)
                })?;
            apply_persisted(&mut inner, &rec).map_err(|_| {
                format!("invalid persisted inbox splice at session seq {}", event.seq)
            })?;
        }
        Ok(Inbox {
            inner: Rc::new(RefCell::new(inner)),
        })
    }

    // ---- getters ----

    pub fn next_turn(&self) -> Vec<Message> {
        self.inner.borrow().next_turn.clone()
    }

    pub fn next_step(&self) -> Vec<Message> {
        self.inner.borrow().next_step.clone()
    }

    pub fn has_pending(&self) -> bool {
        let b = self.inner.borrow();
        !b.next_turn.is_empty() || !b.next_step.is_empty()
    }

    // ---- 公开操作 ----

    /// `clear()`：先 next-step 后 next-turn（各为 canceled 删除）。
    pub fn clear(&self) -> Result<(), String> {
        let step_len = self.inner.borrow().next_step.len() as f64;
        self.mutate(InboxTarget::NextStep, 0.0, step_len, Vec::new(), true)?;
        let turn_len = self.inner.borrow().next_turn.len() as f64;
        self.mutate(InboxTarget::NextTurn, 0.0, turn_len, Vec::new(), true)?;
        Ok(())
    }

    /// `claim(target, turn)`：取走全部 next-step；按需再取队首 1 条 turn。
    pub fn claim(&self, target: InboxTarget, turn: u64) -> Result<ClaimResult, String> {
        let step_len = self.inner.borrow().next_step.len() as f64;
        let next_steps = self
            .mutate(InboxTarget::NextStep, 0.0, step_len, Vec::new(), false)?
            .removed;
        let mut next_turn_front = None;
        if target == InboxTarget::NextTurn {
            next_turn_front = self
                .mutate(InboxTarget::NextTurn, 0.0, 1.0, Vec::new(), false)?
                .removed
                .into_iter()
                .next();
        }
        // claimed 通知：next-step 批次先、队首 turn 后（逐条）
        let notify = self.inner.borrow().notify.clone();
        for m in &next_steps {
            notify(&InboxNotification::Claimed {
                message: m.clone(),
                turn,
            });
        }
        if let Some(m) = &next_turn_front {
            notify(&InboxNotification::Claimed {
                message: m.clone(),
                turn,
            });
        }
        Ok(ClaimResult {
            next_steps,
            next_turn_front,
        })
    }

    /// `splice(target, start, deleteCount, inserted)`（discardRemoved=true）。
    pub fn splice(
        &self,
        target: InboxTarget,
        start: f64,
        delete_count: f64,
        inserted: Vec<Message>,
    ) -> Result<Mutation, String> {
        self.mutate(target, start, delete_count, inserted, true)
    }

    /// `append`：追加到队尾。
    pub fn append_msg(&self, target: InboxTarget, message: Message) -> Result<(), String> {
        let len = queue_length(&self.inner.borrow(), target) as f64;
        self.mutate(target, len, 0.0, vec![message], true)?;
        Ok(())
    }

    /// `prepend`：插入队首。
    pub fn prepend_msg(&self, target: InboxTarget, message: Message) -> Result<(), String> {
        self.mutate(target, 0.0, 0.0, vec![message], true)?;
        Ok(())
    }

    /// `replace(messageId, newMessage)`：跨双队列按身份替换；找不到返回 false。
    pub fn replace(&self, message_id: &MessageId, new_message: Message) -> Result<bool, String> {
        let found = find_id(&self.inner.borrow(), message_id);
        let Some((target, idx)) = found else {
            return Ok(false);
        };
        self.mutate(target, idx as f64, 1.0, vec![new_message], true)?;
        Ok(true)
    }

    /// `remove(messageId)`：跨双队列按身份删除；找不到返回 false。
    pub fn remove(&self, message_id: &MessageId) -> Result<bool, String> {
        let found = find_id(&self.inner.borrow(), message_id);
        let Some((target, idx)) = found else {
            return Ok(false);
        };
        self.mutate(target, idx as f64, 1.0, Vec::new(), true)?;
        Ok(true)
    }

    // ---- mutate 核心（JS Array.splice 语义） ----

    /// `mutate(target, start, deleteCount, inserted, discardRemoved)`：
    /// 标准化 → （非 no-op 时）身份唯一校验 → durable append → 投影 → 通知。
    pub fn mutate(
        &self,
        target: InboxTarget,
        start: f64,
        delete_count: f64,
        inserted: Vec<Message>,
        discard_removed: bool,
    ) -> Result<Mutation, String> {
        let mut inner = self.inner.borrow_mut();
        let len = queue_length(&inner, target);        let actual_start = normalize_start(start, len);
        let actual_delete = normalize_delete(delete_count, len - actual_start);

        // 0 删 0 插 → 不写事件
        if actual_delete == 0 && inserted.is_empty() {
            return Ok(Mutation::default());
        }

        let record = InboxSpliceRecord {
            target,
            start: actual_start as u64,
            removed_count: (actual_delete > 0).then_some(actual_delete as u64),
            inserted: inserted.clone(),
            outcome: if discard_removed && actual_delete > 0 {
                Some(InboxSpliceOutcome::Canceled)
            } else {
                None
            },
        };

        // 身份唯一（投影后跨两队列全局）
        assert_unique_after_project(&inner, target, actual_start, actual_delete, &inserted)?;

        // durable 先提交，随后投影才变更
        let value = serde_json::to_value(&record).map_err(|e| e.to_string())?;
        inner
            .session
            .append(EventKind::AgentInboxSpliced, value, None)
            .map_err(|e| e.0)?;

        let removed: Vec<Message> = {
            let queue = queue_mut(&mut inner, target);
            queue
                .splice(actual_start..actual_start + actual_delete, inserted)
                .collect()
        };

        let notify = inner.notify.clone();
        if discard_removed {
            for m in &removed {
                notify(&InboxNotification::Discarded { message: m.clone() });
            }
        }
        for m in &record.inserted {
            notify(&InboxNotification::Inserted { message: m.clone() });
        }

        Ok(Mutation { removed })
    }
}

// ---------------------------------------------------------------------------
// 私有翻译
// ---------------------------------------------------------------------------

fn normalize_start(start: f64, len: usize) -> usize {
    let truncated = start.trunc();
    let offset = if truncated.is_nan() { 0.0 } else { truncated };
    if offset < 0.0 {
        ((len as f64) + offset).max(0.0) as usize
    } else {
        (offset.min(len as f64)) as usize
    }
}

fn normalize_delete(delete_count: f64, remaining: usize) -> usize {
    let truncated = delete_count.trunc();
    let t = if truncated.is_nan() { 0.0 } else { truncated };
    (t.max(0.0) as usize).min(remaining)
}

fn queue_length(inner: &InboxInner, target: InboxTarget) -> usize {
    match target {
        InboxTarget::NextTurn => inner.next_turn.len(),
        InboxTarget::NextStep => inner.next_step.len(),
    }
}

fn queue_mut(inner: &mut InboxInner, target: InboxTarget) -> &mut Vec<Message> {
    match target {
        InboxTarget::NextTurn => &mut inner.next_turn,
        InboxTarget::NextStep => &mut inner.next_step,
    }
}

fn find_id(inner: &InboxInner, id: &MessageId) -> Option<(InboxTarget, usize)> {
    if let Some(i) = inner.next_turn.iter().position(|m| &m.id == id) {
        return Some((InboxTarget::NextTurn, i));
    }
    if let Some(i) = inner.next_step.iter().position(|m| &m.id == id) {
        return Some((InboxTarget::NextStep, i));
    }
    None
}

/// 投影后跨两队列身份唯一（对齐 assertUnique）；违反 → `message "<id>" is already pending`。
fn assert_unique_after_project(
    inner: &InboxInner,
    target: InboxTarget,
    start: usize,
    removed: usize,
    inserted: &[Message],
) -> Result<(), String> {
    let mut turn = inner.next_turn.clone();
    let mut step = inner.next_step.clone();
    let affected = match target {
        InboxTarget::NextTurn => &mut turn,
        InboxTarget::NextStep => &mut step,
    };
    let end = (start + removed).min(affected.len());
    affected.splice(start..end, inserted.iter().cloned());
    let mut seen: HashSet<&str> = HashSet::new();
    for m in turn.iter().chain(step.iter()) {
        if !seen.insert(&m.id.0) {
            return Err(format!("message \"{}\" is already pending", m.id.0));
        }
    }
    Ok(())
}

/// 持久重放：校验几何 + 身份唯一并投影（不触发 live 通知——通知是运行时事件）。
fn apply_persisted(inner: &mut InboxInner, rec: &InboxSpliceRecord) -> Result<(), String> {
    let len = queue_length(inner, rec.target);
    let start = rec.start as usize;
    let removed = rec.removed_count.unwrap_or(0) as usize;
    if start > len || start + removed > len {
        return Err("invalid inbox splice".into());
    }
    // 身份唯一（投影后）
    assert_unique_after_project(inner, rec.target, start, removed, &rec.inserted)?;
    let queue = queue_mut(inner, rec.target);
    let end = (start + removed).min(len);
    let _removed: Vec<Message> = queue
        .splice(start..end, rec.inserted.iter().cloned())
        .collect();
    Ok(())
}

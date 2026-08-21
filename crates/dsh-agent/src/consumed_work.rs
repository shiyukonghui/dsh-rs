//! `foldConsumedWork`：从 log 折叠「已消费工作」记账（对齐报告 §A.3 consumed-work）。
//!
//! 设计动机：仅凭 turn/step 无法区分「空 claim」与「被截断的 work」；inbox 自身的
//! `removedCount`/`outcome` 是唯一区分事实（A.3）。

use std::collections::HashSet;

use dsh_session::{EventKind, SessionEvent, TurnEndReason};

use crate::types::ConsumedWork;

/// 单遍折叠：输入即 log 或其 owned 后缀（事件组）。
pub fn fold_consumed_work(events: &[SessionEvent]) -> ConsumedWork {
    let mut stepped: HashSet<u64> = HashSet::new();
    let mut claimed: HashSet<u64> = HashSet::new();
    let mut open: Option<u64> = None;
    let mut end: Option<SessionEvent> = None;
    let mut dropped_unrun = false;

    for event in events {
        match event.kind {
            EventKind::TurnStart => {
                if let Ok(p) = serde_json::from_value::<dsh_session::TurnStartPayload>(event.data.clone()) {
                    open = Some(p.turn);
                }
            }
            EventKind::StepStart => {
                if let Ok(p) = serde_json::from_value::<dsh_session::StepStartPayload>(event.data.clone()) {
                    stepped.insert(p.turn);
                }
            }
            EventKind::AgentInboxSpliced => {
                let data = &event.data;
                let removed_count = data.get("removedCount").and_then(serde_json::Value::as_u64);
                let Some(_removed_count) = removed_count else {
                    // removedCount 缺省（仅插入）→ 忽略，不算 claim
                    continue;
                };
                let outcome = data.get("outcome").and_then(serde_json::Value::as_str);
                let inserted_len = data
                    .get("inserted")
                    .and_then(serde_json::Value::as_array)
                    .map(|a| a.len())
                    .unwrap_or(0);
                if outcome == Some("canceled") {
                    // 替换（inserted 非空）保持 pending，不算 dropped
                    dropped_unrun = dropped_unrun || inserted_len == 0;
                } else if let Some(t) = open {
                    // claim 类（总是 turn 内）
                    claimed.insert(t);
                }
            }
            EventKind::TurnEnd => {
                open = None;
                if let Ok(p) = serde_json::from_value::<dsh_session::TurnEndPayload>(event.data.clone()) {
                    if stepped.remove(&p.turn)
                        || (claimed.contains(&p.turn)
                            && claimed.remove(&p.turn)
                            && accounts_for_claim(&p.reason))
                    {
                        end = Some(event.clone());
                        // 本 turn 关闭前的 drop 由其自身结局报告
                        dropped_unrun = false;
                    }
                }
            }
            _ => {}
        }
    }

    ConsumedWork {
        end,
        dropped_unrun,
    }
}

/// claim 后该 turn 的结局是否「账户成立」。
/// `completed → false`（成账 turn 被清空不记账）；`blocked | aborted | interrupted |
/// error → true`；`default → true`（max-tokens 必然已 stepped 走不到这里；merge-
/// extensible 后端变体也必须按成功读——「无法命名的结局不得读作成功」）。
fn accounts_for_claim(reason: &TurnEndReason) -> bool {
    match reason {
        TurnEndReason::Completed => false,
        TurnEndReason::Blocked
        | TurnEndReason::Aborted { .. }
        | TurnEndReason::Interrupted
        | TurnEndReason::Error { .. } => true,
        _ => true,
    }
}

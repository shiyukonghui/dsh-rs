//! M4g dsh-session-query todo 承载测试（TDD 红-绿）。
//!
//! 对齐 `deepseek-harness/packages/todo/tool-todo/src/index.ts`：
//! - `to_todo_list`：trim 非空唯一 content；并行策略（allowParallelInProgress）下可多
//!   in_progress，否则至多一个；schema 边界已拒未知键。
//! - `todos` 投影 unit：todo/write → 整表；turn/start → null；其余事件保持；view=state。
//! - counts 派生。

use dsh_session::types::{EventKind, SessionEvent, TodoItem, TodoStatus};
use dsh_session_query::todo::{to_todo_list, todo_counts, todos_projection_unit, TodoListError};
use serde_json::json;

fn ev(seq: u64, kind: EventKind, data: serde_json::Value) -> SessionEvent {
    SessionEvent::new(seq, 1000 + seq as i64, kind, data)
}

fn todo_write(seq: u64, todos: serde_json::Value) -> SessionEvent {
    ev(seq, EventKind::TodoWrite, json!({ "todos": todos }))
}

fn turn_start(seq: u64) -> SessionEvent {
    ev(seq, EventKind::TurnStart, json!({}))
}

/// to_todo_list：trim 内容 + 非空校验。
#[test]
fn list_normalizes_and_trims() {
    let list = to_todo_list(
        &[
            json!({ "content": "  build  ", "status": "pending" }),
            json!({ "content": "test", "status": "completed" }),
        ],
        true,
    )
    .expect("ok");
    assert_eq!(list, vec![
        TodoItem { content: "build".into(), status: TodoStatus::Pending },
        TodoItem { content: "test".into(), status: TodoStatus::Completed },
    ]);
}

/// 空 content / 重复 content → 错误。
#[test]
fn list_rejects_empty_and_duplicates() {
    let e1 = to_todo_list(&[json!({ "content": "  ", "status": "pending" })], true);
    assert!(matches!(e1, Err(TodoListError::EmptyContent)));
    let d = to_todo_list(
        &[
            json!({ "content": "x", "status": "pending" }),
            json!({ "content": "x", "status": "pending" }),
        ],
        true,
    );
    assert!(matches!(d, Err(TodoListError::DuplicateContent)));
}

/// 单活动纪律：allow_parallel=false 时 >1 in_progress → 错误；true 则放行。
#[test]
fn parallel_policy_gate() {
    let two_active = vec![
        json!({ "content": "a", "status": "in_progress" }),
        json!({ "content": "b", "status": "in_progress" }),
    ];
    assert!(matches!(to_todo_list(&two_active, false), Err(TodoListError::TooManyInProgress)));
    assert!(to_todo_list(&two_active, true).is_ok());
}

/// 投影 unit 折叠：todo/write → 整表；turn/start → null；无关事件保持。
#[test]
fn todos_projection_fold() {
    let unit = todos_projection_unit();
    let mut state = (unit.init)();
    let events = vec![
        todo_write(0, json!([{ "content": "a", "status": "pending" }])),
        ev(1, EventKind::TurnEnd, json!({})), // 无关事件保持
    ];
    for e in &events {
        (unit.apply)(&mut state, e);
    }
    assert_eq!(state, json!([{ "content": "a", "status": "pending" }]));
    // turn/start → null
    let e = turn_start(2);
    (unit.apply)(&mut state, &e);
    assert_eq!(state, json!(null));
}

/// 投影初始态 null（first write 前）；view = state。
#[test]
fn todos_projection_init_and_view() {
    let unit = todos_projection_unit();
    let mut state = (unit.init)();
    assert_eq!(state, json!(null));
    let e = todo_write(0, json!([{ "content": "a", "status": "completed" }]));
    (unit.apply)(&mut state, &e);
    let view = (unit.view)(&state);
    assert_eq!(view, json!([{ "content": "a", "status": "completed" }]));
}

/// counts：pending/inProgress/completed。
#[test]
fn counts_derived() {
    let list = vec![
        TodoItem { content: "a".into(), status: TodoStatus::Pending },
        TodoItem { content: "b".into(), status: TodoStatus::InProgress },
        TodoItem { content: "c".into(), status: TodoStatus::Completed },
    ];
    let c = todo_counts(&list);
    assert_eq!(c["pending"], 1);
    assert_eq!(c["inProgress"], 1);
    assert_eq!(c["completed"], 1);
}

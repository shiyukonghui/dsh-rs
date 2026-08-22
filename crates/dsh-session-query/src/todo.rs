//! `dsh-session-query` todo 承载 —— 对齐
//! `deepseek-harness/packages/todo/tool-todo/src/index.ts`。
//!
//! M4g 交付 `to_todo_list`（模型输入校验/规范化）+ `todos` 投影 unit（todo/write →
//! 整表，next turn/start 清空 → null）+ `todo_counts`。M4h 把 `into_unit()` 注册进
//! `ProjectionRegistry`，并注册 `todo_write` 工具（依赖本模块校验）。

use dsh_session::types::{EventKind, TodoItem, TodoStatus};
use serde_json::{json, Value};

/// todo 列表校验错误（对齐 TS toTodoList 报错面）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TodoListError {
    EmptyContent,
    DuplicateContent,
    TooManyInProgress,
}

/// 规范化模型输入的 todo 表（trim 非空唯一 content；并行纪律）。
///
/// - `content` trim 后非空、表内唯一；
/// - `allow_parallel=false` 时至多一个 `in_progress`（单活动纪律）；
/// - 返回按输入序、已 trim 的规范表。
pub fn to_todo_list(raw: &[Value], allow_parallel: bool) -> Result<Vec<TodoItem>, TodoListError> {
    let mut todos: Vec<TodoItem> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut active = 0usize;
    for item in raw {
        let content = item
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .trim()
            .to_string();
        if content.is_empty() {
            return Err(TodoListError::EmptyContent);
        }
        if !seen.insert(content.clone()) {
            return Err(TodoListError::DuplicateContent);
        }
        let status = status_of(item.get("status"));
        if status == TodoStatus::InProgress {
            active += 1;
        }
        todos.push(TodoItem { content, status });
    }
    if !allow_parallel && active > 1 {
        return Err(TodoListError::TooManyInProgress);
    }
    Ok(todos)
}

/// 把 todo 状态字符串解析为 `TodoStatus`（schema 边界已保证合法 → 默认 pending）。
fn status_of(v: Option<&Value>) -> TodoStatus {
    match v.and_then(|v| v.as_str()) {
        Some("in_progress") => TodoStatus::InProgress,
        Some("completed") => TodoStatus::Completed,
        _ => TodoStatus::Pending,
    }
}

/// `todos` 投影单元的处理函数集（init/apply/view 都是纯函数，够注册进 Registry）。
pub struct TodosProjection {
    /// 初始态：pre-first-write null。
    pub init: fn() -> Value,
    /// 折叠：todo/write → 整表；turn/start → null；其余事件保持。
    pub apply: fn(&mut Value, &dsh_session::types::SessionEvent),
    /// 视图：整表（state 即表，无额外投影）。
    pub view: fn(&Value) -> Value,
}

impl TodosProjection {
    /// 注册进 `ProjectionRegistry` 的 unit 形状。
    pub fn into_unit(self) -> crate::projection::ProjectionUnit {
        crate::projection::ProjectionUnit::new(
            "todos",
            2,
            self.init,
            self.apply,
            self.view,
        )
    }
}

/// `todos` 投影单元（stand-alone 供测试/宿主注册）。
pub fn todos_projection_unit() -> TodosProjection {
    TodosProjection {
        init: || json!(null),
        apply: |state, event| {
            if event.kind == EventKind::TodoWrite {
                *state = event.data.get("todos").cloned().unwrap_or(json!(null));
            } else if event.kind == EventKind::TurnStart {
                *state = json!(null);
            }
        },
        view: |state| state.clone(),
    }
}

/// 从规范表派生 counts（{pending, inProgress, completed}）。
pub fn todo_counts(todos: &[TodoItem]) -> Value {
    let mut pending = 0u64;
    let mut in_progress = 0u64;
    let mut completed = 0u64;
    for t in todos {
        match t.status {
            TodoStatus::Pending => pending += 1,
            TodoStatus::InProgress => in_progress += 1,
            TodoStatus::Completed => completed += 1,
        }
    }
    json!({ "pending": pending, "inProgress": in_progress, "completed": completed })
}

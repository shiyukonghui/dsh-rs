//! `dsh-subagent` 目录 —— 对齐 `packages/subagent/subagent/{list-children,projection}.ts`。
//!
//! `list` 的完整可达目录行：child（one-shot / continuable）+ diagnostic，均带
//! activity/hasChildren；descendant 行带 parentId/depth（稳定前序）。

/// 子代理目录行（wire `SubagentListEntry`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildEntry {
    pub kind: &'static str,
    pub id: String,
    pub mode: String,
    pub activity: String,
    pub has_children: bool,
    pub label: Option<String>,
    /// diagnostic 行专用 reason。
    pub reason: Option<String>,
}

/// 派生一条 child 目录行。
pub fn category_child(
    id: &str,
    mode: &str,
    activity: &str,
    has_children: bool,
    label: Option<String>,
) -> ChildEntry {
    ChildEntry {
        kind: "child",
        id: id.to_string(),
        mode: mode.to_string(),
        activity: activity.to_string(),
        has_children,
        label,
        reason: None,
    }
}

/// 派生一条 diagnostic 目录行。
pub fn diagnostic_row(id: &str, reason: &str) -> ChildEntry {
    ChildEntry {
        kind: "diagnostic",
        id: id.to_string(),
        mode: String::new(),
        activity: String::new(),
        has_children: false,
        label: None,
        reason: Some(reason.to_string()),
    }
}

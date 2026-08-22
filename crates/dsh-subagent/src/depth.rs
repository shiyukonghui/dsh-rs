//! `dsh-subagent` 深度预算 —— 对齐 `packages/subagent/subagent/src/depth.ts`。
//!
//! 递归预算：根 agent 深度 0；child = max(header, runtime) + 1。header（persisted
//! `delegationDepth`）是单调下限，runtime `subagentDepth` 只能加深不能降低。

/// 深度越界错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DepthError {
    Overflow { attempted: u64, max: usize },
}

impl std::fmt::Display for DepthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DepthError::Overflow { attempted, max } => {
                write!(f, "subagent depth {attempted} exceeds max {max}")
            }
        }
    }
}

/// 校验可选 maxDepth（非负整数；None 或 Some(非负) 合法）。
pub fn validate_max_depth(max_depth: Option<i64>) -> Result<(), String> {
    if max_depth.is_some_and(|m| m < 0) {
        return Err("subagent maxDepth must be a non-negative safe integer".into());
    }
    Ok(())
}

/// 解析 child 深度 = max(header, runtime) + 1（无界）。
pub fn resolve_child_depth(header: Option<u64>, runtime: Option<u64>) -> Result<u64, DepthError> {
    let floor = header.unwrap_or(0).max(runtime.unwrap_or(0));
    Ok(floor + 1)
}

/// 解析 child 深度并套 max 预算：attempted = max(header,runtime)+1 > max → Overflow。
pub fn resolve_child_depth_bounded(
    header: Option<u64>,
    runtime: Option<u64>,
    max: usize,
) -> Result<u64, DepthError> {
    let attempted = resolve_child_depth(header, runtime)?;
    if attempted as usize > max {
        return Err(DepthError::Overflow { attempted, max });
    }
    Ok(attempted)
}

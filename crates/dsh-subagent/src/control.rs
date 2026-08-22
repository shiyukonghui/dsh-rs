//! `dsh-subagent` 控制面基础 —— prompt/interrupt 的 mode 判别（M4d）。
//!
//! prompt 仅对 continuable child（mode 校验）；interrupt fire-and-return（absent 目标
//! 也 accepted）。完整活代/inbox 投递由 M4h 接 agent-loop 实配；本模块是纯判别与回执。

/// prompt 请求地址（必要字段：parent + child + mode）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptAddress {
    pub parent_session_id: String,
    pub child_session_id: String,
    /// 仅 'continuable' 允许 prompt。
    pub mode: String,
}

/// interrupt 请求地址。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterruptAddress {
    pub parent_session_id: String,
    pub child_session_id: String,
    pub mode: String,
}

/// prompt 模式校验错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptError {
    NotContinuable,
}

/// 校验 prompt 前置：child 必须 continuable。
pub fn prompt_gate(addr: &PromptAddress) -> Result<(), PromptError> {
    if addr.mode != "continuable" {
        return Err(PromptError::NotContinuable);
    }
    Ok(())
}

/// interrupt 回执：fire-and-return 恒 accepted（absent 目标 = no-op 同样 accepted）。
pub fn interrupt_receipt(addr: &InterruptAddress) -> bool {
    // 未来可在此登记 target 是否存在；M4d 保持恒 accepted（对齐 TS accepted 语义）。
    let _ = addr;
    true
}

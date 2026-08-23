//! dsh-terminal — M5 终端会话注册表（设计见 M5-DESIGN.md §6）。
//!
//! 阶段三 TDD：先落会话注册表核心（owner 授权 / SEND_ACTIVE / 崩溃回滚 / dispose），
//! PTY 后端与 6 工具随红测继续加入（本箱 bash 不可用 → 真实 PTY 集成门控）。

mod registry;
mod types;

pub use registry::{
    BackendDefinition, BackendProvider, OwnerLiveness, TerminalBackend, TerminalSessionService,
};
pub use types::{
    TerminalBackendKind, TerminalConfig, TerminalError, TerminalErrorCode, TerminalSendRequest,
    TerminalSendResult, TerminalSessionId, TerminalSessionStatus, TerminalSessionView,
    TerminalSignal, TerminalWaitReason,
};

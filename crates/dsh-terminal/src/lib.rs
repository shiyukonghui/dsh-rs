//! dsh-terminal — M5 终端会话注册表（设计见 M5-DESIGN.md §6）。
//!
//! 阶段三 TDD：会话注册表核心（owner 授权 / SEND_ACTIVE / 崩溃回滚 / dispose）+ 真实
//! PTY 后端（portable-pty，Windows=ConPTY；shell 程序参数化、滚动缓冲、idle 推断）；
//! 6 工具随后续红测加入。

mod backend;
mod registry;
mod tool_terminal;
mod types;

pub use backend::PtyBackend;
pub use registry::{
    BackendDefinition, BackendProvider, OwnerLiveness, TerminalBackend, TerminalSessionService,
};
pub use tool_terminal::{
    bound_terminal_text, parse_terminal_close_args, parse_terminal_open_args,
    parse_terminal_read_args, parse_terminal_send_args, parse_terminal_signal_args,
    render_terminal_close, render_terminal_list, render_terminal_read, render_terminal_send,
    render_terminal_send_read, render_terminal_signal, render_terminal_spawn,
    terminal_close_schema, terminal_list_schema, terminal_open_schema, terminal_read_schema,
    terminal_send_schema, terminal_signal_schema, RenderedTerminalSession, TerminalCloseOutcome,
    TerminalRenderStatus, DEFAULT_TERMINAL_READ_LINES,
};
pub use types::{
    TerminalBackendKind, TerminalConfig, TerminalError, TerminalErrorCode, TerminalSendRequest,
    TerminalSendResult, TerminalSessionId, TerminalSessionStatus, TerminalSessionView,
    TerminalSignal, TerminalWaitReason,
};

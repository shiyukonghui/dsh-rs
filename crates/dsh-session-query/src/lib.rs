//! dsh-session-query：读模型投影（watermark readFrom + 状态折叠）+ session-log-export
//! （前端日志导出形状）。M1d 交付（见 M1-REQUIREMENTS.md §9）。
//!
//! 权威参考：
//! - 读模型投影：`deepseek-harness/packages/session/session-projection/`（registry/units +
//!   watermark observedSeq + snapshot/checkpoint）；
//! - 日志导出：`deepseek-harness/packages/session/session-log-export/` + host 侧导出形状。

pub mod export;
pub mod projection;
pub mod todo;

pub use export::*;
pub use projection::*;
pub use todo::*;

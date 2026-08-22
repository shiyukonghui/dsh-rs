//! `dsh-workflow` — 宿主侧工作流能力缝（`@deepseek-ai/dsh-workflow` 等效迁移）。
//!
//! M4g 目标：meta 校验（META_INVALID 列全 violations）+ WorkflowErrorCode 全码 + 事件
//! 载荷构造 + 诚实执行桩（JS 引擎不可低成本复刻，M4 保持桩——对未知能力返回结构化
//! isError 而非伪装成功）。权威参考：
//! `deepseek-harness/packages/workflow/{workflow,workflow-worker-thread}/src/{types,meta}.ts`。

pub mod error;
pub mod event;
pub mod meta;
pub mod stub;
pub mod types;

pub use error::{WorkflowError, WorkflowErrorCode};
pub use event::{agent_end_info, agent_start_info, run_info};
pub use meta::{validate_meta, MetaViolation};
pub use stub::{run_stub, StubRequest};

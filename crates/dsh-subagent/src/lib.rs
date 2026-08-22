//! `dsh-subagent` — 宿主侧子代理能力缝（`@deepseek-ai/dsh-subagent` 等效迁移）。
//!
//! M4d 目标：in-process spawn/fork 两 provider 能力登记 + 目录（catalog）+ 描述符 +
//! 深度预算 + 控制面基础（prompt/interrupt 判别）。权威参考：
//! `deepseek-harness/packages/subagent/subagent/src/{descriptor,depth,list-children}.ts`
//! 与 `subagent-in-process-driver/*`、`subagent-spawn-in-process/*`、
//! `subagent-fork-in-process/*`。

pub mod catalog;
pub mod control;
pub mod depth;
pub mod descriptor;
pub mod provider;
pub mod types;

pub use catalog::{category_child, diagnostic_row, ChildEntry};
pub use control::{interrupt_receipt, prompt_gate, InterruptAddress, PromptAddress, PromptError};
pub use depth::{resolve_child_depth, resolve_child_depth_bounded, validate_max_depth, DepthError};
pub use descriptor::{
    fold_descriptor_from_events, snapshot_descriptor, Descriptor, DescriptorInput, ToolRestriction,
    SUBAGENT_DESCRIPTOR_VERSION,
};
pub use provider::{for_provider_name, ProviderCapabilities};

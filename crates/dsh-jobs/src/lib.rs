//! `dsh-jobs` — 宿主侧后台任务注册表能力缝（`@deepseek-ai/dsh-jobs` 等效迁移）。
//!
//! M4e 目标：任务注册表状态机（start/list/get/read/kill）、id `<kind>-N` 分配、授权围栏、
//! 活跃上限、生命周期 first-wins 结算。权威参考：
//! `deepseek-harness/packages/jobs/jobs/src/{types,index}.ts` 与 `jobs-local`。

pub mod registry;
pub mod types;

pub use registry::{
    JobRead, JobRegistry, JobRegistryConfig, JobSnapshot, JobStartError, JobStatus, JobSettlement,
    JobOpsError, KillOutcome, ProducerHooks, StartSpec, jobs_frame, snapshot_to_view,
};

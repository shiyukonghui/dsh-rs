//! dsh-compaction：长会话压缩（M1c）。
//!
//! 权威参考：`deepseek-harness/packages/compaction/{compaction,compaction-basic,
//! compaction-tool-result-pruner}`。
//!
//! 模块划分（对齐 M1-REQUIREMENTS §8）：
//! - `engine`：CompactionEngine 缝（compactIfNeeded/compactNow/compactRegion）+
//!   toolPairingBalancedBefore/After（tool-call/result 配对平衡折叠）；
//! - `basic`：compact-basic 后端（阈值/retained-tail/overflow cap/trigger policy +
//!   routed summary + token 估算/测量 + 区域选择/压缩事务 + 一次性摘要框架）；
//! - `pruner`：ToolResultPruner（Unicode 码点裁剪、保留 rich-block 顺序）；
//! - `absorb`：compaction/start|summary|end|prune 事件 wire 形状 + user/message
//!   Replace（checkpoint source）吸收进 session（含 shadow-price 协议）；
//! - `checkpoint`：压缩检查点来源（`compactCheckpointSource`/`isCompactCheckpointSource`）；
//! - `types`：共享压缩词汇（CompactionTrigger/CompactionResult/ManualCompactionError）。

mod absorb;
mod basic;
mod checkpoint;
mod engine;
mod pruner;
mod types;

pub use absorb::{
    commit_compaction_body, compaction_end_payload, compaction_prune_payload,
    compaction_start_payload, compaction_summary_payload, checkpoint_user_message,
};
pub use basic::{
    estimate_content, estimate_header, estimate_message, estimate_system_tokens,
    estimate_tools_tokens, fold_surface_tokens, frame_summary, measure,
    assert_compaction_inactive, assert_no_active_compaction, compact_surface_region,
    inspect_compaction_entry_state, resolve_compact_spec, resolve_config, resolve_target_policy,
    select_compactable_range, validate_surface_region, BasicCompactionEngine,
    BasicCompactionConfig, CompactionEntryState, CompactionPolicyConfig,
    CompactionTransactionOptions, ModelCompactPolicyConfig, ModelInfoProvider, Owner,
    RegionDependencies, ResolvedBasicConfig, ResolvedCompactSpec, ResolvedRetention,
    ResolvedTargetPolicy, StabilityRule, SummarizationInput, Summarizer, SummaryResult,
    SurfaceChangedError, SurfaceTokenFold, TargetPressureConfigError, TokenMeasurement,
    TokenMeasurementBaseline, TokenSurfaceNode,
};
pub use checkpoint::{
    checkpoint_message_source, compact_checkpoint_source, is_compact_checkpoint_source,
    CompactionCheckpointSource,
};
pub use engine::{
    balance_cache, tool_pairing_balanced_after, tool_pairing_balanced_before, BalanceCache,
    CompactionEngine,
};pub use pruner::{
    code_point_length, resolve_prune_config, EstimateFn, PrunerConfig, ToolResultPruner,
    ToolResultPruneConfig,
};
pub use types::{
    CompactionResult, CompactionTrigger, ManualCompactionError, ManualCompactionErrorCode,
    ShadowedRange,
};

/// 压缩事务标识（dsh-compaction 拥有者；dsh-brand 承载类型，与 TS ownership 一致）。
pub use dsh_brand::CompactionId;

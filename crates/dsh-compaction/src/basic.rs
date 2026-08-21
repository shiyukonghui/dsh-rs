//! compact-basic 后端：阈值/retained-tail/overflow cap/trigger policy + routed
//! summary + token 估算/测量 + 区域选择与压缩事务 + 一次性摘要。
//!
//! 权威参考：
//! - `deepseek-harness/packages/compaction/compaction-basic/src/{config,region,summarizer,index}.ts`
//! - `deepseek-harness/packages/llm/token-meter/src/{estimate,surface-fold,index}.ts`
//!
//! M1c 范围声明：核心是**确定性纯语义**（同步、无真实 IO）。测量折叠按
//! token-meter `measure()` 语义逐事件重放；usage-anchored 分支完整实现（经
//! BlockAssembler 从 cites 的 chunk seqs 重建 provider 输出再定价）。摘要缝
//! [`crate::basic::Summarizer`] 与模型容量缝 [`crate::basic::ModelInfoProvider`]
//! 以 trait 注入，M1e 服务层线程桥接入真实 LLM；本包内用于测试的是确定性替身。

use std::collections::HashSet;

use dsh_llm::assembler::BlockAssembler;
use dsh_llm::types::{ContentBlock, Message, TokenUsage};
use dsh_session::runtime::Session;
use dsh_session::surface::derive_event_message;
use dsh_session::types::{
    EventKind, EpochHeader, RequestHeaderPayload, SessionEvent, StepEndPayload, StepStartPayload,
    SurfaceOp,
};

use crate::engine::CompactionEngine;
use crate::engine::{tool_pairing_balanced_after, tool_pairing_balanced_before, BalanceCache};
use crate::types::{
    CompactionResult, CompactionTrigger, ManualCompactionError, ManualCompactionErrorCode,
    ShadowedRange,
};
use crate::CompactionId;

// =====================================================================
// token 估算（对齐 estimate.ts —— 长度按 UTF-16 code units，对齐 JS String.length）
// =====================================================================

/// 固定文本密度估算（到达精确 tokenization 前使用的启发式）。
pub const CHARS_PER_TOKEN: u64 = 4;
/// 每条定价消息的角色字段框架开销。
pub const ROLE_OVERHEAD: u64 = 4;
/// 每 block 的结构性 JSON 框架 + 类型标签开销。
pub const BLOCK_OVERHEAD: u64 = 4;

/// UTF-16 code unit 长度（对齐 JS `String.length`）。
pub fn utf16_len(s: &str) -> u64 {
    s.encode_utf16().count() as u64
}

fn ceil_div(a: u64, b: u64) -> u64 {
    a.div_ceil(b)
}

/// 递归定价内容块（固定密度启发式）。
pub fn estimate_content(blocks: &[ContentBlock]) -> u64 {
    let mut tokens = 0u64;
    for block in blocks {
        match block {
            ContentBlock::Text(t) => {
                tokens += ceil_div(utf16_len(&t.text), CHARS_PER_TOKEN) + BLOCK_OVERHEAD;
            }
            ContentBlock::Reasoning(t) => {
                tokens += ceil_div(utf16_len(&t.text), CHARS_PER_TOKEN) + BLOCK_OVERHEAD;
            }
            ContentBlock::ToolCall(c) => {
                tokens += ceil_div(utf16_len(&c.name), CHARS_PER_TOKEN)
                    + ceil_div(utf16_len(&c.arguments), CHARS_PER_TOKEN)
                    + BLOCK_OVERHEAD;
            }
            ContentBlock::ToolResult(r) => {
                tokens += estimate_content(&r.content) + BLOCK_OVERHEAD;
            }
            // ContentBlockMap 合并可扩展：未知 block 保守的结构性 JSON 价格。
            ContentBlock::Unknown { data, .. } => {
                let raw = serde_json::to_string(&serde_json::Value::Object(data.clone()))
                    .map(|s| utf16_len(&s))
                    .unwrap_or(0);
                tokens += BLOCK_OVERHEAD + ceil_div(raw, CHARS_PER_TOKEN);
            }
            ContentBlock::Image(_) => {
                tokens += BLOCK_OVERHEAD;
            }
        }
    }
    tokens
}

/// 固定密度定价一条模型可见消息。
pub fn estimate_message(message: &Message) -> u64 {
    estimate_content(&message.content) + ROLE_OVERHEAD
}

/// 定价 request envelope 的 system prompt 部分。
pub fn estimate_system_tokens(header: Option<&EpochHeader>) -> u64 {
    match header.and_then(|h| h.system.as_ref()) {
        Some(system) => ceil_div(utf16_len(system), CHARS_PER_TOKEN) + ROLE_OVERHEAD,
        None => 0,
    }
}

/// 定价 request envelope 的工具 schema 部分。
pub fn estimate_tools_tokens(header: Option<&EpochHeader>) -> u64 {
    match header.and_then(|h| h.tools.as_ref()) {
        Some(tools) if !tools.is_empty() => {
            let s = serde_json::to_string(tools).map(|s| utf16_len(&s)).unwrap_or(0);
            ceil_div(s, CHARS_PER_TOKEN) + BLOCK_OVERHEAD
        }
        _ => 0,
    }
}

/// 定价完整非-surface request envelope。
pub fn estimate_header(header: Option<&EpochHeader>) -> u64 {
    estimate_system_tokens(header) + estimate_tools_tokens(header)
}

// =====================================================================
// token 测量（对齐 surface-fold.ts + token-meter index.ts）
// =====================================================================

/// 一个 token 定价的 surface 节点。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenSurfaceNode {
    pub seq: u64,
    pub tokens: u64,
}

/// 基线来源。
#[derive(Debug, Clone, PartialEq)]
pub enum TokenMeasurementBaseline {
    /// 空会话，无可定价发生。
    None { tokens: u64 },
    /// 基于 fixed heuristic 的重定价。
    Estimated { tokens: u64 },
    /// provider 上报的用量锚点。
    Usage { tokens: u64, usage: TokenUsage },
}

impl TokenMeasurementBaseline {
    pub fn tokens(&self) -> u64 {
        match self {
            TokenMeasurementBaseline::None { tokens }
            | TokenMeasurementBaseline::Estimated { tokens }
            | TokenMeasurementBaseline::Usage { tokens, .. } => *tokens,
        }
    }
}

/// 统一压力量测：`totalTokens = max(0, baseline + surfaceDeltaTokens)`。
#[derive(Debug, Clone, PartialEq)]
pub struct TokenMeasurement {
    pub log_revision: u64,
    pub baseline: TokenMeasurementBaseline,
    pub surface_delta_tokens: i64,
    pub total_tokens: u64,
    pub surface_tokens: u64,
    /// 已定价 surface 节点（当前模型可见顺序）。
    pub nodes: Vec<TokenSurfaceNode>,
}

/// 一个 surface 事件的放置与开销。
#[derive(Debug, Clone, PartialEq)]
pub struct SurfaceTokenFold {
    pub tokens: u64,
    pub nodes: Vec<TokenSurfaceNode>,
    pub delta_tokens: i64,
}

/// 在已定价表面上折叠一个 surface 事件（总与分配全新，损坏事件每次同样失败）。
pub fn fold_surface_tokens(
    nodes: &[TokenSurfaceNode],
    event: &SessionEvent,
    message: Option<&Message>,
) -> Result<SurfaceTokenFold, String> {
    let tokens = message.map(estimate_message).unwrap_or(0);
    let op = event
        .surface_op()
        .ok_or_else(|| format!("token surface: event at seq {} has no surface op", event.seq))?;
    match *op {
        SurfaceOp::Append => {
            let mut next = nodes.to_vec();
            next.push(TokenSurfaceNode { seq: event.seq, tokens });
            Ok(SurfaceTokenFold { tokens, nodes: next, delta_tokens: tokens as i64 })
        }
        SurfaceOp::Replace { start, end } => {
            let start_idx = nodes.iter().position(|n| n.seq == start);
            let end_idx = nodes.iter().position(|n| n.seq == end);
            match (start_idx, end_idx) {
                (Some(si), Some(ei)) if si <= ei => {
                    let removed: u64 = nodes[si..=ei].iter().map(|n| n.tokens).sum();
                    let mut next = nodes.to_vec();
                    next.splice(si..=ei, std::iter::once(TokenSurfaceNode { seq: event.seq, tokens }));
                    Ok(SurfaceTokenFold {
                        tokens,
                        nodes: next,
                        delta_tokens: tokens as i64 - removed as i64,
                    })
                }
                _ => Err(format!(
                    "token surface: replace at seq {} has invalid current range {start}-{end}",
                    event.seq
                )),
            }
        }
    }
}

/// 一条事件是否 surface-eligible。
pub fn is_surface_eligible(kind: EventKind) -> bool {
    matches!(kind, EventKind::UserMessage | EventKind::AssistantMessage | EventKind::ToolResult)
}

/// 一次性测量：从持久事件流折叠统一压力与表面（对齐 `TokenMeter.measure()`）。
pub fn measure(events: &[SessionEvent]) -> Result<TokenMeasurement, String> {
    #[derive(Clone)]
    struct StepStart {
        turn: u64,
        step: u64,
        surface_tokens: u64,
    }
    #[derive(Clone)]
    struct Anchor {
        header: Option<EpochHeader>,
        surface_tokens: u64,
        baseline: TokenMeasurementBaseline,
    }
    let mut header: Option<EpochHeader> = None;
    let mut surface: Vec<TokenSurfaceNode> = Vec::new();
    let mut surface_tokens: u64 = 0;
    let mut step_start: Option<StepStart> = None;
    let mut anchor: Option<Anchor> = None;
    let mut consumed = 0usize;

    for event in events {
        let mut next_header = header.clone();
        let mut next_step_start = step_start.clone();
        let mut next_anchor = anchor.clone();

        match event.kind {
            EventKind::RequestHeader => {
                let payload: RequestHeaderPayload = serde_json::from_value(event.data.clone())
                    .map_err(|e| format!("token meter: request/header at seq {} malformed: {e}", event.seq))?;
                next_header = Some(payload.header);
            }
            EventKind::StepStart => {
                if let Some(prev) = &step_start {
                    return Err(format!(
                        "token meter: step/start at seq {} arrived before turn {}/step {} ended",
                        event.seq, prev.turn, prev.step
                    ));
                }
                let payload: StepStartPayload = serde_json::from_value(event.data.clone())
                    .map_err(|e| format!("token meter: step/start at seq {} malformed: {e}", event.seq))?;
                next_step_start = Some(StepStart {
                    turn: payload.turn,
                    step: payload.step,
                    surface_tokens,
                });
            }
            EventKind::StepEnd => {
                let payload: StepEndPayload = serde_json::from_value(event.data.clone())
                    .map_err(|e| format!("token meter: step/end at seq {} malformed: {e}", event.seq))?;
                match &step_start {
                    Some(prev) if prev.turn == payload.turn && prev.step == payload.step => {}
                    _ => {
                        return Err(format!(
                            "token meter: step/end at seq {} has no matching step/start event",
                            event.seq
                        ));
                    }
                }
                next_step_start = None;
            }
            _ => {}
        }

        let surface_fold = if event.surface_op().is_some() && is_surface_eligible(event.kind.clone()) {
            let message = derive_event_message(event)
                .map_err(|e| format!("token meter: surface event {} malformed: {e}", event.seq))?;
            Some(fold_surface_tokens(&surface, event, message.as_ref())?)
        } else {
            None
        };

        if event.kind == EventKind::AssistantMessage {
            let payload: dsh_session::types::AssistantMessagePayload =
                serde_json::from_value(event.data.clone())
                    .map_err(|e| format!("token meter: assistant/message at seq {} malformed: {e}", event.seq))?;
            let step = step_start.as_ref().ok_or_else(|| {
                format!("token meter: assistant/message at seq {} has no matching step/start event", event.seq)
            })?;
            if step.turn != payload.turn || step.step != payload.step {
                return Err(format!(
                    "token meter: assistant/message at seq {} has no matching step/start event",
                    event.seq
                ));
            }
            let event_tokens = surface_fold.as_ref().map(|f| f.tokens).unwrap_or(0);
            if let Some(usage) = &payload.usage {
                if let Some(hdr) = &next_header {
                    let provider_assistant_tokens =
                        estimate_provider_assistant(events, event, event_tokens)?;
                    let anchor_surface_tokens = step.surface_tokens + provider_assistant_tokens;
                    let provider_tokens = usage_tokens(usage);
                    let estimated_anchor_tokens = estimate_header(Some(hdr)) + anchor_surface_tokens;
                    next_anchor = Some(Anchor {
                        header: Some(hdr.clone()),
                        surface_tokens: anchor_surface_tokens,
                        baseline: if provider_tokens >= estimated_anchor_tokens {
                            TokenMeasurementBaseline::Usage {
                                tokens: provider_tokens,
                                usage: usage.clone(),
                            }
                        } else {
                            TokenMeasurementBaseline::Estimated { tokens: estimated_anchor_tokens }
                        },
                    });
                } else {
                    let anchor_surface_tokens = step.surface_tokens + event_tokens;
                    let estimated = estimate_header(None) + anchor_surface_tokens;
                    next_anchor = Some(Anchor {
                        header: None,
                        surface_tokens: anchor_surface_tokens,
                        baseline: TokenMeasurementBaseline::Estimated { tokens: estimated },
                    });
                }
            } else {
                let anchor_surface_tokens = step.surface_tokens + event_tokens;
                let estimated = estimate_header(next_header.as_ref()) + anchor_surface_tokens;
                next_anchor = Some(Anchor {
                    header: next_header.clone(),
                    surface_tokens: anchor_surface_tokens,
                    baseline: TokenMeasurementBaseline::Estimated { tokens: estimated },
                });
            }
        }

        header = next_header;
        step_start = next_step_start;
        if let Some(fold) = surface_fold {
            surface = fold.nodes;
            surface_tokens = (surface_tokens as i64 + fold.delta_tokens).max(0) as u64;
        }
        anchor = next_anchor;
        consumed += 1;
    }

    let (baseline, surface_delta_tokens) = match &anchor {
        Some(a) if headers_equal(a.header.as_ref(), header.as_ref()) => {
            (a.baseline.clone(), surface_tokens as i64 - a.surface_tokens as i64)
        }
        _ if header.is_none() && surface_tokens == 0 => {
            (TokenMeasurementBaseline::None { tokens: 0 }, 0)
        }
        _ => (
            TokenMeasurementBaseline::Estimated {
                tokens: estimate_header(header.as_ref()) + surface_tokens,
            },
            0,
        ),
    };
    let total = baseline.tokens() as i64 + surface_delta_tokens;
    Ok(TokenMeasurement {
        log_revision: consumed as u64,
        baseline,
        surface_delta_tokens,
        total_tokens: total.max(0) as u64,
        surface_tokens,
        nodes: surface,
    })
}

/// 可选 envelope 比较（headerless estimate 可跟踪后续 surface 增量）。
fn headers_equal(a: Option<&EpochHeader>, b: Option<&EpochHeader>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a == b,
        (None, None) => true,
        _ => false,
    }
}

/// 求和互斥的 provider usage 桶，不重复计 reasoning output。
fn usage_tokens(usage: &TokenUsage) -> u64 {
    usage.input_tokens
        + usage.cache_read_tokens.unwrap_or(0)
        + usage.cache_write_tokens.unwrap_or(0)
        + usage.output_tokens
}

/// 从 cites 的 chunk seqs 重建 provider 输出再定价；缺失 legacy source seqs
/// 保守地把 durable 输出当作 provider 输出。
fn estimate_provider_assistant(
    events: &[SessionEvent],
    event: &SessionEvent,
    durable_event_tokens: u64,
) -> Result<u64, String> {
    let source_seqs = match event.source_event_seqs() {
        None => return Ok(durable_event_tokens),
        Some(s) => s.clone(),
    };
    let mut assembler = BlockAssembler::new();
    let mut seen = HashSet::new();
    for seq in &source_seqs {
        if *seq >= event.seq {
            return Err(format!(
                "token meter: assistant/message at seq {} source seq {seq} is not earlier",
                event.seq
            ));
        }
        if !seen.insert(*seq) {
            return Err(format!(
                "token meter: assistant/message at seq {} repeats source seq {seq}",
                event.seq
            ));
        }
        let source = events
            .get(*seq as usize)
            .ok_or_else(|| format!("token meter: assistant/message source seq {seq} missing"))?;
        if source.kind != EventKind::AssistantChunk {
            return Err(format!(
                "token meter: assistant/message at seq {} source seq {seq} is not assistant/chunk",
                event.seq
            ));
        }
        let chunk_payload: dsh_session::types::AssistantChunkPayload =
            serde_json::from_value(source.data.clone())
                .map_err(|e| format!("token meter: assistant/chunk at seq {seq} malformed: {e}"))?;
        let event_turn = event.data.get("turn").and_then(|v| v.as_u64());
        let event_step = event.data.get("step").and_then(|v| v.as_u64());
        if Some(chunk_payload.turn) != event_turn || Some(chunk_payload.step) != event_step {
            return Err(format!(
                "token meter: assistant/message at seq {} source seq {seq} belongs to another step",
                event.seq
            ));
        }
        assembler.push(chunk_payload.chunk);
    }
    let provider_content = assembler.blocks();
    if provider_content.is_empty() {
        Ok(0)
    } else {
        Ok(estimate_content(&provider_content) + ROLE_OVERHEAD)
    }
}

// =====================================================================
// 配置解析（对齐 config.ts）
// =====================================================================

/// 默认 request-pressure 分数。
pub const DEFAULT_THRESHOLD_RATIO: f64 = 0.8;
/// 默认 verbatim-tail 分数。
pub const DEFAULT_RETAIN_RATIO: f64 = 0.16;
/// 默认摘要生成上限。
pub const DEFAULT_MAX_TOKENS: u64 = 8192;
/// 默认 compaction 重试次数（每前端的若干远端尝试）。
pub const DEFAULT_COMPACTION_RETRIES: u64 = 1;
/// 默认 overflow 重试次数（对每个 agent）。
pub const DEFAULT_MAX_OVERFLOW_RETRIES: u64 = 1;
/// 默认自动压缩开关。
pub const DEFAULT_AUTO: bool = true;

/// 已解析且校验的 retention 形式（ratio 或精确 tokens，互斥）。
#[derive(Debug, Clone, PartialEq)]
pub enum ResolvedRetention {
    RetainRatio { retain_ratio: f64 },
    RetainTokens { retain_tokens: u64 },
}

/// 一个路由目标的完整解析策略。
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedTargetPolicy {
    pub target: (String, String),
    pub threshold_ratio: f64,
    pub retention: ResolvedRetention,
    pub summarization_provider: String,
    pub summarization_model: String,
    pub max_tokens: u64,
    pub compaction_retries: u64,
    pub max_overflow_retries: u64,
}

/// 已解析的服务级配置。
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedBasicConfig {
    pub threshold_ratio: f64,
    pub retention: ResolvedRetention,
    pub summarization_provider: String,
    pub summarization_model: String,
    pub max_tokens: u64,
    pub compaction_retries: u64,
    pub max_overflow_retries: u64,
    pub model_policies: Vec<ModelCompactPolicyConfig>,
    pub auto: bool,
}

/// 一个精确 target override（provider+model）。
#[derive(Debug, Clone, PartialEq)]
pub struct ModelCompactPolicyConfig {
    pub provider: String,
    pub model: String,
    pub policy: CompactionPolicyConfig,
}

/// 顶层或缺省配置的共享策略字段。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CompactionPolicyConfig {
    pub threshold_ratio: Option<f64>,
    pub retain_ratio: Option<f64>,
    pub retain_tokens: Option<u64>,
    pub summarization_provider: Option<String>,
    pub summarization_model: Option<String>,
    pub max_tokens: Option<u64>,
    pub compaction_retries: Option<u64>,
    pub max_overflow_retries: Option<u64>,
}

/// 原始（可能不合法的）BasicCompactionConfig 输入。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BasicCompactionConfig {
    pub policy: CompactionPolicyConfig,
    pub model_policies: Option<Vec<ModelCompactPolicyConfig>>,
    pub auto: Option<bool>,
}

fn assert_ratio(name: &str, value: f64) -> Result<(), String> {
    if value.is_finite() && value > 0.0 && value <= 1.0 {
        Ok(())
    } else {
        Err(format!("{name} ({value}) must be a number in (0, 1]"))
    }
}

/// 校验 (ratio, retention) 组合。
fn validate_ratio_retention(
    threshold_ratio: f64,
    retention: &ResolvedRetention,
    name: &str,
) -> Result<(), String> {
    if let ResolvedRetention::RetainRatio { retain_ratio } = retention {
        if *retain_ratio >= threshold_ratio {
            return Err(format!(
                "{name}: retainRatio ({retain_ratio}) must be less than the resolved thresholdRatio ({threshold_ratio})"
            ));
        }
    }
    Ok(())
}

/// 选择一个显式 retention 形式或继承已解析的 fallback。
fn resolve_retention(
    policy: &CompactionPolicyConfig,
    fallback: &ResolvedRetention,
) -> ResolvedRetention {
    if let Some(tokens) = policy.retain_tokens {
        ResolvedRetention::RetainTokens { retain_tokens: tokens }
    } else if let Some(ratio) = policy.retain_ratio {
        ResolvedRetention::RetainRatio { retain_ratio: ratio }
    } else {
        fallback.clone()
    }
}

/// 解析并校验服务默认与精确 target 部分覆盖（Rust 按结构字段而非字符串键集合）。
pub fn resolve_config(config: &BasicCompactionConfig) -> Result<ResolvedBasicConfig, String> {
    let policy = &config.policy;
    let threshold_ratio = policy.threshold_ratio.unwrap_or(DEFAULT_THRESHOLD_RATIO);
    assert_ratio("BasicCompactionConfig.thresholdRatio", threshold_ratio)?;
    let retention = resolve_retention(
        policy,
        &ResolvedRetention::RetainRatio { retain_ratio: DEFAULT_RETAIN_RATIO },
    );
    validate_ratio_retention(threshold_ratio, &retention, "BasicCompactionConfig")?;
    validate_policy_fields(policy, "BasicCompactionConfig")?;

    let model_policies = match &config.model_policies {
        None => Vec::new(),
        Some(list) => {
            let mut seen = std::collections::HashSet::new();
            for (index, mp) in list.iter().enumerate() {
                if mp.provider.is_empty() || mp.model.is_empty() {
                    return Err(format!(
                        "BasicCompactionConfig: modelPolicies[{index}].provider/model must be non-empty strings"
                    ));
                }
                let key = format!("{}\u{0}{}", mp.provider, mp.model);
                if !seen.insert(key) {
                    return Err(format!(
                        "BasicCompactionConfig: duplicate model policy for {}/{}",
                        mp.provider, mp.model
                    ));
                }
                let name = format!("BasicCompactionConfig: modelPolicies[{index}]");
                validate_policy_fields(&mp.policy, &name)?;
                validate_ratio_retention(
                    mp.policy.threshold_ratio.unwrap_or(threshold_ratio),
                    &resolve_retention(&mp.policy, &retention),
                    &name,
                )?;
            }
            list.clone()
        }
    };

    Ok(ResolvedBasicConfig {
        threshold_ratio,
        retention,
        summarization_provider: policy.summarization_provider.clone().unwrap_or_default(),
        summarization_model: policy.summarization_model.clone().unwrap_or_default(),
        max_tokens: policy.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        compaction_retries: policy.compaction_retries.unwrap_or(DEFAULT_COMPACTION_RETRIES),
        max_overflow_retries: policy.max_overflow_retries.unwrap_or(DEFAULT_MAX_OVERFLOW_RETRIES),
        model_policies,
        auto: config.auto.unwrap_or(DEFAULT_AUTO),
    })
}

/// 校验共享策略字段（互斥、数值界、summarization 配对）。
fn validate_policy_fields(policy: &CompactionPolicyConfig, name: &str) -> Result<(), String> {
    if let Some(r) = policy.threshold_ratio {
        assert_ratio(&format!("{name}.thresholdRatio"), r)?;
    }
    if let Some(r) = policy.retain_ratio {
        assert_ratio(&format!("{name}.retainRatio"), r)?;
    }
    if let Some(t) = policy.retain_tokens {
        assert_non_negative(&format!("{name}.retainTokens"), t)?;
    }
    if policy.retain_ratio.is_some() && policy.retain_tokens.is_some() {
        return Err(format!("{name}: retainRatio and retainTokens are mutually exclusive"));
    }
    if let Some(m) = policy.max_tokens {
        if m == 0 {
            return Err(format!("{name}.maxTokens (0) must be a positive integer"));
        }
    }
    if let Some(c) = policy.compaction_retries {
        assert_non_negative(&format!("{name}.compactionRetries"), c)?;
    }
    if let Some(c) = policy.max_overflow_retries {
        assert_non_negative(&format!("{name}.maxOverflowRetries"), c)?;
    }
    validate_summarization_pair(policy, name)?;
    Ok(())
}

fn assert_non_negative(_name: &str, _value: u64) -> Result<(), String> {
    Ok(())
}

/// 要求一个作用域一并省略/清空/替换 summarization 目标（成对规则）。
fn validate_summarization_pair(policy: &CompactionPolicyConfig, name: &str) -> Result<(), String> {
    match (&policy.summarization_provider, &policy.summarization_model) {
        (None, None) => Ok(()),
        (Some(p), Some(m)) if p.is_empty() == m.is_empty() => Ok(()),
        _ => Err(format!(
            "{name}: summarizationProvider and summarizationModel must be set together as an empty or non-empty pair"
        )),
    }
}

/// 合并精确 provider/model override 到已校验默认策略。
pub fn resolve_target_policy(
    config: &ResolvedBasicConfig,
    provider: &str,
    model: &str,
) -> ResolvedTargetPolicy {
    let override_policy = config
        .model_policies
        .iter()
        .find(|mp| mp.provider == provider && mp.model == model)
        .map(|mp| &mp.policy);
    let inherited_retention = config.retention.clone();
    let threshold_ratio = override_policy
        .and_then(|p| p.threshold_ratio)
        .unwrap_or(config.threshold_ratio);
    let retention = match override_policy {
        Some(p) => resolve_retention(p, &inherited_retention),
        None => inherited_retention,
    };
    ResolvedTargetPolicy {
        target: (provider.to_string(), model.to_string()),
        threshold_ratio,
        retention,
        summarization_provider: override_policy
            .and_then(|p| p.summarization_provider.clone())
            .unwrap_or_else(|| config.summarization_provider.clone()),
        summarization_model: override_policy
            .and_then(|p| p.summarization_model.clone())
            .unwrap_or_else(|| config.summarization_model.clone()),
        max_tokens: override_policy.and_then(|p| p.max_tokens).unwrap_or(config.max_tokens),
        compaction_retries: override_policy
            .and_then(|p| p.compaction_retries)
            .unwrap_or(config.compaction_retries),
        max_overflow_retries: override_policy
            .and_then(|p| p.max_overflow_retries)
            .unwrap_or(config.max_overflow_retries),
    }
}

/// 把一个 routed policy 换算成其模型容量下的具体 token 预算。
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedCompactSpec {
    pub target: (String, String),
    pub context_window: u64,
    pub threshold_ratio: f64,
    pub threshold_tokens: u64,
    pub retain_tokens: u64,
    pub summarization_provider: String,
    pub summarization_model: String,
    pub max_tokens: u64,
    pub compaction_retries: u64,
    pub max_overflow_retries: u64,
}

/// 无法为确切 target 解析压力配置的失败（targetKey 供告警抑制）。
#[derive(Debug, Clone, PartialEq)]
pub struct TargetPressureConfigError {
    pub target_key: String,
    pub message: String,
}

impl std::fmt::Display for TargetPressureConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.target_key, self.message)
    }
}

/// 换算临界与保留预算。
pub fn resolve_compact_spec(
    policy: &ResolvedTargetPolicy,
    context_window: u64,
) -> Result<ResolvedCompactSpec, TargetPressureConfigError> {
    let target_key = format!("{}/{}", policy.target.0, policy.target.1);
    if context_window == 0 {
        return Err(TargetPressureConfigError {
            target_key,
            message: format!("BasicCompactionConfig: contextWindow ({context_window}) must be a positive integer"),
        });
    }
    let threshold_tokens = ((context_window as f64) * policy.threshold_ratio).floor() as u64;
    let retain_tokens = match &policy.retention {
        ResolvedRetention::RetainTokens { retain_tokens } => *retain_tokens,
        ResolvedRetention::RetainRatio { retain_ratio } => {
            ((context_window as f64) * retain_ratio).floor() as u64
        }
    };
    if retain_tokens >= threshold_tokens {
        return Err(TargetPressureConfigError {
            target_key,
            message: format!(
                "BasicCompactionConfig: {}/{} retainTokens ({retain_tokens}) must be less than threshold tokens {threshold_tokens}",
                policy.target.0, policy.target.1
            ),
        });
    }
    Ok(ResolvedCompactSpec {
        target: policy.target.clone(),
        context_window,
        threshold_ratio: policy.threshold_ratio,
        threshold_tokens,
        retain_tokens,
        summarization_provider: policy.summarization_provider.clone(),
        summarization_model: policy.summarization_model.clone(),
        max_tokens: policy.max_tokens,
        compaction_retries: policy.compaction_retries,
        max_overflow_retries: policy.max_overflow_retries,
    })
}

// =====================================================================
// 摘要缝（对齐 summarizer.ts 的纯语义；LLM 调用由 M1e 服务层注入）
// =====================================================================

/// 摘要输入（system+tools 缺省用 header；messages 是被压缩区域）。
#[derive(Debug, Clone, PartialEq)]
pub struct SummarizationInput {
    pub system: Option<String>,
    pub tools: Option<Vec<serde_json::Value>>,
    pub messages: Vec<Message>,
}

/// 安全摘要内容 + 记录的辅助调用 envelope。
#[derive(Debug, Clone, PartialEq)]
pub struct SummaryResult {
    pub summary: Vec<ContentBlock>,
    pub provider: String,
    pub model: String,
    pub max_tokens: Option<u64>,
    pub usage: Option<TokenUsage>,
    /// 完整 provider 输出（text-only 投影前）。
    pub raw_output: Vec<ContentBlock>,
    /// 是否标识通过上下文 LLM 缝的一次调用。
    pub llm_stream_call: bool,
}

/// 摘要缝统一签名（同步；M1e 线程桥把真实 LLM 调用接进来）。
///
/// `Rc` 使缝可克隆、可跨多次压缩事务复用（单线程核心纪律）。
pub type Summarizer = std::rc::Rc<dyn Fn(&SummarizationInput) -> Result<SummaryResult, String>>;

/// 包装原始摘要块为 durable checkpoint 框架（content 用于合成的替换 user 消息）。
pub fn frame_summary(summary: &[ContentBlock]) -> Vec<ContentBlock> {
    let mut out = Vec::new();
    out.push(ContentBlock::text(format!("{CHECKPOINT_PREAMBLE}\n\n{SUMMARY_OPEN_TAG}")));
    out.extend_from_slice(summary);
    out.push(ContentBlock::text(SUMMARY_CLOSE_TAG));
    out
}

/// 检查点前言（模板的固定正文）。
pub const CHECKPOINT_PREAMBLE: &str = "This is an automatically generated checkpoint condensing an earlier span of the conversation to free up context. Treat the captured context as established background and build on it without restating it. Continue the task directly from the messages that follow, without acknowledging this checkpoint.";

pub const SUMMARY_OPEN_TAG: &str = "<compacted-summary>";
pub const SUMMARY_CLOSE_TAG: &str = "</compacted-summary>";

// =====================================================================
// 区域选择 + 事务（对齐 region.ts 的纯语义部分；Session 绑定见 engine.rs）
// =====================================================================

/// 校验后的 surface 位置跨度。
#[derive(Debug, Clone, PartialEq)]
pub struct SurfaceSelection {
    pub start: u64,
    pub end: u64,
    pub start_idx: usize,
    pub end_idx: usize,
    pub shadowed_seqs: Vec<u64>,
}

/// 解析下一个 head-anchored 范围：保留 priced recent tail、不拆 tool-call/result 对。
/// @returns 含的 positional seq 范围，或 None（无 range 可压）。
pub fn select_compactable_range(
    events: &[SessionEvent],
    surface_nodes: &[u64],
    generation: u64,
    cache: &mut Option<BalanceCache>,
    measurement: &TokenMeasurement,
    retain_tokens: u64,
) -> Result<Option<(u64, u64)>, String> {
    let priced_nodes = &measurement.nodes;
    if priced_nodes.is_empty() {
        return Ok(None);
    }
    if surface_nodes.len() != priced_nodes.len()
        || surface_nodes.iter().zip(priced_nodes.iter()).any(|(seq, node)| *seq != node.seq)
    {
        return Err("compaction: token-meter surface does not match the current session surface".into());
    }

    let mut accumulated = 0u64;
    let mut keep_from_idx = priced_nodes.len();
    for index in (0..priced_nodes.len()).rev() {
        accumulated += priced_nodes[index].tokens;
        keep_from_idx = index;
        if accumulated >= retain_tokens {
            break;
        }
    }
    if keep_from_idx == 0 {
        return Ok(None);
    }
    while keep_from_idx > 0 {
        if tool_pairing_balanced_before(events, surface_nodes, generation, cache, surface_nodes[keep_from_idx])? {
            break;
        }
        keep_from_idx -= 1;
    }
    if keep_from_idx == 0 {
        return Ok(None);
    }
    let first = surface_nodes[0];
    let cutoff = surface_nodes[keep_from_idx - 1];
    Ok(Some((first, cutoff)))
}

/// 校验一个请求的 surface-position 跨度（只读；不落任何事件）。
pub fn validate_surface_region(
    events: &[SessionEvent],
    surface_nodes: &[u64],
    generation: u64,
    cache: &mut Option<BalanceCache>,
    start: u64,
    end: u64,
) -> Result<SurfaceSelection, String> {
    let start_idx = surface_nodes
        .iter()
        .position(|s| *s == start)
        .ok_or_else(|| format!("compactRegion: start seq {start} not found in surface"))?;
    let end_idx = surface_nodes
        .iter()
        .position(|s| *s == end)
        .ok_or_else(|| format!("compactRegion: end seq {end} not found in surface"))?;
    if start_idx > end_idx {
        return Err(format!(
            "compactRegion: start seq {start} (position {start_idx}) is after end seq {end} (position {end_idx}) on the surface"
        ));
    }
    if !tool_pairing_balanced_before(events, surface_nodes, generation, cache, surface_nodes[start_idx])? {
        return Err(format!(
            "compactRegion: start seq {start} is not a balanced boundary (would split a step's tool-call/result pair)"
        ));
    }
    if !tool_pairing_balanced_after(events, surface_nodes, generation, cache, surface_nodes[end_idx])? {
        return Err(format!(
            "compactRegion: end seq {end} is not a balanced boundary (would split a step, or the step is still open)"
        ));
    }
    Ok(SurfaceSelection {
        start,
        end,
        start_idx,
        end_idx,
        shadowed_seqs: surface_nodes[start_idx..=end_idx].to_vec(),
    })
}

/// `compaction/start`/`compaction/end` 状态独立检查（从尾部扫描到三态齐全）。
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionEntryState {
    pub open_turn: Option<u64>,
    pub unmatched_start_seq: Option<u64>,
    pub latest_end_seed_seq: Option<u64>,
}

/// 从持久事件流检查 open-turn、unmatched-compaction、最新 seed 边界状态。
pub fn inspect_compaction_entry_state(events: &[SessionEvent]) -> CompactionEntryState {
    let mut open_turn: Option<u64> = None;
    let mut open_turn_state_known = false;
    let mut unmatched_start: Option<u64> = None;
    let mut compaction_entry_state_known = false;
    let mut latest_end_seed_seq: Option<u64> = None;
    for event in events.iter().rev() {
        if latest_end_seed_seq.is_none() && event.kind == EventKind::SessionEndSeed {
            latest_end_seed_seq = Some(event.seq);
        }
        if !compaction_entry_state_known {
            match event.kind {
                EventKind::CompactionStart => {
                    unmatched_start = Some(event.seq);
                    compaction_entry_state_known = true;
                }
                EventKind::CompactionEnd => {
                    compaction_entry_state_known = true;
                }
                _ => {}
            }
        }
        if !open_turn_state_known {
            if event.kind == EventKind::TurnStart {
                open_turn = event.data.get("turn").and_then(|v| v.as_u64());
                open_turn_state_known = true;
            } else if event.kind == EventKind::TurnEnd {
                open_turn_state_known = true;
            }
        }
        if open_turn_state_known && compaction_entry_state_known && latest_end_seed_seq.is_some() {
            break;
        }
    }
    CompactionEntryState {
        open_turn,
        unmatched_start_seq: unmatched_start,
        latest_end_seed_seq,
    }
}

/// 拒绝未匹配的 durable compaction 标记，除非后来的 constructor-seed 边界证明其
/// 属于更早的会话生命周期。
pub fn assert_compaction_inactive(
    unmatched_start: Option<u64>,
    latest_end_seed_seq: Option<u64>,
    stage: &str,
) -> Result<(), ManualCompactionError> {
    match unmatched_start {
        Some(start) if latest_end_seed_seq.is_none_or(|seed| seed < start) => {
            Err(ManualCompactionError {
                code: ManualCompactionErrorCode::Busy,
                message: format!(
                    "{stage}: compaction already in progress; the session compaction lock is already active"
                ),
            })
        }
        _ => Ok(()),
    }
}

/// 异步策略决策后重查 durable compaction 锁。
pub fn assert_no_active_compaction(
    events: &[SessionEvent],
    stage: &str,
) -> Result<(), ManualCompactionError> {
    let state = inspect_compaction_entry_state(events);
    assert_compaction_inactive(state.unmatched_start_seq, state.latest_end_seed_seq, stage)
}

/// surface 在摘要期间改变时抛出的失败。
#[derive(Debug, Clone, PartialEq)]
pub struct SurfaceChangedError {
    pub message: String,
    pub cause: Option<String>,
}

impl std::fmt::Display for SurfaceChangedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

// =====================================================================
// 压缩事务（对齐 region.ts 的 compactSurfaceRegion 语义）
// =====================================================================

/// Region 事务的依赖：动态派发摘要钩子（M1e 服务层绑定真实 LLM）。
pub struct RegionDependencies {
    pub summarize: Summarizer,
}

/// 事务选项：主人（None=回合之间手动）、稳定性规则、可选 durability 检查点。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StabilityRule {
    /// 摘要期间要求整个 surface 不变。
    WholeSurface,
    /// 只要求选中跨度保持同一 present、contiguous、等价的替换目标。
    SelectedSpan,
}

/// 一次压缩事务的内部准备状态。
struct PreparedCompaction {
    start: u64,
    end: u64,
    shadowed_seqs: Vec<u64>,
    measurement: TokenMeasurement,
    selected_nodes: Vec<TokenSurfaceNode>,
    shadowed_token_count: u64,
    input: SummarizationInput,
}

/// 快照定价与重放输入（`prepareCompaction`）。
fn prepare_compaction(
    session: &Session,
    selection: &SurfaceSelection,
) -> Result<PreparedCompaction, String> {
    let events = session.events();
    let measurement = measure(&events)?;
    let selected_nodes = measurement.nodes[selection.start_idx..=selection.end_idx].to_vec();
    if selected_nodes.len() != selection.shadowed_seqs.len()
        || selected_nodes
            .iter()
            .zip(selection.shadowed_seqs.iter())
            .any(|(node, seq)| node.seq != *seq)
    {
        return Err("compaction: selected surface changed before summarization began".into());
    }
    let shadowed_token_count = selected_nodes.iter().map(|n| n.tokens).sum();
    let input = build_summarization_input(session, &selection.shadowed_seqs)?;
    Ok(PreparedCompaction {
        start: selection.start,
        end: selection.end,
        shadowed_seqs: selection.shadowed_seqs.clone(),
        measurement,
        selected_nodes,
        shadowed_token_count,
        input,
    })
}

/// 构造摘要输入：最近 routed request 的 system/tools + 区域内 messages（surface 顺序）。
fn build_summarization_input(
    session: &Session,
    shadowed_seqs: &[u64],
) -> Result<SummarizationInput, String> {
    let header = session.request_header();
    let events = session.events();
    let mut region_messages = Vec::new();
    for seq in shadowed_seqs {
        let event = events
            .get(*seq as usize)
            .ok_or_else(|| format!("compaction: region seq {seq} missing"))?;
        if let Some(msg) = derive_event_message(event)
            .map_err(|e| format!("compaction: region event {seq} malformed: {e}"))?
        {
            region_messages.push(msg);
        }
    }
    Ok(SummarizationInput {
        system: header.as_ref().and_then(|h| h.system.clone()),
        tools: header
            .as_ref()
            .and_then(|h| h.tools.clone())
            .map(|t| serde_json::to_value(&t).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default()),
        messages: region_messages,
    })
}

/// 运行摘要器并架好替换检查点（`summarizeCompaction` 的同步面）。
fn summarize_compaction(
    deps: &RegionDependencies,
    prepared: &PreparedCompaction,
    compaction_id: &CompactionId,
    source_command_id: Option<&str>,
) -> Result<(SummaryResult, dsh_llm::types::Message), String> {
    let summary_result = (deps.summarize)(&prepared.input)?;
    let checkpoint_source = crate::checkpoint::CompactionCheckpointSource {
        compaction_id: compaction_id.clone(),
        source_command_id: source_command_id.map(str::to_string),
    };
    let framed = frame_summary(&summary_result.summary);
    let checkpoint_message = crate::absorb::checkpoint_user_message(
        dsh_llm::types::MessageId::from_raw("compaction-checkpoint"),
        framed,
        &checkpoint_source,
    );
    let framed_token_count = estimate_message(&checkpoint_message);
    if framed_token_count >= prepared.shadowed_token_count {
        return Err(format!(
            "summary is not smaller than the shadowed content ({framed_token_count} estimated framed tokens >= {})",
            prepared.shadowed_token_count
        ));
    }
    Ok((summary_result, checkpoint_message))
}

/// 整 surface 不变（摘要针对任何更早代）。
fn assert_whole_surface_unchanged(
    session: &Session,
    prepared: &PreparedCompaction,
) -> Result<(), SurfaceChangedError> {
    let current = measure(&session.events()).map_err(|e| SurfaceChangedError {
        message: e,
        cause: None,
    })?;
    if current.nodes != prepared.measurement.nodes {
        return Err(SurfaceChangedError {
            message: "compaction: session surface changed during summarization".into(),
            cause: None,
        });
    }
    Ok(())
}

/// 只要求选中跨度保持同一 present、contiguous、等价的替换目标。
fn assert_selected_span_stable(
    session: &Session,
    prepared: &PreparedCompaction,
) -> Result<(), SurfaceChangedError> {
    let events = session.events();
    let nodes = session.surface_nodes().map_err(|e| SurfaceChangedError {
        message: e.to_string(),
        cause: None,
    })?;
    let generation = session
        .surface_replace_generation()
        .map_err(|e| SurfaceChangedError { message: e.to_string(), cause: None })?;
    let mut cache = None;
    let current = validate_surface_region(
        &events,
        &nodes,
        generation,
        &mut cache,
        prepared.start,
        prepared.end,
    )
    .map_err(|e| SurfaceChangedError { message: e, cause: None })?;
    if current.shadowed_seqs != prepared.shadowed_seqs {
        return Err(SurfaceChangedError {
            message: "compaction: the selected span changed during summarization".into(),
            cause: None,
        });
    }
    let measured = measure(&events).map_err(|e| SurfaceChangedError {
        message: e,
        cause: None,
    })?;
    let selected = measured.nodes[current.start_idx..=current.end_idx].to_vec();
    if selected != prepared.selected_nodes {
        return Err(SurfaceChangedError {
            message: "compaction: the selected span was rewritten during summarization".into(),
            cause: None,
        });
    }
    Ok(())
}

/// 运行单个压缩事务（`compactSurfaceRegion` 的同步、确定性面）。
///
/// 选择与校验只读；`compaction/start` 在摘要让位前落盘（压缩锁）。之后的每个失败
/// 只做一次 `compaction/end` 尝试；失败的关闭会故意留下可检测的未匹配 start。
pub fn compact_surface_region(
    deps: &RegionDependencies,
    session: &Session,
    start: u64,
    end: u64,
    options: &CompactionTransactionOptions,
) -> Result<CompactionResult, crate::engine::CompactionError> {
    let events = session.events();
    let nodes = session
        .surface_nodes()
        .map_err(|e| crate::engine::CompactionError::Core(e.to_string()))?;
    let generation = session
        .surface_replace_generation()
        .map_err(|e| crate::engine::CompactionError::Core(e.to_string()))?;
    let mut cache: Option<BalanceCache> = None;
    let selection = validate_surface_region(&events, &nodes, generation, &mut cache, start, end)
        .map_err(crate::engine::CompactionError::Core)?;
    let entry_state = inspect_compaction_entry_state(&events);
    assert_compaction_inactive(
        entry_state.unmatched_start_seq,
        entry_state.latest_end_seed_seq,
        "compaction",
    )
    .map_err(crate::engine::CompactionError::Manual)?;

    // 属主解析：自动压缩要求已打开 turn；手动要求无打开 turn。
    let owner = match options.owner {
        Owner::CurrentTurn => match entry_state.open_turn {
            Some(turn) => Some(turn),
            None => {
                return Err(crate::engine::CompactionError::Core(
                    "compactRegion: no open turn — automatic compaction events must be enclosed in a turn"
                        .into(),
                ))
            }
        },
        Owner::Manual => {
            if entry_state.open_turn.is_some() {
                return Err(ManualCompactionError {
                    code: ManualCompactionErrorCode::Busy,
                    message: "manual compaction: the session already has an open turn".into(),
                }
                .into());
            }
            None
        }
    };

    let compaction_id = CompactionId::from_raw(UuidLike::generate());
    let start_event = session
        .append(
            EventKind::CompactionStart,
            crate::absorb::compaction_start_payload(
                &compaction_id,
                options.source_command_id.as_deref(),
                owner,
            ),
            None,
        )
        .map_err(|e| crate::engine::CompactionError::Core(format!("compaction start: {e}")))?;

    // 摘要 + 稳定性 + commit（任何失败 stage 化以便分类）。
    let stage_result = (|| -> Result<CompactionCommitted, (String, &'static str)> {
        let prepared = prepare_compaction(session, &selection).map_err(|e| (e, "summary"))?;
        let summarized = summarize_compaction(
            deps,
            &prepared,
            &compaction_id,
            options.source_command_id.as_deref(),
        )
        .map_err(|e| (e, "summary"))?;
        match options.stability {
            StabilityRule::WholeSurface => assert_whole_surface_unchanged(session, &prepared)
                .map_err(|e| (e.message, "summary"))?,
            StabilityRule::SelectedSpan => assert_selected_span_stable(session, &prepared)
                .map_err(|e| (e.message, "summary"))?,
        }
        let (summary_result, checkpoint_message) = &summarized;
        let checkpoint_id = checkpoint_message.id.clone();
        let (summary_event, _replacement) = crate::absorb::commit_compaction_body(
            session,
            start_event.seq,
            &compaction_id,
            options.source_command_id.as_deref(),
            &summary_result.summary,
            checkpoint_message.content.clone(),
            Some(&summary_result.raw_output),
            summary_result.llm_stream_call,
            ShadowedRange { start: prepared.start, end: prepared.end },
            &prepared.shadowed_seqs,
            prepared.shadowed_token_count,
            &summary_result.provider,
            &summary_result.model,
            summary_result.max_tokens,
            summary_result.usage.as_ref(),
            checkpoint_id,
        )
        .map_err(|e| (e.to_string(), "commit"))?;
        Ok(CompactionCommitted {
            summary_seq: summary_event.seq,
            shadowed_token_count: prepared.shadowed_token_count,
            shadowed_seqs: prepared.shadowed_seqs,
        })
    })();

    match stage_result {
        Ok(committed) => {
            let end_event = session
                .append(
                    EventKind::CompactionEnd,
                    crate::absorb::compaction_end_payload(
                        &compaction_id,
                        options.source_command_id.as_deref(),
                        owner,
                        None,
                    ),
                    None,
                )
                .map_err(|e| crate::engine::CompactionError::Core(format!("compaction end: {e}")))?;
            Ok(CompactionResult {
                compaction_id,
                source_command_id: options.source_command_id.clone(),
                start_seq: start_event.seq,
                summary_seq: committed.summary_seq,
                end_seq: end_event.seq,
                summary: vec![], // 完整摘要内容在 compaction/summary 事件；结果结构保留摘要块
                shadowed_range: ShadowedRange { start: selection.start, end: selection.end },
                shadowed_seqs: committed.shadowed_seqs,
                shadowed_token_count: committed.shadowed_token_count,
            })
        }
        Err((error, stage)) => {
            let close = session.append(
                EventKind::CompactionEnd,
                crate::absorb::compaction_end_payload(
                    &compaction_id,
                    options.source_command_id.as_deref(),
                    owner,
                    Some(&error),
                ),
                None,
            );
            if let Err(e) = close {
                return Err(crate::engine::CompactionError::Core(format!(
                    "compaction close after {stage}: {e}"
                )));
            }
            Err(classify_failure(error, stage, options))
        }
    }
}

/// commit 阶段已落盘的信息（返回给事务主线）。
struct CompactionCommitted {
    summary_seq: u64,
    shadowed_token_count: u64,
    shadowed_seqs: Vec<u64>,
}

/// 压缩事务选项。
#[derive(Debug, Clone)]
pub struct CompactionTransactionOptions {
    pub owner: Owner,
    pub stability: StabilityRule,
    pub source_command_id: Option<String>,
}

/// 事务属主。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    /// 自动占用：事件必须落在已打开的 turn 内。
    CurrentTurn,
    /// 手动：回合之间，无 turn 打开。
    Manual,
}

/// 把核心阶段失败分类成 ManualCompactionError（手动时）；自动路径直接抛原错误。
fn classify_failure(
    error: String,
    stage: &'static str,
    options: &CompactionTransactionOptions,
) -> crate::engine::CompactionError {
    match options.owner {
        Owner::CurrentTurn => crate::engine::CompactionError::Core(error),
        Owner::Manual => {
            let code = match stage {
                "commit" => ManualCompactionErrorCode::Commit,
                _ if error.contains("changed during") => ManualCompactionErrorCode::Changed,
                _ => ManualCompactionErrorCode::Summary,
            };
            ManualCompactionError { code, message: error }.into()
        }
    }
}

/// 简单压缩事务标识（无外部依赖；全局唯一足够）。
struct UuidLike;
impl UuidLike {
    fn generate() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("cid-{nanos:x}-{:x}", (nanos as u32) ^ 0x9e37_79b9)
    }
}

// =====================================================================
// 引擎实现（对齐 compaction-basic index.ts 的 BasicCompactionEngine）
// =====================================================================

/// implementor 提供模型的上下文容量（M1e 服务层接 adapter metadata）。
pub trait ModelInfoProvider {
    fn context_window(&self, provider: &str, model: &str) -> Result<u64, String>;
}

/// 最小纯语义实现：以固定容量替身运行自动/手动压缩（测试用）。
///
/// 摘要缝按调用方（服务层）每调用注入，对齐 `CompactionEngine` 接口。
pub struct BasicCompactionEngine {
    pub config: ResolvedBasicConfig,
    /// 模型容量查询（默认固定 65536）。
    pub model_info: Option<Box<dyn ModelInfoProvider>>,
}

impl BasicCompactionEngine {
    pub fn new(config: BasicCompactionConfig) -> Result<Self, String> {
        Ok(BasicCompactionEngine {
            config: resolve_config(&config)?,
            model_info: None,
        })
    }

    fn context_window(&self, provider: &str, model: &str) -> Result<u64, String> {
        match &self.model_info {
            Some(p) => p.context_window(provider, model),
            None => Ok(65536),
        }
    }

    fn region_deps(summarize: &Summarizer) -> RegionDependencies {
        RegionDependencies { summarize: summarize.clone() }
    }
}

impl CompactionEngine for BasicCompactionEngine {
    /// compactIfNeeded 的确定性面（对齐 index.ts compactIfNeeded）。
    fn compact_if_needed(
        &self,
        session: &Session,
        trigger: CompactionTrigger,
        summarize: &Summarizer,
    ) -> Result<Option<CompactionResult>, crate::engine::CompactionError> {
        let header = session.request_header();
        let (provider, model) = match header {
            Some(h) if !h.config.provider.is_empty() && !h.config.model.is_empty() => {
                (h.config.provider.clone(), h.config.model.clone())
            }
            _ => return Ok(None),
        };
        let policy = resolve_target_policy(&self.config, &provider, &model);
        let mut events = session.events();
        let measurement = measure(&events).map_err(crate::engine::CompactionError::Core)?;
        match trigger {
            CompactionTrigger::ContextOverflow => {
                let nodes = session
                    .surface_nodes()
                    .map_err(|e| crate::engine::CompactionError::Core(e.to_string()))?;
                let generation = session
                    .surface_replace_generation()
                    .map_err(|e| crate::engine::CompactionError::Core(e.to_string()))?;
                let mut cache = None;
                let range = select_compactable_range(
                    &events,
                    &nodes,
                    generation,
                    &mut cache,
                    &measurement,
                    0,
                )
                .map_err(crate::engine::CompactionError::Core)?;
                let range = match range {
                    Some(r) => r,
                    None => return Ok(None),
                };
                let result = self.compact_region(session, range.0, range.1, summarize)?;
                Ok(Some(result))
            }
            CompactionTrigger::Pressure => {
                assert_no_active_compaction(&events, "automatic pressure compaction")
                    .map_err(crate::engine::CompactionError::Manual)?;
                let target_key = format!("{provider}/{model}");
                let context_window = self.context_window(&provider, &model).map_err(|e| {
                    crate::engine::CompactionError::Core(format!(
                        "compaction-basic: no context capacity for {target_key}; {e}"
                    ))
                })?;
                let spec = resolve_compact_spec(&policy, context_window)
                    .map_err(|e| crate::engine::CompactionError::Core(e.to_string()))?;
                let mut measurement = measurement;
                if measurement.total_tokens < spec.threshold_tokens {
                    return Ok(None);
                }
                let mut summary_result: Option<CompactionResult> = None;
                for _attempt in 0..=spec.compaction_retries {
                    events = session.events();
                    let nodes = session
                        .surface_nodes()
                        .map_err(|e| crate::engine::CompactionError::Core(e.to_string()))?;
                    let generation = session
                        .surface_replace_generation()
                        .map_err(|e| crate::engine::CompactionError::Core(e.to_string()))?;
                    let mut cache = None;
                    let range = select_compactable_range(
                        &events,
                        &nodes,
                        generation,
                        &mut cache,
                        &measurement,
                        spec.retain_tokens,
                    )
                    .map_err(crate::engine::CompactionError::Core)?;
                    let range = match range {
                        Some(r) => r,
                        None => return Ok(summary_result),
                    };
                    let result = self.compact_region(session, range.0, range.1, summarize)?;
                    events = session.events();
                    measurement =
                        measure(&events).map_err(crate::engine::CompactionError::Core)?;
                    if measurement.total_tokens < spec.threshold_tokens {
                        return Ok(Some(result));
                    }
                    summary_result = Some(result);
                }
                Err(crate::engine::CompactionError::Core(format!(
                    "compaction still above threshold after {} compaction attempts",
                    spec.compaction_retries + 1
                )))
            }
        }
    }

    /// 强制压缩一个范围（owner=current-turn；stability=whole-surface）。
    fn compact_region(
        &self,
        session: &Session,
        start: u64,
        end: u64,
        summarize: &Summarizer,
    ) -> Result<CompactionResult, crate::engine::CompactionError> {
        compact_surface_region(
            &Self::region_deps(summarize),
            session,
            start,
            end,
            &CompactionTransactionOptions {
                owner: Owner::CurrentTurn,
                stability: StabilityRule::WholeSurface,
                source_command_id: None,
            },
        )
    }

    /// 手动压缩：空闲会话、standalone marker pair、selected-span 稳定性。
    fn compact_now(
        &self,
        session: &Session,
        summarize: &Summarizer,
        source_command_id: Option<String>,
    ) -> Result<Option<CompactionResult>, ManualCompactionError> {
        let events = session.events();
        let nodes = session.surface_nodes().map_err(|e| {
            ManualCompactionError { code: ManualCompactionErrorCode::Busy, message: e.to_string() }
        })?;
        let generation = session.surface_replace_generation().map_err(|e| {
            ManualCompactionError { code: ManualCompactionErrorCode::Busy, message: e.to_string() }
        })?;
        let measurement = measure(&events).map_err(|e| {
            ManualCompactionError { code: ManualCompactionErrorCode::Summary, message: e }
        })?;
        let mut cache = None;
        let range =
            select_compactable_range(&events, &nodes, generation, &mut cache, &measurement, 0)
                .map_err(|e| {
                    ManualCompactionError { code: ManualCompactionErrorCode::Summary, message: e }
                })?;
        let range = match range {
            Some(r) => r,
            None => return Ok(None),
        };
        compact_surface_region(
            &Self::region_deps(summarize),
            session,
            range.0,
            range.1,
            &CompactionTransactionOptions {
                owner: Owner::Manual,
                stability: StabilityRule::SelectedSpan,
                source_command_id,
            },
        )
        .map(Some)
        .map_err(|e| match e {
            crate::engine::CompactionError::Manual(m) => m,
            crate::engine::CompactionError::Core(msg) => {
                ManualCompactionError { code: ManualCompactionErrorCode::Summary, message: msg }
            }
            crate::engine::CompactionError::SurfaceChanged(sc) => {
                ManualCompactionError { code: ManualCompactionErrorCode::Changed, message: sc.message }
            }
        })
    }
}

// touch marker

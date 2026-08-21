//! CompactionEngine 能力缝 + tool-call/result 配对平衡折叠。
//!
//! 权威参考：`deepseek-harness/packages/compaction/compaction/src/tool-pairing.ts`
//! 与 `.../compaction/src/index.ts` 的 `CompactionEngine` 抽象。
//!
//! 压缩改变 surface 位置，所以安全切割点要从**当前 surface 顺序**下的
//! tool-call/result 内容推导，而不是 step 标记。`BalanceCache` 是增量折叠
//! 状态（generation 变化时重建；长度没变则复用）；每次查询 O(1)。

use std::collections::HashMap;

use dsh_session::runtime::Session;
use dsh_session::types::{EventKind, SessionEvent};

use crate::basic::{Summarizer, SurfaceChangedError};
use crate::types::{CompactionResult, CompactionTrigger, ManualCompactionError};

// =====================================================================
// CompactionEngine 能力缝（对齐 TS `CompactionEngine` 抽象）
// =====================================================================

/// 一次压缩操作的统一失败（核心阶段分类）。
#[derive(Debug, Clone, PartialEq)]
pub enum CompactionError {
    /// 手动压缩预期失败（启用了 ManualCompactionErrorCode）。
    Manual(ManualCompactionError),
    /// 核心阶段失败（自动路径直接抛出）。
    Core(String),
    /// surface 在摘要期间改变。
    SurfaceChanged(SurfaceChangedError),
}

impl From<ManualCompactionError> for CompactionError {
    fn from(e: ManualCompactionError) -> Self {
        CompactionError::Manual(e)
    }
}

impl std::fmt::Display for CompactionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompactionError::Manual(e) => write!(f, "[{}] {}", e.code.as_str(), e.message),
            CompactionError::Core(msg) => write!(f, "{msg}"),
            CompactionError::SurfaceChanged(sc) => write!(f, "{}", sc.message),
        }
    }
}

impl std::error::Error for CompactionError {}

/// 抽象压缩服务：实现者自己拥有 trigger policy、retention 与摘要。成功的运行把
/// 选中的 surface 跨度替换为一个摘要节点，并阻止同一会话的并发压缩。替换 user
/// 消息使用 `compactCheckpointSource` 携带事务标识。
pub trait CompactionEngine {
    /// 为一次显式 trigger 考虑自动压缩。`null` = 无需压缩。
    ///
    /// `summarize` 缝在每次调用时注入（服务层线程桥接真实 LLM；核心测试用替身）。
    fn compact_if_needed(
        &self,
        session: &Session,
        trigger: CompactionTrigger,
        summarize: &Summarizer,
    ) -> Result<Option<CompactionResult>, CompactionError>;

    /// 显式压缩有用历史（即使低于自动压力阈值）。空闲会话按需启动。
    fn compact_now(
        &self,
        session: &Session,
        summarize: &Summarizer,
        source_command_id: Option<String>,
    ) -> Result<Option<CompactionResult>, ManualCompactionError>;

    /// 强制把一个 surface 节点范围压缩进单个摘要节点。`start`/`end` 以 surface
    /// 位置（非数值 seq 序）命名含的范围；替换可让可见 seq 非单调。两边边缘必须
    /// 平衡（assistant tool-call 与其 result 配对）。
    fn compact_region(
        &self,
        session: &Session,
        start: u64,
        end: u64,
        summarize: &Summarizer,
    ) -> Result<CompactionResult, CompactionError>;
}

// =====================================================================
// tool-pairing 平衡折叠（对齐 tool-pairing.ts）
// =====================================================================

/// 一个 surface 代的增量平衡状态。
#[derive(Debug, Clone)]
pub struct BalanceCache {
    /// 该状态描述的 surface 重写代。
    pub generation: u64,
    /// 当前顺序下每条 surface 切割的平衡度：N 序列 surface 有 N+1 条切割，
    /// 第 i 条是序列 i 之前的切割，最后一条是 surface 尾之后的切割。
    pub cut_balanced: Vec<bool>,
    /// 每个事件 seq 的当前 surface 位置（索引进 [`BalanceCache::cut_balanced`]）。
    pub index_by_seq: HashMap<u64, usize>,
    /// 处理完 surface 尾后的进行中 tool-call 数。
    pub in_progress_tool_calls: u64,
}

impl BalanceCache {
    fn empty(generation: u64) -> Self {
        BalanceCache {
            generation,
            cut_balanced: vec![true],
            index_by_seq: HashMap::new(),
            in_progress_tool_calls: 0,
        }
    }
}

/// 一个 surface 事件对进行中 tool-call 数的贡献。
fn event_delta(event: &SessionEvent) -> Result<i64, String> {
    match event.kind {
        EventKind::AssistantMessage => {
            let count = event
                .data
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array())
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool-call"))
                        .count() as i64
                })
                .unwrap_or(0);
            Ok(count)
        }
        EventKind::ToolResult => Ok(-1),
        _ => Ok(0),
    }
}

/// 读取并校验一个 surface 序列命名的事件。
fn event_for_seq(events: &[SessionEvent], seq: u64) -> Result<&SessionEvent, String> {
    let idx = seq as usize;
    match events.get(idx) {
        Some(e) if e.seq == seq => Ok(e),
        _ => Err(format!(
            "tool-pairing balance: surface seq {seq} has no matching session event (corrupt surface)"
        )),
    }
}

/// 把尚未进缓存的 surface 序列折叠进其平衡状态（先校验未见尾，再变更缓存）。
fn extend_cache(
    events: &[SessionEvent],
    cache: &mut BalanceCache,
    seqs: &[u64],
) -> Result<(), String> {
    let processed = cache.cut_balanced.len() - 1;
    let tail = &seqs[processed.min(seqs.len())..];
    let mut pending_cuts: Vec<bool> = Vec::new();
    let mut in_progress_tool_calls = cache.in_progress_tool_calls as i64;
    for (i, seq) in tail.iter().enumerate() {
        let delta = event_delta(event_for_seq(events, *seq)?)?;
        in_progress_tool_calls += delta;
        if in_progress_tool_calls < 0 {
            return Err(format!(
                "tool-pairing balance: tool/result at surface seq {seq} has no matching tool-call (corrupt surface)"
            ));
        }
        pending_cuts.push(in_progress_tool_calls == 0);
        cache.index_by_seq.insert(*seq, processed + i);
    }
    cache.cut_balanced.extend(pending_cuts);
    cache.in_progress_tool_calls = in_progress_tool_calls as u64;
    Ok(())
}

/// 把当前 surface 折叠到平衡缓存（自空 surface 状态同构出发）。
pub fn balance_cache(
    events: &[SessionEvent],
    surface_nodes: &[u64],
    generation: u64,
    cache: &mut Option<BalanceCache>,
) -> Result<BalanceCache, String> {
    let need_rebuild = match cache {
        None => true,
        Some(c) => {
            c.generation != generation || c.cut_balanced.len() - 1 > surface_nodes.len()
        }
    };
    if need_rebuild {
        let mut rebuilt = BalanceCache::empty(generation);
        extend_cache(events, &mut rebuilt, surface_nodes)?;
        *cache = Some(rebuilt.clone());
        return Ok(rebuilt);
    }
    if cache.as_ref().unwrap().cut_balanced.len() - 1 < surface_nodes.len() {
        let mut c = cache.clone().unwrap();
        extend_cache(events, &mut c, surface_nodes)?;
        *cache = Some(c.clone());
        return Ok(c);
    }
    Ok(cache.clone().unwrap())
}

/// 一个 seq 位置处（加 offset）的切割平衡度，拒绝当前 surface 外的 seq。
fn cut_balance(cache: &BalanceCache, seq: u64, offset: u8) -> Result<bool, String> {
    let balanced = cache
        .index_by_seq
        .get(&seq)
        .and_then(|i| cache.cut_balanced.get(*i + offset as usize))
        .copied();
    balanced.ok_or_else(|| format!("tool-pairing balance: surface seq {seq} not found"))
}

/// 当前 surface 序列**正前方**的切割是否 tool-pairing 平衡。
pub fn tool_pairing_balanced_before(
    events: &[SessionEvent],
    surface_nodes: &[u64],
    generation: u64,
    cache: &mut Option<BalanceCache>,
    seq: u64,
) -> Result<bool, String> {
    let cache = balance_cache(events, surface_nodes, generation, cache)?;
    cut_balance(&cache, seq, 0)
}

/// 当前 surface 序列**正后方**的切割是否 tool-pairing 平衡。
pub fn tool_pairing_balanced_after(
    events: &[SessionEvent],
    surface_nodes: &[u64],
    generation: u64,
    cache: &mut Option<BalanceCache>,
    seq: u64,
) -> Result<bool, String> {
    let cache = balance_cache(events, surface_nodes, generation, cache)?;
    cut_balance(&cache, seq, 1)
}

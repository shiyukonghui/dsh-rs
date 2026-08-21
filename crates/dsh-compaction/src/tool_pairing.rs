//! M1c：dsh-compaction 的工具配对平衡折叠（对齐
//! `deepseek-harness/packages/compaction/compaction/src/tool-pairing.ts`）。
//!
//! 压缩改变 surface 位置，所以安全切割点要从**当前 surface 顺序**下的
//! tool-call/result 内容推导，而不是 step 标记。`BalanceCache` 是增量折叠
//! 状态（generation 变化时重建；长度没变则复用）；每次查询 O(1)。

use std::collections::HashMap;

use dsh_llm::types::ContentBlock;
use dsh_session::types::{EventKind, SessionEvent};

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
fn event_delta(event: &SessionEvent) -> i64 {
    match event.kind {
        EventKind::AssistantMessage => {
            // data.message.content 里的 tool-call block 数
            let msg: Option<dsh_llm::types::Message> = event
                .data
                .get("message")
                .and_then(|m| serde_json::from_value(m.clone()).ok());
            match msg {
                Some(m) => m
                    .content
                    .iter()
                    .filter(|b| matches!(b, ContentBlock::ToolCall(_)))
                    .count() as i64,
                None => 0,
            }
        }
        EventKind::ToolResult => -1,
        _ => 0,
    }
}

/// 读取并校验一个 surface 序列命名的事件。
fn event_for_seq(events: &[SessionEvent], seq: u64) -> Result<&SessionEvent, String> {
    let idx = seq as usize;
    let event = events.get(idx);
    match event {
        Some(e) if e.seq == seq => Ok(e),
        _ => Err(format!(
            "tool-pairing balance: surface seq {seq} has no matching session event (corrupt surface)"
        )),
    }
}

/// 把尚未进缓存的 surface 序列折叠进其平衡状态。先校验未见的尾，再变更缓存，
/// 使损坏的 append 不会留下部分推进的状态。
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
        in_progress_tool_calls += event_delta(event_for_seq(events, *seq)?);
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
    let index = cache.index_by_seq.get(&seq).copied();
    let balanced = index.and_then(|i| cache.cut_balanced.get(i + offset as usize).copied());
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

//! 固定密度 token 估算 + surface 折叠测量（对齐
//! `deepseek-harness/packages/llm/token-meter/src/{estimate,surface-fold}.ts`）。
//!
//! 本模块是纯函数语义：以「事件序列 + surface 折叠」为输入，产出 token-priced
//! 节点与压力测量；真实服务编排（usage anchor、step 边界关联）属于服务层
//! TokenMeter（M1e 线程桥），核心只保留 compaction-basic 依赖的确定性部分。

use dsh_llm::types::{ContentBlock, Message};
use dsh_session::types::SessionEvent;

/// 固定文本密度估算（到达精确 tokenization 前使用的启发式）。
pub const CHARS_PER_TOKEN: u64 = 4;
/// 每个 block 的结构性 JSON 框架 + 类型标签开销。
pub const BLOCK_OVERHEAD: u64 = 4;
/// 每条定价消息的角色字段框架开销。
pub const ROLE_OVERHEAD: u64 = 4;

/// 递归定价内容块（固定密度启发式）。
pub fn estimate_content(blocks: &[ContentBlock]) -> u64 {
    let mut tokens = 0u64;
    for block in blocks {
        match block {
            ContentBlock::Text(t) | ContentBlock::Reasoning(t) => {
                tokens += ceil_div(t.text.len() as u64, CHARS_PER_TOKEN) + BLOCK_OVERHEAD;
            }
            ContentBlock::ToolCall(c) => {
                tokens += ceil_div(c.name.len() as u64, CHARS_PER_TOKEN)
                    + ceil_div(c.arguments.len() as u64, CHARS_PER_TOKEN)
                    + BLOCK_OVERHEAD;
            }
            ContentBlock::ToolResult(r) => {
                tokens += estimate_content(&r.content) + BLOCK_OVERHEAD;
            }
            // ContentBlockMap 是合并可扩展的；未知 block 保留保守的结构性 JSON 价格。
            ContentBlock::Unknown { data, .. } => {
                let raw = serde_json::to_string(&serde_json::Value::Object(data.clone()))
                    .map(|s| s.len() as u64)
                    .unwrap_or(0);
                tokens += BLOCK_OVERHEAD + ceil_div(raw, CHARS_PER_TOKEN);
            }
            ContentBlock::Image(_) => {
                tokens += BLOCK_OVERHEAD; // 保守结构价格
            }
        }
    }
    tokens
}

/// 固定密度定价一条模型可见消息。
pub fn estimate_message(message: &Message) -> u64 {
    estimate_content(&message.content) + ROLE_OVERHEAD
}

fn ceil_div(a: u64, b: u64) -> u64 {
    a.div_ceil(b)
}

/// 一个 token 定价的 surface 节点。
#[derive(Debug, Clone, PartialEq)]
pub struct TokenSurfaceNode {
    /// surface 事件的持久序号。
    pub seq: u64,
    /// 此节点投影的消息的启发式 token 数。
    pub tokens: u64,
}

/// 一个 surface 事件在一次折叠中的放置与开销。
#[derive(Debug, Clone, PartialEq)]
pub struct SurfaceTokenFold {
    /// 事件自身消息的启发式价格；不派生时为 0。
    pub tokens: u64,
    /// 事件之后的 surface（脱离输入）。
    pub nodes: Vec<TokenSurfaceNode>,
    /// surface 总量的符号变化：`tokens` 减任何被遮蔽的部分。
    pub delta_tokens: i64,
}

/// 在已定价的 surface 上折叠一个 surface 事件。
///
/// 总量与分配全新：调用方赋值结果而非原地变更，因此 throw 时调用方状态不动，
/// 同一个损坏事件在每次重试上都以相同方式失败。
pub fn fold_surface_tokens(
    nodes: &[TokenSurfaceNode],
    event: &SessionEvent,
    message: Option<&Message>,
) -> Result<SurfaceTokenFold, String> {
    let tokens = message.map(estimate_message).unwrap_or(0);
    let op = event.surface_op().cloned();
    match op {
        Some(dsh_session::types::SurfaceOp::Append) => {
            let mut next = nodes.to_vec();
            next.push(TokenSurfaceNode { seq: event.seq, tokens });
            Ok(SurfaceTokenFold {
                tokens,
                nodes: next,
                delta_tokens: tokens as i64,
            })
        }
        Some(dsh_session::types::SurfaceOp::Replace { start, end }) => {
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
        None => Err(format!("token surface: event at seq {} has no surface op", event.seq)),
    }
}

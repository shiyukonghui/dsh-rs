//! request/header 重建工具（对齐
//! `deepseek-harness/packages/core/session/src/request-header.ts`）。
//!
//! 持有 session 日志的任何人，取最新 canonical 快照即可重建任意请求构建时使用的
//! `EpochHeader`；loop 用同一相等辅助避免记录未变化的 header。

use dsh_llm::call_config::{call_config_equals, CallConfig, CallConfigAdapterDefaults};
use dsh_llm::types::ToolSchema;

use crate::types::{EpochHeader, RequestHeaderReason, SessionEvent};

/// 规范化一个 header：空 system prompt 与空 tool list 变为缺席字段
/// （与请求如何构建一致；logging/folding/comparison 共用一种表示）。
pub fn canonical_header(header: &EpochHeader) -> EpochHeader {
    let adapter_defaults = header.adapter_defaults.as_ref();
    let has_adapter_defaults = matches!(
        adapter_defaults,
        Some(a) if a.reasoning_effort == Some(true) || a.max_tokens == Some(true)
    );
    EpochHeader {
        config: header.config.clone(),
        adapter_defaults: if has_adapter_defaults {
            header.adapter_defaults.clone()
        } else {
            None
        },
        system: header
            .system
            .as_ref()
            .filter(|s| !s.is_empty())
            .cloned(),
        tools: header.tools.as_ref().filter(|t| !t.is_empty()).cloned(),
    }
}

/// tool schema 按序 canonical JSON 相等（同一路径组装的 schema）。
fn same_schema(a: &ToolSchema, b: &ToolSchema) -> bool {
    a == b
}

/// 规范化 header 的字段级相等；tool schema 按序比较。
pub fn header_equals(a: &EpochHeader, b: &EpochHeader) -> bool {
    if !call_config_equals(&a.config, &b.config)
        || adapter_flag(&a.adapter_defaults, true) != adapter_flag(&b.adapter_defaults, true)
        || adapter_flag(&a.adapter_defaults, false) != adapter_flag(&b.adapter_defaults, false)
        || a.system != b.system
    {
        return false;
    }
    let at = a.tools.as_deref().unwrap_or(&[]);
    let bt = b.tools.as_deref().unwrap_or(&[]);
    at.len() == bt.len() && at.iter().zip(bt.iter()).all(|(x, y)| same_schema(x, y))
}

fn adapter_flag(d: &Option<CallConfigAdapterDefaults>, reasoning: bool) -> bool {
    match d {
        Some(a) => {
            if reasoning {
                a.reasoning_effort == Some(true)
            } else {
                a.max_tokens == Some(true)
            }
        }
        None => false,
    }
}

/// 折叠一个日志（或任意前缀）的 header 事件为最后一个快照后的 `EpochHeader`。
/// 非 header 事件跳过。这是纯离线重建路径；live session 增量维护同一折叠。
pub fn fold_request_header(events: &[SessionEvent], from: Option<&EpochHeader>) -> Option<EpochHeader> {
    let mut state = from.cloned();
    for event in events {
        if event.kind == crate::types::EventKind::RequestHeader {
            let payload: Result<crate::types::RequestHeaderPayload, serde_json::Error> =
                serde_json::from_value(event.data.clone());
            if let Ok(p) = payload {
                state = Some(canonical_header(&p.header));
            }
        }
    }
    state
}

/// 请求 header 的 reason 判定辅助（对齐 `RequestHeaderReason` wire 值）。
pub fn request_header_reason_str(r: &RequestHeaderReason) -> &'static str {
    match r {
        RequestHeaderReason::Initial => "initial",
        RequestHeaderReason::Resume => "resume",
        RequestHeaderReason::Change => "change",
    }
}

/// 一个 `CallConfig` 是否存在非空 provider/model（header 合法性入口）。
pub fn has_provider_model(config: &CallConfig) -> bool {
    !config.provider.is_empty() && !config.model.is_empty()
}

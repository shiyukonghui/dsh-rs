//! M3e: guard 切片——timeout-policy + repeat-tool-reminder（纯逻辑 seam + 最小 executor
//! 路径）。完整 agent-loop 接线（依赖 fs/shell M5 通道）不在 M3：此处交付：
//! - `tool_timeout_result`/`timeout_exceeded`——TOOL_TIMEOUT 结构化替换结果；
//! - `RepeatTracker`——按 agent 的连续重复探测（阈值 [3,5,8]，gentle/detailed 逐字）。
//!
//! 消息逐字对齐 deepseek-harness packages/guard/{timeout-policy,repeat-tool-reminder}。

use crate::runtime::{ToolErrorInfo, ToolExecutionResult};
use crate::types::{ToolExecution, ToolFailureData};
use dsh_llm::ContentBlock;
use std::collections::HashMap;

/// timeout-policy: 结构化替换结果的稳定 code（对齐 TS `TOOL_TIMEOUT`）。
pub const TOOL_TIMEOUT: &str = "TOOL_TIMEOUT";

/// `tool call timed out after {ms}ms`（逐字对齐 toolTimeoutResult 的 message）。
pub fn tool_timeout_message(timeout_ms: u64) -> String {
    format!("tool call timed out after {timeout_ms}ms")
}

/// 是否判定超时：声明了正有限预算且 elapsed >= 预算。
/// 无预算 / 非正 / 非有限 → 永不超时（对齐 TS：`timeoutMs === undefined` → 委托）。
pub fn timeout_exceeded(declared_timeout_ms: Option<f64>, elapsed_ms: u64) -> bool {
    match declared_timeout_ms {
        Some(t) if t.is_finite() && t > 0.0 => elapsed_ms as f64 >= t,
        _ => false,
    }
}

/// TOOL_TIMEOUT 结构化替换结果（对齐 `toolTimeoutResult`）：
/// content = `Error: tool call timed out after {ms}ms`；error = {message,
/// info:{name:'ToolTimeoutError', code:'TOOL_TIMEOUT'}}；isError。execution 沿用调用者。
pub fn tool_timeout_result(exec: &ToolExecution, timeout_ms: u64) -> ToolExecutionResult {
    let message = tool_timeout_message(timeout_ms);
    let info = ToolErrorInfo {
        message: message.clone(),
        info: Some(ToolFailureData {
            message: message.clone(),
            code: TOOL_TIMEOUT.to_string(),
            name: "ToolTimeoutError".to_string(),
        }),
    };
    ToolExecutionResult {
        execution: exec.clone(),
        value: None,
        content: vec![ContentBlock::text(format!("Error: {message}"))],
        content_annotation: None,
        is_error: true,
        error: Some(info),
        additional_contexts: Vec::new(),
        concludes_turn: false,
    }
}

// ---------------------------------------------------------------------------
// repeat-tool-reminder
// ---------------------------------------------------------------------------

/// 默认阈值（`[3, 5, 8]`）。
pub const DEFAULT_THRESHOLDS: [u64; 3] = [3, 5, 8];

/// gentle 首阈值提醒（逐字对齐 TS `GENTLE_REMINDER`）。
pub const GENTLE_REMINDER: &str =
    "You are repeating the exact same tool call with identical arguments. \
     Carefully analyze the previous result before calling again: if the task is \
     not complete, try a different approach or different arguments instead of \
     repeating the call.";

/// detailed 后续阈值提醒（逐字对齐 TS `detailedReminder`）。
pub fn detailed_reminder(tool_name: &str, count: u64, canonical_arguments: &str) -> String {
    format!(
        "Repeated tool call detected:\n\
         - tool: {tool_name}\n\
         - consecutive_calls: {count}\n\
         - arguments: {canonical_arguments}\n\
         The repeated calls are not making progress. Do not call this tool with these \
         exact arguments again. Inspect the latest result and choose a different action, \
         different arguments, or finish the task if enough evidence has been gathered."
    )
}

/// 深键排序（对齐 TS `sortJsonValue`）：object 键字典序递归排序，数组保序，标量原样。
fn sort_json_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut sorted: Vec<(String, serde_json::Value)> = map
                .iter()
                .map(|(k, v)| (k.clone(), sort_json_value(v)))
                .collect();
            sorted.sort_by(|(a, _), (b, _)| a.cmp(b));
            serde_json::Value::Object(sorted.into_iter().collect())
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(sort_json_value).collect())
        }
        other => other.clone(),
    }
}

/// canonical 参数串（对齐 TS `canonicalize` = stringify(sortJsonValue(args))）。
pub fn canonicalize(arguments: &serde_json::Value) -> String {
    serde_json::to_string(&sort_json_value(arguments)).unwrap_or_default()
}

/// `*`-wildcard 匹配（对齐 TS `wildcardToRegExp`：其余所有 regex 元字符按字面量处理，
/// `*` → `.*`，锚定全串）。用迭代实现避免引入 regex 依赖。
pub fn wildcard_matches(pattern: &str, name: &str) -> bool {
    let segments: Vec<&str> = pattern.split('*').collect();
    let (first, tail) = segments.split_first().unwrap();
    // 首段（无 `*` 时即全串）必须是前缀。
    if !name.starts_with(first) {
        return false;
    }
    let mut rest = &name[first.len()..];
    if tail.is_empty() {
        // 无 `*`：整串相等。
        return rest.is_empty();
    }
    // 尾段必须是剩余后缀；中间段从左到右顺序出现。
    let (last, middles) = tail.split_last().unwrap();
    for seg in middles {
        let Some(pos) = rest.find(seg) else {
            return false;
        };
        rest = &rest[pos + seg.len()..];
    }
    if last.is_empty() {
        return true; // 以 `*` 结尾：尾段为空，`.*` 匹配剩余。
    }
    rest.ends_with(last)
}

/// 校验阈值并排序（对齐 TS `validateThresholds`：非空 / 整数>=2 / 无重复 / 升序）。
/// 失败返回逐字或近似消息（fail-loud，不做静默回退）。
pub fn validate_thresholds(values: &[u64]) -> Result<Vec<u64>, String> {
    if values.is_empty() {
        return Err("repeat-tool-reminder: `thresholds` must not be empty".to_string());
    }
    for value in values {
        if *value < 2 {
            return Err(format!(
                "repeat-tool-reminder: invalid threshold {value} — every threshold must be an integer >= 2"
            ));
        }
    }
    let mut set = values.to_vec();
    set.sort_unstable();
    for pair in set.windows(2) {
        if pair[0] == pair[1] {
            return Err("repeat-tool-reminder: `thresholds` must not contain duplicates".to_string());
        }
    }
    Ok(set)
}

/// 截断 canonical 参数用于 detailed 提醒（对齐 TS `previewArguments`：只截展示文本，
/// 链 key 恒用完整 canonical）。`{}… (+N more chars)`。
pub fn preview_arguments(canonical: &str, cap: usize) -> String {
    if canonical.len() <= cap {
        canonical.to_string()
    } else {
        let head: String = canonical.chars().take(cap).collect();
        format!("{head}… (+{} more chars)", canonical.chars().count() - cap)
    }
}

/// 一条提醒（对齐 TS 的 `createUserMessage` 文本 + summary）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reminder {
    pub text: String,
    pub count: u64,
    pub summary: String,
}

/// 一个 agent 的连续重复链（对齐 TS `Chain`：last key + run length）。
#[derive(Debug, Clone)]
struct Chain {
    key: String,
    count: u64,
}

/// repeat-tool-reminder 追踪器（纯状态机；agent-loop 接线在 M5）。
pub struct RepeatTracker {
    thresholds: Vec<u64>,
    threshold_set: std::collections::BTreeSet<u64>,
    include: Vec<String>,
    exclude: Vec<String>,
    preview_chars: usize,
    chains: HashMap<String, Chain>,
}

impl RepeatTracker {
    /// `thresholds`/`include`/`exclude`/`preview_chars` 配置；校验失败 fail-loud。
    pub fn new(
        thresholds: &[u64],
        include: &[&str],
        exclude: &[&str],
        preview_chars: usize,
    ) -> Result<Self, String> {
        if preview_chars < 1 {
            return Err(format!(
                "repeat-tool-reminder: invalid argumentsPreviewChars {preview_chars} — must be an integer >= 1"
            ));
        }
        let mut sorted = validate_thresholds(thresholds)?;
        // 阈值即配置项缺省值——若调用者传空数组表示「用默认」，由构造侧约定；这里
        // 对齐 TS：validateThresholds 直接拒绝空，缺省由上层 schema default 提供。
        if sorted.is_empty() {
            sorted = DEFAULT_THRESHOLDS.to_vec();
        }
        let threshold_set = sorted.iter().copied().collect();
        let include: Vec<String> = include.iter().map(|s| s.to_string()).collect();
        let exclude: Vec<String> = exclude.iter().map(|s| s.to_string()).collect();
        Ok(RepeatTracker {
            thresholds: sorted,
            threshold_set,
            include,
            exclude,
            preview_chars,
            chains: HashMap::new(),
        })
    }

    /// 工具是否参与链（对齐 TS `tracked`：include 非空则须匹配其一；exclude 不匹配）。
    fn tracked(&self, tool_name: &str) -> bool {
        if !self.include.is_empty()
            && !self.include.iter().any(|p| wildcard_matches(p, tool_name))
        {
            return false;
        }
        !self.exclude.iter().any(|p| wildcard_matches(p, tool_name))
    }

    /// 观察一次尝试：推进链并返回触发阈值时的提醒。无 agent / 未 tracked → None
    /// （既不计数也不重置，对齐 TS）。
    pub fn observe(
        &mut self,
        agent: Option<&str>,
        tool_name: &str,
        arguments: &serde_json::Value,
    ) -> Option<Reminder> {
        let agent = agent?;
        if !self.tracked(tool_name) {
            return None;
        }
        let canonical = canonicalize(arguments);
        let key = format!("[{tool_name},{canonical}]");
        let chain = self.chains.get_mut(agent);
        let count = match chain {
            Some(c) if c.key == key => {
                c.count += 1;
                c.count
            }
            _ => {
                self.chains.insert(
                    agent.to_string(),
                    Chain { key: key.clone(), count: 1 },
                );
                1
            }
        };
        if !self.threshold_set.contains(&count) {
            return None;
        }
        let text = if count == self.thresholds[0] {
            GENTLE_REMINDER.to_string()
        } else {
            detailed_reminder(tool_name, count, &preview_arguments(&canonical, self.preview_chars))
        };
        Some(Reminder {
            text,
            count,
            summary: format!("{tool_name} × {count}"),
        })
    }

    /// 用户插话重置（对齐 TS `agent/pre-step`：messages 含 user → 删链）。
    pub fn reset(&mut self, agent: &str) {
        self.chains.remove(agent);
    }

    /// agent dispose 时清理其链（对齐 TS WeakMap 生命周期）。
    pub fn drop_agent(&mut self, agent: &str) {
        self.chains.remove(agent);
    }
}

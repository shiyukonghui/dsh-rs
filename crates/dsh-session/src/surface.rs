//! 会话日志的 surface 层：产生 LLM 消息的事件的有序视图（对齐
//! `deepseek-harness/packages/core/session/src/surface.ts`）。
//!
//! append-only 日志仍是事实源；本模块提供**规范 surface 折叠**：
//! - `fold_surface`：完整重放一个日志 → 当前 surface 节点序列 + 替换历史；
//! - `SurfaceManager`：增量（validate-then-commit），供 live `Session.append` 使用；
//! - `derive_event_message`：单节点投影规则（THE per-node projection）。
//!
//! 纯数据/纯函数，不依赖任何 IO —— 可差分主体。

use serde_json::Value;

use dsh_llm::types::Message;

use crate::types::{EventKind, SessionEvent, SurfaceOp};

/// surface-eligible 事件类型（产生模型消息的三个类型）。
// 复用 types::SURFACE_EVENT_TYPES（同一常量，避免 glob 重导出冲突）。
pub fn is_surface_eligible_type(kind: &str) -> bool {
    crate::types::SURFACE_EVENT_TYPES.contains(&kind)
}

/// 某事件是否是一条 surface 事件：类型 eligible 且携带 `surfaceOp`。
pub fn is_surface_event(event: &SessionEvent) -> bool {
    is_surface_eligible_type(event.kind.as_str()) && event.surface_op().is_some()
}

/// 是否是一条 append 起源的 surface 事件（在自身日志位置入尾部，非替换副本）。
pub fn is_append_surface_event(event: &SessionEvent) -> bool {
    is_surface_event(event) && event.surface_op() == Some(&SurfaceOp::Append)
}

/// 是否是一条 surface 替换（shadow 了既有范围而非追加尾部）。
pub fn is_replacement_surface_event(event: &SessionEvent) -> bool {
    is_surface_event(event) && event.surface_op() != Some(&SurfaceOp::Append)
}

/// 单节点投影：把一条事件投影为它派生出的 LLM 消息，或 None（无产出）。
///
/// THE 规范投影规则：`Session.deriveMessages` 在 live surface 上折叠它；外部重构器在日志前缀的
/// surface 上折叠同一函数以重建任意请求构建时完全相同的历史。
/// - `user/message` → `event.data` 逐字透传（data 本身即完整 `Message`）；
/// - `assistant/message` → `data.message`；**空 content 跳过**（仅承载 usage 的 max-tokens 步骤）；
/// - `tool/result` → `data.message`；
/// - 其余（边界/chunk/log-only）→ None。
pub fn derive_event_message(event: &SessionEvent) -> Result<Option<Message>, serde_json::Error> {
    match event.kind {
        EventKind::UserMessage => serde_json::from_value(event.data.clone()).map(Some),
        EventKind::AssistantMessage => {
            let msg: Message = serde_json::from_value(pick(&event.data, "message"))?;
            // 空 content 助手消息跳过（只承载 usage）
            if msg.content.is_empty() {
                return Ok(None);
            }
            Ok(Some(msg))
        }
        EventKind::ToolResult => {
            let msg: Message = serde_json::from_value(pick(&event.data, "message"))?;
            Ok(Some(msg))
        }
        _ => Ok(None),
    }
}

fn pick(v: &Value, key: &str) -> Value {
    v.get(key).cloned().unwrap_or(Value::Null)
}

/// 折叠一个 surface 时观察到的替换操作。
#[derive(Debug, Clone, PartialEq)]
pub struct SurfaceFoldReplacement {
    /// 替换了先前 surface 范围的事件 seq。
    pub seq: u64,
    /// 声明的被替换 surface 范围（含）起始 seq。
    pub start: u64,
    /// 声明的被替换 surface 范围（含）结束 seq。
    pub end: u64,
    /// 该操作实际移除的 surface 条目（surface 顺序）。
    pub shadowed_seqs: Vec<u64>,
}

/// 重放一个 session 日志中 surface 操作的完整结果。
#[derive(Debug, Clone, PartialEq)]
pub struct SurfaceFoldResult {
    /// 当前 surface 事件 seq（模型可见顺序）。
    pub nodes: Vec<u64>,
    /// 替换操作（事件顺序）。
    pub replacements: Vec<SurfaceFoldReplacement>,
}

/// 可变折叠状态（完整 + 增量折叠共享）。
#[derive(Debug, Clone, Default)]
pub(crate) struct SurfaceFoldState {
    pub nodes: Vec<u64>,
    pub replace_generation: u64,
}

/// 一个已校验、尚未改变折叠状态的替换计划。
#[derive(Debug, Clone)]
struct SurfaceReplacePlan {
    seq: u64,
    start: u64,
    end: u64,
    start_idx: usize,
    end_idx: usize,
    shadowed_seqs: Vec<u64>,
}

/// 一个尚未改变折叠状态的 surface 迁移计划。
#[derive(Debug, Clone)]
enum SurfacePlan {
    Append { seq: u64 },
    Replace(SurfaceReplacePlan),
}

/// surface 校验错误（TS 用 throw Error；Rust 用带消息的错误）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceError(pub String);

impl std::fmt::Display for SurfaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SurfaceError {}

fn is_event_seq(v: u64) -> bool {
    // Rust u64 天然非负整数（TS Number.isSafeInteger 的约束 u64 均满足）
    let _ = v;
    true
}

/// `surfaceOpOf`：校验事件局部的 surface-eligible 性并返回其操作。
/// 非 eligible 且携带 surfaceOp/sourceEventSeqs → 错误；eligible 但缺 surfaceOp → 错误。
fn surface_op_of(event: &SessionEvent) -> Result<Option<SurfaceOp>, SurfaceError> {
    if !is_surface_eligible_type(event.kind.as_str()) {
        if event.surface_op().is_some() {
            return Err(SurfaceError(format!(
                "session event \"{}\" is not surface-eligible and cannot carry surfaceOp",
                event.kind.as_str()
            )));
        }
        if event.source_event_seqs().is_some() {
            return Err(SurfaceError(format!(
                "session event \"{}\" is not surface-eligible and cannot carry sourceEventSeqs",
                event.kind.as_str()
            )));
        }
        return Ok(None);
    }
    let op = event.surface_op().ok_or_else(|| {
        SurfaceError(format!(
            "session event \"{}\" is surface-eligible and requires a surfaceOp marker",
            event.kind.as_str()
        ))
    })?;
    match op {
        SurfaceOp::Append => Ok(Some(SurfaceOp::Append)),
        SurfaceOp::Replace { start, end } => {
            if !is_event_seq(*start) || !is_event_seq(*end) {
                return Err(SurfaceError(format!(
                    "session event \"{}\" carries an invalid surfaceOp",
                    event.kind.as_str()
                )));
            }
            Ok(Some(SurfaceOp::Replace { start: *start, end: *end }))
        }
    }
}

/// `assertProvenance`：校验被引用的来源事件 seq 对早前日志条目与替换范围。
fn assert_provenance(
    event: &SessionEvent,
    shadowed_seqs: &[u64],
) -> Result<(), SurfaceError> {
    let mut sources = std::collections::HashSet::new();
    if let Some(raw) = event.source_event_seqs() {
        if raw.is_empty() && event.kind != EventKind::AssistantMessage {
            return Err(SurfaceError(
                "sourceEventSeqs must not be empty except on assistant/message".to_string(),
            ));
        }
        let mut non_earlier_source: Option<u64> = None;
        for &source in raw {
            if !is_event_seq(source) {
                return Err(SurfaceError(format!(
                    "session event \"{}\" sourceEventSeqs must densely contain non-negative safe integers",
                    event.kind.as_str()
                )));
            }
            sources.insert(source);
            if non_earlier_source.is_none() && source >= event.seq {
                non_earlier_source = Some(source);
            }
        }
        if sources.len() != raw.len() {
            return Err(SurfaceError("sourceEventSeqs must not contain duplicates".to_string()));
        }
        if let Some(non_earlier) = non_earlier_source {
            return Err(SurfaceError(format!(
                "sourceEventSeqs must reference earlier events: {non_earlier} >= current seq {}",
                event.seq
            )));
        }
    }
    let missing: Vec<u64> = shadowed_seqs
        .iter()
        .copied()
        .filter(|seq| !sources.contains(seq))
        .collect();
    if !missing.is_empty() {
        return Err(SurfaceError(format!(
            "surface replace: sourceEventSeqs must include every shadowed surface node; missing {}",
            missing
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    Ok(())
}

/// `replacementRange`：定位替换范围（不动当前折叠状态）。
fn replacement_range(
    state: &SurfaceFoldState,
    op: &SurfaceOp,
) -> Result<(usize, usize, Vec<u64>), SurfaceError> {
    let (start, end) = match op {
        SurfaceOp::Replace { start, end } => (*start, *end),
        SurfaceOp::Append => unreachable!("replacementRange only for replace"),
    };
    let start_idx = state
        .nodes
        .iter()
        .position(|&s| s == start)
        .ok_or_else(|| SurfaceError(format!("surface replace: start seq {start} not found in surface")))?;
    let end_idx = state
        .nodes
        .iter()
        .position(|&s| s == end)
        .ok_or_else(|| SurfaceError(format!("surface replace: end seq {end} not found in surface")))?;
    if start_idx > end_idx {
        return Err(SurfaceError(format!(
            "surface replace: start seq {start} (index {start_idx}) is after end seq {end} (index {end_idx})"
        )));
    }
    let shadowed = state.nodes[start_idx..=end_idx].to_vec();
    Ok((start_idx, end_idx, shadowed))
}

/// 会话事件 JSON 值域上的深结构相等（null/bool/number/string/array/plain object）。
fn is_deep_equal_json(a: &Value, b: &Value) -> bool {
    a == b
}

/// `assertToolResultRewrite`：tool/result 替换受限于一个当前 result 的 content。
fn assert_tool_result_rewrite(
    event: &SessionEvent,
    shadowed_seqs: &[u64],
    events: &[SessionEvent],
    base_seq: u64,
) -> Result<(), SurfaceError> {
    if event.kind != EventKind::ToolResult {
        return Ok(());
    }
    if shadowed_seqs.len() != 1 {
        return Err(SurfaceError(
            "tool/result surface replacement must rewrite exactly one current node".to_string(),
        ));
    }
    for &original_seq in shadowed_seqs {
        let idx = original_seq.checked_sub(base_seq).ok_or_else(|| {
            SurfaceError("tool/result surface replacement: original seq below base".to_string())
        })?;
        let original = events.get(idx as usize).ok_or_else(|| {
            SurfaceError("tool/result surface replacement: original event missing".to_string())
        })?;
        if original.kind != EventKind::ToolResult {
            return Err(SurfaceError(
                "tool/result surface replacement must target a current tool/result".to_string(),
            ));
        }
        // 把双方 message.content[0].content 置 null 后深比较其余字段
        let mut original_rest = original.data.clone();
        let mut replacement_rest = event.data.clone();
        for v in [&mut original_rest, &mut replacement_rest] {
            if let Some(block) = v.pointer_mut("/message/content/0").and_then(|b| b.as_object_mut())
            {
                block.insert("content".into(), Value::Null);
            }
        }
        if !is_deep_equal_json(&original_rest, &replacement_rest) {
            return Err(SurfaceError(
                "tool/result surface replacement may change only content".to_string(),
            ));
        }
    }
    Ok(())
}

/// `planSurfaceEvent`：在一个重放边界校验一条事件，并准备其原子折叠迁移。
fn plan_surface_event(
    state: &SurfaceFoldState,
    event: &SessionEvent,
    expected_seq: u64,
    events: &[SessionEvent],
    base_seq: u64,
) -> Result<Option<SurfacePlan>, SurfaceError> {
    if event.seq != expected_seq {
        return Err(SurfaceError(format!(
            "session event seq {} is not contiguous; expected {expected_seq}",
            event.seq
        )));
    }
    let surface_op = surface_op_of(event)?;
    let Some(op) = surface_op else {
        return Ok(None);
    };
    match op {
        SurfaceOp::Append => {
            assert_provenance(event, &[])?;
            Ok(Some(SurfacePlan::Append { seq: event.seq }))
        }
        SurfaceOp::Replace { start, end } => {
            let (start_idx, end_idx, shadowed_seqs) = replacement_range(state, &op)?;
            assert_provenance(event, &shadowed_seqs)?;
            assert_tool_result_rewrite(event, &shadowed_seqs, events, base_seq)?;
            Ok(Some(SurfacePlan::Replace(SurfaceReplacePlan {
                seq: event.seq,
                start,
                end,
                start_idx,
                end_idx,
                shadowed_seqs,
            })))
        }
    }
}

/// 提交一个此前已校验的 surface 迁移；发生替换时返回替换元数据。
fn apply_surface_plan(
    state: &mut SurfaceFoldState,
    plan: &Option<SurfacePlan>,
) -> Option<SurfaceFoldReplacement> {
    match plan {
        Some(SurfacePlan::Append { seq }) => {
            state.nodes.push(*seq);
            None
        }
        Some(SurfacePlan::Replace(repl)) => {
            state.nodes.splice(
                repl.start_idx..=repl.end_idx,
                std::iter::once(repl.seq),
            );
            state.replace_generation += 1;
            Some(SurfaceFoldReplacement {
                seq: repl.seq,
                start: repl.start,
                end: repl.end,
                shadowed_seqs: repl.shadowed_seqs.clone(),
            })
        }
        None => None,
    }
}

/// 应用一条事件并返回替换元数据（仅在发生替换时）。
fn apply_surface_event(
    state: &mut SurfaceFoldState,
    event: &SessionEvent,
    expected_seq: u64,
    events: &[SessionEvent],
    base_seq: u64,
) -> Result<Option<SurfaceFoldReplacement>, SurfaceError> {
    let plan = plan_surface_event(state, event, expected_seq, events, base_seq)?;
    Ok(apply_surface_plan(state, &plan))
}

/// 完整重放一个连续 seq 的 session 日志，走规范 surface 折叠。
/// 校验失败（surface 元数据/来源引用/范围/tool-result 重写规则违规）返回 Err。
pub fn fold_surface(events: &[SessionEvent]) -> Result<SurfaceFoldResult, SurfaceError> {
    let mut state = SurfaceFoldState::default();
    let mut replacements = Vec::new();
    for (index, event) in events.iter().enumerate() {
        if let Some(replacement) =
            apply_surface_event(&mut state, event, index as u64, events, 0)?
        {
            replacements.push(replacement);
        }
    }
    Ok(SurfaceFoldResult {
        nodes: state.nodes,
        replacements,
    })
}

/// 增量有序 surface 视图 + append 边界校验器（对齐 TS `SurfaceManager`）。
///
/// 不持有日志引用：调用方在每次操作传入当前日志切片（与 TS 构造时捕获
/// `this.log`、此后经 `this.log` 读取的行为等价——Session 控制操作序）。
#[derive(Debug)]
pub struct SurfaceManager {
    /// 共享迁移状态；替换历史不保留。
    state: SurfaceFoldState,
    /// 已处理的绝对 seq（窗口内）。
    last_processed_seq: i64,
    /// 已由 `validate_next` 校验、等待精确日志接收的候选计划。
    pending_plan: Option<(u64, Option<SurfacePlan>)>,
    /// 窗口基底绝对 seq（窗口首事件的 seq）。
    base_seq: u64,
}

impl SurfaceManager {
    pub fn new(base_seq: u64) -> Self {
        SurfaceManager {
            state: SurfaceFoldState::default(),
            last_processed_seq: base_seq as i64 - 1,
            pending_plan: None,
            base_seq,
        }
    }

    /// 多少已折叠的位置替换与当前节点（供 Session 读取）。
    pub(crate) fn state(&self) -> &SurfaceFoldState {
        &self.state
    }

    fn catch_up(
        &mut self,
        events: &[SessionEvent],
    ) -> Result<(), SurfaceError> {
        if events.is_empty() {
            return Ok(());
        }
        let tail_seq = self.base_seq + events.len() as u64 - 1;
        let tail = tail_seq as i64;
        if tail < self.last_processed_seq + 1 {
            return Ok(());
        }
        for seq in (self.last_processed_seq + 1)..=tail {
            let idx = (seq - self.base_seq as i64) as usize;
            let event = &events[idx];
            let pending = self.pending_plan.take();
            match pending {
                Some((expected, plan)) if expected == seq as u64 => {
                    // 已校验的候选精确入日志：应用计划
                    apply_surface_plan(&mut self.state, &plan);
                }
                _ => {
                    // 未配对的计划或计划缺失：按最新日志重放该事件（完整校验）
                    let plan =
                        plan_surface_event(&self.state, event, seq as u64, events, self.base_seq)?;
                    apply_surface_plan(&mut self.state, &plan);
                }
            }
            self.last_processed_seq = seq;
        }
        Ok(())
    }

    /// 校验下一个候选（不改变已提交的 surface）。
    /// `events` 是候选进入日志**前**的当前日志（全切片）。
    pub fn validate_next(
        &mut self,
        event: &SessionEvent,
        events: &[SessionEvent],
    ) -> Result<(), SurfaceError> {
        self.catch_up(events)?;
        let expected_seq = self.base_seq + events.len() as u64;
        let plan = plan_surface_event(&self.state, event, expected_seq, events, self.base_seq)?;
        self.pending_plan = Some((expected_seq, plan));
        Ok(())
    }

    /// 在事件已进入日志后提交增量：应用待决计划（匹配时）或重放 delta。
    /// 任何入口（nodes/replace_generation/derive）都先 `catch_up`。
    pub fn commit_next(
        &mut self,
        events: &[SessionEvent],
    ) -> Result<(), SurfaceError> {
        self.catch_up(events)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_brand::{CallId, MessageId};
    use serde_json::json;

    fn ev(seq: u64, kind: EventKind, data: Value) -> SessionEvent {
        SessionEvent::new(seq, 1000 + seq as i64, kind, data)
    }

    fn user_msg(seq: u64) -> SessionEvent {
        ev(
            seq,
            EventKind::UserMessage,
            json!({
                "id": MessageId(format!("m-{seq}")),
                "role": "user",
                "content": [{"type": "text", "text": format!("hello {seq}")}],
                "source": {"kind": "user"},
            }),
        )
        .with_surface_op(SurfaceOp::Append)
    }

    fn assistant_msg(seq: u64) -> SessionEvent {
        ev(
            seq,
            EventKind::AssistantMessage,
            json!({
                "turn": 1, "step": 1,
                "message": {
                    "id": MessageId(format!("a-{seq}")),
                    "role": "assistant",
                    "content": [{"type": "text", "text": format!("answer {seq}")}],
                    "source": {"kind": "model", "provider": "p", "model": "m"},
                },
            }),
        )
        .with_surface_op(SurfaceOp::Append)
    }

    #[test]
    fn surface_type_predicates() {
        assert!(is_surface_eligible_type("user/message"));
        assert!(is_surface_eligible_type("assistant/message"));
        assert!(is_surface_eligible_type("tool/result"));
        assert!(!is_surface_eligible_type("turn/start"));
        assert!(!is_surface_eligible_type("assistant/chunk"));
    }

    #[test]
    fn fold_append_events_produce_ordered_nodes() {
        let events = vec![user_msg(0), user_msg(1), assistant_msg(2)];
        let fold = fold_surface(&events).unwrap();
        assert_eq!(fold.nodes, vec![0, 1, 2]);
        assert!(fold.replacements.is_empty());
    }

    #[test]
    fn fold_derives_messages_in_surface_order() {
        let events = vec![user_msg(0), user_msg(1), assistant_msg(2)];
        let fold = fold_surface(&events).unwrap();
        let msgs: Vec<Value> = fold
            .nodes
            .iter()
            .map(|&seq| events[seq as usize].data.clone())
            .collect();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0], json!({
            "id": MessageId("m-0".into()),
            "role": "user",
            "content": [{"type": "text", "text": "hello 0"}],
            "source": {"kind": "user"},
        }));
    }

    #[test]
    fn replace_shadows_range_and_folds_replacements() {
        // user/message 0,1 被 assistant compaction 结果 2 替换
        let events = vec![
            user_msg(0),
            user_msg(1),
            ev(2, EventKind::AssistantMessage, json!({
                "turn": 1, "step": 2,
                "message": {
                    "id": MessageId("c-2".into()),
                    "role": "assistant",
                    "content": [{"type": "text", "text": "summary"}],
                    "source": {"kind": "model", "provider": "p", "model": "m"},
                },
            }))
            .with_surface_op(SurfaceOp::Replace { start: 0, end: 1 })
            .with_source_event_seqs(vec![0, 1]),
        ];
        let fold = fold_surface(&events).unwrap();
        assert_eq!(fold.nodes, vec![2]);
        assert_eq!(fold.replacements.len(), 1);
        assert_eq!(fold.replacements[0].shadowed_seqs, vec![0, 1]);
    }

    #[test]
    fn replace_missing_provenance_rejects() {
        // replace 必须 sourceEventSeqs 覆盖全部 shadowed 节点
        let events = vec![
            user_msg(0),
            user_msg(1),
            ev(2, EventKind::AssistantMessage, json!({
                "turn": 1, "step": 2,
                "message": {
                    "id": MessageId("c-2".into()),
                    "role": "assistant",
                    "content": [{"type": "text", "text": "summary"}],
                    "source": {"kind": "model", "provider": "p", "model": "m"},
                },
            }))
            .with_surface_op(SurfaceOp::Replace { start: 0, end: 1 }),
        ];
        let err = fold_surface(&events).unwrap_err();
        assert!(err.0.contains("must include every shadowed surface node"));
    }

    #[test]
    fn replace_start_absent_rejects() {
        let events = vec![
            user_msg(0),
            ev(1, EventKind::AssistantMessage, json!({
                "turn": 1, "step": 2,
                "message": {
                    "id": MessageId("c-1".into()),
                    "role": "assistant",
                    "content": [{"type": "text", "text": "s"}],
                    "source": {"kind": "model", "provider": "p", "model": "m"},
                },
            }))
            .with_surface_op(SurfaceOp::Replace { start: 3, end: 5 })
            .with_source_event_seqs(vec![3, 5]),
        ];
        let err = fold_surface(&events).unwrap_err();
        assert!(err.0.contains("start seq 3 not found in surface"));
    }

    #[test]
    fn non_surface_event_with_surface_op_rejects() {
        let events = vec![ev(0, EventKind::TurnStart, json!({"turn": 1}))
            .with_surface_op(SurfaceOp::Append)];
        let err = fold_surface(&events).unwrap_err();
        assert!(err.0.contains("is not surface-eligible"));
    }

    #[test]
    fn surface_eligible_without_marker_rejects() {
        let events = vec![ev(
            0,
            EventKind::UserMessage,
            json!({"id": MessageId("m0".into()), "role": "user", "content": [], "source": {"kind": "user"}}),
        )];
        let err = fold_surface(&events).unwrap_err();
        assert!(err.0.contains("requires a surfaceOp marker"));
    }

    #[test]
    fn empty_source_seqs_rejected_except_assistant() {
        // user/message 空 sourceEventSeqs → reject
        let events = vec![user_msg(0).with_source_event_seqs(vec![])];
        let err = fold_surface(&events).unwrap_err();
        assert!(err.0.contains("must not be empty except on assistant/message"));
    }

    #[test]
    fn non_earlier_source_rejects() {
        // sourceEventSeqs 引用必须早于当前事件 seq
        let events = vec![
            user_msg(0),
            // assistant/message 引用未来 seq 3
            ev(1, EventKind::AssistantMessage, json!({
                "turn": 1, "step": 1,
                "message": {
                    "id": MessageId("a-1".into()),
                    "role": "assistant",
                    "content": [{"type": "text", "text": "x"}],
                    "source": {"kind": "model", "provider": "p", "model": "m"},
                },
            }))
            .with_surface_op(SurfaceOp::Append)
            .with_source_event_seqs(vec![3]),
        ];
        let err = fold_surface(&events).unwrap_err();
        assert!(err.0.contains("must reference earlier events"));
    }

    #[test]
    fn tool_result_rewrite_must_change_only_content() {
        fn tool_result(seq: u64, text: &str, surface_op: SurfaceOp) -> SessionEvent {
            ev(
                seq,
                EventKind::ToolResult,
                json!({
                    "turn": 1, "step": 1,
                    "message": {
                        "id": MessageId("t-0".into()),
                        "role": "user",
                        "content": [{"type": "tool-result", "toolCallId": CallId("c1".into()), "content": [{"type": "text", "text": text}]}],
                        "source": {"kind": "tool", "callId": CallId("c1".into())},
                    },
                }),
            )
            .with_surface_op(surface_op)
        }
        let original = tool_result(0, "original", SurfaceOp::Append);
        let replacement = tool_result(1, "rewritten", SurfaceOp::Replace { start: 0, end: 0 })
            .with_source_event_seqs(vec![0]);
        let events = vec![original, replacement];
        let fold = fold_surface(&events).unwrap();
        assert_eq!(fold.nodes, vec![1]);
        assert_eq!(fold.replacements[0].shadowed_seqs, vec![0]);
    }

    #[test]
    fn tool_result_rewrite_reject_content_and_other() {
        fn tool_result(seq: u64, error_code: Option<&str>, surface_op: SurfaceOp) -> SessionEvent {
            let mut msg = json!({
                "turn": 1, "step": 1,
                "message": {
                    "id": MessageId(format!("t-{seq}")),
                    "role": "user",
                    "content": [{"type": "tool-result", "toolCallId": CallId("c1".into()), "content": []}],
                    "source": {"kind": "tool", "callId": CallId("c1".into())},
                },
            });
            if let Some(code) = error_code {
                msg["error"] = json!({"name": code, "code": code});
            }
            ev(seq, EventKind::ToolResult, msg).with_surface_op(surface_op)
        }
        let original = tool_result(0, None, SurfaceOp::Append);
        // 替换改了 error（非 content）→ reject
        let replacement = tool_result(1, Some("E"), SurfaceOp::Replace { start: 0, end: 0 })
            .with_source_event_seqs(vec![0]);
        let events = vec![original, replacement];
        let err = fold_surface(&events).unwrap_err();
        assert!(err.0.contains("may change only content"));
    }

    #[test]
    fn tool_result_rewrite_must_target_tool_result() {
        let original = user_msg(0);
        // assistant/message replace 一个 user/message 节点（tool-result 规则只对 tool/result）
        // —— 这里用 tool/result 替换 user/message 节点 → reject
        let replacement = ev(
            1,
            EventKind::ToolResult,
            json!({
                "turn": 1, "step": 1,
                "message": {
                    "id": MessageId("t-1".into()),
                    "role": "user",
                    "content": [{"type": "tool-result", "toolCallId": CallId("c1".into()), "content": []}],
                    "source": {"kind": "tool", "callId": CallId("c1".into())},
                },
            }),
        )
        .with_surface_op(SurfaceOp::Replace { start: 0, end: 0 })
        .with_source_event_seqs(vec![0]);
        let events = vec![original, replacement];
        let err = fold_surface(&events).unwrap_err();
        assert!(err.0.contains("must target a current tool/result"));
    }

    #[test]
    fn backward_replace_start_after_end_rejects() {
        let events = vec![
            user_msg(0),
            user_msg(1),
            ev(2, EventKind::AssistantMessage, json!({
                "turn": 1, "step": 1,
                "message": {
                    "id": MessageId("a-2".into()),
                    "role": "assistant",
                    "content": [{"type": "text", "text": "x"}],
                    "source": {"kind": "model", "provider": "p", "model": "m"},
                },
            }))
            .with_surface_op(SurfaceOp::Replace { start: 1, end: 0 })
            .with_source_event_seqs(vec![0, 1]),
        ];
        let err = fold_surface(&events).unwrap_err();
        assert!(err.0.contains("is after end seq"));
    }

    #[test]
    fn contiguous_seq_invariant_rejects_gap() {
        // seq 必须连续（0,1,3 跳过 2）
        let events = vec![user_msg(0), user_msg(2)];
        let err = fold_surface(&events).unwrap_err();
        assert!(err.0.contains("is not contiguous"));
    }

    #[test]
    fn derived_message_empty_assistant_skipped() {
        // assistant/message 空 content → derive None
        let events = vec![ev(
            0,
            EventKind::AssistantMessage,
            json!({
                "turn": 1, "step": 1,
                "message": {
                    "id": MessageId("a-0".into()),
                    "role": "assistant",
                    "content": [],
                    "source": {"kind": "model", "provider": "p", "model": "m"},
                },
            }),
        )
        .with_surface_op(SurfaceOp::Append)];
        let fold = fold_surface(&events).unwrap();
        assert_eq!(fold.nodes, vec![0]);
        let msg = derive_event_message(&events[0]).unwrap();
        assert!(msg.is_none());
    }

    #[test]
    fn surface_manager_validate_then_commit() {
        let mut mgr = SurfaceManager::new(0);
        let mut log: Vec<SessionEvent> = Vec::new();
        let e0 = user_msg(0);
        mgr.validate_next(&e0, &log).unwrap();
        log.push(e0.clone());
        mgr.commit_next(&log).unwrap();
        assert_eq!(mgr.state().nodes, [0]);

        // append 校验 + 提交
        let e1 = user_msg(1);
        mgr.validate_next(&e1, &log).unwrap();
        log.push(e1.clone());
        mgr.commit_next(&log).unwrap();
        assert_eq!(mgr.state().nodes, [0, 1]);

        let e2 = ev(2, EventKind::AssistantMessage, json!({
            "turn": 1, "step": 1,
            "message": {
                "id": MessageId("a-2".into()),
                "role": "assistant",
                "content": [{"type": "text", "text": "s"}],
                "source": {"kind": "model", "provider": "p", "model": "m"},
            },
        }))
        .with_surface_op(SurfaceOp::Replace { start: 0, end: 1 })
        .with_source_event_seqs(vec![0, 1]);
        mgr.validate_next(&e2, &log).unwrap();
        log.push(e2.clone());
        mgr.commit_next(&log).unwrap();
        assert_eq!(mgr.state().nodes, [2]);
        assert_eq!(mgr.state().replace_generation, 1);
    }
}

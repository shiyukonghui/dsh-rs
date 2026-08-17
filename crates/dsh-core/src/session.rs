//! DSH 层 session 缝的数据承载：append-only 事件日志 + 模型历史投影。
//!
//! 第一性原理：缝的权威契约是 WIT（`dsh-loop.wit` 的 `session` 接口）；本模块是
//! **宿主的承载**——回答「session 日志长什么样、如何投影模型历史」，与 WASM loop
//! 正交。WASM loop 经缝写入（`LoopHost` 桥接本类型），宿主可经此查询。
//!
//! 对应 deepseek-harness `Session`（append-only `SessionEvent` log +
//! `deriveMessages` + **surface 折叠**）：
//! - `append(kind, payload)` → 追加事件，返回序号；
//! - `derive_messages()` → 投影 user/assistant/tool 消息序列（模型历史）；
//! - **M36 surface 折叠**：对齐 DSH `foldSurface`/`SessionSurface`——surface-
//!   eligible 事件（user/message、assistant/message、tool/result）入 surface
//!   节点序列（模型可见顺序）；replace 操作替换 [start, end] 范围（compaction
//!   语义，旧节点被 shadow）；`derive_messages` 只对**当前 surface 节点**投影。
//!   `append(kind, payload)` 保持 WIT 缝签名（默认 append 入列）；宿主侧
//!   compaction 等用 `append_with_op`（replace）。
//!
//! 共享句柄用 `Arc<Mutex<>>`（而非 `Rc<RefCell<>>`）：dsh-core 服务仓库的
//! `Impl.value: Arc<dyn Any + Send + Sync>` 要求服务值 Send+Sync；运行时单线程，
//! Mutex 仅用于满足类型约束（无跨线程竞争）。

use std::sync::{Arc, Mutex};

use crate::types::Value;

/// 一条 session 事件（append-only）。
#[derive(Debug, Clone)]
pub struct SessionEvent {
    /// 单调递增序号（0 起）。
    pub seq: u64,
    /// 事件种类（`turn/start`、`user/message`、`assistant/message`、`tool/result`…）。
    pub kind: String,
    /// 事件载荷（JSON 字节；WIT 缝的 `list<u8>` 形态）。
    pub payload: Vec<u8>,
}

impl SessionEvent {
    /// 解析载荷为 JSON 值。
    pub fn payload_value(&self) -> Value {
        serde_json::from_slice(&self.payload).unwrap_or(Value::Null)
    }
}

/// surface 放置操作（对齐 DSH `SurfaceOp`：`'append'` 或
/// `{op:'replace', start, end}`）。仅 surface-eligible 事件可携带。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceOp {
    /// 追加到 surface 尾部（正常路径）。
    Append,
    /// 替换当前 surface 上 [start, end]（含）范围节点为当前事件。
    Replace { start: u64, end: u64 },
}

/// surface-eligible 事件种类（对齐 DSH `SURFACE_EVENT_TYPES`）。
const SURFACE_ELIGIBLE: [&str; 3] = ["user/message", "assistant/message", "tool/result"];

fn is_surface_eligible(kind: &str) -> bool {
    SURFACE_ELIGIBLE.contains(&kind)
}

/// 对齐 DSH `assertToolResultRewrite`：tool/result 替换仅允许改 content——
/// 把双方 data 的 `message.content[0].content` 置 null 后深比较其余字段
/// （生产：`originalRest`/`replacementRest` 置 content:null 后 isDeepEqualJson）。
fn tool_result_only_content_changed(original_data: &Value, new_payload: &[u8]) -> bool {
    let Ok(new_data) = serde_json::from_slice::<Value>(new_payload) else {
        return false;
    };
    let mut orig = original_data.clone();
    let mut new = new_data.clone();
    // 只把第一条 block 的 content 归零（生产取 message.content[0]）
    for v in [&mut orig, &mut new] {
        if let Some(block) = v
            .pointer_mut("/message/content/0")
            .and_then(|b| b.as_object_mut())
        {
            block.insert("content".into(), Value::Null);
        }
    }
    orig == new
}

/// 会话日志（append-only；内部 `Mutex` 满足服务仓库 Send+Sync 约束）。
#[derive(Debug, Default)]
pub struct SessionLog {
    events: Vec<SessionEvent>,
    /// 当前 surface 节点 seq（模型可见顺序；对齐 `SessionSurface.nodes`）。
    surface: Vec<u64>,
    /// replace 操作计数（对齐 `SessionSurface.replaceGeneration`）。
    replace_generation: u64,
}

impl SessionLog {
    pub fn new() -> Self {
        SessionLog {
            events: Vec::new(),
            surface: Vec::new(),
            replace_generation: 0,
        }
    }

    /// 追加事件（等价 WIT 缝 `session::append`）。
    /// surface-eligible 事件以 `Append` 入 surface 节点序列（纯 append 场景与
    /// 遍历全部事件投影等价）。
    pub fn append(&mut self, kind: &str, payload: Vec<u8>) -> u64 {
        self.append_with_op(kind, payload, SurfaceOp::Append)
            .expect("append: surface op valid for append")
    }

    /// 追加事件并声明 surface 放置（M36：`Append` 或 `Replace`）。
    /// 便捷入口：source_event_seqs 缺省为 None（append 合法；replace 无来源
    /// 覆盖会按生产 `assertProvenance` 报错）。
    pub fn append_with_op(
        &mut self,
        kind: &str,
        payload: Vec<u8>,
        op: SurfaceOp,
    ) -> Result<u64, crate::error::CordisError> {
        self.append_with_provenance(kind, payload, op, None)
    }

    /// 追加事件并声明 surface 放置 + 来源引用（M37，完整对齐 DSH `surface.ts`
    /// 的校验链 `surfaceOpOf` → `replacementRange` → `assertProvenance` →
    /// `assertToolResultRewrite`）：
    /// - 非 surface-eligible 事件带 `Replace` → 报错（`surfaceOpOf`）；
    /// - `Replace` 的 start/end 必须在当前 surface 上且 start ≤ end
    ///   （`replacementRange`）；
    /// - `source_event_seqs`（`assertProvenance`）：引用必须**早于**当前事件
    ///   seq、无重复；空数组仅 `assistant/message` 允许（known empty provider
    ///   stream）；replace 时**必须覆盖全部被 shadow 节点**；
    /// - `tool/result` 的 replace（`assertToolResultRewrite`）：必须恰好重写
    ///   1 个当前 `tool/result` 节点，且只允许改 content（其余字段深比较）；
    /// - 任何失败**原子**（校验全部通过后才 splice + push）。
    pub fn append_with_provenance(
        &mut self,
        kind: &str,
        payload: Vec<u8>,
        op: SurfaceOp,
        source_event_seqs: Option<Vec<u64>>,
    ) -> Result<u64, crate::error::CordisError> {
        use crate::error::CordisError;
        let seq = self.events.len() as u64;

        // ---- surfaceOpOf：非 surface-eligible 事件不能带 replace ----
        if !is_surface_eligible(kind) {
            if let SurfaceOp::Replace { .. } = op {
                return Err(CordisError::Internal(format!(
                    "session event \"{kind}\" is not surface-eligible and cannot carry surfaceOp"
                )));
            }
            // Append on log event：便捷路径（生产要求 surface-eligible 事件
            // 必须带 surfaceOp，但 WIT 缝 append 无 op 参数——宽松接受）。
            self.events.push(SessionEvent {
                seq,
                kind: kind.to_string(),
                payload,
            });
            return Ok(seq);
        }

        // ---- replacementRange：定位被 shadow 范围（含）----
        let shadowed: Vec<u64> = match op {
            SurfaceOp::Append => Vec::new(),
            SurfaceOp::Replace { start, end } => {
                let start_idx = self.surface.iter().position(|&s| s == start).ok_or_else(|| {
                    CordisError::Internal(format!(
                        "surface replace: start seq {start} not found in surface"
                    ))
                })?;
                let end_idx = self.surface.iter().position(|&s| s == end).ok_or_else(|| {
                    CordisError::Internal(format!(
                        "surface replace: end seq {end} not found in surface"
                    ))
                })?;
                if start_idx > end_idx {
                    return Err(CordisError::Internal(format!(
                        "surface replace: start seq {start} (index {start_idx}) is after end seq {end} (index {end_idx})"
                    )));
                }
                self.surface[start_idx..=end_idx].to_vec()
            }
        };

        // ---- assertProvenance：来源引用校验 ----
        if let Some(seqs) = &source_event_seqs {
            if seqs.is_empty() && kind != "assistant/message" {
                return Err(CordisError::Internal(format!(
                    "session event \"{kind}\" source_event_seqs must not be empty except on assistant/message"
                )));
            }
            // 引用必须早于当前事件、无重复
            let mut seen = std::collections::HashSet::new();
            for &s in seqs {
                if s >= seq {
                    return Err(CordisError::Internal(format!(
                        "session event source_event_seqs must reference earlier events: {s} >= current seq {seq}"
                    )));
                }
                if !seen.insert(s) {
                    return Err(CordisError::Internal(format!(
                        "session event source_event_seqs must not contain duplicates: {s}"
                    )));
                }
            }
        }
        // replace 必须覆盖全部被 shadow 节点（sources 缺省空集）
        if let SurfaceOp::Replace { .. } = op {
            let sources: std::collections::HashSet<u64> =
                source_event_seqs.clone().unwrap_or_default().into_iter().collect();
            let missing: Vec<u64> = shadowed
                .iter()
                .copied()
                .filter(|s| !sources.contains(s))
                .collect();
            if !missing.is_empty() {
                return Err(CordisError::Internal(format!(
                    "surface replace: source_event_seqs must include every shadowed surface node; missing {}",
                    missing
                        .iter()
                        .map(|s| s.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
        }

        // ---- assertToolResultRewrite：tool/result replace 只允许改 content ----
        if kind == "tool/result" {
            if let SurfaceOp::Replace { .. } = op {
                if shadowed.len() != 1 {
                    return Err(CordisError::Internal(
                        "tool/result surface replacement must rewrite exactly one current node".into(),
                    ));
                }
                let original = &self.events[shadowed[0] as usize];
                if original.kind != "tool/result" {
                    return Err(CordisError::Internal(
                        "tool/result surface replacement must target a current tool/result".into(),
                    ));
                }
                if !tool_result_only_content_changed(&original.payload_value(), &payload) {
                    return Err(CordisError::Internal(
                        "tool/result surface replacement may change only content".into(),
                    ));
                }
            }
        }

        // ---- 原子提交（校验全通过）----
        match op {
            SurfaceOp::Append => self.surface.push(seq),
            SurfaceOp::Replace { .. } => {
                let start = shadowed[0];
                let end = shadowed[shadowed.len() - 1];
                let start_idx = self.surface.iter().position(|&s| s == start).unwrap();
                let end_idx = self.surface.iter().position(|&s| s == end).unwrap();
                self.surface.splice(start_idx..=end_idx, [seq]);
                self.replace_generation += 1;
            }
        }
        self.events.push(SessionEvent {
            seq,
            kind: kind.to_string(),
            payload,
        });
        Ok(seq)
    }

    /// 事件流（按序号）。
    pub fn events(&self) -> &[SessionEvent] {
        &self.events
    }

    /// 事件种类序列（诊断/断言）。
    pub fn event_kinds(&self) -> Vec<String> {
        self.events.iter().map(|e| e.kind.clone()).collect()
    }

    /// 当前 surface 节点 seq（模型可见顺序；对齐 `SessionSurface.nodes`）。
    pub fn surface_nodes(&self) -> Vec<u64> {
        self.surface.clone()
    }

    /// replace 操作计数（对齐 `SessionSurface.replaceGeneration`）。
    pub fn replace_generation(&self) -> u64 {
        self.replace_generation
    }

    /// 投影模型历史（对应 deepseek-harness `Session.deriveMessages` /
    /// `deriveEventMessage`，M34 对齐生产 `Message` 形状，M36 对齐 surface）：
    /// 只对**当前 surface 节点**投影（replace 后旧节点被 shadow）：
    /// - `user/message` → **data 逐字透传**（data 本身即完整 `Message` 对象——
    ///   生产 `'user/message': UserMessage`）；
    /// - `assistant/message` → `data.message`（data 为 `{turn, step, message}`
    ///   包装）；**content 空数组跳过**（DSH surface 规则：仅承载 usage 的
    ///   空助手消息不入模型历史）；
    /// - `tool/result` → `data.message`（data 为 `{turn, step, message}` 包装，
    ///   ToolResultMessage：role=user + tool-result block + source.tool）；
    /// - 其他 → 跳过。
    ///
    /// 输出的每条消息形状 = 生产 `Message`：`{id, role, content: ContentBlock[],
    /// source}`（对齐 `packages/llm/llm/src/message.ts`）。
    pub fn derive_messages(&self) -> Vec<Value> {
        let mut out = Vec::new();
        for &seq in &self.surface {
            let e = &self.events[seq as usize];
            let v = e.payload_value();
            match e.kind.as_str() {
                // 生产 `deriveEventMessage`：user/message → event.data 逐字
                // （data 即 UserMessage 对象；非 surface 事件不投影）。
                "user/message" => out.push(v),
                "assistant/message" => {
                    let Some(msg) = v.get("message").cloned() else {
                        continue;
                    };
                    let content = msg.get("content").cloned().unwrap_or(Value::Null);
                    // 空 content 数组跳过（对齐 DSH `deriveEventMessage`：
                    // `event.data.message.content.length === 0` → null）。
                    if content
                        .as_array()
                        .map(|a| a.is_empty())
                        .unwrap_or(false)
                    {
                        continue;
                    }
                    out.push(msg);
                }
                "tool/result" => {
                    if let Some(msg) = v.get("message").cloned() {
                        out.push(msg);
                    }
                }
                _ => {}
            }
        }
        out
    }

    // ---- M47：JSONL 持久化（对齐 DSH `session-persistence-jsonl` 核心格式） ----

    /// 保存为 JSONL：首行 header（`{"type":"session","version":0}`），之后
    /// 每事件一行 `{"kind","seq","payload"}`（payload 为事件 data 的 JSON
    /// 字节，内联为 JSON）。append-only；`load_from` 可重建。
    pub fn save_to(&self, path: &std::path::Path) -> Result<(), crate::error::CordisError> {
        use crate::error::CordisError;
        let mut out = String::new();
        out.push_str("{\"type\":\"session\",\"version\":0}\n");
        for e in &self.events {
            let line = serde_json::json!({
                "kind": e.kind,
                "seq": e.seq,
                "payload": payload_to_value(&e.payload),
            });
            out.push_str(&serde_json::to_string(&line).map_err(|e| {
                CordisError::Internal(format!("session save encode: {e}"))
            })?);
            out.push('\n');
        }
        std::fs::write(path, out)
            .map_err(|e| CordisError::Internal(format!("session save {}: {e}", path.display())))
    }

    /// 从 JSONL 重建：读 header（必须 `{"type":"session"}`）+ 事件行；容忍
    /// torn tail（最后一行损坏/无换行——保留完整前缀，对齐生产 `scanLog`）。
    /// 重建 events + surface（append 语义重放；replace 的 sourceEventSeqs
    /// 在事件 payload 中不可恢复——持久化会话为 append 轨迹）。
    pub fn load_from(path: &std::path::Path) -> Result<Self, crate::error::CordisError> {
        use crate::error::CordisError;
        let text = std::fs::read_to_string(path)
            .map_err(|e| CordisError::Internal(format!("session load {}: {e}", path.display())))?;
        let mut lines = text.lines();
        let header = lines.next().ok_or_else(|| {
            CordisError::Internal("session load: empty log".into())
        })?;
        let header_v: Value = serde_json::from_str(header).map_err(|e| {
            CordisError::Internal(format!("session load: bad header: {e}"))
        })?;
        if header_v.get("type").and_then(|t| t.as_str()) != Some("session") {
            return Err(CordisError::Internal("session load: missing session header".into()));
        }
        let mut log = SessionLog::new();
        for line in lines {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                // torn tail：损坏行 + 其后无法恢复 → 停止（保留完整前缀）
                break;
            };
            let Some(kind) = v.get("kind").and_then(|k| k.as_str()) else { break };
            let payload = match v.get("payload") {
                Some(Value::String(s)) => s.as_bytes().to_vec(),
                Some(p) => serde_json::to_vec(p).unwrap_or_default(),
                None => Vec::new(),
            };
            // append 语义重放（surface-eligible 入列；日志事件仅入 events）
            log.append(kind, payload);
        }
        Ok(log)
    }

    /// M49：分支会话（对齐 DSH `Session.fork`）——从**稳定前缀**派生子会话：
    /// - `boundary` 为包含的源事件 seq；省略 = 源的最后事件（空源 → 空子）；
    /// - boundary 必须存在且是连续 seq（`events[boundary].seq == boundary`，
    ///   否则报错，对齐 `INVALID_BOUNDARY`）；
    /// - 前缀内最后一个 turn/start 或 turn/end 若是 **turn/start** → 报错
    ///   （边界落在打开的 turn 内，对齐 `OPEN_TURN`）；
    /// - 返回新 log：events = 前缀（clone）+ surface = 前缀重放（append
    ///   语义）。
    ///
    /// 父会话不受影响（不可变）。
    pub fn fork(&self, boundary: Option<u64>) -> Result<Self, crate::error::CordisError> {
        use crate::error::CordisError;
        let boundary = match boundary {
            Some(b) => b,
            None => match self.events.last() {
                Some(last) => last.seq,
                None => return Ok(SessionLog::new()),
            },
        };
        if boundary >= self.events.len() as u64 {
            return Err(CordisError::Internal(format!(
                "fork boundary {boundary} does not exist in session (last seq: {})",
                self.events.last().map(|e| e.seq).unwrap_or(0)
            )));
        }
        let boundary_event = &self.events[boundary as usize];
        if boundary_event.seq != boundary {
            return Err(CordisError::Internal(format!(
                "fork boundary {boundary} does not match a contiguous event seq"
            )));
        }
        // 前缀内最后一个 turn 边界：turn/start 结尾 → 落在 open turn 内
        let last_turn_boundary = self.events[..=boundary as usize]
            .iter()
            .rev()
            .find(|e| e.kind == "turn/start" || e.kind == "turn/end");
        if let Some(b) = last_turn_boundary {
            if b.kind == "turn/start" {
                return Err(CordisError::Internal(format!(
                    "fork boundary {boundary} ends inside an open turn"
                )));
            }
        }
        // 前缀重放（append 语义重建 events + surface）
        let mut child = SessionLog::new();
        for e in &self.events[..=boundary as usize] {
            child.append(&e.kind, e.payload.clone());
        }
        Ok(child)
    }
}

/// M47：事件 payload（JSON 字节）→ 可内联 JSON 值——合法 JSON 内联，
/// 否则编码为字符串（保真）。
fn payload_to_value(payload: &[u8]) -> Value {
    match serde_json::from_slice::<Value>(payload) {
        Ok(v) => v,
        Err(_) => Value::String(String::from_utf8_lossy(payload).into_owned()),
    }
}

/// 共享会话日志句柄（作为 `ctx.sessions` 服务值；Send+Sync）。
pub type SessionHandle = Arc<Mutex<SessionLog>>;

/// 构造共享会话日志。
pub fn new_session() -> SessionHandle {
    Arc::new(Mutex::new(SessionLog::new()))
}

//! JSONL 持久化后端的磁盘格式（M1d：`dsh-persistence:format`）。
//!
//! 权威参考：`deepseek-harness/packages/session/session-persistence-jsonl/src/format.ts` +
//! `dsh-session/src/chunk-rows.ts`（见 M1d 规范 §D）。逐字对齐：
//! - 路径安全段编码 `encodeSegment`（对 UTF-16 code unit 单射；防 `../`/绝对路径/NUL）；
//! - 项目可读目录键 `projectKey`（分隔符坍缩、有界截断、`--…--` 包裹）；
//! - header 行（`toHeaderLine`/`fromHeaderLine`/`isHeaderLine`/`parseHeaderMeta`）；
//! - 事件行 `eventLines`（packChunks 时用 `dsh_session::chunk_rows`）；
//! - `SessionLogScanner`（增量 JSONL 扫描：完整行才解码、seq gap/坏行损坏语义、
//!   torn 尾容忍）。纯函数、无 IO。

use dsh_brand::SessionId;
use dsh_session::chunk_rows::{decode_storage_record, pack_chunk_runs};
use dsh_session::types::{EventKind, SessionEvent, SessionHeader};
use dsh_session::Origin;
use serde_json::{Map, Value};

/// 物理编码对应的 artifact 后缀。
pub fn log_suffix(compression: JsonlCompression) -> &'static str {
    match compression {
        JsonlCompression::Zstd => ".jsonl.zstd",
        JsonlCompression::None => ".jsonl",
    }
}

/// 为 JSONL artifact 选择的物理编码。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonlCompression {
    Zstd,
    None,
}

impl JsonlCompression {
    /// 配置字符串解析（`zstd`|`none`；TS zod union 的镜像）。
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "zstd" => Some(JsonlCompression::Zstd),
            "none" => Some(JsonlCompression::None),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            JsonlCompression::Zstd => "zstd",
            JsonlCompression::None => "none",
        }
    }
}

/// UTF-16 code unit（对齐 JS `String` 的 code unit 语义，路径编码的基准）。
fn utf16_units(raw: &str) -> Vec<u16> {
    raw.encode_utf16().collect()
}

/// 单个 code unit 是否为安全字面量（`[A-Za-z0-9._-]`，`~` 除外）。
fn is_safe_code_unit(u: u16) -> bool {
    const A: u16 = b'A' as u16;
    const Z: u16 = b'Z' as u16;
    const A2: u16 = b'a' as u16;
    const Z2: u16 = b'z' as u16;
    const ZERO: u16 = b'0' as u16;
    const NINE: u16 = b'9' as u16;
    const DOT: u16 = b'.' as u16;
    const UNDER: u16 = b'_' as u16;
    const DASH: u16 = b'-' as u16;
    matches!(u, A..=Z | A2..=Z2 | ZERO..=NINE | DOT | UNDER | DASH)
}

/// 转义一个不安全 code unit 为 `~XXXX`（4 位大写十六进制）。
fn escape_unit(u: u16) -> String {
    format!("~{u:04X}")
}

/// 把一个 Rust 字符串编码为单个安全路径段，对所有 UTF-16 字符串（含孤立代理对）
/// 单射。`""` → Err；`.`/`..` 整体段转义防止遍历。
pub fn encode_segment(raw: &str) -> Result<String, String> {
    if raw.is_empty() {
        return Err("cannot encode an empty path segment".into());
    }
    if raw == "." {
        return Ok("~002E".into());
    }
    if raw == ".." {
        return Ok("~002E~002E".into());
    }
    let mut out = String::new();
    for u in utf16_units(raw) {
        if is_safe_code_unit(u) {
            out.push(char::from_u32(u32::from(u)).unwrap_or('\u{FFFD}'));
        } else {
            out.push_str(&escape_unit(u));
        }
    }
    Ok(out)
}

/// 构建项目可读目录键：分隔符运行（`/`、`\`、`:` 连续）坍缩为单个 `-`；安全字面量
/// 保留；不安全 code unit 用 `~XXXX` 转义。输出 `--<readable><= "root" 或截断到 251>--`。
pub fn project_key(cwd: &str) -> Result<String, String> {
    if cwd.is_empty() {
        return Err("cannot encode an empty project path".into());
    }
    let mut readable = String::new();
    let mut separator_run = false;
    for u in utf16_units(cwd) {
        let c = char::from_u32(u32::from(u)).unwrap_or('\u{FFFD}');
        if matches!(c, '/' | '\\' | ':') {
            if !separator_run {
                readable.push('-');
            }
            separator_run = true;
        } else if is_safe_code_unit(u) {
            readable.push(c);
            separator_run = false;
        } else {
            readable.push_str(&escape_unit(u));
            separator_run = false;
        }
    }
    let trimmed = readable.trim_start_matches('-');
    let core = if trimmed.is_empty() {
        "root".to_string()
    } else {
        trimmed.chars().take(251).collect()
    };
    Ok(format!("--{core}--"))
}

/// root 下、cwd 对应的人类可导航项目目录。
pub fn project_dir(root: &str, cwd: Option<&str>) -> Result<String, String> {
    match cwd {
        Some(c) => Ok(format!(
            "{}\\{}",
            root.trim_end_matches(['\\', '/']),
            project_key(c)?
        )),
        None => Ok(format!("{}\\_no-cwd", root.trim_end_matches(['\\', '/']))),
    }
}

/// 一个会话拥有的目录（其项目目录之下、编码为单个安全路径段）。
pub fn session_dir(root: &str, cwd: Option<&str>, id: &SessionId) -> Result<String, String> {
    let project = project_dir(root, cwd)?;
    Ok(format!("{}\\{}", project, encode_segment(id.raw())?))
}

/// 一个会话的完整 artifact 目标路径。
pub fn log_path(
    root: &str,
    cwd: Option<&str>,
    id: &SessionId,
    compression: JsonlCompression,
) -> Result<String, String> {
    let dir = session_dir(root, cwd, id)?;
    Ok(format!("{}\\session{}", dir, log_suffix(compression)))
}

/// Header 行的第一识记录（`type: 'session'` 标签，可选字段省略、永不 null；
/// `delegationDepth` 恒存在、缺省 0）。
#[derive(Debug, Clone, PartialEq)]
pub struct HeaderLine {
    pub version: u64,
    pub id: SessionId,
    pub created_at: i64,
    pub cwd: Option<String>,
    pub parent_session: Option<SessionId>,
    pub seed_length: Option<u64>,
    pub origin: Option<Origin>,
    pub delegation_depth: u64,
    pub agent_preset: Option<String>,
}

/// 从 `SessionHeader` 构建 header 行对象。
pub fn to_header_line(header: &SessionHeader) -> HeaderLine {
    HeaderLine {
        version: header.version,
        id: header.id.clone(),
        created_at: header.created_at as i64,
        cwd: header.cwd.clone(),
        parent_session: header.parent_session.clone(),
        seed_length: header.seed_length,
        origin: header.origin,
        delegation_depth: header.delegation_depth.unwrap_or(0),
        agent_preset: header.agent_preset.clone(),
    }
}

fn json_value(v: impl serde::Serialize) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

impl HeaderLine {
    /// 序列化为 `{type:'session', ...}` JSON（键序固定，对齐 `toHeaderLine`）。
    pub fn to_json(&self) -> Value {
        let mut obj = Map::new();
        obj.insert("type".into(), Value::String("session".into()));
        obj.insert("version".into(), json_value(self.version));
        obj.insert("id".into(), Value::String(self.id.raw().to_string()));
        obj.insert("createdAt".into(), json_value(self.created_at));
        if let Some(c) = &self.cwd {
            obj.insert("cwd".into(), Value::String(c.clone()));
        }
        if let Some(p) = &self.parent_session {
            obj.insert("parentSession".into(), Value::String(p.raw().to_string()));
        }
        if let Some(s) = self.seed_length {
            obj.insert("seedLength".into(), json_value(s));
        }
        if let Some(o) = self.origin {
            obj.insert("origin".into(), serde_json::to_value(o).unwrap_or(Value::Null));
        }
        obj.insert("delegationDepth".into(), json_value(self.delegation_depth));
        if let Some(a) = &self.agent_preset {
            obj.insert("agentPreset".into(), Value::String(a.clone()));
        }
        Value::Object(obj)
    }

    /// 解析回 `SessionHeader`；当行保留已退役 policy 字段（sandboxMode/approvalPolicy）
    /// 时 Err。
    pub fn from_json(&self, value: &Value) -> Result<SessionHeader, String> {
        let obj = value.as_object().ok_or("header line must be an object")?;
        if obj.contains_key("sandboxMode") || obj.contains_key("approvalPolicy") {
            return Err("session header uses retired policy baseline fields".into());
        }
        Ok(SessionHeader {
            version: self.version,
            id: self.id.clone(),
            created_at: self.created_at as u64,
            cwd: self.cwd.clone(),
            parent_session: self.parent_session.clone(),
            seed_length: self.seed_length,
            origin: self.origin,
            delegation_depth: Some(self.delegation_depth),
            agent_preset: self.agent_preset.clone(),
        })
    }
}

/// 形状守卫：该行是否为良构 session header（不强制版本）。
pub fn is_header_line(value: &Value) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    if obj.get("type").and_then(Value::as_str) != Some("session") {
        return false;
    }
    if obj.get("version").and_then(Value::as_u64).is_none() {
        return false;
    }
    if obj.get("id").and_then(Value::as_str).is_none() {
        return false;
    }
    match obj.get("createdAt").and_then(Value::as_i64) {
        Some(v) if v >= 0 => {}
        _ => return false,
    }
    if obj.get("delegationDepth").and_then(Value::as_u64).is_none() {
        return false;
    }
    if let Some(o) = obj.get("origin") {
        if o.as_str() != Some("subagent") {
            return false;
        }
    }
    if let Some(a) = obj.get("agentPreset") {
        if !a.is_string() {
            return false;
        }
    }
    true
}

/// 从解析好的 header 行值回读 `HeaderLine`（形状已由 `is_header_line` 守卫）。
pub fn from_header_line_value(value: &Value) -> HeaderLine {
    let obj = value.as_object().expect("is_header_line guard");
    let version = obj.get("version").and_then(Value::as_u64).expect("version");
    let id = SessionId::from_raw(obj.get("id").and_then(Value::as_str).expect("id").to_string());
    let created_at = obj.get("createdAt").and_then(Value::as_i64).expect("createdAt");
    let cwd = obj.get("cwd").and_then(Value::as_str).map(str::to_string);
    let parent = obj
        .get("parentSession")
        .and_then(Value::as_str)
        .map(|s| SessionId::from_raw(s.to_string()));
    let seed_length = obj.get("seedLength").and_then(Value::as_u64);
    let origin = obj
        .get("origin")
        .and_then(Value::as_str)
        .and_then(|s| if s == "subagent" { Some(Origin::Subagent) } else { None });
    let delegation_depth = obj.get("delegationDepth").and_then(Value::as_u64).unwrap_or(0);
    let agent_preset = obj.get("agentPreset").and_then(Value::as_str).map(str::to_string);
    HeaderLine {
        version,
        id,
        created_at,
        cwd,
        parent_session: parent,
        seed_length,
        origin,
        delegation_depth,
        agent_preset,
    }
}

/// 只解析一个日志的首行 header 元数据；非良构 header 返回 None（不校验版本）。
/// 用于 `list()`/`read_raw` 的元数据轻读。
pub fn parse_header_meta(first_line: &str) -> Option<HeaderLine> {
    let value: Value = serde_json::from_str(first_line).ok()?;
    if !is_header_line(&value) {
        return None;
    }
    Some(from_header_line_value(&value))
}

/// 事件行：`pack_chunks` 时先打包再逐行 `JSON.stringify`，用 `\n` 连接。
/// 调用方负责补最终换行。
pub fn event_lines(events: &[SessionEvent], pack_chunks: bool) -> String {
    let records = if pack_chunks {
        let values: Vec<Value> = events
            .iter()
            .map(|e| serde_json::to_value(e).unwrap_or(Value::Null))
            .collect();
        pack_chunk_runs(&values)
    } else {
        events
            .iter()
            .map(|e| serde_json::to_value(e).unwrap_or(Value::Null))
            .collect()
    };
    records
        .iter()
        .map(|v| serde_json::to_string(v).expect("JSONL line serializable"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 单条事件行的字节（含换行）。
pub fn event_lines_bytes(events: &[SessionEvent], pack_chunks: bool) -> Vec<u8> {
    let mut s = event_lines(events, pack_chunks);
    s.push('\n');
    s.into_bytes()
}

/// 逐行（不带结尾换行）→ `SessionEvent`；解析行值经 `decode_storage_record`
/// 展开（坏行 Err）。
pub fn lines_to_events(line: &str) -> Result<Vec<SessionEvent>, String> {
    let value: Value = serde_json::from_str(line)
        .map_err(|e| format!("invalid JSON in committed event line: {e}"))?;
    let expanded = decode_storage_record(value)?;
    let mut out = Vec::with_capacity(expanded.len());
    for v in expanded {
        out.push(
            serde_json::from_value(v)
                .map_err(|e| format!("stored event does not fit SessionEvent: {e}"))?,
        );
    }
    Ok(out)
}

/// `SessionLogScanner`：增量 JSONL 扫描。
///
/// 语义逐字镜像 TS `SessionLogScanner`（M1d 规范 §D）：
/// - `events` 向量下标即期望 seq（seq 从 0 起连续追加在事件上）；
/// - 完整行（含结尾 `\n`）才解码；残缺行留在 fragment 缓冲；
/// - 完整行 JSON/存储行解析失败 → issue 冻结（committedBytes 不推进）；
/// - 行内 seq 不连续 → 截断回 row 起点并 issue；
/// - issue 后若任一行含 `turn/end` → 重抛 issue（不静默丢 turn 边界）；
/// - `finish` 忽略无结尾换行的最终残缺行（torn 尾）。
#[derive(Debug)]
pub struct SessionLogScanner {
    events: Vec<SessionEvent>,
    input_bytes: usize,
    committed_bytes: usize,
    fragments: Vec<u8>,
    event_line: usize,
    issue: Option<String>,
    finished: bool,
}

/// 顺序等着提交的前缀事件。
#[derive(Debug, Default)]
pub struct LogScanResult {
    /// 保留的连续事件前缀（莫破坏回放）。
    pub events: Vec<SessionEvent>,
    /// 安全追加字节偏移 = committed_bytes。
    pub committed_bytes: usize,
    /// 完整解码的事件总数（= events.len()，供调用方快速引用）。
    pub event_count: usize,
}

impl SessionLogScanner {
    /// 构造：以独立提供的单条新行结尾 header 记录开始（`header_record_bytes` =
    /// 首行含换行的字节数；committed 从那里起算）。
    pub fn new(header_record_bytes: usize) -> Self {
        SessionLogScanner {
            events: Vec::new(),
            input_bytes: header_record_bytes,
            committed_bytes: header_record_bytes,
            fragments: Vec::new(),
            event_line: 0,
            issue: None,
            finished: false,
        }
    }

    /// 摄入一段新字节（追加；只在完整记录上解码）。
    /// issue 后出现 `turn/end` → Err（重抛损坏）。
    pub fn write(&mut self, chunk: &[u8]) -> Result<(), String> {
        if self.finished {
            return Err("cannot write to a finished session log scanner".into());
        }
        let chunk_start = self.input_bytes;
        self.input_bytes += chunk.len();
        let mut line_start = 0;
        while let Some(nl_rel) = chunk[line_start..].iter().position(|&b| b == b'\n') {
            let newline = line_start + nl_rel;
            let mut line = Vec::with_capacity(self.fragments.len() + (newline - line_start));
            line.extend_from_slice(&self.fragments);
            line.extend_from_slice(&chunk[line_start..newline]);
            self.fragments.clear();
            let end_byte = chunk_start + newline + 1;
            self.consume_event_line(&line, end_byte)?;
            line_start = newline + 1;
        }
        if line_start < chunk.len() {
            self.fragments.extend_from_slice(&chunk[line_start..]);
        }
        Ok(())
    }

    /// 解码一条完整事件行并更新连续前缀（镜像 TS `consumeEventLine`）。
    fn consume_event_line(&mut self, line: &[u8], end_byte: usize) -> Result<(), String> {
        self.event_line += 1;
        let text = match std::str::from_utf8(line) {
            Ok(t) => t,
            Err(_) => {
                let issue =
                    format!("corrupt session log: unparsable committed event at line {}", self.event_line);
                self.issue.get_or_insert(issue);
                return Ok(());
            }
        };
        let decoded = match lines_to_events(text) {
            Ok(d) => d,
            Err(_) => {
                let issue =
                    format!("corrupt session log: unparsable committed event at line {}", self.event_line);
                self.issue.get_or_insert(issue);
                return Ok(());
            }
        };
        if self.issue.is_some() {
            if decoded.iter().any(|e| e.kind == EventKind::TurnEnd) {
                return Err(self.issue.clone().expect("issue set")); // 重抛
            }
            return Ok(());
        }
        let row_start = self.events.len();
        for event in &decoded {
            if event.seq as usize != self.events.len() {
                let expected = self.events.len();
                self.events.truncate(row_start);
                let msg = format!(
                    "corrupt session log: seq gap in committed region at line {} (expected {expected}, got {})",
                    self.event_line, event.seq
                );
                self.issue = Some(msg.clone());
                if decoded.iter().any(|c| c.kind == EventKind::TurnEnd) {
                    return Err(msg); // 重抛
                }
                return Ok(());
            }
            self.events.push(event.clone());
        }
        self.committed_bytes = end_byte;
        Ok(())
    }

    /// checkpoint：输入字节 / committed 前缀。
    pub fn checkpoint(&self) -> (usize, usize) {
        (self.input_bytes, self.committed_bytes)
    }

    /// finish：忽略无结尾换行的最终残缺行（torn 尾）。返回前缀结果与可选 issue。
    pub fn finish(mut self) -> (LogScanResult, Option<String>) {
        self.finished = true;
        let event_count = self.events.len();
        let result = LogScanResult {
            events: self.events,
            committed_bytes: self.committed_bytes,
            event_count,
        };
        (result, self.issue.take())
    }
}

/// 一次性扫描一个完整日志 buffer：首行独立为 header 记录，其余喂 scanner。
/// `buffer` 必须 header-line-first（空/无 header 行 → Err）。
pub fn scan_log(buffer: &[u8]) -> Result<LogScanResult, String> {
    let header_end = buffer.iter().position(|&b| b == b'\n');
    let Some(header_end) = header_end else {
        return Err("empty or header-less session log".into());
    };
    let mut scanner = SessionLogScanner::new(header_end + 1);
    scanner.write(&buffer[header_end + 1..])?;
    Ok(scanner.finish().0)
}

/// 一条 header 行的 JSONL 字节（含结尾换行）。
pub fn header_line_bytes(header: &SessionHeader) -> Vec<u8> {
    let mut s = serde_json::to_string(&to_header_line(header).to_json()).expect("header line");
    s.push('\n');
    s.into_bytes()
}

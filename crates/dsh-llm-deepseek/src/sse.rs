//! SSE 字节流解码为事件 `data` 载荷（对齐
//! `deepseek-harness/packages/llm/llm-deepseek/src/sse.ts`）。
//!
//! Rust 侧不引入 eventsource-parser：核心单线程纪律下，SSE 解码是纯字节态机
//! （UTF-8/CRLF/BOM/注释/data 行聚合/空行终结），用有状态增量解析器实现，
//! 保持与 TS 相同的框定语义：事件只在空行终结符处分发；EOF 前的未终结尾段
//! 视为截断而非可冲洗载荷（对齐 `parseSse` 的描述）。

use dsh_llm::LlmFailure;

/// DeepSeek（与 OpenAI）在最后一块后发送的终末载荷。
pub const DONE: &str = "[DONE]";

/// SSE 解析失败（对齐 `LlmError` code `STREAM_CLOSED`/`MALFORMED_RESPONSE`）。
#[derive(Debug, Clone, PartialEq)]
pub struct SseError {
    pub message: String,
    pub code: &'static str,
}

impl std::fmt::Display for SseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl SseError {
    pub fn failure(&self) -> LlmFailure {
        LlmFailure {
            message: self.message.clone(),
            code: self.code.to_string(),
            status: None,
            provider_retry_after_ms: None,
            request_id: None,
        }
    }
}

/// 有状态 SSE 行流增量解析器。
///
/// 输入是任意切分的字节片段（可落在多字节 UTF-8 序列中间）；每次
/// `push` 处理完可解析的数据后返回本次跨过空行终结符的完整 `data` 载荷。
/// 通过 `finish` 在 EOF 冲洗剩余缓冲；若 EOF 时存在未终结的未完成事件，
/// 返回 `SseError::STREAM_CLOSED`（截断）。
#[derive(Debug, Default)]
pub struct SseParser {
    /// 跨 push 调用保留的 UTF-8 累积缓冲。
    buf: Vec<u8>,
    /// 当前数据事件累积的行内 `data:` 字段值（允许多条 data: 行，以换行连接）。
    data_lines: Vec<String>,
    /// 上次 push 是否有真正的字节（区分「还没到 EOF」与「EOF」）。
    live: bool,
    /// 完整 data 有效负载（本次已解析、待消费）。
    events: std::collections::VecDeque<String>,
}

impl SseParser {
    pub fn new() -> Self {
        SseParser::default()
    }

    /// 喂入一段 SSE 字节；返回本段内完成的事件 `data` 载荷（可能为空）。
    pub fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.live = !bytes.is_empty();
        self.buf.extend_from_slice(bytes);
        self.drain_lines();
        self.events.drain(..).collect()
    }

    /// EOF：冲洗剩余缓冲并返回余下事件；未终结的尾段视为截断报错。
    pub fn finish(mut self) -> Result<Vec<String>, SseError> {
        self.live = false;
        self.drain_lines();
        // 未终结事件：缓冲里还有完整的 data 行但缺空行终结符
        if !self.buf.is_empty() || !self.data_lines.is_empty() {
            return Err(SseError {
                message: "SSE stream ended without [DONE]".into(),
                code: "STREAM_CLOSED",
            });
        }
        Ok(self.events.drain(..).collect())
    }

    /// 把缓冲切成行；解析 `data:` / 注释 / 空行终结。保持未终结行在 buf。
    fn drain_lines(&mut self) {
        loop {
            // 找行终止符 \n（SSE 标准用 LF；eventsource-parser 摘要 CRLF）
            let Some(nl) = self.buf.iter().position(|&b| b == b'\n') else {
                break;
            };
            let mut line: Vec<u8> = self.buf.drain(..=nl).collect();
            // 去掉换行符
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop(); // CRLF
            }
            self.handle_line(line);
        }
    }

    fn handle_line(&mut self, line: Vec<u8>) {
        // BOM：仅首个事件首行剥离（eventsource-parser 处理 UTF-8 BOM）
        let mut line = line;
        if line.starts_with(&[0xEF, 0xBB, 0xBF]) {
            line.drain(..3);
        }
        if line.is_empty() {
            // 空行 = 事件终结
            if !self.data_lines.is_empty() {
                self.events.push_back(self.data_lines.join("\n"));
                self.data_lines.clear();
            }
            return;
        }
        // 注释行（: 开头）与未知字段被跳过（对齐「comment and non-data field skipping」）
        if line.starts_with(b":") {
            return;
        }
        if let Some(value) = strip_field(&line, b"data:") {
            // data 字段值去掉前导空白
            self.data_lines.push(value);
        }
        // 其它字段（event:, id:, retry:）在纯 data 协议下忽略
    }
}

/// 解析 `field:value`；返回 value（若有）。
fn strip_field(line: &[u8], field: &[u8]) -> Option<String> {
    if !line.starts_with(field) {
        return None;
    }
    let rest = &line[field.len()..];
    let value = if rest.first() == Some(&b' ') { &rest[1..] } else { rest };
    Some(String::from_utf8_lossy(value).into_owned())
}

/// 便捷：把整个 SSE 字节流一次性解析为 data 载荷序列。
pub fn parse_sse(bytes: &[u8]) -> Result<Vec<String>, SseError> {
    let mut parser = SseParser::new();
    let mut events = parser.push(bytes);
    events.extend(parser.finish()?);
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_data_event() {
        let events = parse_sse(b"data: hello\n\n").unwrap();
        assert_eq!(events, vec!["hello".to_string()]);
    }

    #[test]
    fn parses_multiple_choice_events_and_done() {
        let sse = b"data: {\"a\":1}\n\ndata: {\"b\":2}\n\ndata: [DONE]\n\n";
        let events = parse_sse(sse).unwrap();
        assert_eq!(events, vec!["{\"a\":1}".to_string(), "{\"b\":2}".to_string(), DONE.to_string()]);
    }

    #[test]
    fn crlf_and_bom_handled() {
        let sse = b"\xEF\xBB\xBFdata: hi\r\n\r\ndata: there\r\n\r\n";
        let events = parse_sse(sse).unwrap();
        assert_eq!(events, vec!["hi".to_string(), "there".to_string()]);
    }

    #[test]
    fn multi_data_lines_join_with_newline() {
        let sse = b"data: line1\ndata: line2\n\n";
        let events = parse_sse(sse).unwrap();
        assert_eq!(events, vec!["line1\nline2".to_string()]);
    }

    #[test]
    fn comment_and_unknown_fields_skipped() {
        let sse = b": heartbeat\ndata: value\nid: 3\n\n";
        let events = parse_sse(sse).unwrap();
        assert_eq!(events, vec!["value".to_string()]);
    }

    #[test]
    fn split_batches_accumulate() {
        let mut parser = SseParser::new();
        assert!(parser.push(b"data: hel").is_empty());
        // "lo\n\n" 完成第一个事件；"data: [DO" 未终结
        let events = parser.push(b"lo\n\ndata: [DO");
        assert_eq!(events, vec!["hello".to_string()]);
        let events = parser.push(b"NE]\n\n");
        assert_eq!(events, vec!["[DONE]".to_string()]);
    }

    #[test]
    fn unterminated_tail_at_eof_is_truncation_error() {
        let sse = b"data: partial";
        let err = parse_sse(sse).unwrap_err();
        assert_eq!(err.code, "STREAM_CLOSED");
    }
}

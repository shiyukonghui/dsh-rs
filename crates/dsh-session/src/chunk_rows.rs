//! `chunk_rows`（packChunkRuns/decodeStorageRecord 的字节关键移植，M1d）。
//!
//! 权威参考：`deepseek-harness/packages/core/session/src/chunk-rows.ts`（TS `packChunkRuns` /
//! `decodeStorageRecord`，见 subagent 规范 §D）。逐字对齐：
//! - `MIN_RUN = 3`：同 kind/同块连续 chunk 达到阈值才打包成存储行；
//! - 结构识别（`classify`）对**序列化后的 Value** 做精确键集检查，任何不规则的
//!   事件原样存储（绝不错删）；
//! - `decode_storage_record` 对行 tag 先校验后展开，坏行 fail loud；
//! - 键集判定与键序无关（serde_json BTreeMap 规范序，决策 D-014）。

use serde_json::{json, Map, Value};

/// 运行少于该成员数不打包（格式常量；两种布局解码一致）。
pub const MIN_RUN: usize = 3;

/// 是否为 record（object 且非 null）。
fn is_record(value: &Value) -> bool {
    value.is_object()
}

/// 精确键集检查：`value` 有全部 `keys` 且无其它键。
fn has_exact_keys(value: &Value, keys: &[&str]) -> bool {
    match value.as_object() {
        Some(obj) => {
            obj.len() == keys.len() && keys.iter().all(|k| obj.contains_key(*k))
        }
        None => false,
    }
}

/// 事件的宽 data 值（`{"turn","step","chunk"}`）。
fn event_data(value: &Value) -> Option<&Map<String, Value>> {
    value.get("data").and_then(Value::as_object)
}

/// 分类一个事件为可打包 delta kind：仅当整个形状（信封 + data + chunk 的精确键、
/// 原始类型、整数 seq/time）全部白名单命中，否则 `None`（原样存储值）。
///
/// 输入来自 live typed append 与解析后的 fixture 文件，因此检查是**结构性的**、
/// 非类型信任。整数 time 保证 gap 编码精确：非整数 time 经浮点差/和重构不保证往返。
fn classify(event: &Value) -> Option<&'static str> {
    if event.get("type").and_then(Value::as_str) != Some("assistant/chunk") {
        return None;
    }
    if !has_exact_keys(event, &["type", "seq", "time", "data"]) {
        return None;
    }
    let seq = event.get("seq");
    let time = event.get("time");
    let seq_ok = matches!(seq, Some(Value::Number(n)) if n.as_i64().is_some_and(|v| v >= 0));
    let time_ok = matches!(time, Some(Value::Number(n)) if n.as_i64().is_some());
    if !seq_ok || !time_ok {
        return None;
    }
    let data = event_data(event)?;
    let data_value = Value::Object(data.clone());
    if !has_exact_keys(&data_value, &["turn", "step", "chunk"]) {
        return None;
    }
    data.get("turn")?.as_u64()?;
    data.get("step")?.as_u64()?;
    let chunk = data.get("chunk")?;
    chunk.get("index")?.as_u64()?;
    match chunk.get("type").and_then(Value::as_str) {
        Some("text-delta") | Some("reasoning-delta") => {
            let exact = has_exact_keys(chunk, &["type", "index", "text"])
                && matches!(chunk.get("text"), Some(Value::String(_)));
            if exact {
                let kind = chunk.get("type").and_then(Value::as_str);
                match kind {
                    Some("text-delta") => Some("text-delta"),
                    Some("reasoning-delta") => Some("reasoning-delta"),
                    _ => None,
                }
            } else {
                None
            }
        }
        Some("tool-call-delta") => {
            let with_name = has_exact_keys(chunk, &["type", "index", "id", "name", "argumentsDelta"])
                && matches!(chunk.get("name"), Some(Value::String(_)));
            let without_name = has_exact_keys(chunk, &["type", "index", "id", "argumentsDelta"]);
            let id_ok = matches!(chunk.get("id"), Some(Value::String(_)));
            let args_ok = matches!(chunk.get("argumentsDelta"), Some(Value::String(_)));
            if (with_name || without_name) && id_ok && args_ok {
                Some("tool-call-delta")
            } else {
                None
            }
        }
        _ => None,
    }
}

/// 白名单 delta chunk 的工具调用字段（仅 `classify` 返回 tool-call-delta 后调用）。
fn tool_call_of(event: &Value) -> &Value {
    event_data(event)
        .and_then(|d| d.get("chunk"))
        .expect("classified tool-call-delta has a chunk")
}

/// 白名单 delta chunk 的块索引。
fn index_of(event: &Value) -> u64 {
    event_data(event)
        .and_then(|d| d.get("chunk"))
        .and_then(|c| c.get("index"))
        .and_then(Value::as_u64)
        .expect("classified delta has an index")
}

/// `next` 是否延续以 `prev` 结尾的运行（kind 已由调用方校验一致）。
fn continues(prev: &Value, next: &Value, kind: &'static str) -> bool {
    let prev_seq = prev.get("seq").and_then(Value::as_i64);
    let next_seq = next.get("seq").and_then(Value::as_i64);
    if next_seq != prev_seq.map(|v| v + 1) {
        return false;
    }
    let prev_time = prev.get("time").and_then(Value::as_i64);
    let next_time = next.get("time").and_then(Value::as_i64);
    let gap = match (next_time, prev_time) {
        (Some(n), Some(p)) => n.checked_sub(p),
        _ => None,
    };
    if gap.is_none() {
        return false;
    }
    let pd = event_data(prev).expect("classified full shape");
    let nd = event_data(next).expect("classified full shape");
    if pd.get("turn") != nd.get("turn") || pd.get("step") != nd.get("step") {
        return false;
    }
    if index_of(next) != index_of(prev) {
        return false;
    }
    if kind != "tool-call-delta" {
        return true;
    }
    let a = tool_call_of(prev);
    let b = tool_call_of(next);
    let a_has_name = a.get("name").is_some();
    let b_has_name = b.get("name").is_some();
    a.get("id") == b.get("id")
        && a_has_name == b_has_name
        && a.get("name") == b.get("name")
}

/// 为一个完成的运行构建存储行（`run.len() >= MIN_RUN`，每行带 envelope）。
fn build_row(kind: &'static str, run: &[Value]) -> Value {
    let first = &run[0];
    let base = json!({
        "turn": event_data(first).and_then(|d| d.get("turn")),
        "step": event_data(first).and_then(|d| d.get("step")),
        "index": index_of(first),
        "dt": run[1..]
            .iter()
            .zip(run.iter())
            .map(|(next, prev)| {
                next.get("time").and_then(Value::as_i64).unwrap()
                    - prev.get("time").and_then(Value::as_i64).unwrap()
            })
            .collect::<Vec<_>>(),
    });
    let base = match base {
        Value::Object(m) => m,
        _ => unreachable!(),
    };
    let envelope = json!({
        "seq0": first.get("seq"),
        "time0": first.get("time"),
    });
    let envelope = match envelope {
        Value::Object(m) => m,
        _ => unreachable!(),
    };
    if kind == "tool-call-delta" {
        let call = tool_call_of(first);
        let mut data = base;
        if let Some(id) = call.get("id") {
            data.insert("id".into(), id.clone());
        }
        if let Some(name) = call.get("name") {
            data.insert("name".into(), name.clone());
        }
        data.insert(
            "args".into(),
            Value::Array(
                run.iter()
                    .map(|e| {
                        event_data(e)
                            .and_then(|d| d.get("chunk"))
                            .and_then(|c| c.get("argumentsDelta"))
                            .cloned()
                            .expect("classified tool-call-delta has argumentsDelta")
                    })
                    .collect(),
            ),
        );
        let mut row = Map::new();
        row.insert("type".into(), Value::String("tool-call-chunks".into()));
        row.extend(envelope);
        row.insert("data".into(), Value::Object(data));
        return Value::Object(row);
    }
    let mut data = base;
    data.insert(
        "texts".into(),
        Value::Array(
            run.iter()
                .map(|e| {
                    event_data(e)
                        .and_then(|d| d.get("chunk"))
                        .and_then(|c| c.get("text"))
                        .cloned()
                        .expect("classified delta has text")
                })
                .collect(),
        ),
    );
    let tag = if kind == "text-delta" { "text-chunks" } else { "reasoning-chunks" };
    let mut row = Map::new();
    row.insert("type".into(), Value::String(tag.into()));
    row.extend(envelope);
    row.insert("data".into(), Value::Object(data));
    Value::Object(row)
}

/// 将一批事件打包为存储记录：每个至少 `MIN_RUN` 个连续同 kind/同块白名单 delta
/// chunk 的运行为一行（`ChunkRow`），其余事件按序原样透传。
///
/// 纯且无状态——对任意数组安全，包括被 flush 边界拆开的 batch（被拆的运行按 batch
/// 各自打包）。
pub fn pack_chunk_runs(events: &[Value]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let mut kind: Option<&'static str> = None;
    let mut run: Vec<Value> = Vec::new();
    let flush = |out: &mut Vec<Value>, kind: &mut Option<&'static str>, run: &mut Vec<Value>| {
        if let Some(k) = *kind {
            let taken = std::mem::take(run);
            if taken.len() >= MIN_RUN {
                out.push(build_row(k, &taken));
            } else {
                out.extend(taken);
            }
        }
        *kind = None;
        run.clear();
    };
    for event in events {
        let k = classify(event);
        match k {
            None => {
                flush(&mut out, &mut kind, &mut run);
                out.push(event.clone());
            }
            Some(k) => {
                let last = run.last();
                let continues = kind == Some(k)
                    && last.is_some_and(|l| continues(l, event, k));
                if continues {
                    run.push(event.clone());
                } else {
                    flush(&mut out, &mut kind, &mut run);
                    kind = Some(k);
                    run.push(event.clone());
                }
            }
        }
    }
    flush(&mut out, &mut kind, &mut run);
    out
}

/// 统一坏行诊断。
fn malformed(tag: &str, why: &str) -> String {
    format!("malformed {tag} storage row: {why}")
}

/// 校验共享运行数据字段与 payload/dt 元数；返回成员 payload 引用。
fn validate_run_data<'a>(tag: &str, data: &'a Map<String, Value>, payload_key: &str) -> Result<&'a Value, String> {
    if !matches!(data.get("turn"), Some(Value::Number(_)))
        || !matches!(data.get("step"), Some(Value::Number(_)))
        || !matches!(data.get("index"), Some(Value::Number(_)))
    {
        return Err(malformed(tag, "turn/step/index must be numbers"));
    }
    let payload = data.get(payload_key).ok_or_else(|| malformed(tag, "missing payload"))?;
    match payload {
        Value::Array(items) if !items.is_empty() && items.iter().all(|e| e.is_string()) => {}
        _ => return Err(malformed(tag, &format!("{payload_key} must be a non-empty string array"))),
    }
    let dt = data.get("dt").and_then(Value::as_array).ok_or_else(|| malformed(tag, "dt must be an array of safe integers"))?;
    if dt.iter().any(|g| !matches!(g, Value::Number(n) if n.as_i64().is_some())) {
        return Err(malformed(tag, "dt must be an array of safe integers"));
    }
    let payload = payload.as_array().expect("payload is array");
    if dt.len() != payload.len() - 1 {
        return Err(malformed(
            tag,
            &format!("dt length {} does not match {} members", dt.len(), payload.len()),
        ));
    }
    Ok(data.get(payload_key).expect("payload present"))
}

/// 校验一个行 tag 的解析值信封与 data，任何畸形即抛错。
fn validate_row(value: &Value, tag: &str) -> Result<(), String> {
    if !has_exact_keys(value, &["type", "seq0", "time0", "data"]) {
        return Err(malformed(tag, "envelope must be exactly {type, seq0, time0, data}"));
    }
    let seq0 = value.get("seq0").and_then(Value::as_i64);
    let time0 = value.get("time0").and_then(Value::as_i64);
    if seq0.is_none_or(|v| v < 0) {
        return Err(malformed(tag, "seq0 must be a non-negative safe integer"));
    }
    if time0.is_none() {
        return Err(malformed(tag, "time0 must be a safe integer"));
    }
    let data = value.get("data").and_then(Value::as_object).ok_or_else(|| malformed(tag, "data must be an object"))?;
    if tag == "tool-call-chunks" {
        let with_name = has_exact_keys(
            &Value::Object(data.clone()),
            &["turn", "step", "index", "id", "name", "dt", "args"],
        );
        let without_name = has_exact_keys(
            &Value::Object(data.clone()),
            &["turn", "step", "index", "id", "dt", "args"],
        );
        if !with_name && !without_name {
            return Err(malformed(tag, "data must be exactly {turn, step, index, id, name?, dt, args}"));
        }
        let id_ok = matches!(data.get("id"), Some(Value::String(_)));
        let name_ok = !with_name || matches!(data.get("name"), Some(Value::String(_)));
        if !id_ok || !name_ok {
            return Err(malformed(tag, "id (and name when present) must be strings"));
        }
        validate_run_data(tag, data, "args")?;
    } else {
        if !has_exact_keys(&Value::Object(data.clone()), &["turn", "step", "index", "dt", "texts"]) {
            return Err(malformed(tag, "data must be exactly {turn, step, index, dt, texts}"));
        }
        validate_run_data(tag, data, "texts")?;
    }
    let payload = if tag == "tool-call-chunks" { data.get("args") } else { data.get("texts") }
        .and_then(Value::as_array)
        .expect("validated payload is array");
    let end_seq = seq0.unwrap() + payload.len() as i64 - 1;
    if end_seq < 0 {
        return Err(malformed(tag, "member seqs must stay safe integers"));
    }
    let mut time = time0.unwrap();
    for gap in data.get("dt").and_then(Value::as_array).expect("validated dt array") {
        time += gap.as_i64().expect("validated dt safe integer");
    }
    let _ = time; // 成功路径不缺省；数字安全性已由 i64 保证
    Ok(())
}

/// 把一个已校验的行展开回其精确原始事件，按序。
fn expand_row(row: &Value) -> Vec<Value> {
    let tag = row.get("type").and_then(Value::as_str).expect("validated row tag");
    let data = row.get("data").and_then(Value::as_object).expect("validated data");
    let members = if tag == "tool-call-chunks" {
        data.get("args").and_then(Value::as_array)
    } else {
        data.get("texts").and_then(Value::as_array)
    }
    .expect("validated payload");
    let seq0 = row.get("seq0").and_then(Value::as_i64).expect("validated seq0");
    let time0 = row.get("time0").and_then(Value::as_i64).expect("validated time0");
    let dt = data.get("dt").and_then(Value::as_array).expect("validated dt");
    let mut events = Vec::with_capacity(members.len());
    let mut time = time0;
    for (k, member) in members.iter().enumerate() {
        if k > 0 {
            time += dt[k - 1].as_i64().expect("validated gap");
        }
        let index = data.get("index").and_then(Value::as_i64).expect("validated index");
        let chunk = match tag {
            "text-chunks" => {
                json!({ "type": "text-delta", "index": index, "text": member })
            }
            "reasoning-chunks" => {
                json!({ "type": "reasoning-delta", "index": index, "text": member })
            }
            "tool-call-chunks" => {
                let mut c = Map::new();
                c.insert("type".into(), Value::String("tool-call-delta".into()));
                c.insert("index".into(), Value::Number(index.into()));
                c.insert("id".into(), data.get("id").cloned().expect("validated id"));
                if let Some(name) = data.get("name") {
                    c.insert("name".into(), name.clone());
                }
                c.insert("argumentsDelta".into(), member.clone());
                Value::Object(c)
            }
            _ => unreachable!("validateRow only returns the three row tags"),
        };
        let mut event = Map::new();
        event.insert("type".into(), Value::String("assistant/chunk".into()));
        event.insert("seq".into(), Value::Number((seq0 + k as i64).into()));
        event.insert("time".into(), Value::Number(time.into()));
        event.insert(
            "data".into(),
            json!({ "turn": data.get("turn"), "step": data.get("step"), "chunk": chunk }),
        );
        events.push(Value::Object(event));
    }
    events
}

/// 解码一行 JSONL 值：行 tag 则校验并展开（畸形行抛错——是损坏存储，当作事件会
/// 静默丢整段）；其它值作为单事件透传，不校验。
pub fn decode_storage_record(value: Value) -> Result<Vec<Value>, String> {
    if !is_record(&value) {
        return Ok(vec![value]);
    }
    let tag = value.get("type").and_then(Value::as_str);
    match tag {
        Some("text-chunks") => {
            validate_row(&value, "text-chunks")?;
            Ok(expand_row(&value))
        }
        Some("reasoning-chunks") => {
            validate_row(&value, "reasoning-chunks")?;
            Ok(expand_row(&value))
        }
        Some("tool-call-chunks") => {
            validate_row(&value, "tool-call-chunks")?;
            Ok(expand_row(&value))
        }
        _ => Ok(vec![value]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn chunk(seq: i64, time: i64, turn: u64, step: u64, index: u64, text: &str) -> Value {
        json!({
            "type": "assistant/chunk",
            "seq": seq,
            "time": time,
            "data": { "turn": turn, "step": step, "chunk": { "type": "text-delta", "index": index, "text": text } }
        })
    }
    fn reasoning(seq: i64, time: i64, turn: u64, step: u64, index: u64, text: &str) -> Value {
        json!({
            "type": "assistant/chunk",
            "seq": seq,
            "time": time,
            "data": { "turn": turn, "step": step, "chunk": { "type": "reasoning-delta", "index": index, "text": text } }
        })
    }
    #[allow(clippy::too_many_arguments)]
    fn tool_call(seq: i64, time: i64, turn: u64, step: u64, index: u64, id: &str, name: Option<&str>, args: &str) -> Value {
        let mut chunk = Map::new();
        chunk.insert("type".into(), Value::String("tool-call-delta".into()));
        chunk.insert("index".into(), json!(index));
        chunk.insert("id".into(), json!(id));
        if let Some(n) = name {
            chunk.insert("name".into(), json!(n));
        }
        chunk.insert("argumentsDelta".into(), json!(args));
        json!({
            "type": "assistant/chunk",
            "seq": seq,
            "time": time,
            "data": { "turn": turn, "step": step, "chunk": Value::Object(chunk) }
        })
    }

    #[test]
    fn run_below_minimum_passes_through() {
        // 2 个同块连续 text-delta < MIN_RUN=3 → 原样
        let events = vec![chunk(0, 100, 1, 1, 0, "a"), chunk(1, 101, 1, 1, 0, "b")];
        assert_eq!(pack_chunk_runs(&events), events);
    }

    #[test]
    fn run_at_minimum_packs_to_text_chunks() {
        let events = vec![chunk(0, 100, 1, 1, 0, "a"), chunk(1, 102, 1, 1, 0, "bb"), chunk(2, 105, 1, 1, 0, "ccc")];
        let packed = pack_chunk_runs(&events);
        assert_eq!(packed.len(), 1);
        let row = &packed[0];
        assert_eq!(row.get("type").and_then(Value::as_str), Some("text-chunks"));
        assert_eq!(row.get("seq0").and_then(Value::as_i64), Some(0));
        assert_eq!(row.get("time0").and_then(Value::as_i64), Some(100));
        let data = row.get("data").unwrap();
        assert_eq!(data.get("turn").and_then(Value::as_u64), Some(1));
        assert_eq!(data.get("step").and_then(Value::as_u64), Some(1));
        assert_eq!(data.get("index").and_then(Value::as_u64), Some(0));
        assert_eq!(data.get("dt"), Some(&json!([2, 3])));
        assert_eq!(data.get("texts"), Some(&json!(["a", "bb", "ccc"])));
        // 往返
        let decoded = decode_storage_record(row.clone()).unwrap();
        assert_eq!(decoded, events);
    }

    #[test]
    fn mixed_kind_runs_pack_separately_and_interleave_verbatim() {
        let events = vec![
            chunk(0, 100, 1, 1, 0, "a"),
            chunk(1, 101, 1, 1, 0, "b"),
            chunk(2, 102, 1, 1, 0, "c"),
            reasoning(3, 103, 1, 1, 0, "r1"),
            reasoning(4, 105, 1, 1, 0, "r2"),
            reasoning(5, 108, 1, 1, 0, "r3"),
            chunk(6, 109, 1, 1, 0, "single-mid"),
            chunk(7, 110, 1, 1, 0, "d"),
            chunk(8, 111, 1, 1, 0, "e"),
            chunk(9, 113, 1, 1, 0, "f"),
        ];
        let packed = pack_chunk_runs(&events);
        // text run seq0..2, reasoning run seq3..5, text run seq6..9 (consecutive same block)
        assert_eq!(packed.len(), 3);
        assert_eq!(packed[0].get("type").and_then(Value::as_str), Some("text-chunks"));
        assert_eq!(packed[1].get("type").and_then(Value::as_str), Some("reasoning-chunks"));
        assert_eq!(packed[2].get("type").and_then(Value::as_str), Some("text-chunks"));
        let mut back = Vec::new();
        for v in &packed {
            back.extend(decode_storage_record(v.clone()).unwrap());
        }
        assert_eq!(back, events);
    }

    #[test]
    fn tool_call_run_preserves_id_and_name_presence() {
        // 3 个同 id 同名 tool-call-delta → 打包
        let events = vec![
            tool_call(0, 100, 1, 1, 2, "call_1", Some("read_file"), "{\"path\":"),
            tool_call(1, 101, 1, 1, 2, "call_1", Some("read_file"), "\"a.txt\"}"),
            tool_call(2, 102, 1, 1, 2, "call_1", Some("read_file"), "\"\"}"),
        ];
        let packed = pack_chunk_runs(&events);
        assert_eq!(packed.len(), 1);
        assert_eq!(packed[0].get("type").and_then(Value::as_str), Some("tool-call-chunks"));
        let data = packed[0].get("data").unwrap();
        assert_eq!(data.get("id").and_then(Value::as_str), Some("call_1"));
        assert_eq!(data.get("name").and_then(Value::as_str), Some("read_file"));
        assert_eq!(data.get("dt"), Some(&json!([1, 1])));
        assert_eq!(data.get("args"), Some(&json!(["{\"path\":", "\"a.txt\"}", "\"\"}"])));
        let back = decode_storage_record(packed[0].clone()).unwrap();
        assert_eq!(back, events);
    }

    #[test]
    fn different_id_breaks_tool_call_run() {
        let events = vec![
            tool_call(0, 100, 1, 1, 2, "call_1", None, "a"),
            tool_call(1, 101, 1, 1, 2, "call_2", None, "b"),
            tool_call(2, 102, 1, 1, 2, "call_2", None, "c"),
        ];
        let packed = pack_chunk_runs(&events);
        // run: [call_1] (1), then [call_1? no] -> call_1 alone, then call_2, call_2 (< 3) ...
        // 全 < MIN_RUN → 原样
        assert_eq!(packed, events);
    }

    #[test]
    fn non_assistant_chunk_passes_through() {
        let ev = json!({ "type": "user/message", "seq": 0, "time": 100, "data": { "id": "m1" } });
        assert_eq!(pack_chunk_runs(std::slice::from_ref(&ev)), vec![ev]);
    }

    #[test]
    fn extra_envelope_key_blocks_classification() {
        // 带 surfaceOp 的 assistant/chunk 信封多了键 → 不打包（hasExactKeys 拒绝）
        let ev = json!({
            "type": "assistant/chunk", "seq": 0, "time": 100, "data": { "turn": 1, "step": 1, "chunk": { "type": "text-delta", "index": 0, "text": "a" } },
            "surfaceOp": "append"
        });
        assert_eq!(classify(&ev), None);
        assert_eq!(pack_chunk_runs(std::slice::from_ref(&ev)), vec![ev]);
    }

    #[test]
    fn malformed_row_fails_loud() {
        let bad = json!({ "type": "text-chunks", "seq0": 0, "time0": 100, "data": { "turn": 1, "step": 1, "index": 0, "dt": [1], "texts": ["a", "b", "c"] } });
        // dt 长度 1 但 texts 3 成员 → 错误
        assert!(decode_storage_record(bad).is_err());
        let bad2 = json!({ "type": "text-chunks", "seq0": 0, "time0": 100, "data": { "turn": 1, "step": 1, "index": 0, "dt": [1, 1], "texts": [] } });
        assert!(decode_storage_record(bad2).is_err());
    }
}

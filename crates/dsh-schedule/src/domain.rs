//! `dsh-schedule` 域：严格 decode / 重放 fold / 时间校验 / framing。
//!
//! 对齐 `packages/schedule/schedule/src/domain.ts`。时间均为 Unix 毫秒 epoch；durable
//! instant 为规范四位数年 RFC 3339 UTC（`YYYY-MM-DDTHH:mm:ss.sssZ`）。

use serde_json::{json, Value};

/// Durable Schedule 协议版本。
pub const SCHEDULE_CHANGE_VERSION: u64 = 1;

/// 固定速率 reminder 下限（秒）。
pub const MIN_EVERY_INTERVAL_SECONDS: u64 = 300;

const MIN_YEAR_MS: i64 = -62_135_596_800_000; // 0001-01-01T00:00:00.000Z
const MAX_YEAR_MS: i64 = 253_402_300_799_000; // 9999-12-31T23:59:59.999Z

/// 日志错误（durable 数据损坏）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogError(pub String);

impl std::fmt::Display for LogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "corrupt_schedule_log: {}", self.0)
    }
}

/// 输入错误代码（逐字对齐 six ScheduleInputError codes）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputCode {
    InvalidPrompt,
    InvalidRule,
    InvalidTimeZone,
    NotFuture,
    TimeOutOfRange,
    FrequencyTooHigh,
}

/// 模型提供的规则输入错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleInputError {
    pub code: InputCode,
    pub message: String,
}

impl std::fmt::Display for ScheduleInputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}

/// 一条 durable 记录（active 重放结果）。
#[derive(Debug, Clone, PartialEq)]
pub struct ScheduleRecordData {
    pub id: String,
    pub kind: String,
    pub prompt: String,
    pub after_seconds: Option<u64>,
    pub every_seconds: Option<u64>,
    pub scheduled_at: String,
}

/// 纯重放结果：active 记录（原创建序）+ 所有用过的 id。
#[derive(Debug, Clone, Default)]
pub struct FoldedSchedules {
    pub records: Vec<ScheduleRecordData>,
    pub active_ids: Vec<String>,
    pub seen_ids: Vec<String>,
}

/// scheduleView wire（对齐 TS ScheduleView + deliveryMode）。
#[derive(Debug, Clone)]
pub struct ScheduleViewData {
    pub id: String,
    pub kind: String,
    pub prompt: String,
    pub scheduled_at: String,
    /// after/at/every 的时长字段（据 kind 呈现）。
    pub after_seconds: Option<u64>,
    pub every_seconds: Option<u64>,
    pub state: String,
    pub delivery_mode: String,
}

// ---- 基础校验 ----

fn is_record(v: &Value) -> bool {
    v.is_object()
}

/// 精确键集合校验（排序后相等）。
fn has_exact_keys(obj: &serde_json::Map<String, Value>, keys: &[&str]) -> bool {
    let mut o: Vec<&str> = obj.keys().map(|k| k.as_str()).collect();
    let mut w: Vec<&str> = keys.to_vec();
    o.sort_unstable();
    w.sort_unstable();
    o == w
}

/// 校验 session-local id（非空、无周边空白）。
fn decode_id(v: &Value) -> Result<String, LogError> {
    let s = v
        .as_str()
        .ok_or_else(|| LogError("schedule id must be a non-empty string".into()))?;
    if s.is_empty() || s.trim() != s {
        return Err(LogError("schedule id must be a non-empty string without surrounding whitespace".into()));
    }
    Ok(s.to_string())
}

/// 正则风格校验规范 UTC instant 并验证真实日历日期。
pub fn is_utc_instant(s: &str) -> bool {
    // YYYY-MM-DDTHH:MM:SS.mmmZ，四位数年，非 0000，月 01-12，日 01-31，时 00-23 分秒 00-59。
    let bytes = s.as_bytes();
    if bytes.len() != 24 {
        return false;
    }
    let year: i64 = match s[0..4].parse() {
        Ok(y) => y,
        Err(_) => return false,
    };
    if !(1..=9999).contains(&year) {
        return false;
    }
    // 固定格式字符
    let dash = [4, 7];
    for &i in &dash {
        if bytes[i] != b'-' {
            return false;
        }
    }
    if bytes[10] != b'T' || bytes[13] != b':' || bytes[16] != b':' || bytes[19] != b'.' || bytes[23] != b'Z' {
        return false;
    }
    let num = |from: usize, to: usize| -> Option<i64> {
        let t = &s[from..to];
        t.parse::<i64>().ok().filter(|v| *v >= 0)
    };
    let Some(month) = num(5, 7) else { return false };
    let Some(day) = num(8, 10) else { return false };
    let Some(hour) = num(11, 13) else { return false };
    let Some(minute) = num(14, 16) else { return false };
    let Some(second) = num(17, 19) else { return false };
    let Some(millis) = num(20, 23) else { return false };
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return false;
    }
    if hour > 23 || minute > 59 || second > 59 || millis > 999 {
        return false;
    }
    days_in_month(year, month) >= day
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
            if leap {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

/// 把规范 UTC instant 解析为 epoch 毫秒。
pub(crate) fn utc_instant_to_epoch(s: &str) -> Option<i64> {
    if !is_utc_instant(s) {
        return None;
    }
    let year: i64 = s[0..4].parse().ok()?;
    let month: i64 = s[5..7].parse().ok()?;
    let day: i64 = s[8..10].parse().ok()?;
    let hour: i64 = s[11..13].parse().ok()?;
    let minute: i64 = s[14..16].parse().ok()?;
    let second: i64 = s[17..19].parse().ok()?;
    let millis: i64 = s[20..23].parse().ok()?;
    let days = days_from_civil(year, month, day)?;
    let secs = days * 86_400 + hour * 3_600 + minute * 60 + second;
    Some(secs * 1_000 + millis)
}

/// 天数序：civil 日期 → 自 1970-01-01 起的天数（Howard Hinnant days_from_civil）。
fn days_from_civil(y: i64, m: i64, d: i64) -> Option<i64> {
    if !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

/// decode one strict `schedule/change` payload → wire Value（规范化；不 frozen）。
pub fn decode_schedule_change(value: &Value) -> Result<Value, LogError> {
    if !is_record(value) {
        return Err(LogError("schedule/change payload must be an object".into()));
    }
    let obj = value.as_object().expect("is_object");
    let version = obj
        .get("version")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| LogError("schedule/change version must be 1".into()))?;
    if version != SCHEDULE_CHANGE_VERSION {
        return Err(LogError("schedule/change version must be 1".into()));
    }
    let operation = obj
        .get("operation")
        .and_then(|v| v.as_str())
        .ok_or_else(|| LogError("schedule/change operation must be create, delete, or dispatch".into()))?;
    match operation {
        "create" => {
            if !has_exact_keys(obj, &["version", "operation", "schedule"]) {
                return Err(LogError("schedule create must contain exactly version, operation, and schedule".into()));
            }
            let schedule = decode_record(&obj["schedule"])?;
            Ok(json!({ "version": version, "operation": "create", "schedule": schedule }))
        }
        "delete" => {
            if !has_exact_keys(obj, &["version", "operation", "id"]) {
                return Err(LogError("schedule delete must contain exactly version, operation, and id".into()));
            }
            let id = decode_id(&obj["id"])?;
            Ok(json!({ "version": version, "operation": "delete", "id": id }))
        }
        "dispatch" => {
            if has_exact_keys(obj, &["version", "operation", "id"]) {
                let id = decode_id(&obj["id"])?;
                return Ok(json!({ "version": version, "operation": "dispatch", "id": id }));
            }
            if has_exact_keys(obj, &["version", "operation", "id", "acceptedAt"]) {
                let id = decode_id(&obj["id"])?;
                let accepted = decode_instant(&obj["acceptedAt"])?;
                return Ok(json!({
                    "version": version, "operation": "dispatch", "id": id, "acceptedAt": accepted
                }));
            }
            Err(LogError("schedule dispatch must contain id and optional acceptedAt only".into()))
        }
        _ => Err(LogError("schedule/change operation must be create, delete, or dispatch".into())),
    }
}

fn decode_instant(value: &Value) -> Result<String, LogError> {
    let s = value
        .as_str()
        .ok_or_else(|| LogError("scheduledAt must be a canonical four-digit-year RFC 3339 UTC instant".into()))?;
    if !is_utc_instant(s) {
        return Err(LogError("scheduledAt must be a canonical four-digit-year RFC 3339 UTC instant".into()));
    }
    if utc_instant_to_epoch(s).is_none() {
        return Err(LogError("scheduledAt must be a four-digit-year RFC 3339 UTC instant".into()));
    }
    Ok(s.to_string())
}

/// decode one record（after/at/every），精确键 + 规范校验。
fn decode_record(value: &Value) -> Result<Value, LogError> {
    if !is_record(value) {
        return Err(LogError("schedule record must be an object".into()));
    }
    let obj = value.as_object().expect("is_object");
    let kind = obj
        .get("kind")
        .and_then(|v| v.as_str())
        .ok_or_else(|| LogError("v1 schedule kind must be \"after\", \"at\", or \"every\"".into()))?;
    match kind {
        "after" => {
            if !has_exact_keys(obj, &["id", "kind", "prompt", "afterSeconds", "scheduledAt"]) {
                return Err(LogError("after schedule must contain exactly id, kind, prompt, afterSeconds, and scheduledAt".into()));
            }
            let prompt = decode_prompt(&obj["prompt"], "after")?;
            let after_seconds = obj
                .get("afterSeconds")
                .and_then(|v| v.as_u64())
                .filter(|v| *v > 0)
                .ok_or_else(|| LogError("afterSeconds must be a positive safe integer".into()))?;
            let scheduled_at = decode_instant(&obj["scheduledAt"])?;
            Ok(json!({
                "id": decode_id(&obj["id"])?,
                "kind": "after",
                "prompt": prompt,
                "afterSeconds": after_seconds,
                "scheduledAt": scheduled_at,
            }))
        }
        "at" => {
            if !has_exact_keys(obj, &["id", "kind", "prompt", "scheduledAt"]) {
                return Err(LogError("at schedule must contain exactly id, kind, prompt, and scheduledAt".into()));
            }
            let prompt = decode_prompt(&obj["prompt"], "at")?;
            let scheduled_at = decode_instant(&obj["scheduledAt"])?;
            Ok(json!({
                "id": decode_id(&obj["id"])?,
                "kind": "at",
                "prompt": prompt,
                "scheduledAt": scheduled_at,
            }))
        }
        "every" => {
            if !has_exact_keys(obj, &["id", "kind", "prompt", "everySeconds", "scheduledAt"]) {
                return Err(LogError("every schedule must contain exactly id, kind, prompt, everySeconds, and scheduledAt".into()));
            }
            let prompt = decode_prompt(&obj["prompt"], "every")?;
            let every_seconds = obj
                .get("everySeconds")
                .and_then(|v| v.as_u64())
                .filter(|v| *v >= MIN_EVERY_INTERVAL_SECONDS)
                .ok_or_else(|| LogError(format!("everySeconds must be a safe integer of at least {MIN_EVERY_INTERVAL_SECONDS}")))?;
            let scheduled_at = decode_instant(&obj["scheduledAt"])?;
            Ok(json!({
                "id": decode_id(&obj["id"])?,
                "kind": "every",
                "prompt": prompt,
                "everySeconds": every_seconds,
                "scheduledAt": scheduled_at,
            }))
        }
        _ => Err(LogError("v1 schedule kind must be \"after\", \"at\", or \"every\"".into())),
    }
}

fn decode_prompt(value: &Value, kind: &str) -> Result<String, LogError> {
    let s = value
        .as_str()
        .ok_or_else(|| LogError(format!("{kind} prompt must be non-empty and already trimmed")))?;
    if s.is_empty() || s.trim() != s {
        return Err(LogError(format!("{kind} prompt must be non-empty and already trimmed")));
    }
    Ok(s.to_string())
}

/// fold 记录为结构化数据。
fn record_from_value(value: &Value) -> Result<ScheduleRecordData, LogError> {
    let obj = value.as_object().ok_or_else(|| LogError("record must be object".into()))?;
    let id = obj["id"].as_str().unwrap_or_default().to_string();
    let kind = obj["kind"].as_str().unwrap_or_default().to_string();
    let prompt = obj["prompt"].as_str().unwrap_or_default().to_string();
    let after_seconds = obj.get("afterSeconds").and_then(|v| v.as_u64());
    let every_seconds = obj.get("everySeconds").and_then(|v| v.as_u64());
    let scheduled_at = obj["scheduledAt"].as_str().unwrap_or_default().to_string();
    Ok(ScheduleRecordData { id, kind, prompt, after_seconds, every_seconds, scheduled_at })
}

/// 重放（fork 支持 seedLength 分界）：fold 自 stamp 之后的事件。
fn fold_inner(events: &[Value], seed: usize) -> Result<FoldedSchedules, LogError> {
    if seed > events.len() {
        return Err(LogError("schedule seedLength must be within the supplied event log".into()));
    }
    let mut active: Vec<ScheduleRecordData> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for event in &events[seed..] {
        if event.get("type").and_then(|t| t.as_str()) != Some("schedule/change") {
            continue;
        }
        let Some(data) = event.get("data") else {
            return Err(LogError("schedule/change event missing data".into()));
        };
        let change = decode_schedule_change(data)?;
        let op = change["operation"].as_str().unwrap_or_default();
        match op {
            "create" => {
                let rec = record_from_value(&change["schedule"])?;
                if seen.contains(&rec.id) {
                    return Err(LogError(format!("schedule id was reused: {}", rec.id)));
                }
                seen.push(rec.id.clone());
                active.push(rec);
            }
            "delete" => {
                let id = change["id"].as_str().unwrap_or_default().to_string();
                let before = active.len();
                active.retain(|r| r.id != id);
                if active.len() == before {
                    return Err(LogError(format!("schedule delete targets inactive id {}", id)));
                }
            }
            "dispatch" => {
                let id = change["id"].as_str().unwrap_or_default().to_string();
                let idx = active.iter().position(|r| r.id == id);
                let Some(idx) = idx else {
                    return Err(LogError(format!("schedule dispatch targets inactive id {}", id)));
                };
                let rec = &active[idx];
                if rec.kind == "every" {
                    let accepted = change
                        .get("acceptedAt")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| LogError("every dispatch must contain acceptedAt".into()))?;
                    let occ = resolve_every_occurrence(&rec.scheduled_at, rec.every_seconds.unwrap_or(0), accepted)?;
                    match occ.next_scheduled_at {
                        Some(next) => {
                            let mut next_rec = rec.clone();
                            next_rec.scheduled_at = next;
                            active[idx] = next_rec;
                        }
                        None => {
                            active.remove(idx);
                        }
                    }
                } else {
                    if change.get("acceptedAt").is_some() {
                        return Err(LogError("one-shot dispatch must not contain acceptedAt".into()));
                    }
                    active.remove(idx);
                }
            }
            _ => return Err(LogError("unknown decoded schedule change".into())),
        }
    }
    let active_ids: Vec<String> = active.iter().map(|r| r.id.clone()).collect();
    Ok(FoldedSchedules { records: active, active_ids, seen_ids: seen })
}

/// fold 完整日志。
pub fn fold_schedule_events(events: &[Value]) -> Result<FoldedSchedules, LogError> {
    fold_inner(events, 0)
}

/// fold 自 seed 分界（fork 不继承父 active）。
pub fn fold_schedule_events_seeded(events: &[Value], seed: usize) -> Result<FoldedSchedules, LogError> {
    fold_inner(events, seed)
}

/// 分配下一个可读 id（不重用在用过的）。
pub fn allocate_id_from_seen(seen: &[String]) -> String {
    let mut sequence = seen.len() as u64 + 1;
    let mut candidate = format!("schedule-{sequence}");
    while seen.iter().any(|s| s == &candidate) {
        sequence += 1;
        candidate = format!("schedule-{sequence}");
    }
    candidate
}

/// 未来瞬时校验（规范 + 严格 > now）。
fn future_instant(epoch: i64, now: i64) -> Result<String, ScheduleInputError> {
    if !(MIN_YEAR_MS..=MAX_YEAR_MS).contains(&epoch) {
        return Err(ScheduleInputError {
            code: InputCode::TimeOutOfRange,
            message: "The scheduled time must be representable as a four-digit-year RFC 3339 UTC instant.".into(),
        });
    }
    if epoch <= now {
        return Err(ScheduleInputError {
            code: InputCode::NotFuture,
            message: "The scheduled time must be strictly in the future.".into(),
        });
    }
    let instant = epoch_to_utc_instant(epoch)
        .ok_or_else(|| ScheduleInputError {
            code: InputCode::TimeOutOfRange,
            message: "The scheduled time must be representable as a four-digit-year RFC 3339 UTC instant.".into(),
        })?;
    if !is_utc_instant(&instant) {
        return Err(ScheduleInputError {
            code: InputCode::TimeOutOfRange,
            message: "The scheduled time must be representable as a four-digit-year RFC 3339 UTC instant.".into(),
        });
    }
    Ok(instant)
}

/// epoch 毫秒 → 规范 UTC instant（四位数年）。
pub(crate) fn epoch_to_utc_instant(epoch: i64) -> Option<String> {
    let days = epoch.div_euclid(86_400_000);
    let secs_of_day = epoch.rem_euclid(86_400_000) / 1_000;
    let millis = epoch.rem_euclid(1_000);
    let (y, m, d) = civil_from_days(days)?;
    if !(1..=9999).contains(&y) {
        return None;
    }
    let h = secs_of_day / 3_600;
    let mi = (secs_of_day % 3_600) / 60;
    let s = secs_of_day % 60;
    Some(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        y, m, d, h, mi, s, millis
    ))
}

/// days_from_civil 逆运算（civil_from_days）。
fn civil_from_days(z: i64) -> Option<(i64, i64, i64)> {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    Some((y, m, d))
}

/// 解析带显式数值偏移的 RFC 3339 instant → UTC epoch 毫秒。
fn parse_offset_instant(value: &str) -> Result<i64, ScheduleInputError> {
    // 支持 ±HH:MM（或 GMT±HH:MM[:SS]）或 Z；可带 1-3 位小数秒。
    let (body, offset_ms) = split_offset(value)?;
    if !is_time_part_valid(body) {
        return Err(ScheduleInputError {
            code: InputCode::InvalidRule,
            message: "at must use YYYY-MM-DDTHH:mm:ss with optional fractional seconds and an explicit Z or numeric offset.".into(),
        });
    }
    let local_epoch = utc_instant_from_parts(body).ok_or_else(|| ScheduleInputError {
        code: InputCode::InvalidRule,
        message: "The at value must be a real ISO calendar date and time.".into(),
    })?;
    Ok(local_epoch - offset_ms)
}

/// 从 instant 中分解本地时间主体与 UTC 偏移毫秒。
fn split_offset(value: &str) -> Result<(&str, i64), ScheduleInputError> {
    if let Some(stripped) = value.strip_suffix('Z') {
        return Ok((stripped, 0));
    }
    if value.ends_with("+00:00") || value.ends_with("-00:00") {
        let body = &value[..value.len() - 6];
        return Ok((body, 0));
    }
    let bytes = value.as_bytes();
    let n = bytes.len();
    let mut sign_at: Option<usize> = None;
    for (i, &b) in bytes.iter().enumerate().rev() {
        if b == b'+' || b == b'-' {
            // 要求其后形如 HH:MM
            if i + 6 == n && bytes[i + 3] == b':' {
                sign_at = Some(i);
                break;
            }
        }
    }
    let Some(si) = sign_at else {
        return Err(ScheduleInputError {
            code: InputCode::InvalidRule,
            message: "at must include explicit Z or numeric offset".into(),
        });
    };
    let sign: i64 = if bytes[si] == b'+' { 1 } else { -1 };
    let hh: i64 = value[si + 1..si + 3].parse().map_err(|_| ScheduleInputError {
        code: InputCode::InvalidRule,
        message: "The at numeric offset is invalid.".into(),
    })?;
    let mm: i64 = value[si + 4..si + 6].parse().map_err(|_| ScheduleInputError {
        code: InputCode::InvalidRule,
        message: "The at numeric offset is invalid.".into(),
    })?;
    if hh > 23 || mm > 59 || (sign < 0 && hh == 0 && mm == 0) {
        return Err(ScheduleInputError {
            code: InputCode::InvalidRule,
            message: "The at numeric offset is invalid.".into(),
        });
    }
    Ok((&value[..si], sign * (hh * 3_600 + mm * 60) * 1_000))
}

/// 校验 `YYYY-MM-DDTHH:MM:SS[.fff]` 部分（无 Z）。
fn is_time_part_valid(utc: &str) -> bool {
    // 去掉可选小数秒，尾部应为 :SS
    let (head, _) = match utc.rsplit_once('.') {
        Some((h, f)) => {
            if f.is_empty() || f.len() > 3 || !f.bytes().all(|b| b.is_ascii_digit()) {
                return false;
            }
            (h, true)
        }
        None => (utc, false),
    };
    let bytes = head.as_bytes();
    if head.len() != 19 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T'
        || bytes[13] != b':' || bytes[16] != b':'
    {
        return false;
    }
    let n = |from: usize, to: usize| head[from..to].parse::<i64>().ok();
    let year = n(0, 4).unwrap_or(-1);
    let month = n(5, 7).unwrap_or(0);
    let day = n(8, 10).unwrap_or(0);
    let hour = n(11, 13).unwrap_or(99);
    let minute = n(14, 16).unwrap_or(99);
    let second = n(17, 19).unwrap_or(99);
    year >= 1
        && (1..=12).contains(&month)
        && day >= 1
        && day <= days_in_month(year, month)
        && hour <= 23
        && minute <= 59
        && second <= 59
}

fn utc_instant_from_parts(utc: &str) -> Option<i64> {
    if !is_time_part_valid(utc) {
        return None;
    }
    let year: i64 = utc[0..4].parse().ok()?;
    let month: i64 = utc[5..7].parse().ok()?;
    let day: i64 = utc[8..10].parse().ok()?;
    let hour: i64 = utc[11..13].parse().ok()?;
    let minute: i64 = utc[14..16].parse().ok()?;
    let second: i64 = utc[17..19].parse().ok()?;
    let millis: i64 = match utc.get(20..) {
        Some(f) if !f.is_empty() => {
            let padded = format!("{:0<3}", f);
            padded[..3].parse().ok()?
        }
        _ => 0,
    };
    let days = days_from_civil(year, month, day)?;
    let secs = days * 86_400 + hour * 3_600 + minute * 60 + second;
    Some(secs * 1_000 + millis)
}

// ---- create ----

/// after 规则。
pub fn create_after_record(
    id: &str,
    prompt: &str,
    after_seconds: u64,
    now: i64,
) -> Result<ScheduleRecordData, ScheduleInputError> {
    let normalized = prompt.trim();
    if normalized.is_empty() {
        return Err(ScheduleInputError {
            code: InputCode::InvalidPrompt,
            message: "prompt must be non-empty after trimming.".into(),
        });
    }
    if after_seconds == 0 {
        return Err(ScheduleInputError {
            code: InputCode::InvalidRule,
            message: "after_seconds must be a positive safe integer.".into(),
        });
    }
    let target = now + (after_seconds as i64) * 1_000;
    let scheduled_at = future_instant(target, now)?;
    Ok(ScheduleRecordData {
        id: id.to_string(),
        kind: "after".into(),
        prompt: normalized.to_string(),
        after_seconds: Some(after_seconds),
        every_seconds: None,
        scheduled_at,
    })
}

/// at（带显式数值偏移的字符串）。
pub fn create_at_record_from_offset(
    id: &str,
    prompt: &str,
    at: &str,
    now: i64,
) -> Result<ScheduleRecordData, ScheduleInputError> {
    let normalized = prompt.trim();
    if normalized.is_empty() {
        return Err(ScheduleInputError {
            code: InputCode::InvalidPrompt,
            message: "prompt must be non-empty after trimming.".into(),
        });
    }
    let target = parse_offset_instant(at)?;
    let scheduled_at = future_instant(target, now)?;
    Ok(ScheduleRecordData {
        id: id.to_string(),
        kind: "at".into(),
        prompt: normalized.to_string(),
        after_seconds: None,
        every_seconds: None,
        scheduled_at,
    })
}

/// every 规则。
pub fn create_every_record(
    id: &str,
    prompt: &str,
    every_seconds: u64,
    now: i64,
) -> Result<ScheduleRecordData, ScheduleInputError> {
    let normalized = prompt.trim();
    if normalized.is_empty() {
        return Err(ScheduleInputError {
            code: InputCode::InvalidPrompt,
            message: "prompt must be non-empty after trimming.".into(),
        });
    }
    if every_seconds < MIN_EVERY_INTERVAL_SECONDS {
        return Err(ScheduleInputError {
            code: InputCode::FrequencyTooHigh,
            message: format!("every_seconds must be at least {MIN_EVERY_INTERVAL_SECONDS}."),
        });
    }
    let target = now + (every_seconds as i64) * 1_000;
    let scheduled_at = future_instant(target, now)?;
    Ok(ScheduleRecordData {
        id: id.to_string(),
        kind: "every".into(),
        prompt: normalized.to_string(),
        after_seconds: None,
        every_seconds: Some(every_seconds),
        scheduled_at,
    })
}

/// 时区规范化。M4 范围：仅 UTC（IANA 延迟；chrono-tz 离线不可用）。
pub fn canonicalize_time_zone(value: &str) -> Result<String, ScheduleInputError> {
    if value.trim() != value || (value != "UTC" && value != "GMT") {
        return Err(ScheduleInputError {
            code: InputCode::InvalidTimeZone,
            message: "time_zone must be UTC (IANA local zones are deferred in M4).".into(),
        });
    }
    Ok(if value == "GMT" { "UTC".to_string() } else { value.to_string() })
}

// ---- every occurrence + view ----

/// 解析一个固定速率决策：最新锚定 occurrence + 首个严格未来目标。
pub fn resolve_every_occurrence(
    scheduled_at: &str,
    every_seconds: u64,
    accepted_at: &str,
) -> Result<EveryOccurrence, LogError> {
    let target = utc_instant_to_epoch(scheduled_at)
        .ok_or_else(|| LogError("every scheduledAt must be a canonical UTC instant".into()))?;
    let accepted = utc_instant_to_epoch(accepted_at)
        .ok_or_else(|| LogError("every acceptedAt must be a canonical UTC instant".into()))?;
    let interval = (every_seconds as i64) * 1_000;
    if interval <= 0 {
        return Err(LogError("every interval milliseconds must be a positive safe integer".into()));
    }
    if accepted < target {
        return Err(LogError("every dispatch cannot precede the active scheduledAt".into()));
    }
    let steps = (accepted - target) / interval;
    let occurrence = target + steps * interval;
    if occurrence < target || occurrence > accepted {
        return Err(LogError("every occurrence arithmetic must stay within the accepted interval".into()));
    }
    let occurrence_at = epoch_to_utc_instant(occurrence)
        .ok_or_else(|| LogError("every occurrence must be representable".into()))?;
    let next = occurrence + interval;
    let next_scheduled_at = if next > MAX_YEAR_MS {
        None
    } else {
        Some(
            epoch_to_utc_instant(next)
                .ok_or_else(|| LogError("every next must be representable".into()))?,
        )
    };
    Ok(EveryOccurrence { occurrence_at, next_scheduled_at })
}

/// 一个固定速率决策结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EveryOccurrence {
    pub occurrence_at: String,
    pub next_scheduled_at: Option<String>,
}

/// 时间视图：据真实 now 判定 overdue/scheduled，deliveryMode 固定 session-local。
pub fn schedule_view(
    id: &str,
    kind: &str,
    prompt: &str,
    scheduled_at: &str,
    after_seconds: u64,
    every_seconds: u64,
    now: i64,
) -> Value {
    let overdue = utc_instant_to_epoch(scheduled_at).is_some_and(|e| now >= e);
    json!({
        "id": id,
        "kind": kind,
        "prompt": prompt,
        "scheduledAt": scheduled_at,
        "afterSeconds": after_seconds,
        "everySeconds": every_seconds,
        "state": if overdue { "overdue" } else { "scheduled" },
        "deliveryMode": "session-local",
    })
}

// 保留 ScheduleViewData 导出（供 host 用 when 真实 now）。
impl ScheduleViewData {
    pub fn derive(rec: &ScheduleRecordData, now: i64) -> Self {
        let overdue = utc_instant_to_epoch(&rec.scheduled_at).is_some_and(|e| now >= e);
        ScheduleViewData {
            id: rec.id.clone(),
            kind: rec.kind.clone(),
            prompt: rec.prompt.clone(),
            scheduled_at: rec.scheduled_at.clone(),
            after_seconds: rec.after_seconds,
            every_seconds: rec.every_seconds,
            state: if overdue { "overdue".to_string() } else { "scheduled".to_string() },
            delivery_mode: "session-local".to_string(),
        }
    }
}

//! lossless-JSON 跨界（M5-DESIGN §7.3 Rust 端）：serde_json `Number` 保持；
//! 检查非有限/-0 → `invalid-output`；`checkDoneValue` 语义的预算准入（over-budget →
//! `output-limit` 分类）。

use serde_json::Value;

/// 准入分类（checkDoneValue 语义）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionError {
    InvalidOutput,
    OutputLimit,
}

/// 值是否 lossless：非有限/负零拒绝。
pub fn validate_lossless_json(v: &Value) -> Result<(), String> {
    match v {
        Value::Null | Value::Bool(_) | Value::String(_) => Ok(()),
        Value::Number(n) => {
            if let Some(f) = n.as_f64() {
                if !f.is_finite() {
                    return Err(format!("non-finite number {f}"));
                }
                if f == 0.0 && f.is_sign_negative() {
                    return Err("negative zero -0.0".to_string());
                }
            }
            Ok(())
        }
        Value::Array(items) => {
            for item in items {
                validate_lossless_json(item)?;
            }
            Ok(())
        }
        Value::Object(map) => {
            for value in map.values() {
                validate_lossless_json(value)?;
            }
            Ok(())
        }
    }
}

/// 解析一行 JSON（数字 token 保持）；`NaN`/`Infinity`/`-Infinity`/畸形 → Err。
/// 注意：serde_json 拒绝 NaN/Inf 字面量（→ Err），`-0.0` 能解析（数值保持负零，
/// 由 `validate_lossless_json` 拒绝）。
pub fn parse_lossless_json(s: &str) -> Result<Value, String> {
    serde_json::from_str(s).map_err(|e| e.to_string())
}

/// 预算准入：非 lossless → `InvalidOutput`；序列化字节 > 剩余 → `OutputLimit`。
pub fn classify_admission(v: &Value, remaining_bytes: usize) -> Result<(), AdmissionError> {
    validate_lossless_json(v).map_err(|_| AdmissionError::InvalidOutput)?;
    let bytes = serde_json::to_string(v)
        .map(|s| s.len())
        .unwrap_or(usize::MAX);
    if bytes > remaining_bytes {
        return Err(AdmissionError::OutputLimit);
    }
    Ok(())
}

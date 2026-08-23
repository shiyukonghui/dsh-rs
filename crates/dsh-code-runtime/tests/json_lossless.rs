//! dsh-code-runtime：lossless-JSON 跨界校验（M5-DESIGN §7.3 检查非有限/-0 → invalid-output）。

use dsh_code_runtime::json::{classify_admission, parse_lossless_json, validate_lossless_json};
use serde_json::{json, Value};

#[test]
fn valid_lossless_values_pass() {
    assert!(validate_lossless_json(&json!(null)).is_ok());
    assert!(validate_lossless_json(&json!(true)).is_ok());
    assert!(validate_lossless_json(&json!("héllo")).is_ok());
    assert!(validate_lossless_json(&json!(1234567890123456i64)).is_ok());
    assert!(validate_lossless_json(&json!(2_i64.pow(60))).is_ok());
    assert!(validate_lossless_json(&json!(1.5)).is_ok());
    assert!(validate_lossless_json(&json!([1, "a", null, { "k": [true] }])).is_ok());
}

#[test]
fn negative_zero_is_rejected() {
    let v = Value::from(-0.0_f64);
    assert!(validate_lossless_json(&v).is_err(), "-0.0 → invalid-output");
    let parsed = parse_lossless_json("-0.0").expect("-0.0 parses");
    assert!(validate_lossless_json(&parsed).is_err());
}

#[test]
fn non_finite_is_unreachable_via_serde_json_number() {
    // serde_json `Number` 无法承载非有限（from_f64(NaN/Inf) → None → Null），
    // 故 validate 层的非有限检查是防层不变式；真正防线在 parse 层拒绝字面量。
    assert_eq!(Value::from(f64::NAN), Value::Null);
    assert_eq!(Value::from(f64::INFINITY), Value::Null);
}

#[test]
fn raw_nan_token_fails_parse() {
    assert!(parse_lossless_json("NaN").is_err());
    assert!(parse_lossless_json("Infinity").is_err());
    assert!(parse_lossless_json("-Infinity").is_err());
}

#[test]
fn admit_negative_zero_classifies_invalid_output() {
    let parsed = parse_lossless_json("-0.0").expect("parses");
    assert!(matches!(
        classify_admission(&parsed, 1000),
        Err(AdmissionError::InvalidOutput)
    ));
}

#[test]
fn big_integers_survive_exactly() {
    let v = parse_lossless_json("1152921504606846976").expect("2^60 parses"); // 2^60
    assert_eq!(v.as_u64(), Some(1152921504606846976), "整数精确跨界");
}

#[test]
fn admission_classifies_output_limit_and_invalid() {
    // 空预算：任何输出都 over-budget
    assert!(matches!(
        classify_admission(&json!("hello"), 0),
        Err(AdmissionError::OutputLimit)
    ));
    assert!(matches!(classify_admission(&json!("hello"), 1000), Ok(())));
}

// 测试用谓词导入
use dsh_code_runtime::json::AdmissionError;

//! `dsh-workflow` meta 校验 —— 对齐 `packages/workflow/workflow-worker-thread/src/meta.ts`
//! `validateMeta`（逐 violation 列出，code=META_INVALID，返回规范化副本）。

use serde_json::{json, Value};

use crate::error::{WorkflowError, WorkflowErrorCode};

/// 一条 meta 校验违规（TS 用 message 数组；本实现给每条一个稳定字段名便于排序断言）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetaViolation {
    /// 违规所属路径（如 `meta.name`、`meta.phases[0].title`）。
    pub path: String,
    /// 人读的消息（对齐 TS 文案）。
    pub message: String,
}

fn is_record(v: &Value) -> bool {
    v.is_object()
}

/// 检查一次 meta 值，收集全部 shape 违规；无违规则返回规范化副本。
fn validate_meta_shape(value: &Value) -> Result<Value, Vec<MetaViolation>> {
    let mut violations: Vec<MetaViolation> = Vec::new();
    if !is_record(value) {
        return Err(vec![MetaViolation {
            path: "meta".into(),
            message: "meta must be an object".into(),
        }]);
    }
    let record = value.as_object().expect("is_object");
    let known = ["name", "description", "whenToUse", "phases"];
    for key in record.keys() {
        if !known.contains(&key.as_str()) {
            violations.push(MetaViolation {
                path: format!("meta.{key}"),
                message: format!("meta.{key} is not a recognized field (name/description/whenToUse/phases)"),
            });
        }
    }
    check_string(record, "name", true, &mut violations);
    check_string(record, "description", true, &mut violations);
    check_string(record, "whenToUse", false, &mut violations);

    let mut phases_out: Vec<Value> = Vec::new();
    match record.get("phases") {
        None => {}
        Some(phases) => {
            if !phases.is_array() {
                violations.push(MetaViolation {
                    path: "meta.phases".into(),
                    message: "meta.phases must be an array".into(),
                });
            } else {
                for (index, phase) in phases.as_array().expect("is_array").iter().enumerate() {
                    if !is_record(phase) {
                        violations.push(MetaViolation {
                            path: format!("meta.phases[{index}]"),
                            message: format!("meta.phases[{index}] must be an object"),
                        });
                        continue;
                    }
                    let entry = phase.as_object().expect("is_object");
                    let phase_known = ["title", "detail", "provider", "model"];
                    for key in entry.keys() {
                        if !phase_known.contains(&key.as_str()) {
                            violations.push(MetaViolation {
                                path: format!("meta.phases[{index}].{key}"),
                                message: format!("meta.phases[{index}].{key} is not a recognized field"),
                            });
                        }
                    }
                    check_phase_string(entry, "title", true, index, &mut violations);
                    check_phase_string(entry, "detail", false, index, &mut violations);
                    check_phase_string(entry, "provider", false, index, &mut violations);
                    check_phase_string(entry, "model", false, index, &mut violations);
                    if !violations.iter().any(|v| v.path.starts_with(&format!("meta.phases[{index}]"))) {
                        phases_out.push(json!({
                            "title": entry["title"].as_str().expect("checked"),
                            "detail": optional_str(entry, "detail"),
                            "provider": optional_str(entry, "provider"),
                            "model": optional_str(entry, "model"),
                        }));
                    }
                }
            }
        }
    }

    if !violations.is_empty() {
        return Err(violations);
    }
    let mut out = serde_json::Map::new();
    out.insert("name".into(), record["name"].clone());
    out.insert("description".into(), record["description"].clone());
    if let Some(w) = optional_str(record, "whenToUse") {
        out.insert("whenToUse".into(), Value::String(w));
    }
    if let Some(phases) = record.get("phases") {
        if phases.is_array() {
            out.insert("phases".into(), Value::Array(phases_out));
        }
    }
    Ok(Value::Object(out))
}

fn check_string(
    record: &serde_json::Map<String, Value>,
    key: &str,
    required: bool,
    violations: &mut Vec<MetaViolation>,
) {
    match record.get(key) {
        None => {
            if required {
                violations.push(MetaViolation {
                    path: format!("meta.{key}"),
                    message: format!("meta.{key} must be a non-empty string"),
                });
            }
        }
        Some(v) => {
            let ok = v.as_str().is_some_and(|s| !s.is_empty());
            if !ok {
                violations.push(MetaViolation {
                    path: format!("meta.{key}"),
                    message: format!("meta.{key} must be a non-empty string"),
                });
            }
        }
    }
}

fn check_phase_string(
    record: &serde_json::Map<String, Value>,
    key: &str,
    required: bool,
    index: usize,
    violations: &mut Vec<MetaViolation>,
) {
    let path = format!("meta.phases[{index}]");
    match record.get(key) {
        None => {
            if required {
                violations.push(MetaViolation {
                    path: format!("{path}.{key}"),
                    message: format!("{path}.{key} must be a non-empty string"),
                });
            }
        }
        Some(v) => {
            let ok = v.as_str().is_some_and(|s| !s.is_empty());
            if !ok {
                violations.push(MetaViolation {
                    path: format!("{path}.{key}"),
                    message: format!("{path}.{key} must be a non-empty string"),
                });
            }
        }
    }
}

fn optional_str(record: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    record.get(key).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(|s| s.to_string())
}

/// 校验 meta：违规全列（code=META_INVALID）；无违规返回规范化副本。
pub fn validate_meta(value: &Value) -> Result<Value, WorkflowError> {
    match validate_meta_shape(value) {
        Ok(meta) => Ok(meta),
        Err(violations) => {
            let message = violations
                .iter()
                .map(|v| v.message.as_str())
                .collect::<Vec<_>>()
                .join("; ");
            Err(WorkflowError {
                code: WorkflowErrorCode::MetaInvalid,
                message: format!("invalid meta: {message}"),
                fatal: true,
                violations,
            })
        }
    }
}

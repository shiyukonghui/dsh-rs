//! L3（D-105 compaction 档位 3）：comaction 的**接口预留**。
//!
//! `dsh-compaction-tool-result-pruner` 行的 config 语义（`thresholdChars` /
//! `headChars` / `tailChars`）在此定型为可解析、可校验的规范类型——契约形状固定，
//! **行为明确不实现**（不接 `dsh-agent-loop::tool_calls::append_tool_result`、
//! 不裁剪结果）。未来落地只消费本规格，不重新猜测语义。
//!
//! 保留的契约（对齐预设传达的意图）：
//! - 结果文本长度超过 `threshold_chars` 时，保留 `head_chars` 前缀 + `tail_chars`
//!   后缀，其余省略；
//! - 不变量：`head_chars > 0 && tail_chars > 0 && head_chars + tail_chars <
//!   threshold_chars`（裁剪产物应短于阈值才不空耗）。违反 → 解析错误（fail-loud，
//!   不打折接受）。

use serde_json::Value;

/// `dsh-compaction-tool-result-pruner` 的保留契约（L3 接口预留，无行为）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolResultPrunerSpec {
    pub threshold_chars: u64,
    pub head_chars: u64,
    pub tail_chars: u64,
}

impl ToolResultPrunerSpec {
    /// 从行的 config 解析：字段齐且校验通过 → `Ok(Some)`；无这些字段（该行未被该
    /// 插件配置）→ `Ok(None)`；字段畸形/违反不变量 → `Err`（fail-loud）。
    pub fn from_config(config: &Value) -> Result<Option<Self>, String> {
        let obj = match config.as_object() {
            Some(o) => o,
            None => return Ok(None),
        };
        let Some(threshold) = obj.get("thresholdChars") else {
            return Ok(None);
        };
        let head = obj.get("headChars").ok_or_else(|| {
            "tool-result-pruner: headChars missing (thresholdChars present)".to_string()
        })?;
        let tail = obj.get("tailChars").ok_or_else(|| {
            "tool-result-pruner: tailChars missing (thresholdChars present)".to_string()
        })?;
        let pick = |v: &Value, name: &str| -> Result<u64, String> {
            v.as_u64().ok_or_else(|| {
                format!("tool-result-pruner: {name} must be a non-negative integer")
            })
        };
        let spec = ToolResultPrunerSpec {
            threshold_chars: pick(threshold, "thresholdChars")?,
            head_chars: pick(head, "headChars")?,
            tail_chars: pick(tail, "tailChars")?,
        };
        spec.validate()?;
        Ok(Some(spec))
    }

    /// 校验保留不变量（见模块文档）。失败 → `Err`（fail-loud，不打折接受）。
    pub fn validate(&self) -> Result<(), String> {
        if self.head_chars == 0 || self.tail_chars == 0 {
            return Err("tool-result-pruner: headChars/tailChars must both be > 0".to_string());
        }
        if self.head_chars + self.tail_chars >= self.threshold_chars {
            return Err(format!(
                "tool-result-pruner: head({})+tail({}) must be < threshold({})",
                self.head_chars, self.tail_chars, self.threshold_chars
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn real_preset_pruner_config_parses() {
        // standard/code preset `dsh-compaction-tool-result-pruner` 行 config（真实值）。
        let spec = ToolResultPrunerSpec::from_config(&json!({
            "thresholdChars": 8192, "headChars": 4096, "tailChars": 1024
        }))
        .unwrap()
        .expect("real preset config yields a spec");
        assert_eq!(
            (spec.threshold_chars, spec.head_chars, spec.tail_chars),
            (8192, 4096, 1024)
        );
        assert!(spec.validate().is_ok(), "real values satisfy invariants");
    }

    #[test]
    fn absent_config_is_none_partial_or_invalid_is_err() {
        assert!(
            ToolResultPrunerSpec::from_config(&json!({})).unwrap().is_none(),
            "no pruner fields -> None"
        );
        assert!(
            ToolResultPrunerSpec::from_config(&serde_json::Value::Null)
                .unwrap()
                .is_none(),
            "null config -> None"
        );
        assert!(
            ToolResultPrunerSpec::from_config(&json!({"thresholdChars": 100})).is_err(),
            "threshold without head/tail -> Err (fail-loud)"
        );
        assert!(
            ToolResultPrunerSpec::from_config(&json!({
                "thresholdChars": 10, "headChars": 20, "tailChars": 1
            }))
            .is_err(),
            "head+tail >= threshold -> Err"
        );
        assert!(
            ToolResultPrunerSpec::from_config(&json!({
                "thresholdChars": 10, "headChars": 0, "tailChars": 1
            }))
            .is_err(),
            "zero head -> Err"
        );
        assert!(
            ToolResultPrunerSpec::from_config(&json!({
                "thresholdChars": -1, "headChars": 1, "tailChars": 1
            }))
            .is_err(),
            "negative threshold -> Err"
        );
    }
}

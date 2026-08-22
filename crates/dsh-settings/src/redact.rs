//! Secret 位置枚举与值剥离（对齐 `@deepseek-ai/dsh-settings/redact`）。
//!
//! `role('secret')` 字段在跨 wire 前从 value 移除；sidecar 记录每个 schema 声明的
//! 保密位置与当前是否持值。object 属性总是枚举 slot（即使未设置，表单需知道槽位
//! 存在）；dict/array 只在 value 拥有该位置时枚举。输入不被修改。
//!
//! 注意：TS 判定 `node.meta?.role === 'secret'`——role 在 `Schema.meta` 而非
//! kind 上，所以这里全程以 `&SchemaRef` 走（可读 meta）。TS 的 `undefined` 哨兵
//! 语义在 Rust 侧表现为 `None`：值缺失（且非容器）输出「不呈现该键」。

use dsh_schema::{SchemaKind, SchemaRef};
use serde_json::Value;

/// 一个 schema 声明的保密槽位。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretSlot {
    pub path: Vec<String>,
    pub set: bool,
}

/// `role('secret')` 判定（对齐 TS `node.meta?.role === 'secret'`）。
fn is_secret(schema: &SchemaRef) -> bool {
    schema.meta.role.as_deref() == Some("secret")
}

/// redact 一个值：返回 `(剥离后不一定有值, 枚举的保密槽位)`。
/// 顶层值恒不缺席（调用者传的值即存在）。
pub fn walk_redact(schema: &SchemaRef, value: &Value) -> (Value, Vec<SecretSlot>) {
    let mut secrets = Vec::new();
    let stripped = walk(schema, Some(value), &[], &mut secrets);
    (stripped.unwrap_or(Value::Null), secrets)
}

/// 返回 `Some(重建值)` 或 `None`（该位置应缺席——对齐 TS 的 `undefined`）。
fn walk(
    schema: &SchemaRef,
    value: Option<&Value>,
    path: &[String],
    secrets: &mut Vec<SecretSlot>,
) -> Option<Value> {
    // 任何节点标了 secret role → 整值剥离（TS 同：walk 开头检查）。
    if is_secret(schema) {
        secrets.push(SecretSlot {
            path: path.to_vec(),
            set: value.is_some(),
        });
        return None;
    }
    match &schema.kind {
        SchemaKind::Object(dict) => {
            let source = value.and_then(|v| v.as_object());
            let mut rebuilt = serde_json::Map::new();
            if let Some(src) = source {
                // 未声明到 schema 的额外键保留。
                for (key, entry) in src {
                    if !dict.contains_key(key) {
                        rebuilt.insert(key.clone(), entry.clone());
                    }
                }
            }
            for (key, child) in dict {
                let mut child_path = path.to_vec();
                child_path.push(key.clone());
                let child_value = source.and_then(|s| s.get(key));
                let stripped = walk(child, child_value, &child_path, secrets);
                if let Some(stripped) = stripped {
                    rebuilt.insert(key.clone(), stripped);
                }
            }
            if source.is_none() && rebuilt.is_empty() {
                // 值缺席且重建为空 → 整节点缺席（TS 返回 value = undefined）。
                None
            } else {
                Some(Value::Object(rebuilt))
            }
        }
        SchemaKind::Dict { inner, .. } => {
            let Some(obj) = value.and_then(|v| v.as_object()) else {
                // 非对象值（含缺席）：TS `if (!isRecord(value)) return value`。
                return value.cloned();
            };
            let mut rebuilt = serde_json::Map::new();
            for (key, entry) in obj {
                let mut child_path = path.to_vec();
                child_path.push(key.clone());
                let stripped = walk(inner, Some(entry), &child_path, secrets);
                if let Some(stripped) = stripped {
                    rebuilt.insert(key.clone(), stripped);
                }
            }
            Some(Value::Object(rebuilt))
        }
        SchemaKind::Array(inner) => {
            let Some(arr) = value.and_then(|v| v.as_array()) else {
                return value.cloned();
            };
            let mut rebuilt = Vec::with_capacity(arr.len());
            for (index, entry) in arr.iter().enumerate() {
                let mut child_path = path.to_vec();
                child_path.push(index.to_string());
                let stripped = walk(inner, Some(entry), &child_path, secrets);
                if let Some(stripped) = stripped {
                    rebuilt.push(stripped);
                }
            }
            Some(Value::Array(rebuilt))
        }
        _ => value.cloned(),
        // any/never/const/string/number/boolean/tuple/union/intersect/is/bitset/
        // function/transform/lazy/custom：非容器，值缺席 → None（不呈现）。
    }
}

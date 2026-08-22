//! section 编辑原语（对齐 `@deepseek-ai/dsh-settings` 的 applyPathOp / 路径语义）。
//!
//! mutate 的 ops 以**写到达时**的 section 为准应用（M3b 同步单线程，即当前
//! section）；path 为 key 段数组，空路径指向 section 根。

use serde_json::Value;

/// 应用一组 path ops（`[{op:'set'|'unset', path:[...], value?}]`）。
/// 顺序应用；set 创建中间对象；set 根/字段写 JSON 值；unset 删除字段。
/// 返回下一个 section；非法 op 返回错误消息。
pub fn apply_path_ops(section: &Value, ops: &Value) -> Result<Value, String> {
    let array = ops
        .as_array()
        .ok_or_else(|| "settings mutate ops must be an array".to_string())?;
    let mut current = section.clone();
    for op in array {
        let op_obj = op
            .as_object()
            .ok_or_else(|| "settings mutate ops must be objects".to_string())?;
        let kind = op_obj
            .get("op")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "op must have a string op field".to_string())?;
        let path: Vec<String> = op_obj
            .get("path")
            .and_then(|v| v.as_array())
            .ok_or_else(|| "op path must be an array".to_string())?
            .iter()
            .map(|p| {
                p.as_str()
                    .map(|s| s.to_string())
                    .ok_or_else(|| "op path parts must be strings".to_string())
            })
            .collect::<Result<Vec<String>, String>>()?;
        match kind {
            "set" => {
                let value = op_obj
                    .get("value")
                    .ok_or_else(|| "set op requires value".to_string())?;
                current = set_at(&current, &path, value)?;
            }
            "unset" => {
                current = unset_at(&current, &path);
            }
            other => return Err(format!("unknown op {other:?}")),
        }
    }
    Ok(current)
}

/// 在 `path` 写入 `value`（创建中间对象；空路径替换根，需 plain object）。
fn set_at(root: &Value, path: &[String], value: &Value) -> Result<Value, String> {
    if path.is_empty() {
        if value.is_object() {
            return Ok(value.clone());
        }
        return Err("settings mutate: setting the section root requires a plain object".to_string());
    }
    set_recursive(root, path, value)
}

fn set_recursive(root: &Value, path: &[String], value: &Value) -> Result<Value, String> {
    let (head, rest) = path
        .split_first()
        .ok_or_else(|| "empty path".to_string())?;
    let mut obj = root
        .as_object()
        .cloned()
        .unwrap_or_default();
    if rest.is_empty() {
        obj.insert(head.clone(), value.clone());
        return Ok(Value::Object(obj));
    }
    let child = obj.get(head).cloned().unwrap_or_else(|| Value::Object(Default::default()));
    let next = set_recursive(&child, rest, value)?;
    obj.insert(head.clone(), next);
    Ok(Value::Object(obj))
}

/// 在 `path` 删除字段（路径不存在/途经非对象 → no-op；空路径 → 全清）。
fn unset_at(root: &Value, path: &[String]) -> Value {
    if path.is_empty() {
        return Value::Object(Default::default());
    }
    unset_path(root, path)
}

fn unset_path(root: &Value, path: &[String]) -> Value {
    let Some((head, rest)) = path.split_first() else {
        return Value::Object(Default::default());
    };
    let Value::Object(map) = root else {
        return root.clone();
    };
    if rest.is_empty() {
        let mut next = map.clone();
        next.remove(head);
        return Value::Object(next);
    }
    match map.get(head) {
        Some(child) if child.is_object() => {
            let mut next = map.clone();
            next.insert(head.clone(), unset_path(child, rest));
            Value::Object(next)
        }
        _ => root.clone(),
    }
}

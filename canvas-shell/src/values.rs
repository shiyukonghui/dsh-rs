//! 值/数据面纯函数（core.js 179–277 行移植）：collectValues / rpcEnvelope /
//! pollDecision / extractPath / listRows / statusItems / rowActionBody / needsConfirm。

use serde_json::{json, Value};

/// 收集表单值（read 返回原始字符串；list JSON.parse 失败 → Err{field,message} 动作
/// 不得发出；number→数值；write-only 秘密空值=不改动 D-204）。
pub fn collect_values<F>(view: &Value, read: F) -> Result<Value, (String, String)>
where
    F: Fn(&str) -> Option<String>,
{
    let mut map = serde_json::Map::new();
    let fields = view.get("fields").and_then(Value::as_array).cloned().unwrap_or_default();
    for f in &fields {
        let name = f.get("name").and_then(Value::as_str).unwrap_or("").to_string();
        let raw = read(&name).unwrap_or_default();
        let ty = f.get("type").and_then(Value::as_str).unwrap_or("");
        if ty == "list" {
            let src = if raw.is_empty() { "[]" } else { &raw };
            match serde_json::from_str::<Value>(src) {
                Ok(v) => {
                    map.insert(name, v);
                }
                Err(e) => {
                    let msg = format!("字段 {} 不是合法 JSON: {}", name, e);
                    return Err((name, msg));
                }
            }
        } else if ty == "number" {
            // JS Number(raw) 经 JSON.stringify 上线：42.0 → "42"（整数形态保持）；
            // 非数 → NaN → 线上 null。移植须一致：可整则 i64，否则 f64，NaN 落 Null。
            let n: f64 = raw.trim().parse().unwrap_or(f64::NAN);
            let v = if n.fract() == 0.0 && n.abs() < 9.223372036854776e18 {
                json!(n as i64)
            } else {
                Value::from(n)
            };
            map.insert(name, v);
        } else if ty == "checkbox" {
            map.insert(name, json!(raw == "true"));
        } else if f.get("secretWriteOnly").and_then(Value::as_bool) == Some(true) {
            if !raw.is_empty() {
                map.insert(name, json!(raw));
            }
        } else {
            map.insert(name, json!(raw));
        }
    }
    Ok(Value::Object(map))
}

/// client-request 信封（历史教训 D-184：裸 {args} 经真实 HTTP 必 400）。
pub fn rpc_envelope(method: &str, args: Option<Value>, rpc_id: &str) -> Value {
    json!({
        "type": "client-request",
        "rpcId": rpc_id,
        "method": method,
        "payload": { "args": args.unwrap_or(json!({})) }
    })
}

/// 轮询决策：unchanged→keep；rev 变→整模型替换。
pub fn poll_decision(value: Option<&Value>) -> Value {
    match value {
        Some(v) if v.get("unchanged").and_then(Value::as_bool) != Some(true) => json!({
            "action": "replace",
            "rev": v.get("rev").cloned().unwrap_or(Value::Null),
            "cards": v.get("cards").cloned().unwrap_or(Value::Null),
        }),
        _ => json!({ "action": "keep" }),
    }
}

/// 点路径提取；任何一段不是对象 → None。
pub fn extract_path(obj: &Value, dotted: &str) -> Option<Value> {
    if !obj.is_object() || dotted.is_empty() {
        return None;
    }
    let mut cur = obj.clone();
    for seg in dotted.split('.') {
        if !cur.is_object() {
            return None;
        }
        cur = cur.get(seg)?.clone();
    }
    Some(cur)
}

/// list 行提取（dataRpc[rowsPath] > 静态 rows > 诚实空；非数组视同无数据——绝不拼行）。
pub fn list_rows(view: &Value, data_value: &Value) -> Value {
    let from_data = extract_path(data_value, view.get("rowsPath").and_then(Value::as_str).unwrap_or(""))
        .filter(|v| v.is_array());
    let rows = from_data.or_else(|| view.get("rows").and_then(Value::as_array).cloned().map(Value::Array)).unwrap_or(json!([]));
    json!({
        "rows": rows,
        "columns": view.get("columns").filter(|c| c.is_array()).cloned().unwrap_or(json!([])),
        "emptyText": view.get("emptyText").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or("暂无条目"),
    })
}

/// status 项提取（dataRpc.items > 静态 items > 诚实空）。
pub fn status_items(view: &Value, data_value: &Value) -> Vec<Value> {
    extract_path(data_value, "items")
        .filter(|v| v.is_array())
        .or_else(|| view.get("items").and_then(Value::as_array).cloned().map(Value::Array))
        .as_ref()
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// 行动作线形状：整行入 row；action.args（对象）并入顶层（D-198）。
pub fn row_action_body(row: &Value, action: Option<&Value>) -> Value {
    let mut body = json!({ "row": row });
    if let Some(extra) = action.and_then(|a| a.get("args")).filter(|x| x.is_object()) {
        if let (Some(obj), Some(src)) = (body.as_object_mut(), extra.as_object()) {
            for (k, v) in src {
                obj.insert(k.clone(), v.clone());
            }
        }
    }
    body
}

/// confirm 只认严格 true（缺省/其他值 = 直接执行，向后兼容）。
pub fn needs_confirm(action: &Value) -> bool {
    action.get("confirm").and_then(Value::as_bool) == Some(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_values_types_and_secret_gate() {
        let view = json!({"fields":[
            {"name":"model","type":"text"},
            {"name":"max","type":"number"},
            {"name":"mods","type":"list"},
            {"name":"key","type":"text","secretWriteOnly":true}
        ]});
        let vals = collect_values(&view, |n| match n {
            "model" => Some("deepseek-chat".into()),
            "max" => Some("42".into()),
            "mods" => Some("[1,2]".into()),
            "key" => Some("".into()), // 空秘密 = 不改动
            _ => None,
        })
        .unwrap();
        assert_eq!(vals["model"], "deepseek-chat");
        assert_eq!(vals["max"], 42);
        assert_eq!(vals["mods"], json!([1, 2]));
        assert!(vals.get("key").is_none(), "空 write-only 秘密绝不出现在 patch");
        // 非法 list → Err（动作不得发出）
        assert!(collect_values(&json!({"fields":[{"name":"m","type":"list"}]}), |_| Some("{oops".into())).is_err());
        // list 空文本 = []（JS raw||"[]"）
        let ok = collect_values(&json!({"fields":[{"name":"m","type":"list"}]}), |_| Some("".into())).unwrap();
        assert_eq!(ok["m"], json!([]));
    }

    #[test]
    fn envelope_and_poll_decision() {
        let env = rpc_envelope("session/list", Some(json!({"a":1})), "r7");
        assert_eq!(env["type"], "client-request");
        assert_eq!(env["payload"]["args"]["a"], 1);
        assert_eq!(rpc_envelope("x", None, "r")["payload"]["args"], json!({}));
        assert_eq!(poll_decision(Some(&json!({"rev":"2","unchanged":true})))["action"], "keep");
        assert_eq!(poll_decision(None)["action"], "keep");
        let r = poll_decision(Some(&json!({"rev":"3","cards":[1,2]})));
        assert_eq!(r["action"], "replace");
        assert_eq!(r["rev"], "3");
        assert_eq!(r["cards"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn paths_rows_status_never_fabricate() {
        let d = json!({"items":[{"n":1}],"deep":{"list":[{"x":1}]}});
        assert_eq!(extract_path(&d, "deep.list").unwrap().as_array().unwrap().len(), 1);
        assert!(extract_path(&d, "items.0").is_none(), "数组段不是对象→None（不索引数组）");
        assert!(extract_path(&json!("str"), "x").is_none());
        let rows = list_rows(&json!({"rowsPath":"deep.list","columns":["x"]}), &d);
        assert_eq!(rows["rows"].as_array().unwrap().len(), 1);
        assert_eq!(rows["emptyText"], "暂无条目");
        // 非数组 rowsPath 值 → 诚实空（绝不拼行）
        let bad = list_rows(&json!({"rowsPath":"items"}), &json!({"items":"oops"}));
        assert_eq!(bad["rows"], json!([]));
        // 静态回退
        let st = list_rows(&json!({"rows":[{"a":1}]}), &json!({}));
        assert_eq!(st["rows"].as_array().unwrap().len(), 1);
        assert_eq!(status_items(&json!({"items":[{"k":1}]}), &json!({})).len(), 1);
        assert_eq!(status_items(&json!({}), &d).len(), 1);
    }

    #[test]
    fn row_action_shape_and_confirm_strict() {
        let b = row_action_body(&json!({"id":9,"s":"x"}), Some(&json!({"name":"decide","args":{"decision":"approve"}})));
        assert_eq!(b["row"]["id"], 9);
        assert_eq!(b["decision"], "approve", "args 并入顶层（D-198）");
        assert_eq!(row_action_body(&json!({}), None)["row"], json!({}));
        assert!(needs_confirm(&json!({"confirm":true})));
        assert!(!needs_confirm(&json!({"confirm":"true"})), "只认严格 true");
        assert!(!needs_confirm(&json!({})));
    }
}

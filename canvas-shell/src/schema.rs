//! schemaFields（D-208 真实 wire refs 表形的 Rust 移植）。夹具 = live 抓样形
//! （dsh-schema refs 序列化），断言逐条移植自 core.test.mjs。

use serde_json::{json, Value};

fn deref(refs: &Value, id: &Value) -> Value {
    let key = match id {
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        _ => return Value::Null,
    };
    match refs.get(&key) {
        Some(v) if v.is_object() => v.clone(),
        _ => Value::Null,
    }
}

pub fn schema_fields(ns_view: &Value) -> Value {
    let empty = json!({});
    let v = if ns_view.is_object() { ns_view } else { &empty };
    let schema = v.get("schema").filter(|s| s.is_object());
    let refs = schema.and_then(|s| s.get("refs")).filter(|r| r.is_object());
    let root = match (refs, schema.and_then(|s| s.get("uid"))) {
        (Some(r), Some(uid)) => deref(r, uid),
        _ => Value::Null,
    };
    let dict = root
        .get("dict")
        .filter(|d| d.is_object() && root.get("type").and_then(Value::as_str) == Some("object"));
    let value = v.get("value").filter(|x| x.is_object()).cloned().unwrap_or_else(|| json!({}));
    let refs_null = json!({});
    let refs = refs.unwrap_or(&refs_null);

    let secret_slots: Vec<&Value> = v
        .get("secrets")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter(|s| {
                    s.get("path").and_then(Value::as_array).map(|p| p.len() == 1).unwrap_or(false)
                })
                .collect()
        })
        .unwrap_or_default();
    let secret_of = |key: &str| -> Option<&Value> {
        secret_slots.iter().copied().find(|s| {
            s.get("path").and_then(Value::as_array).and_then(|p| p[0].as_str()) == Some(key)
        })
    };

    let mut fields: Vec<Value> = Vec::new();
    let mut readonly: Vec<Value> = Vec::new();
    if let Some(dict) = dict {
        let mut keys: Vec<&String> = dict.as_object().unwrap().keys().collect();
        keys.sort();
        for key in keys {
            let p = deref(refs, &dict[key]);
            let cur = value.get(key);
            let pt = p.get("type").and_then(Value::as_str).unwrap_or("");
            let sec = secret_of(key);
            // union(const…) = 枚举
            let consts: Option<Vec<Value>> = if pt == "union" {
                p.get("list").and_then(Value::as_array).filter(|l| !l.is_empty()).map(|l| {
                    l.iter().map(|m| deref(refs, m)).filter(|m| m.get("type").and_then(Value::as_str) == Some("const")).filter_map(|m| m.get("value").cloned()).collect()
                }).filter(|c: &Vec<Value>| {
                    let n = p.get("list").and_then(Value::as_array).map(|l| l.len()).unwrap_or(0);
                    c.len() == n
                })
            } else {
                None
            };
            let _ = &p; // owned 值就地使用
            if let Some(sec) = sec {
                fields.push(json!({ "key": key, "label": key, "type": "text",
                    "secretWriteOnly": true, "value": "",
                    "exists": sec.get("set").and_then(Value::as_bool).unwrap_or(false) }));
            } else if let Some(opts) = consts {
                let cur_s = cur.and_then(Value::as_str).unwrap_or("").to_string();
                fields.push(json!({ "key": key, "label": key, "type": "select",
                    "options": opts, "value": cur_s }));
            } else if pt == "number" {
                fields.push(json!({ "key": key, "label": key, "type": "number",
                    "value": cur.and_then(Value::as_i64).unwrap_or(0) }));
            } else if pt == "boolean" {
                fields.push(json!({ "key": key, "label": key, "type": "checkbox",
                    "value": cur.and_then(Value::as_bool).unwrap_or(false) }));
            } else if pt == "string" || pt == "const" {
                fields.push(json!({ "key": key, "label": key, "type": "text",
                    "value": cur.and_then(Value::as_str).unwrap_or("") }));
            } else {
                let nested = matches!(pt, "object" | "array" | "dict" | "list" | "tuple");
                readonly.push(json!({ "key": key,
                    "note": if nested { "嵌套结构 v1 只读".to_string() }
                            else { format!("未知形态（{}）v1 只读", pt) } }));
            }
        }
    }
    let revision = v.get("revision").and_then(Value::as_i64);
    let applies = match v.get("applies").and_then(Value::as_str) {
        Some("live") | Some("restart") => v.get("applies").cloned().unwrap_or(Value::Null),
        _ => Value::Null,
    };
    json!({
        "fields": fields,
        "readonly": readonly,
        "secrets": v.get("secrets").cloned().unwrap_or(json!([])),
        "revision": revision.map(Value::from).unwrap_or(Value::Null),
        "applies": applies,
    })
}

/// live 抓样形夹具（= core.test.mjs themeNsView，D-208 纪律：不臆造形状）。
fn theme_ns_view() -> Value {
    json!({
      "ns": "ui-theme", "applies": "live", "revision": 7,
      "schema": { "refs": {
        "0": { "type": "object", "dict": { "mode": 1, "fontSize": 2, "showTips": 3, "nested": 4 }, "meta": {} },
        "1": { "type": "union", "list": [5, 6], "meta": { "default": "light" } },
        "2": { "type": "number", "meta": {} },
        "3": { "type": "boolean", "meta": {} },
        "4": { "type": "object", "dict": { "a": 7 }, "meta": {} },
        "5": { "type": "const", "value": "dark", "meta": {} },
        "6": { "type": "const", "value": "light", "meta": {} },
        "7": { "type": "string", "meta": {} } },
        "uid": 0 },
      "value": { "mode": "dark", "fontSize": 14, "showTips": true, "nested": { "a": 1 } },
      "secrets": [{ "path": ["apiKey"], "set": true }]
    })
}

fn by_key<'a>(proj: &'a Value, key: &str) -> Option<&'a Value> {
    proj["fields"].as_array()?.iter().find(|f| f["key"] == json!(key))
}

/// D-201 nsSelect 模型：describe value → {options, current}。current = pick 命中，
/// 否则首项回退；空/坏 → 空模型（core.js 341–346 行移植）。
pub fn ns_select_model(value: &Value, selected: &str) -> Value {
    let options: Vec<Value> = value
        .get("namespaces")
        .and_then(Value::as_array)
        .map(|ns| {
            ns.iter()
                .filter_map(|n| n.get("ns").and_then(Value::as_str).map(|s| json!(s)))
                .collect()
        })
        .unwrap_or_default();
    let hit = options.iter().any(|o| o.as_str() == Some(selected));
    let current = if hit {
        json!(selected)
    } else {
        options.first().cloned().unwrap_or(json!(""))
    };
    json!({"options": options, "current": current})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ns_select_model_pick_hit_then_first_then_empty() {
        let v = json!({"namespaces":[{"ns":"ui-theme"},{"ns":"llm"},{"ns":"locale"}]});
        assert_eq!(ns_select_model(&v, "llm")["current"], "llm");
        assert_eq!(ns_select_model(&v, "nope")["current"], "ui-theme", "回退首项");
        assert_eq!(ns_select_model(&v, "nope")["options"].as_array().unwrap().len(), 3);
        let junk = ns_select_model(&json!(null), "");
        assert_eq!(junk["options"].as_array().unwrap().len(), 0);
        assert_eq!(junk["current"], "");
        assert_eq!(ns_select_model(&json!({"namespaces":[{"x":1},{"ns":"ok"}]}), "ok")["options"], json!(["ok"]), "脏条目跳过");
    }

    #[test]
    fn projects_scalars_enum_readonly_secrets() {
        let r = schema_fields(&theme_ns_view());
        assert_eq!(by_key(&r, "mode").unwrap()["type"], "select");
        assert_eq!(by_key(&r, "mode").unwrap()["options"], json!(["dark", "light"]));
        assert_eq!(by_key(&r, "mode").unwrap()["value"], "dark");
        assert_eq!(by_key(&r, "fontSize").unwrap()["type"], "number");
        assert_eq!(by_key(&r, "fontSize").unwrap()["value"], 14);
        assert_eq!(by_key(&r, "showTips").unwrap()["type"], "checkbox");
        assert!(by_key(&r, "nested").is_none(), "嵌套对象不进可编辑 fields");
        assert_eq!(r["readonly"].as_array().unwrap().len(), 1);
        assert_eq!(r["readonly"][0]["key"], "nested");
        assert_eq!(r["revision"], 7);
        assert_eq!(r["applies"], "live");
    }

    #[test]
    fn missing_values_fall_to_honest_empty_inits() {
        let mut v = theme_ns_view();
        v["value"] = json!({});
        let r = schema_fields(&v);
        assert_eq!(by_key(&r, "mode").unwrap()["value"], "");
        assert_eq!(by_key(&r, "fontSize").unwrap()["value"], 0);
        assert_eq!(by_key(&r, "showTips").unwrap()["value"], false);
    }

    #[test]
    fn junk_ns_view_degrades_to_empty_projection() {
        let r = schema_fields(&json!({}));
        assert_eq!(r["fields"].as_array().unwrap().len(), 0);
        assert_eq!(r["readonly"].as_array().unwrap().len(), 0);
        assert_eq!(r["revision"], Value::Null);
    }

    #[test]
    fn top_level_secret_is_write_only_nested_is_not_promoted() {
        let r = schema_fields(&json!({
          "schema": { "refs": {
              "0": { "type": "object", "dict": { "token": 1, "lang": 2 }, "meta": {} },
              "1": { "type": "string", "meta": {} },
              "2": { "type": "string", "meta": {} } },
            "uid": 0 },
          "value": { "lang": "zh" },
          "secrets": [{ "path": ["token"], "set": true }, { "path": ["deep", "pw"], "set": false }]
        }));
        let token = by_key(&r, "token").expect("token 应为 write-only 字段");
        assert_eq!(token["secretWriteOnly"], true);
        assert_eq!(token["value"], "");
        assert_eq!(token["exists"], true);
        assert_eq!(by_key(&r, "lang").unwrap()["value"], "zh");
        assert_eq!(r["fields"].as_array().unwrap().len(), 2, "嵌套 secret 不额外成字段");
    }
}

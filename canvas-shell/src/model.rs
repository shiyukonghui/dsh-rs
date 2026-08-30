//! 清单→展示模型 + 声明校验（core.js 8–177 行移植；TYPE_ORDER 闭集、fail-loud 表）。
//! 断言语义逐条对齐 core.test.mjs（C3/C4/C8-1 契约）。

use serde_json::{json, Value};

pub const TYPE_ORDER: [&str; 7] = ["model", "config", "capability", "runtime", "resource", "session", "misc"];
const SCHEMA: &str = "dsh.panel-ui/v2";
const REJECTED: [&str; 1] = ["board"];
const RESERVED: [&str; 2] = ["chart", "table"];
const IMPLEMENTED: [&str; 4] = ["form", "status", "list", "chat"];

pub fn focus_key(plugin_name: &str, card_id: &str) -> String {
    format!("{}/{}", plugin_name, card_id)
}

/// 清单值 → 展示模型（error 条目 = 装了但坏了 → misc 坏卡；组按 TYPE_ORDER，组内保序）。
pub fn build_model(manifest: &Value) -> Value {
    let cards_in: Vec<Value> = manifest
        .get("cards")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let cards: Vec<Value> = cards_in
        .iter()
        .map(|c| {
            if c.get("error").is_some() {
                let mut m = c.clone();
                m["bad"] = json!(true);
                if m.get("type").map(|t| t.is_null()).unwrap_or(true) {
                    m["type"] = json!("misc");
                }
                if m.get("size").is_none() {
                    m["size"] = json!({"w":2,"h":3});
                }
                if m.get("title").is_none() {
                    let pn = c.get("pluginName").and_then(Value::as_str).unwrap_or("?");
                    m["title"] = json!(format!("{}（声明损坏）", pn));
                }
                m
            } else {
                let mut m = c.clone();
                m["bad"] = json!(false);
                m
            }
        })
        .collect();
    let mut groups: Vec<Value> = Vec::new();
    for ty in TYPE_ORDER {
        let in_group: Vec<&Value> = cards
            .iter()
            .filter(|c| {
                c.get("type")
                    .and_then(Value::as_str)
                    .or(Some("misc"))
                    // JS: c.type || "misc"（空串也算缺省；None 或空串→misc）
                    .filter(|s| !s.is_empty())
                    .unwrap_or("misc")
                    == ty
            })
            .collect();
        if !in_group.is_empty() {
            groups.push(json!({"type": ty, "cards": in_group, "count": in_group.len()}));
        }
    }
    json!({
        "rev": manifest.get("rev").and_then(Value::as_str).unwrap_or(""),
        "cards": cards,
        "groups": groups,
    })
}

/// 声明校验（§7 fail-loud 表）；None = 可画。
pub fn validate_declaration(d: &Value) -> Option<(String, String)> {
    let bad = |code: &str, msg: String| Some((code.to_string(), msg));
    if !d.is_object() {
        return bad("declaration-unparseable", "声明不是 JSON 对象".into());
    }
    if d.get("$schema").and_then(Value::as_str) != Some(SCHEMA) {
        return bad(
            "schema-version-unsupported",
            format!("仅支持 {}，收到 {}（不做静默兼容）", SCHEMA, d.get("$schema").map(|v| v.to_string()).unwrap_or_else(|| "null".into())),
        );
    }
    if d.get("kind").and_then(Value::as_str) != Some("card") {
        return bad("card-kind-unknown", format!("顶层唯一容器是 kind:\"card\"，收到 {}", d.get("kind").map(|v| v.to_string()).unwrap_or_else(|| "null".into())));
    }
    let view = d.get("view").filter(|v| v.is_object());
    let view = match view {
        Some(v) if v.get("kind").and_then(Value::as_str).map(|k| !k.is_empty()).unwrap_or(false) => v,
        _ => return bad("view-malformed", "卡片缺 view 或 view.kind".into()),
    };
    let k = view.get("kind").and_then(Value::as_str).unwrap_or("");
    if REJECTED.contains(&k) {
        return bad("view-kind-rejected", format!("view.kind=\"{}\" 被契约否决（画布本身，卡内嵌画布为递归陷阱）", k));
    }
    let pair = |v: Option<&Value>| -> bool {
        v.and_then(Value::as_array)
            .map(|a| a.len() == 2 && a.iter().all(|x| x.is_string()))
            .unwrap_or(false)
    };
    if k == "chat" {
        if !(pair(view.get("sessionSource")) && pair(view.get("historyRpc")) && pair(view.get("sendRpc"))) {
            return bad("view-malformed", "chat 视图须有 sessionSource/historyRpc/sendRpc 三个 [ns,method] 面".into());
        }
        if view.get("stream").and_then(Value::as_str) != Some("session-events") {
            return bad("view-malformed", "chat.stream 必须恰为 \"session-events\"（闭集）".into());
        }
    }
    if RESERVED.contains(&k) {
        return bad("renderer-unimplemented", format!("view.kind=\"{}\" 渲染器尚未实现（契约已定档）", k));
    }
    if !IMPLEMENTED.contains(&k) {
        return bad("view-kind-unknown", format!("未定义的 view.kind=\"{}\"", k));
    }
    if k == "form" {
        let has_fields = view.get("fields").map(|f| f.is_array()).unwrap_or(false);
        let ff = view.get("fieldsFrom").filter(|x| x.is_object());
        let has_ff = ff
            .map(|f| {
                pair(f.get("rpc")) && f.get("pick").map(|p| p.is_string()).unwrap_or(false)
            })
            .unwrap_or(false);
        if has_fields == has_ff {
            return bad("view-malformed", "form 视图须有 fields 或形正的 fieldsFrom（二选一）".into());
        }
        if !view.get("actions").map(|a| a.is_array()).unwrap_or(false) {
            return bad("view-malformed", "form 视图缺 actions 数组".into());
        }
    }
    if k == "list" && !view.get("rowsPath").map(|r| r.is_string()).unwrap_or(false) {
        return bad("view-malformed", "list 视图缺 rowsPath（数据面位置必须显式）".into());
    }
    if k == "list" && view.get("rowActions").is_some() {
        match view.get("rowActions").and_then(Value::as_array) {
            None => return bad("view-malformed", "list.rowActions 必须是数组".into()),
            Some(arr) => {
                for ra in arr {
                    let rpc_ok = pair(ra.get("rpc"));
                    if !ra.is_object() || !ra.get("name").map(|n| n.is_string()).unwrap_or(false) || !rpc_ok {
                        return bad("view-malformed", "rowActions 项须含 name 与 [ns,method] rpc".into());
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(pn: &str, ty: &str) -> Value {
        json!({"pluginName": pn, "cardId": "c", "type": ty, "size": {"w":2,"h":3}})
    }

    #[test]
    fn build_model_groups_and_bad_cards() {
        let m = json!({"rev":"r1","cards":[
            card("b","model"),
            {"pluginName":"坏蛋","error":{"code":"declaration-unparseable","message":"x"}},
            card("a","model"),
            card("c","zzz")
        ]});
        let model = build_model(&m);
        assert_eq!(model["rev"], "r1");
        assert_eq!(model["cards"][0]["bad"], false);
        // 坏卡：misc、2×3、标题标注、bad=true
        assert_eq!(model["cards"][1]["bad"], true);
        assert_eq!(model["cards"][1]["type"], "misc");
        assert_eq!(model["cards"][1]["size"]["w"], 2);
        assert_eq!(model["cards"][1]["title"], "坏蛋（声明损坏）");
        // 组按 TYPE_ORDER：model(2, 保声明序 b,a) → misc(2: 坏卡 + zzz? 不，zzz 归 misc 仅清单已归一；core.js 用 c.type||misc——zzz 保持 zzz 不成组)
        let groups = model["groups"].as_array().unwrap();
        assert_eq!(groups[0]["type"], "model");
        assert_eq!(groups[0]["count"], 2);
        assert_eq!(groups[0]["cards"][0]["pluginName"], "b", "组内保声明序");
        assert_eq!(groups[1]["type"], "misc");
        assert_eq!(groups.len(), 2, "未知 type 自成孤儿不成组（z≠misc）");
    }

    #[test]
    fn validate_table_fail_loud() {
        assert!(validate_declaration(&json!([])).is_some());
        assert_eq!(validate_declaration(&json!([])).unwrap().0, "declaration-unparseable");
        assert_eq!(validate_declaration(&json!({"$schema":"dsh/plugin-ui/v1","kind":"card","view":{"kind":"status"}})).unwrap().0, "schema-version-unsupported");
        // D-216 P0：旧方言串此后同 v1 同罪（纯标准文法，零兼容）。
        assert_eq!(validate_declaration(&json!({"$schema":"dsh/plugin-ui/v2","kind":"card","view":{"kind":"status"}})).unwrap().0, "schema-version-unsupported");
        assert_eq!(validate_declaration(&json!({"$schema":SCHEMA,"kind":"panel","view":{"kind":"status"}})).unwrap().0, "card-kind-unknown");
        assert_eq!(validate_declaration(&json!({"$schema":SCHEMA,"kind":"card"})).unwrap().0, "view-malformed");
        assert_eq!(validate_declaration(&json!({"$schema":SCHEMA,"kind":"card","view":{"kind":"board"}})).unwrap().0, "view-kind-rejected");
        assert_eq!(validate_declaration(&json!({"$schema":SCHEMA,"kind":"card","view":{"kind":"chart"}})).unwrap().0, "renderer-unimplemented");
        assert_eq!(validate_declaration(&json!({"$schema":SCHEMA,"kind":"card","view":{"kind":"wizard"}})).unwrap().0, "view-kind-unknown");
        // chat 三面前置（先声明缺陷优先于渲染器进度——D-193）
        assert_eq!(validate_declaration(&json!({"$schema":SCHEMA,"kind":"card","view":{"kind":"chat"}})).unwrap().0, "view-malformed");
        assert!(validate_declaration(&json!({"$schema":SCHEMA,"kind":"card","view":{"kind":"chat",
            "sessionSource":["session","list"],"historyRpc":["session","history"],"sendRpc":["session","prompt"],
            "stream":"session-events"}})).is_none());
        // form 二选一：fields=[] 在 JS 是 Array.isArray→true = 有 fields（有 fields 无 ff → 通过）
        assert!(validate_declaration(&json!({"$schema":SCHEMA,"kind":"card","view":{"kind":"form","fields":[],"actions":[]}})).is_none());
        // 二者皆无 → 否决
        assert_eq!(validate_declaration(&json!({"$schema":SCHEMA,"kind":"card","view":{"kind":"form","actions":[]}})).unwrap().0, "view-malformed");
        // 二者皆有 → 同样否决（二选一）
        assert_eq!(validate_declaration(&json!({"$schema":SCHEMA,"kind":"card","view":{"kind":"form","fields":[],
            "fieldsFrom":{"rpc":["a","b"],"pick":"x"},"actions":[]}})).unwrap().0, "view-malformed");
    }

    #[test]
    fn form_and_list_rules() {
        // fieldsFrom 形正 + actions → 可画
        assert!(validate_declaration(&json!({"$schema":SCHEMA,"kind":"card","view":{"kind":"form",
            "fieldsFrom":{"rpc":["settings","describe"],"pick":"ui-theme"},"actions":[]}})).is_none());
        // list rowsPath 必须
        assert_eq!(validate_declaration(&json!({"$schema":SCHEMA,"kind":"card","view":{"kind":"list"}})).unwrap().0, "view-malformed");
        assert!(validate_declaration(&json!({"$schema":SCHEMA,"kind":"card","view":{"kind":"list","rowsPath":"items"}})).is_none());
        // rowActions 形状
        assert_eq!(validate_declaration(&json!({"$schema":SCHEMA,"kind":"card","view":{"kind":"list","rowsPath":"i","rowActions":[{"name":"删","rpc":["x","y","z"]}]}})).unwrap().0, "view-malformed");
        assert!(validate_declaration(&json!({"$schema":SCHEMA,"kind":"card","view":{"kind":"list","rowsPath":"i","rowActions":[{"name":"删","rpc":["x","y"]}]}})).is_none());
    }

    #[test]
    fn focus_key_shape() {
        assert_eq!(focus_key("panel-chat", "chat"), "panel-chat/chat");
    }
}

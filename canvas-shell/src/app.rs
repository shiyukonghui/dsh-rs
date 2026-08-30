//! Dioxus 桌布壳（S2 骨架）：清单/SSE/轮询 → 侧栏五分类 → 实测布局卡框 → ✕ 关闭。
//! 纪律：可证逻辑零在此层（全在 canvas_shell lib）；S2 只立壳，form/status/list/chat
//! 数据体按设计文档在 S3/S4 接线（诚实占位，不伪造渲染）。

use dioxus::prelude::*;
// S6a：JS 回调（setInterval/SSE）无 dioxus 作用域——异步任务必须走 spawn_local，
// 否则 dioxus::spawn 找 current_owner → runtime.rs unwrap(None) panic（s6-audit 根因）。
use wasm_bindgen_futures::spawn_local;
use serde_json::{json, Value};

use canvas_shell::chat::{chat_fold_frame, chat_options};
use canvas_shell::layout::{columns_for_width, layout_grid, layout_measured, GRID_COL, GRID_GAP};
use canvas_shell::model::{build_model, focus_key, validate_declaration};
use canvas_shell::schema::{ns_select_model, schema_fields};
use canvas_shell::values::{collect_values, list_rows, needs_confirm, poll_decision, row_action_body, status_items};

fn fk(c: &Value) -> String {
    focus_key(
        c.get("pluginName").and_then(Value::as_str).unwrap_or("?"),
        c.get("cardId").and_then(Value::as_str).unwrap_or("?"),
    )
}

async fn load_manifest(mut model: Signal<Option<Value>>, mut status: Signal<(String, String)>) {
    let revarg = match model.read().as_ref().and_then(|m| m.get("rev")).cloned() {
        Some(r) => json!({ "rev": r }),
        None => json!({}),
    };
    match crate::interop::fetch_rpc("uiManifest/list", revarg).await {
        Err(e) => {
            status.set((format!("✗ 清单拉取失败：{}（保留现状）", e), "err".into()));
        }
        Ok(res) => {
            if res.get("ok").and_then(Value::as_bool) == Some(false) {
                let msg = res.get("error").and_then(|e| e.get("message")).and_then(Value::as_str).unwrap_or("?");
                status.set((format!("✗ 清单错误：{}（保留现状）", msg), "err".into()));
                return;
            }
            let value = res.get("value").cloned().unwrap_or(res.clone());
            let dec = poll_decision(Some(&value));
            if dec.get("action").and_then(Value::as_str) == Some("keep") {
                return;
            }
            let manifest = json!({ "rev": dec.get("rev"), "cards": dec.get("cards") });
            model.set(Some(build_model(&manifest)));
        }
    }
}

fn visible_cards(model: &Option<Value>, selected: &Option<String>, closed: &[String]) -> Vec<Value> {
    let m = match model { Some(m) => m, None => return vec![] };
    let src: Vec<Value> = match selected {
        Some(t) => m
            .get("groups")
            .and_then(Value::as_array)
            .and_then(|gs| gs.iter().find(|g| g.get("type").and_then(Value::as_str) == Some(t.as_str())))
            .and_then(|g| g.get("cards").and_then(Value::as_array).cloned())
            .unwrap_or_default(),
        None => m.get("cards").and_then(Value::as_array).cloned().unwrap_or_default(),
    };
    src.into_iter().filter(|c| !closed.contains(&fk(c))).collect()
}

fn grid_px(w: i64) -> f64 {
    w as f64 * GRID_COL as f64 + (w as f64 - 1.0) * GRID_GAP as f64
}

fn scalar_text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(o) => o.to_string(),
    }
}

/// 卡片体面管线：ui.json → validate → dataRpc →（视图渲染在 card_body）。
async fn load_card_body(fk: String, pn: String, mut body: Signal<serde_json::Map<String, Value>>) {
    let mut entry = json!({ "stage": "load" });
    match crate::interop::fetch_get_json(&format!("/plugins/{}/ui.json", pn)).await {
        Err(e) => entry = json!({"stage":"decl","msg":format!("ui.json 拉取失败：{}", e),"code":"ui-fetch-failed"}),
        Ok(ui) => {
            if let Some((code, msg)) = validate_declaration(&ui) {
                entry = json!({"stage":"decl","msg":msg,"code":code});
            } else {
                let view = ui.get("view").cloned().unwrap_or(Value::Null);
                let rpc_src = view.get("dataRpc").and_then(Value::as_array)
                    .or_else(|| view.get("fieldsFrom").and_then(|ff| ff.get("rpc")).and_then(Value::as_array))
                    .or_else(|| view.get("sessionSource").and_then(Value::as_array))
                    .cloned();
                if let Some(drpc) = rpc_src {
                    let method = drpc.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("/");
                    match crate::interop::fetch_rpc(&method, json!({})).await {
                        Ok(res) if res.get("ok").and_then(Value::as_bool) == Some(false) => {
                            let msg = res.get("error").and_then(|e| e.get("message")).cloned().unwrap_or(json!("?"));
                            entry = json!({"stage":"view","view":view,"dataErr":msg});
                        }
                        Ok(res) => entry = json!({"stage":"view","view":view,"data":res.get("value").cloned().unwrap_or(Value::Null)}),
                        Err(e) => entry = json!({"stage":"view","view":view,"dataErr":e}),
                    }
                } else {
                    entry = json!({"stage":"view","view":view});
                }
            }
        }
    }
    body.write().insert(fk.clone(), entry.clone());
    // chat 引导（JS 同款）：选 default 或首项 sid → 初始历史折叠。
    let view_v = entry.get("view").cloned().unwrap_or(Value::Null);
    if view_v.get("kind").and_then(Value::as_str) == Some("chat") {
        let rows = entry.get("data").and_then(|d| d.get("items")).cloned().unwrap_or(Value::Null);
        let opts = chat_options(&rows);
        if !opts.is_empty() {
            let sid = if opts.iter().any(|o| o.get("value").and_then(Value::as_str) == Some("default")) {
                "default".to_string()
            } else {
                scalar_text(opts.first().and_then(|o| o.get("value")))
            };
            {
                let mut g = body.write();
                if let Some(en) = g.get_mut(&fk) {
                    en["chat"] = json!({"sessionId": sid, "busy": false, "messages": []});
                }
            }
            let hist = rpc_join(view_v.get("historyRpc"));
            let bd2 = body;
            let fk2 = fk.clone();
            spawn_local(async move { load_chat_history(fk2, hist, sid, bd2).await; });
        }
    }
}

fn frame_text(d: Option<&Value>) -> String {
    let Some(d) = d else { return String::new() };
    let c = d.get("content").cloned()
        .or_else(|| d.get("message").and_then(|m| m.get("content")).cloned())
        .or_else(|| d.get("text").cloned());
    match c {
        Some(Value::String(s)) => s,
        Some(Value::Array(a)) => a
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .map(|b| scalar_text(b.get("text")))
            .collect::<String>(),
        _ => String::new(),
    }
}

fn rpc_join(v: Option<&Value>) -> String {
    v.and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("/"))
        .unwrap_or_default()
}

fn set_act(mut body: Signal<serde_json::Map<String, Value>>, k: &str, msg: String) {
    let mut g = body.write();
    let en = g.entry(k.to_string()).or_insert_with(|| json!({ "stage": "view" }));
    en["act"] = json!(msg);
}

/// 历史 = session.history 事件面折叠（与 SSE 同一事实源，JS 壳 C8-3 同款）。
async fn load_chat_history(k: String, hist_rpc: String, sid: String, mut body: Signal<serde_json::Map<String, Value>>) {
    match crate::interop::fetch_rpc(&hist_rpc, json!({ "sessionId": sid })).await {
        Err(e) => set_act(body, &k, format!("✗ 历史拉取：{}", e)),
        Ok(res) if res.get("ok").and_then(Value::as_bool) == Some(false) => {
            let m = res.get("error").and_then(|x| x.get("message")).and_then(Value::as_str).unwrap_or("?");
            set_act(body, &k, format!("✗ 历史拉取：{}", m));
        }
        Ok(res) => {
            let mut s = json!({ "sessionId": sid, "busy": false, "messages": [] });
            let evs = res.get("value").and_then(|v| v.get("events")).and_then(Value::as_array).cloned().unwrap_or_default();
            for wrap in &evs {
                let Some(ev) = wrap.get("event") else { continue };
                let nf = json!({ "sessionId": sid, "kind": ev.get("type").cloned().unwrap_or(Value::Null),
                                 "data": { "text": frame_text(ev.get("data")) }, "time": ev.get("time").cloned().unwrap_or(Value::Null) });
                if let Some(next) = chat_fold_frame(&s, &nf) {
                    s = next;
                }
            }
            let mut g = body.write();
            if let Some(en) = g.get_mut(&k) {
                en["chat"] = s;
            }
        }
    }
    crate::interop::scroll_chat_bottom(&k);
}

/// chat 岛（D-193 契约：选择/历史/发送乐观气泡/停止；折叠唯一事实源 chat_fold_frame）。
#[component]
fn ChatIsland(view: Value, k: String, body: Signal<serde_json::Map<String, Value>>, opts: Vec<Value>, sid: String, msgs: Value, has_cancel: bool) -> Element {
    let pairs: Vec<(String, String)> = msgs
        .as_array()
        .map(|a| a.iter().map(|m| {
            let role = scalar_text(m.get("role"));
            let cls = format!("chat-bubble {}", role);
            let who = match role.as_str() { "user" => "我: ", "assistant" => "助手: ", _ => "· " };
            let mut t = format!("{}{}", who, scalar_text(m.get("text")));
            if m.get("pending").and_then(Value::as_bool) == Some(true) { t.push_str(" …"); }
            (cls, t)
        }).collect())
        .unwrap_or_default();
    let hist_rpc = rpc_join(view.get("historyRpc"));
    let send_rpc = rpc_join(view.get("sendRpc"));
    let cancel_rpc = rpc_join(view.get("cancelRpc"));

    let sid_for_sel = sid.clone();
    let opts_sel: Vec<(String, String)> = opts
        .iter()
        .map(|o| (scalar_text(o.get("value")), scalar_text(o.get("label"))))
        .collect();

    let (k_r, h_r, b_r) = (k.clone(), hist_rpc.clone(), body);
    let reload = move |_| {
        let sid_now = {
            let g = b_r.read();
            g.get(&k_r).and_then(|e| e.get("chat")).and_then(|c| c.get("sessionId")).and_then(Value::as_str).unwrap_or("").to_string()
        };
        if sid_now.is_empty() { return; }
        let (k2, h2) = (k_r.clone(), h_r.clone());
        let b2 = b_r;
        spawn(async move { load_chat_history(k2, h2, sid_now, b2).await; });
    };

    let (k_s, h_s, mut b_s) = (k.clone(), hist_rpc.clone(), body);
    let sel_change = move |ev: dioxus::prelude::FormEvent| {
        let v = ev.value();
        {
            let mut g = b_s.write();
            if let Some(en) = g.get_mut(&k_s) {
                en["chat"] = json!({"sessionId": v.clone(), "busy": false, "messages": []});
            }
        }
        let (k2, h2, b2) = (k_s.clone(), h_s.clone(), b_s);
        spawn(async move { load_chat_history(k2, h2, v, b2).await; });
    };

    let (k_send, ss_rpc, mut b_send) = (k.clone(), send_rpc.clone(), body);
    let send = move |ev: dioxus::prelude::FormEvent| {
        ev.prevent_default();
        let vals = crate::interop::read_form(&k_send);
        let text = scalar_text(vals.get("chat-input")).trim().to_string();
        let sid_now = {
            let g = b_send.read();
            g.get(&k_send).and_then(|e| e.get("chat")).and_then(|c| c.get("sessionId")).and_then(Value::as_str).unwrap_or("").to_string()
        };
        if sid_now.is_empty() {
            set_act(b_send, &k_send, "✗ 当前无会话".into());
            return;
        }
        if text.is_empty() { return; }
        {
            let mut g = b_send.write();
            if let Some(en) = g.get_mut(&k_send) {
                if en.get("chat").is_none() {
                    en["chat"] = json!({"sessionId": sid_now.clone(), "busy": false, "messages": []});
                }
                if let Some(arr) = en["chat"]["messages"].as_array_mut() {
                    arr.push(json!({"role": "user", "text": text, "pending": true, "ts": js_sys::Date::now()}));
                }
            }
        }
        crate::interop::set_input_value(&k_send, "chat-input", "");
        let (k2, mut b2) = (k_send.clone(), b_send);
        let (ss, sid2, txt2) = (ss_rpc.clone(), sid_now.clone(), text.clone());
        spawn(async move {
            match crate::interop::fetch_rpc(&ss, json!({"sessionId": sid2, "text": txt2})).await {
                Err(e) => {
                    mark_pending_fail(&mut b2, &k2);
                    set_act(b2, &k2, format!("✗ 发送：{}", e));
                }
                Ok(res) if res.get("ok").and_then(Value::as_bool) == Some(false) => {
                    mark_pending_fail(&mut b2, &k2);
                    let m = res.get("error").and_then(|x| x.get("message")).and_then(Value::as_str).unwrap_or("?");
                    set_act(b2, &k2, format!("✗ {}", m));
                }
                Ok(_) => set_act(b2, &k2, "✓ 已发送".into()),
            }
        });
    };

    let (k_c, c_rpc, b_c) = (k.clone(), cancel_rpc.clone(), body);
    let stop = move |_| {
        let sid_now = {
            let g = b_c.read();
            g.get(&k_c).and_then(|e| e.get("chat")).and_then(|c| c.get("sessionId")).and_then(Value::as_str).unwrap_or("").to_string()
        };
        if sid_now.is_empty() {
            set_act(b_c, &k_c, "✗ 当前无会话".into());
            return;
        }
        set_act(b_c, &k_c, format!("→ 取消 {} …", sid_now));
        let (k2, b2) = (k_c.clone(), b_c);
        let (cc, sid2) = (c_rpc.clone(), sid_now);
        spawn(async move {
            let msg = match crate::interop::fetch_rpc(&cc, json!({"sessionId": sid2})).await {
                Err(e) => format!("✗ 取消：{}", e),
                Ok(res) if res.get("ok").and_then(Value::as_bool) == Some(false) => {
                    format!("✗ {}", res.get("error").and_then(|x| x.get("message")).and_then(Value::as_str).unwrap_or("?"))
                }
                Ok(_) => "✓ 已请求取消".into(),
            };
            set_act(b2, &k2, msg);
        });
    };

    rsx! {
        div { class: "chat-bar",
            select { value: sid_for_sel, onchange: sel_change,
                for (ov, ol) in opts_sel {
                    option { value: "{ov}", "{ol}" }
                }
            }
            button { onclick: reload, "↻" }
        }
        div { class: "chat-msgs",
            for (mi, (cls, txt)) in pairs.iter().enumerate() {
                div { class: "{cls}", key: "{mi}", "{txt}" }
            }
        }
        form { class: "chat-send", onsubmit: send,
            input { name: "chat-input", placeholder: "发消息…", autocomplete: "off" }
            button { r#type: "submit", class: "primary", "发送" }
            if has_cancel {
                button { onclick: stop, "停止" }
            }
        }
    }
}

fn mark_pending_fail(body: &mut Signal<serde_json::Map<String, Value>>, k: &str) {
    let mut g = body.write();
    if let Some(en) = g.get_mut(k) {
        if let Some(arr) = en.get_mut("chat").and_then(|c| c.get_mut("messages")).and_then(Value::as_array_mut) {
            if let Some(last) = arr.last_mut() {
                if last.get("pending").and_then(Value::as_bool) == Some(true) {
                    let t = scalar_text(last.get("text"));
                    last["text"] = json!(format!("{}（发送失败）", t));
                }
            }
        }
    }
}

fn action_click(
    k: &str,
    view: &Value,
    row: &Value,
    a: &Value,
    mut body: Signal<serde_json::Map<String, Value>>,
) {
    if needs_confirm(a)
        && !crate::interop::confirm_dialog(&format!("确认「{}」？", a.get("label").and_then(Value::as_str).unwrap_or("")))
    {
        return;
    }
    let method = a.get("rpc").and_then(Value::as_array).map(|r| r.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("/")).unwrap_or_default();
    let args = row_action_body(row, Some(a));
    let f2 = k.to_string();
    let v2 = view.clone();
    spawn(async move {
        let msg = match crate::interop::fetch_rpc(&method, args).await {
            Err(e) => format!("✗ 动作失败：{}", e),
            Ok(res) if res.get("ok").and_then(Value::as_bool) == Some(false) => {
                format!("✗ {}", res.get("error").and_then(|e| e.get("message")).and_then(Value::as_str).unwrap_or("?"))
            }
            Ok(_) => "✓ 已完成".to_string(),
        };
        let mut entry = json!({"stage":"view","view":v2.clone(),"act":msg});
        if let Some(drpc) = v2.get("dataRpc").and_then(Value::as_array) {
            let m2 = drpc.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("/");
            if let Ok(r) = crate::interop::fetch_rpc(&m2, json!({})).await {
                if r.get("ok").and_then(Value::as_bool) != Some(false) {
                    entry["data"] = r.get("value").cloned().unwrap_or(Value::Null);
                }
            }
        }
        body.write().insert(f2, entry);
    });
}

/// 行动作按钮（组件化以获得 key/props 语义）。
#[component]
fn ActionBtn(k: String, view: Value, row: Value, a: Value, body: Signal<serde_json::Map<String, Value>>) -> Element {
    let label = if a.get("label").is_some() { scalar_text(a.get("label")) } else { scalar_text(a.get("name")) };
    rsx! {
        button {
            class: "row-action",
            onclick: move |_| action_click(&k, &view, &row, &a, body),
            "{label}"
        }
    }
}

/// 表单控件（归一化字段：name/label/type/value/options/secret/exists/required）。
#[component]
fn FormField(f: Value) -> Element {
    let name = scalar_text(f.get("name"));
    let label = scalar_text(f.get("label"));
    let ty = scalar_text(f.get("type"));
    let val = scalar_text(f.get("value"));
    let required = f.get("required").and_then(Value::as_bool).unwrap_or(false);
    let secret = f.get("secret").and_then(Value::as_bool).unwrap_or(false);
    let exists = f.get("exists").and_then(Value::as_bool).unwrap_or(false);
    let opts: Vec<String> = f.get("options").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
    let star = if required { " *" } else { "" };
    let ph = if secret && exists { "已设（留空=不改动）" } else if secret { "首次设置" } else { "" };
    rsx! {
        label {
            span { "{label}{star}" }
            if secret {
                input { type: "password", name: "{name}", placeholder: "{ph}" }
            } else if ty == "select" {
                select { name: "{name}",
                    for o in opts { option { value: "{o}", selected: o == val, "{o}" } }
                }
            } else if ty == "checkbox" {
                input { type: "checkbox", name: "{name}", checked: val == "true" }
            } else if ty == "list" {
                textarea { name: "{name}", "{val}" }
            } else if ty == "number" {
                input { type: "number", name: "{name}", value: "{val}" }
            } else {
                input { type: "text", name: "{name}", value: "{val}" }
            }
        }
    }
}

/// ns 选择器（fieldsFrom.nsSelect；不带 name——不进表单收集）。
#[component]
fn NsPick(options: Vec<String>, current: String, k: String, mut body: Signal<serde_json::Map<String, Value>>) -> Element {
    rsx! {
        select {
            class: "ns-pick",
            onchange: move |ev| {
                let v = ev.value();
                let mut bw = body;
                let mut g = bw.write();
                if let Some(e) = g.get_mut(&k) {
                    e["nsSel"] = json!(v);
                }
            },
            for o in options {
                option { value: "{o}", selected: o == current, "{o}" }
            }
        }
    }
}

/// 保存/动作按钮（spec: {rpc, mode, ns, revision, fields:[desc]}）。
#[component]
fn FormSave(spec: Value, k: String, mut body: Signal<serde_json::Map<String, Value>>) -> Element {
    let label = scalar_text(spec.get("label"));
    let primary = spec.get("primary").and_then(Value::as_bool).unwrap_or(false);
    rsx! {
        button {
            class: if primary { "primary" } else { "" },
            onclick: move |_| {
                let vals = crate::interop::read_form(&k);
                let desc = json!({ "fields": spec.get("fields").cloned().unwrap_or(json!([])) });
                let patch = match collect_values(&desc, |n| vals.get(n).and_then(Value::as_str).map(str::to_string)) {
                    Ok(p) => p,
                    Err((field, e)) => {
                        let mut bw = body;
                        let mut g = bw.write();
                        if let Some(en) = g.get_mut(&k) { en["act"] = json!(format!("✗ 字段 {}：{}", field, e)); }
                        return;
                    }
                };
                let mode = spec.get("mode").and_then(Value::as_str).unwrap_or("values");
                let args = if mode == "settings-update" {
                    json!({"ns": spec.get("ns").cloned().unwrap_or(Value::Null),
                           "patch": patch,
                           "expectedRevision": spec.get("revision").cloned().unwrap_or(Value::Null)})
                } else {
                    patch
                };
                let rpc = scalar_text(spec.get("rpc"));
                let k2 = k.clone();
                let mut b2 = body;
                spawn(async move {
                    let msg = match crate::interop::fetch_rpc(&rpc, args).await {
                        Err(e) => format!("✗ 保存失败：{}", e),
                        Ok(res) if res.get("ok").and_then(Value::as_bool) == Some(false) => {
                            let m = res.get("error").and_then(|x| x.get("message")).and_then(Value::as_str).unwrap_or("?");
                            let c = res.get("error").and_then(|x| x.get("code")).and_then(Value::as_str).unwrap_or("");
                            format!("✗ {}（code={}）", m, c)
                        }
                        Ok(res) => {
                            let applies = res.get("value").and_then(|v| v.get("applies")).and_then(Value::as_str).unwrap_or("");
                            if applies == "restart" { "✓ 已保存——需重启生效".to_string() } else { "✓ 已保存".to_string() }
                        }
                    };
                    // JS 对齐：保存后不自动重拉（保留 stale revision → 再点必显式 CONFLICT）。
                    let mut b = b2.write();
                    let en = b.entry(k2.clone()).or_insert_with(|| json!({"stage": "view"}));
                    en["act"] = json!(msg);
                });
            },
            "{label}"
        }
    }
}

fn value_text(v: Option<&Value>, default: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => match default {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Null) | None => String::new(),
            Some(o) => o.to_string(),
        },
        Some(o) => o.to_string(),
    }
}

/// 体面渲染（S3a：status/list 实渲；form=S3b、chat=S4——诚实占位）。
fn card_body(k: String, st: Option<Value>, body: Signal<serde_json::Map<String, Value>>) -> Element {
    let st = match st {
        Some(s) => s,
        None => return rsx! { div { class: "cstat", "载入体面…" } },
    };
    let stage = st.get("stage").and_then(Value::as_str).unwrap_or("").to_string();
    if stage == "load" {
        return rsx! { div { class: "cstat", "载入当前值…" } };
    }
    if stage == "decl" {
        let msg = scalar_text(st.get("msg"));
        let code = scalar_text(st.get("code"));
        return rsx! {
            div { class: "fail-msg", "✗ {msg}" }
            div { class: "code", "code={code}" }
        };
    }
    let view = st.get("view").cloned().unwrap_or(Value::Null);
    let kind = view.get("kind").and_then(Value::as_str).unwrap_or("").to_string();
    let data = st.get("data").cloned().unwrap_or(Value::Null);
    let data_err = st.get("dataErr").cloned();
    let err_msg = data_err.as_ref().map(|e| if e.is_string() { e.as_str().unwrap_or("").to_string() } else { e.to_string() }).unwrap_or_default();
    let act = scalar_text(st.get("act"));
    let items = status_items(&view, &data);
    let lr = list_rows(&view, &data);
    let columns = lr.get("columns").and_then(Value::as_array).cloned().unwrap_or_default();
    let rows = lr.get("rows").and_then(Value::as_array).cloned().unwrap_or_default();
    let empty_text = lr.get("emptyText").and_then(Value::as_str).unwrap_or("暂无条目").to_string();
    let row_actions = view.get("rowActions").and_then(Value::as_array).cloned().unwrap_or_default();
    // rsx for 体不收 let：先在外部把 wire 形状压成纯字符串对（视图层零逻辑再证一次）。
    let status_pairs: Vec<(String, String)> = items
        .iter()
        .map(|it| (scalar_text(it.get("label")), scalar_text(it.get("value"))))
        .collect();
    let th_labels: Vec<String> = columns.iter().map(|c| scalar_text(c.get("label"))).collect();
    let cells_all: Vec<Vec<String>> = rows
        .iter()
        .map(|r| columns.iter().map(|c| scalar_text(r.get(scalar_text(c.get("key"))))).collect())
        .collect();
    // ---- form 预计算（rsx for 体零语句纪律：全部归一化在视图外完成） ----
    let mut form_fields: Vec<Value> = Vec::new();
    let mut form_desc: Vec<Value> = Vec::new();
    let mut form_specs: Vec<Value> = Vec::new();
    let mut show_ns = false;
    let mut ns_opts: Vec<String> = Vec::new();
    let mut ns_cur = String::new();
    if kind == "form" {
        let actions = view.get("actions").and_then(Value::as_array).cloned().unwrap_or_default();
        let ff = view.get("fieldsFrom").cloned().filter(|x| x.is_object());
        let (n_rev, n_ns) = match &ff {
            Some(ff) => {
                let pick = ff.get("pick").and_then(Value::as_str).unwrap_or("").to_string();
                let model_v = ns_select_model(&data, &pick);
                let opts: Vec<String> = model_v.get("options").and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
                    .unwrap_or_default();
                let cur = st.get("nsSel").and_then(Value::as_str)
                    .filter(|s| opts.iter().any(|o| o == s))
                    .map(str::to_string)
                    .unwrap_or_else(|| scalar_text(model_v.get("current")));
                show_ns = ff.get("nsSelect").and_then(Value::as_bool).unwrap_or(false);
                ns_opts = opts;
                ns_cur = cur.clone();
                let ns_view = data.get("namespaces").and_then(Value::as_array)
                    .and_then(|a| a.iter().find(|n| n.get("ns").and_then(Value::as_str) == Some(cur.as_str())).cloned())
                    .unwrap_or(Value::Null);
                let proj = schema_fields(&ns_view);
                for pf in proj.get("fields").and_then(Value::as_array).unwrap_or(&vec![]) {
                    let mut nf = json!({
                        "name": scalar_text(pf.get("key")),
                        "label": scalar_text(pf.get("label")),
                        "type": scalar_text(pf.get("type")),
                        "value": scalar_text(pf.get("value")),
                        "options": pf.get("options").cloned().unwrap_or(json!([])),
                    });
                    if pf.get("secretWriteOnly").and_then(Value::as_bool) == Some(true) {
                        nf["secret"] = json!(true);
                        nf["exists"] = json!(pf.get("exists").and_then(Value::as_bool).unwrap_or(false));
                    }
                    form_fields.push(nf);
                    let mut d = json!({"name": scalar_text(pf.get("key")), "type": scalar_text(pf.get("type"))});
                    if pf.get("secretWriteOnly").and_then(Value::as_bool) == Some(true) {
                        d["secretWriteOnly"] = json!(true);
                    }
                    form_desc.push(d);
                }
                (proj.get("revision").cloned().unwrap_or(Value::Null), cur)
            }
            None => {
                let vals = data.get("values").cloned().unwrap_or(Value::Null);
                for fd in view.get("fields").and_then(Value::as_array).unwrap_or(&vec![]) {
                    let name = scalar_text(fd.get("name"));
                    let initial = value_text(vals.get(&name), fd.get("default"));
                    form_fields.push(json!({
                        "name": name, "label": scalar_text(fd.get("label")),
                        "type": scalar_text(fd.get("type")), "value": initial,
                        "options": fd.get("options").cloned().unwrap_or(json!([])),
                        "required": fd.get("required").and_then(Value::as_bool).unwrap_or(false),
                    }));
                    form_desc.push(json!({"name": name, "type": scalar_text(fd.get("type"))}));
                }
                (Value::Null, String::new())
            }
        };
        for a in &actions {
            let rpc = a.get("rpc").and_then(Value::as_array)
                .map(|r| r.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("/"))
                .unwrap_or_default();
            let mode = if rpc == "settings/update" { "settings-update" } else { "values" };
            form_specs.push(json!({
                "label": if a.get("label").is_some() { scalar_text(a.get("label")) } else { scalar_text(a.get("name")) },
                "primary": a.get("primary").and_then(Value::as_bool).unwrap_or(false),
                "rpc": rpc, "mode": mode, "ns": json!(n_ns.clone()), "revision": n_rev.clone(), "fields": form_desc.clone(),
            }));
        }
    }
    let chat_opts = chat_options(&data.get("items").cloned().unwrap_or(Value::Null));
    let chat_note = if data.is_null() { "载入会话列表…".to_string() } else { "没有可选会话".to_string() };
    let chat_sid = {
        let cur = st.get("chat").and_then(|c| c.get("sessionId")).and_then(Value::as_str).unwrap_or("").to_string();
        if !cur.is_empty() {
            cur
        } else if chat_opts.iter().any(|o| o.get("value").and_then(Value::as_str) == Some("default")) {
            "default".to_string()
        } else {
            scalar_text(chat_opts.first().and_then(|o| o.get("value")))
        }
    };
    let chat_msgs = st.get("chat").and_then(|c| c.get("messages")).cloned().unwrap_or(json!([]));
    let chat_cancel = view.get("cancelRpc").and_then(Value::as_array).map(|a| a.len() == 2).unwrap_or(false);
    rsx! {
        if !err_msg.is_empty() {
            div { class: "cstat err", "✗ 数据面失败：{err_msg}（静态兜底）" }
        }
        if kind == "status" {
            if items.is_empty() && err_msg.is_empty() {
                div { class: "row", "暂无条目" }
            }
            for (idx, (label, value)) in status_pairs.iter().enumerate() {
                div {
                    class: "row",
                    key: "{idx}",
                    span { "{label}" }
                    span { "{value}" }
                }
            }
        }
        if kind == "list" {
            if rows.is_empty() {
                div { class: "row", "{empty_text}" }
            } else {
                table {
                    thead {
                        tr {
                            for (ci, th_label) in th_labels.iter().enumerate() {
                                th { key: "{ci}", "{th_label}" }
                            }
                        }
                    }
                    tbody {
                        for (ri, row) in rows.iter().enumerate() {
                            tr {
                                key: "{ri}",
                                for cell in cells_all.get(ri).cloned().unwrap_or_default() {
                                    td { "{cell}" }
                                }
                                if !row_actions.is_empty() {
                                    td {
                                        for (ai, a) in row_actions.iter().enumerate() {
                                            ActionBtn {
                                                key: "{ai}",
                                                k: k.clone(),
                                                view: view.clone(),
                                                row: row.clone(),
                                                a: a.clone(),
                                                body,
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        if kind == "form" {
            if show_ns {
                NsPick { options: ns_opts, current: ns_cur, k: k.clone(), body }
            }
            for f in form_fields {
                FormField { f }
            }
            div { class: "actions",
                for s in form_specs {
                    FormSave { spec: s, k: k.clone(), body }
                }
            }
        }
        if kind == "chat" {
            if chat_opts.is_empty() {
                div { class: "cstat", "{chat_note}" }
            } else {
                ChatIsland {
                    view: view.clone(),
                    k: k.clone(),
                    body,
                    opts: chat_opts,
                    sid: chat_sid,
                    msgs: chat_msgs,
                    has_cancel: chat_cancel,
                }
            }
        }
        if !act.is_empty() {
            div { class: "cstat", "{act}" }
        }
    }
}

#[component]
pub fn App() -> Element {
    let status = use_signal(|| ("载入清单…".to_string(), String::new()));
    let model = use_signal(|| Option::<Value>::None);
    // D-213：初始板从 #board= hash 直达；关闭集=板级 map（板→列表）。
    let selected = use_signal(crate::interop::hash_board);
    let closed = use_signal(crate::interop::ls_closed_map);
    // D-214：摆位记忆（板→卡→{x,y}）。
    let pos = use_signal(crate::interop::ls_pos_map);
    let bump = use_signal(|| 0u32);
    let body = use_signal(serde_json::Map::<String, Value>::new);

    // 体面管线：清单版本变 → 未加载卡逐个 ui.json→validate→dataRpc（S3a）。
    use_effect(move || {
        // S4：mux 会话事件单监听（全壳一条，逐卡按 sid 匹配折叠——引用差语义同款）。
        crate::interop::watch_session_events(move |frame| {
            if frame.get("method").and_then(Value::as_str) != Some("session/event") {
                return;
            }
            let Some(p) = frame.get("payload") else { return };
            let Some(psid) = p.get("sessionId").and_then(Value::as_str) else { return };
            let Some(ev) = p.get("event") else { return };
            let nf = json!({"sessionId": psid, "kind": ev.get("type").cloned().unwrap_or(Value::Null),
                            "data": { "text": frame_text(ev.get("data")) }, "time": ev.get("time").cloned().unwrap_or(Value::Null)});
            let mut bw = body;
            let mut g = bw.write();
            for (_kk, en) in g.iter_mut() {
                let is_chat = en.get("view").and_then(|v| v.get("kind")).and_then(Value::as_str) == Some("chat");
                if !is_chat { continue; }
                if en.get("chat").and_then(|c| c.get("sessionId")).and_then(Value::as_str) != Some(psid) { continue; }
                let st = en.get("chat").cloned().unwrap_or(json!({"sessionId": psid, "busy": false, "messages": []}));
                if let Some(next) = chat_fold_frame(&st, &nf) {
                    en["chat"] = next;
                }
            }
        });
        // 断线兜底轮询（JS 壳同款 5000ms 历史重载）。
        crate::interop::spawn_poll(move || {
            let snap = body.read().clone();
            for (kk, en) in snap.iter() {
                if en.get("view").and_then(|v| v.get("kind")).and_then(Value::as_str) != Some("chat") { continue; }
                let Some(sid) = en.get("chat").and_then(|c| c.get("sessionId")).and_then(Value::as_str) else { continue };
                let hist = rpc_join(en.get("view").and_then(|v| v.get("historyRpc")));
                if hist.is_empty() { continue; }
                let (k2, s2, b2) = (kk.clone(), sid.to_string(), body);
                spawn_local(async move { load_chat_history(k2, hist, s2, b2).await; });
            }
        }, 5000);
        let mut body = body;
        let m = model.read().clone();
        let rev = m.as_ref().and_then(|x| x.get("rev").and_then(Value::as_str)).unwrap_or("").to_string();
        if rev.is_empty() {
            return;
        }
        let cards = m.as_ref().and_then(|x| x.get("cards").and_then(Value::as_array).cloned()).unwrap_or_default();
        let mut todo: Vec<(String, String)> = Vec::new();
        {
            let mut b = body.write();
            for c in &cards {
                if c.get("bad").and_then(Value::as_bool).unwrap_or(false) {
                    continue;
                }
                let k = fk(c);
                if b.contains_key(&k) {
                    continue;
                }
                b.insert(k.clone(), json!({ "stage": "load" }));
                let pn = c.get("pluginName").and_then(Value::as_str).unwrap_or("").to_string();
                todo.push((k, pn));
            }
        }
        for (k, pn) in todo {
            let bd = body;
            spawn(async move { load_card_body(k, pn, bd).await; });
        }
    });

    // 启动一次性：首拉 + rev 轮询兜底（SSE 主通道）+ SSE + 测量脉冲。
    use_effect(move || {
        let (m, s) = (model, status);
        spawn(async move { load_manifest(m, s).await; });
        let (m1, s1) = (model, status);
        crate::interop::spawn_poll(
            move || {
                let (m, s) = (m1, s1);
                spawn_local(async move { load_manifest(m, s).await; });
            },
            10000,
        );
        let (m2, s2) = (model, status);
        crate::interop::watch_manifest(move || {
            let (m, s) = (m2, s2);
            spawn_local(async move { load_manifest(m, s).await; });
        });
        let b = bump;
        crate::interop::spawn_poll(move || { let mut b = b; *b.write() += 1; }, 1500);
        crate::interop::observe_bump(move || { let mut b = b; *b.write() += 1; });
        // D-214：拖拽落位 → 写当前板钉位（一次性写信号+存储；期间零信号风暴）。
        let mut pos_d = pos;
        let sel_d = selected;
        crate::interop::install_drag_listener(move |id, x, y| {
            let board = canvas_shell::board::board_of(sel_d.read().as_ref()).to_string();
            {
                let mut pd = pos_d;
                let mut p = pd.write();
                canvas_shell::board::set_pin(&mut p, &board, &id, x, y);
                crate::interop::ls_set_pos(&p);
            }
        });
    });

    // 测量-重排：bump 或清单版本变化 → 实测覆盖声明位（D-209 语义）。
    use_effect(move || {
        let t = bump();
        let _rev = model
            .read()
            .as_ref()
            .and_then(|m| m.get("rev").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_default();
        // D-214：拖拽中让路（松手后下一脉冲自然收敛）。
        if crate::interop::is_dragging() {
            return;
        }
        crate::interop::reobserve();
        let cols = columns_for_width(crate::interop::workbench_width());
        let metrics = crate::interop::card_metrics();
        if metrics.is_empty() || t == 0 {
            return;
        }
        // D-214：钉卡不进自动装箱；钉位直接并入成品，总高含钉卡底边。
        let board_now = canvas_shell::board::board_of(selected.read().as_ref()).to_string();
        let pins = canvas_shell::board::pins_of(&pos.read(), &board_now);
        let items: Vec<Value> = metrics
            .iter()
            .filter(|(k, _, _)| !pins.iter().any(|(pk, _, _)| pk == k))
            .map(|(k, h, w)| json!({"key": k, "w": (*w as i64), "hPx": *h as i64}))
            .collect();
        let heights: std::collections::HashMap<String, i64> =
            metrics.iter().map(|(k, h, _)| (k.clone(), *h as i64)).collect();
        let (positions, auto_total) = layout_measured(&items, cols);
        let auto: Vec<(String, i64, i64)> = positions
            .into_iter()
            .map(|p| (p.key, p.col as i64 * (GRID_COL + GRID_GAP), p.y_px))
            .collect();
        let (merged, total) = canvas_shell::board::merge_pinned(auto, auto_total, &pins, &heights);
        let mut map = serde_json::Map::new();
        for (k, x, y) in merged {
            map.insert(k, json!([x, y]));
        }
        crate::interop::set_positions(&map, total as f64);
    });

    // ---- 渲染输入（读快照） ----
    let modelv = model.read().clone();
    let (statustxt, statuscls) = { let g = status.read(); (g.0.clone(), g.1.clone()) };
    let sel = selected.read().clone();
    let closedmap = closed.read().clone();
    let groups = modelv
        .as_ref()
        .and_then(|m| m.get("groups").and_then(Value::as_array).cloned())
        .unwrap_or_default();
    // D-213：板级校验——hash/选中指向不存在的组（如热插拔后）→ 回总览。
    let sel_eff = sel
        .as_ref()
        .filter(|t| groups.iter().any(|g| g.get("type").and_then(Value::as_str) == Some(t.as_str())))
        .cloned();
    let board = canvas_shell::board::board_of(sel_eff.as_ref()).to_string();
    let closedv = canvas_shell::board::closed_for(&closedmap, &board);
    // D-214：本板钉位（渲染层钉位优先；重置按钮可见性同源）。
    let posv = pos.read().clone();
    let pins = canvas_shell::board::pins_of(&posv, &board);
    let allcards = modelv
        .as_ref()
        .and_then(|m| m.get("cards").and_then(Value::as_array).cloned())
        .unwrap_or_default();
    let rev = modelv.as_ref().and_then(|m| m.get("rev").and_then(Value::as_str)).unwrap_or("").to_string();
    let revshort: String = rev.chars().take(12).collect();
    let visible = visible_cards(&modelv, &sel_eff, &closedv);
    let bodyv = body.read().clone();
    let cols = columns_for_width(crate::interop::workbench_width());
    let grid = layout_grid(
        &visible.iter().map(|c| json!({"key": fk(c), "w": c.get("size").and_then(|s| s.get("w")).and_then(Value::as_i64).unwrap_or(2), "h": c.get("size").and_then(|s| s.get("h")).and_then(Value::as_i64).unwrap_or(3)})).collect::<Vec<_>>(),
        cols,
    );
    let declared: Vec<(String, f64, f64)> = grid
        .0
        .iter()
        .map(|p| (p.key.clone(), (p.col as i64 * (GRID_COL + GRID_GAP)) as f64, (p.row * (100 + GRID_GAP)) as f64))
        .collect();
    let mut pos_r = pos;
    let board_r = board.clone();
    let mut bump_r = bump;

    rsx! {
        header {
            h1 { "服务装配单元 · 桌布 " }
            span { class: "rev", id: "rev", title: "清单内容哈希（rev）",
                if rev.is_empty() { "" } else { "rev {revshort}…" }
            }
            span { class: statuscls, id: "status",
                if modelv.is_some() { "✓ 清单 {allcards.len()} 卡" } else { "{statustxt}" }
            }
            if canvas_shell::board::has_pins(&posv, &board) {
                button {
                    id: "reset-positions",
                    class: "reset-pos",
                    title: "清掉本桌板的钉位，回到自动排布",
                    onclick: move |_| {
                        {
                            let mut pr = pos_r;
                            let mut p = pr.write();
                            canvas_shell::board::reset_board(&mut p, &board_r);
                            crate::interop::ls_set_pos(&p);
                        }
                        *bump_r.write() += 1;
                    },
                    "⟲ 重置摆位"
                }
            }
        }
        div { class: "layout",
            nav { id: "sidebar", "aria-label": "卡片分类",
                button {
                    class: if sel_eff.is_none() { "all active" } else { "all" },
                    onclick: move |_| {
                        let mut s = selected;
                        s.set(None);
                        crate::interop::set_hash_board(canvas_shell::board::BOARD_ALL);
                    },
                    "全部（{allcards.len()}）"
                }
                for g in groups.iter() {
                    {
                        let gtype = g.get("type").and_then(Value::as_str).unwrap_or("").to_string();
                        let gcount = g.get("count").and_then(Value::as_i64).unwrap_or(0);
                        let gcards: Vec<Value> = g.get("cards").and_then(Value::as_array).cloned().unwrap_or_default();
                        let active = sel_eff.as_deref() == Some(gtype.as_str());
                        let mut sel_h = selected;
                        let gt = gtype.clone();
                        rsx! {
                            button {
                                class: if active { "group-title active" } else { "group-title" },
                                onclick: move |_| {
                                    let cur = sel_h.read().clone();
                                    let next: Option<String> =
                                        if cur.as_deref() == Some(gt.as_str()) { None } else { Some(gt.clone()) };
                                    crate::interop::set_hash_board(
                                        next.as_deref().unwrap_or(canvas_shell::board::BOARD_ALL),
                                    );
                                    sel_h.set(next);
                                },
                                "{gtype}"
                                span { class: "count", "{gcount}" }
                            }
                            for c in gcards.iter() {
                                {
                                    let k = fk(c);
                                    let bad = c.get("bad").and_then(Value::as_bool).unwrap_or(false);
                                    // D-213：卡片项灰显状态按其「视野板」——当前板是本组或总览
                                    // 时看当前板闭合集，否则看该卡所属组的原生板。
                                    let view_board =
                                        if board == "all" || board == gtype { board.clone() } else { gtype.clone() };
                                    let shut =
                                        canvas_shell::board::closed_for(&closedmap, &view_board).contains(&k);
                                    let label = format!("{}{}", if bad { "✗ " } else { "" }, c.get("pluginName").and_then(Value::as_str).unwrap_or("?"));
                                    let mut closed_h = closed;
                                    let kk = k.clone();
                                    // D-213 点击语义：灰显=本视野板重开（原地，不跳板）；
                                    // 正常标题=切到所属组桌板 + 聚焦（顺带重开目标/当前板关闭态）。
                                    let gt2 = gtype.clone();
                                    let board2 = board.clone();
                                    let vb2 = view_board.clone();
                                    let shut2 = shut;
                                    let mut sel_w = selected;
                                    rsx! {
                                    button {
                                        class: if shut { "name shut" } else { "name" },
                                        title: if shut { "已关闭——点击在本桌板重开" } else { "切到所属桌板并聚焦" },
                                        onclick: move |_| {
                                            if shut2 {
                                                {
                                                    let mut c = closed_h.write();
                                                    canvas_shell::board::open_on(&mut c, &vb2, &kk);
                                                }
                                                crate::interop::ls_set_closed_map(&closed_h.read().clone());
                                                if vb2 != board2 {
                                                    crate::interop::set_hash_board(&gt2);
                                                    sel_w.set(Some(gt2.clone()));
                                                }
                                                crate::interop::focus_card(&kk);
                                                return;
                                            }
                                            {
                                                let mut c = closed_h.write();
                                                canvas_shell::board::open_on(&mut c, &gt2, &kk);
                                                if gt2 != board2 {
                                                    canvas_shell::board::open_on(&mut c, &board2, &kk);
                                                }
                                            }
                                            crate::interop::ls_set_closed_map(&closed_h.read().clone());
                                            crate::interop::set_hash_board(&gt2);
                                            sel_w.set(Some(gt2.clone()));
                                            crate::interop::focus_card(&kk);
                                        },
                                        "{label}"
                                    }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            main { id: "workbench",
                if visible.is_empty() {
                    div { class: "empty",
                        if !closedv.is_empty() { "本组卡片已全部关闭——点左侧灰显标题可重新打开。" }
                        else { "还没有服务装配单元声明 UI——向 wasm-plugins/<name>/web/ui.json 提交 v2 卡片声明即自动出现。" }
                    }
                }
                for c in visible.iter() {
                    {
                        let k = fk(c);
                        let bad = c.get("bad").and_then(Value::as_bool).unwrap_or(false);
                        let w = c.get("size").and_then(|s| s.get("w")).and_then(Value::as_i64).unwrap_or(2);
                        let h = c.get("size").and_then(|s| s.get("h")).and_then(Value::as_i64).unwrap_or(3);
                        let min_h = h as f64 * 100.0 + (h as f64 - 1.0) * GRID_GAP as f64;
                        // D-214：钉位优先于自动/声明位。
                        let (dx, dy) = match pins.iter().find(|(pk, _, _)| *pk == k) {
                            Some((_, px, py)) => (*px, *py),
                            None => declared
                                .iter()
                                .find(|(dk, _, _)| *dk == k)
                                .map(|d| (d.1, d.2))
                                .unwrap_or((0.0, 0.0)),
                        };
                        let title = c.get("title").and_then(Value::as_str).unwrap_or("").to_string();
                        let ty = c.get("type").and_then(Value::as_str).unwrap_or("").to_string();
                        let pn = c.get("pluginName").and_then(Value::as_str).unwrap_or("").to_string();
                        let emsg = c.get("error").and_then(|e| e.get("message")).and_then(Value::as_str).unwrap_or("").to_string();
                        let ecode = c.get("error").and_then(|e| e.get("code")).and_then(Value::as_str).unwrap_or("").to_string();
                        let mut closed_b = closed;
                        let kk = k.clone();
                        let board_c = board.clone();
                        rsx! {
                        section {
                            key: "{kk}",
                            class: if bad { "card fail" } else { "card" },
                            id: "{k}",
                            style: "left:{dx}px;top:{dy}px;width:{grid_px(w)}px;min-height:{min_h}px",
                            div { class: "cap",
                                "{title}"
                                button {
                                    class: "card-close",
                                    title: "关闭卡片（仅本桌板；侧栏点标题可重开）",
                                    onclick: move |_| {
                                        {
                                            let mut c = closed_b.write();
                                            canvas_shell::board::close_on(&mut c, &board_c, &kk);
                                        }
                                        crate::interop::ls_set_closed_map(&closed_b.read().clone());
                                    },
                                    "✕"
                                }
                            }
                            div { class: "badges",
                                span { class: "type", "{ty}" }
                                span { class: "plugin", "{pn}" }
                                span { class: "size", "格 {w}×{h}" }
                            }
                            if bad {
                                div { class: "fail-msg", "✗ {emsg}" }
                                div { class: "code", "code={ecode}" }
                            } else {
                                { card_body(k.clone(), bodyv.get(&k).cloned(), body) }
                            }
                        }
                        }
                    }
                }
            }
        }
    }
}

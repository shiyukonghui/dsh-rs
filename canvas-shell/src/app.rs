//! Dioxus 桌布壳（S2 骨架）：清单/SSE/轮询 → 侧栏五分类 → 实测布局卡框 → ✕ 关闭。
//! 纪律：可证逻辑零在此层（全在 canvas_shell lib）；S2 只立壳，form/status/list/chat
//! 数据体按设计文档在 S3/S4 接线（诚实占位，不伪造渲染）。

use dioxus::prelude::*;
use serde_json::{json, Value};

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

/// Vec<Element> → 可作子节点的 Vec<VNode>（rsx 只收 VNode 迭代器）。
fn nodes(v: Vec<Element>) -> Vec<dioxus::prelude::VNode> {
    v.into_iter().map(|r| r.unwrap_or_default()).collect()
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
    body.write().insert(fk, entry);
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
        .map(|r| columns.iter().map(|c| scalar_text(r.get(&scalar_text(c.get("key"))))).collect())
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
        let (mut n_rev, n_ns) = match &ff {
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
            div { class: "cstat", "chat 岛待 S4（选择/历史/发送/停止/SSE）" }
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
    let selected = use_signal(|| Option::<String>::None);
    let closed = use_signal(|| crate::interop::ls_closed());
    let bump = use_signal(|| 0u32);
    let body = use_signal(serde_json::Map::<String, Value>::new);

    // 体面管线：清单版本变 → 未加载卡逐个 ui.json→validate→dataRpc（S3a）。
    use_effect(move || {
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
            let mut bd = body;
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
                spawn(async move { load_manifest(m, s).await; });
            },
            10000,
        );
        let (m2, s2) = (model, status);
        crate::interop::watch_manifest(move || {
            let (m, s) = (m2, s2);
            spawn(async move { load_manifest(m, s).await; });
        });
        let b = bump;
        crate::interop::spawn_poll(move || { let mut b = b; *b.write() += 1; }, 1500);
        crate::interop::observe_bump(move || { let mut b = b; *b.write() += 1; });
    });

    // 测量-重排：bump 或清单版本变化 → 实测覆盖声明位（D-209 语义）。
    use_effect(move || {
        let t = bump();
        let _rev = model
            .read()
            .as_ref()
            .and_then(|m| m.get("rev").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_default();
        crate::interop::reobserve();
        let cols = columns_for_width(crate::interop::workbench_width());
        let metrics = crate::interop::card_metrics();
        if metrics.is_empty() || t == 0 {
            return;
        }
        let items: Vec<Value> = metrics
            .iter()
            .map(|(k, h, w)| json!({"key": k, "w": (*w as i64), "hPx": *h as i64}))
            .collect();
        let (positions, total) = layout_measured(&items, cols);
        let mut map = serde_json::Map::new();
        for p in positions {
            map.insert(p.key, json!([p.col as i64 * (GRID_COL + GRID_GAP), p.y_px]));
        }
        crate::interop::set_positions(&map, total as f64);
    });

    // ---- 渲染输入（读快照） ----
    let modelv = model.read().clone();
    let (statustxt, statuscls) = { let g = status.read(); (g.0.clone(), g.1.clone()) };
    let sel = selected.read().clone();
    let closedv = closed.read().clone();
    let groups = modelv
        .as_ref()
        .and_then(|m| m.get("groups").and_then(Value::as_array).cloned())
        .unwrap_or_default();
    let allcards = modelv
        .as_ref()
        .and_then(|m| m.get("cards").and_then(Value::as_array).cloned())
        .unwrap_or_default();
    let rev = modelv.as_ref().and_then(|m| m.get("rev").and_then(Value::as_str)).unwrap_or("").to_string();
    let revshort: String = rev.chars().take(12).collect();
    let visible = visible_cards(&modelv, &sel, &closedv);
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

    rsx! {
        header {
            h1 { "服务装配单元 · 桌布 " }
            span { class: "rev", id: "rev", title: "清单内容哈希（rev）",
                if rev.is_empty() { "" } else { "rev {revshort}…" }
            }
            span { class: statuscls, id: "status",
                if modelv.is_some() { "✓ 清单 {allcards.len()} 卡" } else { "{statustxt}" }
            }
        }
        div { class: "layout",
            nav { id: "sidebar", "aria-label": "卡片分类",
                button {
                    class: if sel.is_none() { "all active" } else { "all" },
                    onclick: move |_| {
                        let mut s = selected;
                        s.set(None);
                    },
                    "全部（{allcards.len()}）"
                }
                for g in groups.iter() {
                    {
                        let gtype = g.get("type").and_then(Value::as_str).unwrap_or("").to_string();
                        let gcount = g.get("count").and_then(Value::as_i64).unwrap_or(0);
                        let gcards: Vec<Value> = g.get("cards").and_then(Value::as_array).cloned().unwrap_or_default();
                        let active = sel.as_deref() == Some(gtype.as_str());
                        let mut sel_h = selected;
                        let gt = gtype.clone();
                        rsx! {
                            button {
                                class: if active { "group-title active" } else { "group-title" },
                                onclick: move |_| {
                                    let cur = sel_h.read().clone();
                                    sel_h.set(if cur.as_deref() == Some(gt.as_str()) { None } else { Some(gt.clone()) });
                                },
                                "{gtype}"
                                span { class: "count", "{gcount}" }
                            }
                            for c in gcards.iter() {
                                {
                                    let k = fk(c);
                                    let bad = c.get("bad").and_then(Value::as_bool).unwrap_or(false);
                                    let shut = closedv.contains(&k);
                                    let label = format!("{}{}", if bad { "✗ " } else { "" }, c.get("pluginName").and_then(Value::as_str).unwrap_or("?"));
                                    let mut closed_h = closed;
                                    let kk = k.clone();
                                    rsx! {
                                    button {
                                        class: if shut { "name shut" } else { "name" },
                                        title: if shut { "已关闭——点击重新打开" } else { "" },
                                        onclick: move |_| {
                                            {
                                                let mut c = closed_h.write();
                                                c.retain(|x| x != &kk);
                                            }
                                            crate::interop::ls_set_closed(&closed_h.read().clone());
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
                for (idx, c) in visible.iter().enumerate() {
                    {
                        let k = fk(c);
                        let bad = c.get("bad").and_then(Value::as_bool).unwrap_or(false);
                        let w = c.get("size").and_then(|s| s.get("w")).and_then(Value::as_i64).unwrap_or(2);
                        let h = c.get("size").and_then(|s| s.get("h")).and_then(Value::as_i64).unwrap_or(3);
                        let min_h = h as f64 * 100.0 + (h as f64 - 1.0) * GRID_GAP as f64;
                        let (dx, dy) = declared.iter().find(|(dk, _, _)| *dk == k).map(|d| (d.1, d.2)).unwrap_or((0.0, 0.0));
                        let title = c.get("title").and_then(Value::as_str).unwrap_or("").to_string();
                        let ty = c.get("type").and_then(Value::as_str).unwrap_or("").to_string();
                        let pn = c.get("pluginName").and_then(Value::as_str).unwrap_or("").to_string();
                        let emsg = c.get("error").and_then(|e| e.get("message")).and_then(Value::as_str).unwrap_or("").to_string();
                        let ecode = c.get("error").and_then(|e| e.get("code")).and_then(Value::as_str).unwrap_or("").to_string();
                        let mut closed_b = closed;
                        let kk = k.clone();
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
                                    title: "关闭卡片（侧栏点标题可重开）",
                                    onclick: move |_| {
                                        {
                                            let mut c = closed_b.write();
                                            if !c.contains(&kk) { c.push(kk.clone()); }
                                        }
                                        crate::interop::ls_set_closed(&closed_b.read().clone());
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

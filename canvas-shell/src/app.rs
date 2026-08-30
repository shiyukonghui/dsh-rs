//! Dioxus 桌布壳（S2 骨架）：清单/SSE/轮询 → 侧栏五分类 → 实测布局卡框 → ✕ 关闭。
//! 纪律：可证逻辑零在此层（全在 canvas_shell lib）；S2 只立壳，form/status/list/chat
//! 数据体按设计文档在 S3/S4 接线（诚实占位，不伪造渲染）。

use dioxus::prelude::*;
use serde_json::{json, Value};

use canvas_shell::layout::{columns_for_width, layout_grid, layout_measured, GRID_COL, GRID_GAP};
use canvas_shell::model::{build_model, focus_key};
use canvas_shell::values::poll_decision;

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

#[component]
pub fn App() -> Element {
    let status = use_signal(|| ("载入清单…".to_string(), String::new()));
    let model = use_signal(|| Option::<Value>::None);
    let selected = use_signal(|| Option::<String>::None);
    let closed = use_signal(|| crate::interop::ls_closed());
    let bump = use_signal(|| 0u32);

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
                                div { class: "cstat", "体面待 S3：拉取 /plugins/{pn}/ui.json + 声明校验 + 视图渲染" }
                            }
                        }
                        }
                    }
                }
            }
        }
    }
}

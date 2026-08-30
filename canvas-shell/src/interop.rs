//! JS 互操作薄层（仅 wasm32）：fetch/SSE/localStorage/DOM 度量。
//! 纪律：业务逻辑零在此层——只搬运（可证逻辑全在 canvas_shell lib）。

use canvas_shell::values::rpc_envelope;
use js_sys::JSON;
use serde_json::{json, Value};
use std::cell::Cell;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{EventSource, HtmlElement, MessageEvent, Request, RequestInit, RequestMode, ResizeObserver};

fn win() -> web_sys::Window {
    web_sys::window().expect("no window")
}
fn doc() -> web_sys::Document {
    win().document().expect("no document")
}
fn ls() -> Option<web_sys::Storage> {
    win().local_storage().ok().flatten()
}

fn js_cb<F: FnMut() + 'static>(f: F) -> js_sys::Function {
    Closure::wrap(Box::new(f) as Box<dyn FnMut()>).into_js_value().unchecked_into()
}

fn to_js(v: &Value) -> JsValue {
    JSON::parse(&v.to_string()).unwrap_or(JsValue::NULL)
}

fn from_js(v: &JsValue) -> Result<Value, String> {
    let s = JSON::stringify(v).map_err(|_| "不可序列化".to_string())?;
    serde_json::from_str(&String::from(s)).map_err(|e| e.to_string())
}

/// POST /api/<method>（client-request 信封）→ arm 层 {ok,value|error}。
/// 信封解包单点（D-207 语义移植）。
pub async fn fetch_rpc(method: &str, args: Value) -> Result<Value, String> {
    let body = JSON::stringify(&to_js(&rpc_envelope(method, Some(args), "rust-shell")))
        .map_err(|_| "body".to_string())?;
    let opts = RequestInit::new();
    opts.set_method("POST");
    opts.set_mode(RequestMode::SameOrigin);
    opts.set_body(&body);
    let req = Request::new_with_str_and_init(&format!("/api/{}", method), &opts)
        .map_err(|e| format!("req: {:?}", e))?;
    req.headers().set("content-type", "application/json").map_err(|e| format!("hdr: {:?}", e))?;
    let resp_val = JsFuture::from(win().fetch_with_request(&req))
        .await
        .map_err(|e| format!("fetch: {:?}", e))?;
    let resp: web_sys::Response = resp_val.dyn_into().map_err(|e| format!("resp: {:?}", e))?;
    let json = from_js(&JsFuture::from(resp.json().map_err(|e| format!("json: {:?}", e))?).await.map_err(|e| format!("jsonf: {:?}", e))?)?;
    Ok(json.get("result").cloned().unwrap_or(json))
}

/// GET 静态 JSON（ui.json 体面——非信封通道）。
pub async fn fetch_get_json(url: &str) -> Result<Value, String> {
    let resp_val = JsFuture::from(win().fetch_with_str(url))
        .await
        .map_err(|e| format!("fetch: {:?}", e))?;
    let resp: web_sys::Response = resp_val.dyn_into().map_err(|e| format!("resp: {:?}", e))?;
    from_js(&JsFuture::from(resp.json().map_err(|e| format!("json: {:?}", e))?).await.map_err(|e| format!("jsonf: {:?}", e))?)
}

/// 读表单控件：卡容器 id 内 input/select/textarea → {name: 字符串}（checkbox 读 checked）。
pub fn read_form(card_id: &str) -> Value {
    let mut map = serde_json::Map::new();
    let Some(root) = doc().get_element_by_id(card_id) else { return Value::Object(map) };
    let Ok(nodes) = root.query_selector_all("input,select,textarea") else { return Value::Object(map) };
    for i in 0..nodes.length() {
        let Some(el) = nodes.item(i).and_then(|n| n.dyn_into::<HtmlElement>().ok()) else { continue };
        let name = el.get_attribute("name").unwrap_or_default();
        if name.is_empty() { continue; }
        if let Some(inp) = el.dyn_ref::<web_sys::HtmlInputElement>() {
            if inp.type_() == "checkbox" {
                map.insert(name, json!(if inp.checked() { "true" } else { "false" }));
                continue;
            }
            map.insert(name, json!(inp.value()));
        } else if let Some(sel) = el.dyn_ref::<web_sys::HtmlSelectElement>() {
            map.insert(name, json!(sel.value()));
        } else if let Some(ta) = el.dyn_ref::<web_sys::HtmlTextAreaElement>() {
            map.insert(name, json!(ta.value()));
        }
    }
    Value::Object(map)
}

/// 原生确认框（needsConfirm 严格 true 才走到这里）。
pub fn confirm_dialog(msg: &str) -> bool {
    win().confirm_with_message(msg).unwrap_or(false)
}

/// SSE 会话事件通道：/api/events.mux 帧原样转发（帧筛选/折叠在 Rust 侧）。
pub fn watch_session_events<F: FnMut(Value) + 'static>(f: F) {
    let Ok(es) = EventSource::new("/api/events.mux") else { return };
    let mut f = f;
    let outer = Closure::wrap(Box::new(move |ev: MessageEvent| {
        let Some(txt) = ev.data().as_string() else { return };
        if let Ok(v) = serde_json::from_str::<Value>(&txt) {
            f(v);
        }
    }) as Box<dyn FnMut(MessageEvent)>);
    es.set_onmessage(Some(outer.as_ref().unchecked_ref()));
    outer.forget();
}

/// 写控件值（发送后清空输入框）。
pub fn set_input_value(card_id: &str, name: &str, val: &str) {
    let sel = format!("[id='{}'] [name='{}']", card_id.replace('\'', ""), name.replace('\'', ""));
    if let Some(el) = doc().query_selector(&sel).ok().flatten().and_then(|e| e.dyn_into::<HtmlElement>().ok()) {
        let _ = el.set_attribute("value", val);
        if let Some(inp) = el.dyn_ref::<web_sys::HtmlInputElement>().cloned() {
            inp.set_value(val);
        }
    }
}

/// 消息区滚到底（paint 尾步）。
pub fn scroll_chat_bottom(card_id: &str) {
    let sel = format!("[id='{}'] .chat-msgs", card_id.replace('\'', ""));
    if let Some(el) = doc().query_selector(&sel).ok().flatten().and_then(|e| e.dyn_into::<web_sys::Element>().ok()) {
        let h = el.scroll_height();
        el.set_scroll_top(h);
    }
}

pub fn spawn_poll<F: FnMut() + 'static>(f: F, ms: i32) {
    let _h = win().set_interval_with_callback_and_timeout_and_arguments_0(&js_cb(f), ms);
}

/// SSE 主通道：/plugins/events 的 ui-manifest-changed 帧 → cb（帧形按 live 抓样解析）。
pub fn watch_manifest<F: FnMut() + 'static>(f: F) {
    let Ok(es) = EventSource::new("/plugins/events") else { return };
    let inner = js_cb(f);
    let outer = Closure::wrap(Box::new(move |ev: MessageEvent| {
        let Some(txt) = ev.data().as_string() else { return };
        if let Ok(v) = serde_json::from_str::<Value>(&txt) {
            if v.get("type").and_then(Value::as_str) == Some("ui-manifest-changed") {
                let _ = inner.call0(&JsValue::NULL);
            }
        }
    }) as Box<dyn FnMut(MessageEvent)>);
    es.set_onmessage(Some(outer.as_ref().unchecked_ref()));
    outer.forget();
}

// ---------- localStorage（D-213：板级 closed map，旧全局键一次性迁移） ----------

const LS_CLOSED_V2: &str = "dsh.canvas.closed.v2";
const LS_CLOSED_OLD: &str = "dsh.canvas.closed";

/// 读板级关闭集：v2 对象优先；无 v2 时把旧全局数组迁移为「全部」板并立即回写 v2。
pub fn ls_closed_map() -> serde_json::Map<String, Value> {
    let Some(s) = ls() else { return Default::default() };
    if let Some(txt) = s.get_item(LS_CLOSED_V2).ok().flatten() {
        if let Ok(v) = serde_json::from_str::<Value>(&txt) {
            if let Some(m) = v.as_object() {
                return m.clone();
            }
        }
    }
    let old_txt = s.get_item(LS_CLOSED_OLD).ok().flatten().unwrap_or_default();
    let old: Vec<String> = serde_json::from_str::<Vec<String>>(&old_txt).unwrap_or_default();
    let m = canvas_shell::board::migrate_legacy(&old);
    if !m.is_empty() {
        let _ = s.set_item(LS_CLOSED_V2, &Value::Object(m.clone()).to_string());
    }
    m
}

pub fn ls_set_closed_map(map: &serde_json::Map<String, Value>) {
    if let Some(s) = ls() {
        let _ = s.set_item(LS_CLOSED_V2, &Value::Object(map.clone()).to_string());
    }
}

// ---------- URL hash（板深链 #board=<id>） ----------

/// 读当前 hash 的 board 值（无/非法 → None；"all" 也返回 None=总览）。
pub fn hash_board() -> Option<String> {
    let h = web_sys::window()?.location().hash().ok()?;
    let v = h.trim_start_matches('#').strip_prefix("board=")?.to_string();
    if v.is_empty() || v == canvas_shell::board::BOARD_ALL {
        None
    } else {
        Some(v)
    }
}

/// 写 hash 到指定板（不影响页面加载；total 总览也显式写 #board=all 便于收藏）。
pub fn set_hash_board(board: &str) {
    if let Some(w) = web_sys::window() {
        let _ = w.location().set_hash(&format!("board={board}"));
    }
}

// ---------- 布局度量 ----------

pub fn workbench_width() -> f64 {
    doc().get_element_by_id("workbench")
        .and_then(|e| e.dyn_into::<HtmlElement>().ok())
        .map(|e| e.client_width() as f64)
        .unwrap_or(1200.0)
}

/// 每张在排卡：[id, offsetHeight, 声明格宽 w]。w 由宽反推（= round((w+gap)/(col+gap))），
/// offsetHeight 天然 >= min-height，故 hPx 直接取实测（D-209 语义）。
pub fn card_metrics() -> Vec<(String, f64, f64)> {
    let mut out = Vec::new();
    let Ok(nodes) = doc().query_selector_all("#workbench .card") else { return out };
    for i in 0..nodes.length() {
        let Some(node) = nodes.item(i) else { continue };
        let Ok(el) = node.dyn_into::<HtmlElement>() else { continue };
        let key = el.id();
        if key.is_empty() { continue; }
        let h = el.offset_height() as f64;
        // 反推声明格宽 w：width = w*COL + (w-1)*GAP ⇒ w = (width+GAP)/(COL+GAP)
        let w = (((el.offset_width() as f64) + 10.0) / (260.0 + 10.0)).round().max(1.0);
        out.push((key, h, w));
    }
    out
}

/// 写回坐标（按 id 直取）+ 工作区总高。positions: key → [x, y]。
pub fn set_positions(positions: &serde_json::Map<String, Value>, total_h: f64) {
    for (key, xy) in positions {
        if let Some(el) = doc().get_element_by_id(key).and_then(|e| e.dyn_into::<HtmlElement>().ok()) {
            let x = xy.get(0).and_then(Value::as_f64).unwrap_or(0.0);
            let y = xy.get(1).and_then(Value::as_f64).unwrap_or(0.0);
            let _ = el.style().set_property("left", &format!("{}px", x));
            let _ = el.style().set_property("top", &format!("{}px", y));
        }
    }
    if let Some(wb) = doc().get_element_by_id("workbench").and_then(|e| e.dyn_into::<HtmlElement>().ok()) {
        let _ = wb.style().set_property("min-height", &format!("{}px", total_h + 28.0));
    }
}

/// 聚焦：滚动 + 高亮 1600ms（focusCard 语义移植；id 直取，无选择器转义）。
pub fn focus_card(key: &str) {
    let Some(el) = doc().get_element_by_id(key).and_then(|e| e.dyn_into::<HtmlElement>().ok()) else { return };
    el.scroll_into_view();
    let _ = el.class_list().add_1("focus-hl");
    let el2 = el.clone();
    let rm = Closure::wrap(Box::new(move || {
        let _ = el2.class_list().remove_1("focus-hl");
    }) as Box<dyn FnMut()>);
    let _h = win().set_timeout_with_callback_and_timeout_and_arguments_0(rm.as_ref().unchecked_ref::<js_sys::Function>(), 1600);
    rm.forget();
}

/// RO → debounce(200ms) → bump。S2 另配 1500ms 脉冲兜底（RO 精细化列后续打磨）。
pub fn observe_bump<F: FnMut() + 'static>(f: F) {
    let bump = js_cb(f);
    let handle = Rc::new(Cell::new(0i32));
    let outer = js_cb(move || {
        let b = bump.clone();
        let fire = js_cb(move || {
            let _ = b.call0(&JsValue::NULL);
        });
        let w = win();
        if handle.get() != 0 {
            w.clear_timeout_with_handle(handle.get());
        }
        if let Ok(id) = w.set_timeout_with_callback_and_timeout_and_arguments_0(&fire, 200) {
            handle.set(id);
        }
    });
    if let Ok(ro) = ResizeObserver::new(&outer) {
        observe_all(&ro);
        RO_SLOT.with(|slot| {
            if let Some(prev) = slot.borrow_mut().replace(ro) {
                prev.disconnect();
            }
        });
    }
}

fn observe_all(ro: &ResizeObserver) {
    if let Ok(nodes) = doc().query_selector_all("#workbench .card") {
        for i in 0..nodes.length() {
            if let Some(el) = nodes.item(i).and_then(|n| n.dyn_into::<web_sys::Element>().ok()) {
                ro.observe(&el);
            }
        }
    }
}

pub fn reobserve() {
    RO_SLOT.with(|slot| {
        if let Some(ro) = slot.borrow().as_ref() {
            ro.disconnect();
            observe_all(ro);
        }
    });
}

thread_local! {
    static RO_SLOT: std::cell::RefCell<Option<ResizeObserver>> = const { std::cell::RefCell::new(None) };
}


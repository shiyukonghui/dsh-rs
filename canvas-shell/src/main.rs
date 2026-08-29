//! canvas-shell：桌布壳的 Dioxus 实现（D-210 选项 C）。
//! 本文件 = 工具链可行性哨兵：拉真清单、渲染卡数、可点按钮。
//! 功能对齐矩阵见 .spec/service-assembly-ui-shell-dioxus/design.md。

use dioxus::prelude::*;

fn main() {
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    let mut clicks = use_signal(|| 0u32);
    let mut status = use_signal(|| "载入清单…".to_string());
    use_effect(move || {
        spawn(async move {
            status.set(match fetch_card_count().await {
                Ok(n) => format!("🦀 壳存活，cards={n}"),
                Err(e) => format!("✗ 壳错误：{e}"),
            });
        });
    });
    rsx! {
        div { id: "rust-shell-probe", "{status}" }
        button { onclick: move |_| clicks += 1, "点击计数: {clicks}" }
    }
}

#[cfg(target_arch = "wasm32")]
async fn fetch_card_count() -> Result<usize, String> {
    use wasm_bindgen::prelude::*;
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::JsFuture;
    let opts = web_sys::RequestInit::new();
    opts.set_method("POST");
    opts.set_mode(web_sys::RequestMode::SameOrigin);
    opts.set_body(&JsValue::from_str(
        r#"{"type":"client-request","rpcId":"shell","method":"uiManifest/list","payload":{}}"#,
    ));
    let req = web_sys::Request::new_with_str_and_init("/api/uiManifest/list", &opts)
        .map_err(|e| format!("req: {e:?}"))?;
    req.headers()
        .set("content-type", "application/json")
        .map_err(|e| format!("hdr: {e:?}"))?;
    let window = web_sys::window().ok_or("no window")?;
    let resp_val = JsFuture::from(window.fetch_with_request(&req))
        .await
        .map_err(|e| format!("fetch: {e:?}"))?;
    let resp: web_sys::Response = resp_val.dyn_into().map_err(|e| format!("resp: {e:?}"))?;
    let json = JsFuture::from(resp.json().map_err(|e| format!("json: {e:?}"))?)
        .await
        .map_err(|e| format!("jsonf: {e:?}"))?;
    let mut cur = json;
    for key in ["result", "value", "cards"] {
        cur = js_sys::Reflect::get(&cur, &JsValue::from_str(key))
            .map_err(|_| format!("missing {key}"))?;
    }
    let arr = cur
        .dyn_into::<js_sys::Array>()
        .map_err(|_| "cards 非数组".to_string())?;
    Ok(arr.length() as usize)
}

#[cfg(not(target_arch = "wasm32"))]
async fn fetch_card_count() -> Result<usize, String> {
    Err("native stub".into())
}

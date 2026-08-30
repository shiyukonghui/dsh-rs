//! canvas-shell：桌布壳的 Dioxus 实现（D-210 选项 C）。
//! 宿主目标 = 无实体桩（纯逻辑可测面在 lib）；wasm32 = 真壳。

#[cfg(target_arch = "wasm32")]
mod app;
#[cfg(target_arch = "wasm32")]
mod interop;

#[cfg(target_arch = "wasm32")]
fn main() {
    dioxus::launch(app::App);
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    println!("canvas-shell：宿主目标无实体——纯逻辑测试面见 cargo test --lib。");
}

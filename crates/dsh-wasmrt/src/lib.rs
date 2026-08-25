//! `dsh-wasmrt` —— WASM 插件后端（对应旧方案 M2-M3 验收）。
//!
//! 两条等价路径：
//! - **core-module C ABI**（`plugin.rs`，M6）：`alloc`/`dealloc`/`plugin_apply`/
//!   `plugin_handle_event`/`plugin_dispose` + 宿主 `host_*` 导入，线性内存 FFI。
//! - **组件模型**（`component.rs`，M8）：`wasmtime::component::bindgen!` 从
//!   `wit/dsh-plugin.wit` 生成类型化接口，加载 `wasm-tools component new` 编码的组件。
//!
//! 两者都把 WASM 插件适配为 [`dsh_core::Plugin`]；副作用（provide/on）经 fiber
//! 机制注册、随卸载自动回滚；能力授予用 [`Capabilities`]，host 导入侧检查。
//!
//! **DSH 层 loop 宿主**（`loop.rs`，M8）：把实现 `dsh-loop` world 的 WASM 组件
//! （如 `echo-loop`）适配为 [`Plugin`]——「loop 本身可替换」的 WASM 形态；
//! session/tools/llm 缝由宿主 Host 实现承载。

// 同 dsh-core：单线程运行时，`Arc` 仅共享所有权。
#![allow(clippy::arc_with_non_send_sync)]

use std::sync::Arc;

use dsh_core::*;

mod abi;
mod combo;
mod component;
mod host;
mod plugin;
mod services;

pub mod r#loop;

pub use abi::{
    CAPS_EMIT, CAPS_GET, CAPS_PROVIDE, CAPS_WASI_ENV, CAPS_WASI_FS, CAPS_WASI_NET, Capabilities,
};
pub use combo::{ComboEvaluator, FallbackEval, NativeComboEvaluator, WasmComboEvaluator};
pub use component::WasmComponentPlugin;
pub use host::{NativeHost, PluginHost, PluginKind, PluginManifest};
pub use plugin::WasmPlugin;
pub use r#loop::{LoopHost, WasmLoopPlugin};
pub use services::DshServicesPlugin;

pub use wasmtime;

/// 从 wasm 字节构造 WASM 插件（宿主 API 入口）。
pub fn load_wasm_plugin(
    name: &'static str,
    bytes: &[u8],
    caps: Capabilities,
) -> Result<Arc<dyn Plugin>, CordisError> {
    plugin::WasmPlugin::new(name, bytes, caps).map(|p| -> Arc<dyn Plugin> { Arc::new(p) })
}

/// 从组件字节构造 WASM 组件插件（M8 组件模型路径）。
pub fn load_wasm_component_plugin(
    name: &'static str,
    bytes: &[u8],
    caps: Capabilities,
) -> Result<Arc<dyn Plugin>, CordisError> {
    component::WasmComponentPlugin::new(name, bytes, caps)
        .map(|p| -> Arc<dyn Plugin> { Arc::new(p) })
}

/// 从 dsh-loop world 组件字节构造 WASM loop 宿主插件（M8 DSH 层闭环）。
pub fn load_wasm_loop_plugin(
    name: &'static str,
    bytes: &[u8],
    caps: Capabilities,
) -> Result<Arc<WasmLoopPlugin>, CordisError> {
    r#loop::WasmLoopPlugin::new(name, bytes, caps).map(Arc::new)
}

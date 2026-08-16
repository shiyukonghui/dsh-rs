//! `dsh-wasmrt` —— WASM 插件后端（对应旧方案 M2-M3 验收）。
//!
//! 把编译为 wasm32 的插件加载为 [`dsh_core::Plugin`]，经线性内存 FFI 桥接：
//! - 插件导出：`alloc`/`dealloc`/`plugin_apply`/`plugin_handle_event`/`plugin_dispose`
//! - 宿主导入：`host_log`/`host_emit`/`host_on`/`host_provide`/`host_get`
//!
//! 副作用（provide/on）经 dsh-core 的 fiber 机制注册，随 fiber 卸载自动回滚。
//! 能力授予：每插件 [`Capabilities`]，host 导入侧检查，被拒返回错误码。

// 同 dsh-core：单线程运行时，`Arc` 仅共享所有权。
#![allow(clippy::arc_with_non_send_sync)]

use std::sync::Arc;

use dsh_core::*;

mod abi;
mod host;
mod plugin;

pub use abi::{CAPS_EMIT, CAPS_GET, CAPS_PROVIDE, Capabilities};
pub use host::{NativeHost, PluginHost, PluginKind, PluginManifest};
pub use plugin::WasmPlugin;

pub use wasmtime;

/// 从 wasm 字节构造 WASM 插件（宿主 API 入口）。
pub fn load_wasm_plugin(
    name: &'static str,
    bytes: &[u8],
    caps: Capabilities,
) -> Result<Arc<dyn Plugin>, CordisError> {
    plugin::WasmPlugin::new(name, bytes, caps).map(|p| -> Arc<dyn Plugin> { Arc::new(p) })
}

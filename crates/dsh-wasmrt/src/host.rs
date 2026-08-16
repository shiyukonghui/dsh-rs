//! 统一插件宿主抽象：native 与 WASM 插件经同一入口加载。

use std::sync::Arc;

use dsh_core::*;

use crate::abi::Capabilities;

/// 插件清单：名字 + 实现来源 + 能力。
pub struct PluginManifest {
    pub name: &'static str,
    pub kind: PluginKind,
    pub caps: Capabilities,
}

/// 插件实现来源。
pub enum PluginKind {
    /// 进程内 native 插件。
    Native(Arc<dyn Plugin>),
    /// WASM 插件字节（wasm32）。
    WasmBytes(Vec<u8>),
}

/// 插件宿主：加载任意来源的插件为统一 `Plugin`。
pub trait PluginHost {
    fn load(&self, manifest: &PluginManifest) -> Result<Arc<dyn Plugin>, CordisError>;
}

/// 进程内宿主（native 直通）。
pub struct NativeHost;

impl PluginHost for NativeHost {
    fn load(&self, manifest: &PluginManifest) -> Result<Arc<dyn Plugin>, CordisError> {
        match &manifest.kind {
            PluginKind::Native(p) => Ok(p.clone()),
            PluginKind::WasmBytes(bytes) => {
                crate::load_wasm_plugin(manifest.name, bytes, manifest.caps)
            }
        }
    }
}

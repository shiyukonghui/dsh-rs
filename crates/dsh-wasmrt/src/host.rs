//! 统一插件宿主抽象：native 与 WASM 插件（core-module C ABI 或组件模型）经同一入口加载。

use std::sync::Arc;

use dsh_core::*;

use crate::abi::Capabilities;

/// 插件清单：名字 + 实现来源 + 能力。
pub struct PluginManifest {
    pub name: &'static str,
    pub kind: PluginKind,
    pub caps: Capabilities,
}

impl PluginManifest {
    /// 从 entry 配置构造清单（M16：能力按 entry 配置的统一入口）。
    /// `config.caps` 数组 → `Capabilities::from_json`（缺省 = abi_only；
    /// `all` = 全量）。C ABI 与组件两路径共用；native 直通（caps 无 host 侧检查）。
    pub fn from_config(name: &'static str, kind: PluginKind, config: &dsh_core::Value) -> Self {
        PluginManifest {
            name,
            kind,
            caps: Capabilities::from_json(config.get("caps")),
        }
    }
}

/// 插件实现来源。
pub enum PluginKind {
    /// 进程内 native 插件。
    Native(Arc<dyn Plugin>),
    /// WASM core-module 字节（C ABI，M6）。
    WasmBytes(Vec<u8>),
    /// WASM 组件字节（组件模型，M8）。
    ComponentBytes(Vec<u8>),
}

/// 插件宿主：加载任意来源的插件为统一 `Plugin`。
pub trait PluginHost {
    fn load(&self, manifest: &PluginManifest) -> Result<Arc<dyn Plugin>, CordisError>;
}

/// 进程内宿主（native 直通；WASM 按形态分派）。
pub struct NativeHost;

impl PluginHost for NativeHost {
    fn load(&self, manifest: &PluginManifest) -> Result<Arc<dyn Plugin>, CordisError> {
        match &manifest.kind {
            PluginKind::Native(p) => Ok(p.clone()),
            PluginKind::WasmBytes(bytes) => {
                crate::load_wasm_plugin(manifest.name, bytes, manifest.caps)
            }
            PluginKind::ComponentBytes(bytes) => {
                crate::load_wasm_component_plugin(manifest.name, bytes, manifest.caps)
            }
        }
    }
}

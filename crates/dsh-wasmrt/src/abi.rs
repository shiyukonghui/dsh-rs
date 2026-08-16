//! WASM 插件 ABI 常量与能力位。

/// 插件导出函数名。
pub const EXPORT_ALLOC: &str = "alloc";
pub const EXPORT_DEALLOC: &str = "dealloc";
pub const EXPORT_APPLY: &str = "plugin_apply";
pub const EXPORT_HANDLE_EVENT: &str = "plugin_handle_event";
pub const EXPORT_DISPOSE: &str = "plugin_dispose";

/// 宿主导入函数名。
pub const IMPORT_LOG: &str = "host_log";
pub const IMPORT_EMIT: &str = "host_emit";
pub const IMPORT_ON: &str = "host_on";
pub const IMPORT_PROVIDE: &str = "host_provide";
pub const IMPORT_GET: &str = "host_get";

/// 能力位。
pub const CAPS_PROVIDE: u32 = 1 << 0;
pub const CAPS_EMIT: u32 = 1 << 1;
pub const CAPS_GET: u32 = 1 << 2;

/// 每插件能力集合（M3 能力授予）。
#[derive(Debug, Clone, Copy, Default)]
pub struct Capabilities {
    pub bits: u32,
}

impl Capabilities {
    pub fn new(bits: u32) -> Self {
        Capabilities { bits }
    }

    pub fn all() -> Self {
        Capabilities {
            bits: CAPS_PROVIDE | CAPS_EMIT | CAPS_GET,
        }
    }

    pub fn allows(&self, bit: u32) -> bool {
        self.bits & bit != 0
    }
}

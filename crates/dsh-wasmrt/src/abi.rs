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
/// WASI preview2 能力：环境变量（`WasiCtxBuilder::inherit_env`）。
pub const CAPS_WASI_ENV: u32 = 1 << 3;
/// WASI preview2 能力：文件系统（`WasiCtxBuilder::preopened_dir`，根目录只读）。
pub const CAPS_WASI_FS: u32 = 1 << 4;
/// WASI preview2 能力：网络（`WasiCtxBuilder::inherit_network`）。
pub const CAPS_WASI_NET: u32 = 1 << 5;

/// 每插件能力集合（M3 能力授予；WASI 位 M10 扩展）。
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
            bits: CAPS_PROVIDE | CAPS_EMIT | CAPS_GET | CAPS_WASI_ENV | CAPS_WASI_FS | CAPS_WASI_NET,
        }
    }

    /// 仅 ABI 能力（无 WASI）。
    pub fn abi_only() -> Self {
        Capabilities {
            bits: CAPS_PROVIDE | CAPS_EMIT | CAPS_GET,
        }
    }

    pub fn allows(&self, bit: u32) -> bool {
        self.bits & bit != 0
    }

    /// 从 JSON 配置解析能力集（`caps: ["provide","emit","get","wasi-env","wasi-fs","wasi-net"]`）。
    /// - 缺省/空数组 → `abi_only`（ABI 能力，无 WASI）；
    /// - 含 `"all"` → 全部能力。
    pub fn from_json(value: Option<&dsh_core::Value>) -> Self {
        let Some(v) = value else {
            return Self::abi_only();
        };
        let Some(list) = v.as_array() else {
            return Self::abi_only();
        };
        let names: Vec<&str> = list
            .iter()
            .filter_map(|s| s.as_str())
            .collect();
        let mut bits = 0u32;
        for name in names {
            match name {
                "all" => return Self::all(),
                "provide" => bits |= CAPS_PROVIDE,
                "emit" => bits |= CAPS_EMIT,
                "get" => bits |= CAPS_GET,
                "wasi-env" => bits |= CAPS_WASI_ENV,
                "wasi-fs" => bits |= CAPS_WASI_FS,
                "wasi-net" => bits |= CAPS_WASI_NET,
                _ => {}
            }
        }
        Capabilities { bits }
    }

    /// 按能力集构建 WASI preview2 上下文（精细授予）：
    /// - `CAPS_WASI_ENV` → 继承环境变量；
    /// - `CAPS_WASI_FS` → 预打开根目录（只读）；
    /// - `CAPS_WASI_NET` → 继承网络 + 允许 TCP/UDP/域名解析。
    ///
    /// 无任何 WASI 位 → 最小空上下文（仅 stdio 关闭）。
    pub fn build_wasi_ctx(&self) -> wasmtime_wasi::p2::WasiCtx {
        let mut builder = wasmtime_wasi::p2::WasiCtxBuilder::new();
        if self.allows(CAPS_WASI_ENV) {
            builder.inherit_env();
        }
        if self.allows(CAPS_WASI_FS) {
            if let Ok(cwd) = std::env::current_dir() {
                let _ = builder.preopened_dir(
                    cwd,
                    "/",
                    wasmtime_wasi::DirPerms::READ,
                    wasmtime_wasi::FilePerms::READ,
                );
            }
        }
        if self.allows(CAPS_WASI_NET) {
            builder.inherit_network();
            builder.allow_tcp(true);
            builder.allow_udp(true);
            builder.allow_ip_name_lookup(true);
        }
        builder.build()
    }

    /// 按能力集构建 WASI **preview1** 上下文（C ABI core-module 路径，M19）：
    /// 与 `build_wasi_ctx` 相同的精细授予，产出 `WasiP1Ctx`（`build_p1`）。
    /// 无任何 WASI 位 → None（不注册 WASI import，纯 env ABI——wasip1 插件
    /// 若 import wasi_snapshot_preview1 将实例化失败，能力拒绝）。
    pub fn build_wasi_p1_ctx(&self) -> Option<wasmtime_wasi::preview1::WasiP1Ctx> {
        use wasmtime_wasi::p2::WasiCtxBuilder;
        if !(self.allows(CAPS_WASI_ENV)
            || self.allows(CAPS_WASI_FS)
            || self.allows(CAPS_WASI_NET))
        {
            return None;
        }
        let mut builder = WasiCtxBuilder::new();
        if self.allows(CAPS_WASI_ENV) {
            builder.inherit_env();
        }
        if self.allows(CAPS_WASI_FS) {
            if let Ok(cwd) = std::env::current_dir() {
                let _ = builder.preopened_dir(
                    cwd,
                    "/",
                    wasmtime_wasi::DirPerms::READ,
                    wasmtime_wasi::FilePerms::READ,
                );
            }
        }
        if self.allows(CAPS_WASI_NET) {
            builder.inherit_network();
            builder.allow_tcp(true);
            builder.allow_udp(true);
            builder.allow_ip_name_lookup(true);
        }
        Some(builder.build_p1())
    }
}

//! WASM 组件承载 host 侧 remote 端点（D-115-Web D3）。
//!
//! **组件模型专用（用户裁定：禁止功能漂移到 C ABI）**。本模块把实现 `host-remote`
//! world 的 WASM 组件（`wasm-plugins/host-remote`，导出 `remote.handle`）适配为宿主
//! 的「remote 端点提供者」：
//! - 宿主经 `WasmRemoteEndpointPlugin::handle(namespace, method, body)` 把 `/api` 端点
//!   请求交给 WASM 组件；组件返回结果 JSON 字节（严格对齐前端 remote 端点 schema）。
//! - 组件经 `host-services.get(service, payload)` 反查宿主真实状态；宿主按服务名投影
//!   出真实数据（sessions/loader/settings/workspaces/持久 KV 等），未知 → 规范化错误。
//!
//! 与 `loop.rs`（dsh-loop 缝）、`component.rs`（dsh-plugin 载体）正交：本 world 只回答
//! 「host 侧 remote 端点如何由 WASM 组件提供」。既有大面（commands/goals/session）留
//! 宿主原生，经 EndpointHost 统一路由，不经本世界。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use dsh_core::*;
use wasmtime::component::{Component, Linker};
use wasmtime::{Engine, Store};

use crate::abi::Capabilities;

// 编译期从 `wit-host-remote/host-remote.wit` 生成 host 侧绑定（独立 package
// `dsh:host-remote` → 模块 `dsh::host_remote`，不与 wit-dsh/dsh-loop 的 `dsh:dsh`
// 冲突；D-115-Web D3 组件模型专用）。
wasmtime::component::bindgen!({
    path: "wit-host-remote/host-remote.wit",
    world: "host-remote",
    async: false,
});

// apply 期间当前挂载上下文（host 回调经此访问 Cordis 或宿主投影面；单线程内安全）。
thread_local! {
    static CURRENT_PROJECTOR: RefCell<Option<Rc<dyn RemoteServiceProjector>>> =
        const { RefCell::new(None) };
}

/// 宿主侧服务投影（`host-services.get/set` 的后端）：WASM 端点反查宿主真实状态时，
/// 宿主按 `service` 名投影/写入真实数据。实现者由 dsh-cli 装配（注入 session store /
/// loader / settings / 持久 KV 等真实来源）。
pub trait RemoteServiceProjector {
    /// 投影一个宿主服务为 JSON 字节。
    /// 未知 service / 无权限 → `Ok(err_json)`（规范化错误字节），绝不伪造成功。
    fn get(&self, service: &str, payload: &[u8]) -> Vec<u8>;

    /// 写入宿主服务（真实持久）。缺省 = 该服务只读（规范化错误）。
    /// 未知 service / 无权限 / 只读 → `Ok(err_json)`。
    fn set(&self, service: &str, _payload: &[u8]) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "ok": false,
            "error": { "code": "read-only", "message": format!("service {service} is read-only (no set backend)") },
        }))
        .unwrap_or_default()
    }
}

/// 运行时状态（`Host` trait 经 `caller.data()` 访问；不含 Cordis，Send 兼容）。
struct RemoteHostState {
    /// WASI preview2 上下文（组件按能力集注入；含 net/fs 则授予）。
    wasi: wasmtime_wasi::p2::WasiCtx,
    table: wasmtime::component::ResourceTable,
    /// 当前 handle 调用是否有效（host 回调只在 handle 生命周期内被允许）。
    inside: Cell<bool>,
}

impl RemoteHostState {
    fn new(caps: Capabilities) -> Self {
        RemoteHostState {
            wasi: caps.build_wasi_ctx(),
            table: wasmtime::component::ResourceTable::new(),
            inside: Cell::new(false),
        }
    }
}

impl wasmtime_wasi::p2::IoView for RemoteHostState {
    fn table(&mut self) -> &mut wasmtime::component::ResourceTable {
        &mut self.table
    }
}

impl wasmtime_wasi::p2::WasiView for RemoteHostState {
    fn ctx(&mut self) -> &mut wasmtime_wasi::p2::WasiCtx {
        &mut self.wasi
    }
}

/// 组件运行时（Store + 实例化后的 world 句柄）。
struct RemoteRuntime {
    store: Store<RemoteHostState>,
    plugin: HostRemote,
}

/// 组件端点会调用的 `host-services` 接口宿主实现（模块 = `dsh::host_remote::host_services`）。
impl dsh::host_remote::host_services::Host for RemoteHostState {
    fn get(&mut self, service: String, payload: Vec<u8>) -> Vec<u8> {
        if !self.inside.get() {
            return err_json("host-services.get called outside an endpoint handle");
        }
        CURRENT_PROJECTOR.with(|p| {
            let Some(proj) = p.borrow().clone() else {
                return err_json("host-services.get: no host projector assembled");
            };
            proj.get(&service, &payload)
        })
    }

    fn set(&mut self, service: String, payload: Vec<u8>) -> Vec<u8> {
        if !self.inside.get() {
            return err_json("host-services.set called outside an endpoint handle");
        }
        CURRENT_PROJECTOR.with(|p| {
            let Some(proj) = p.borrow().clone() else {
                return err_json("host-services.set: no host projector assembled");
            };
            proj.set(&service, &payload)
        })
    }
}

/// 规范化错误字节（fail-loud：绝不伪装成功）。
fn err_json(message: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "ok": false,
        "error": { "code": "internal", "message": message },
    }))
    .unwrap_or_default()
}

/// WASM 组件承载 host 侧 remote 端点。
pub struct WasmRemoteEndpointPlugin {
    name: &'static str,
    engine: Engine,
    component: Component,
    rt: Rc<RefCell<Option<RemoteRuntime>>>,
    caps: Capabilities,
    projector: Option<Rc<dyn RemoteServiceProjector>>,
}

impl WasmRemoteEndpointPlugin {
    /// 从 host-remote world 组件字节构造插件。
    pub fn new(
        name: &'static str,
        bytes: &[u8],
        caps: Capabilities,
        projector: Option<Rc<dyn RemoteServiceProjector>>,
    ) -> Result<Self, CordisError> {
        let engine = Engine::default();
        let component = Component::from_binary(&engine, bytes)
            .map_err(|e| CordisError::Internal(format!("host-remote component: {e}")))?;
        Ok(WasmRemoteEndpointPlugin {
            name,
            engine,
            component,
            rt: Rc::new(RefCell::new(None)),
            caps,
            projector,
        })
    }

    fn instantiate(&self) -> Result<RemoteRuntime, CordisError> {
        let mut store = Store::new(&self.engine, RemoteHostState::new(self.caps));
        let mut linker = Linker::<RemoteHostState>::new(&self.engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
            .map_err(|e| CordisError::Internal(format!("linker wasi: {e}")))?;
        dsh::host_remote::host_services::add_to_linker::<RemoteHostState, wasmtime::component::HasSelf<RemoteHostState>>(
            &mut linker,
            |s| s,
        )
        .map_err(|e| CordisError::Internal(format!("linker host-services: {e}")))?;
        let plugin = HostRemote::instantiate(&mut store, &self.component, &linker)
            .map_err(|e| CordisError::Internal(format!("instantiate host-remote: {e}")))?;
        Ok(RemoteRuntime { store, plugin })
    }

    fn runtime(&self) -> Result<std::cell::RefMut<'_, Option<RemoteRuntime>>, CordisError> {
        let mut rt = self.rt.borrow_mut();
        if rt.is_none() {
            *rt = Some(self.instantiate()?);
        }
        Ok(rt)
    }

    /// 处理一个 host 侧 remote 端点。`body` 为请求参数 JSON 字节；
    /// 返回结果 JSON 字节（成功 = 前端 value 结构；失败 = 规范化错误字节）。
    pub fn handle(
        &self,
        namespace: &str,
        method: &str,
        body: &[u8],
        projector: Option<Rc<dyn RemoteServiceProjector>>,
    ) -> Result<Value, CordisError> {
        CURRENT_PROJECTOR.with(|p| {
            *p.borrow_mut() = self
                .projector
                .clone()
                .or_else(|| projector.clone());
        });
        let result = self.handle_inner(namespace, method, body);
        CURRENT_PROJECTOR.with(|p| *p.borrow_mut() = None);
        result
    }

    fn handle_inner(
        &self,
        namespace: &str,
        method: &str,
        body: &[u8],
    ) -> Result<Value, CordisError> {
        let mut rt = self.runtime()?;
        let runtime = rt.as_mut().expect("host-remote runtime ready");
        runtime.store.data_mut().inside.set(true);
        let out = runtime
            .plugin
            .dsh_host_remote_remote()
            .call_handle(&mut runtime.store, namespace, method, body)
            .map_err(|e| CordisError::Internal(format!("host-remote handle: {e}")));
        runtime.store.data_mut().inside.set(false);
        match out? {
            b if b.is_empty() => Ok(Value::Null),
            b => serde_json::from_slice(&b)
                .map_err(|e| CordisError::Internal(format!("host-remote result decode: {e}"))),
        }
    }
}

impl Plugin for WasmRemoteEndpointPlugin {
    fn name(&self) -> &'static str {
        self.name
    }
    fn apply(&self, _ctx: &Cordis, _config: Value) -> Result<EffectOutcome, CordisError> {
        // 懒实例化：组件在首次 handle 调用时才实例化（apply 不做重活）。
        Ok(EffectOutcome::None)
    }
}

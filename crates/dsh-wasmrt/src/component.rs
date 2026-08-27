//! 组件模型路径：把 WASM **组件**（component，而非 core module）适配为 dsh-core `Plugin`。
//!
//! 与 C ABI 路径（`plugin.rs`）的区别：
//! - 输入是 `cargo-component` 构建的**组件**字节（含 WIT 元数据，可能 import WASI）。
//! - 宿主用 `wasmtime::component::bindgen!` 在编译期解析 `wit/plugin.wit`，
//!   生成类型化的 `DshPlugin`（导出 `apply`/`handle-event`/`dispose`）与
//!   `host_api::Host` trait（导入 `log`/`emit`/`on`/`provide`/`get`）。
//! - WASI preview2 能力经 `wasmtime_wasi::p2::add_to_linker_sync` 注册（能力授予）。
//!
//! Send 纪律：wasmtime 的 `IoView: Send` 要求 Store data 可跨线程；而 Cordis 是
//! `Rc<RefCell>` 单线程。因此 **`ComponentHostState` 不含 Cordis**——apply 时把当前
//! `Cordis` 存入 `thread_local`，host 回调经 thread_local 访问（单线程内安全）。
//! 监听器注册同样由 apply 返回后统一完成（func_wrap 闭包需 Send）。

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use dsh_core::*;
use wasmtime::component::{Component, Linker};
use wasmtime::{Engine, Store};

use crate::abi::{CAPS_EMIT, CAPS_GET, CAPS_PROVIDE, Capabilities};

// 编译期从 `wit/plugin.wit` 生成 host 侧绑定。
wasmtime::component::bindgen!({
    path: "wit/plugin.wit",
    world: "dsh-plugin",
    async: false,
});

// apply 期间当前挂载上下文（host 回调经此访问 Cordis；单线程内安全）。
thread_local! {
    static CURRENT_CTX: RefCell<Option<Cordis>> = const { RefCell::new(None) };
}

/// 运行时状态（`Host` trait 与 WASI 视图通过 `caller.data()` 访问；Send 兼容）。
pub struct ComponentHostState {
    pub caps: Capabilities,
    pub log: Vec<String>,
    pub listeners: Vec<String>,
    /// WASI preview2 上下文（能力授予：按 caps 决定注入的 WASI 能力）。
    pub wasi: wasmtime_wasi::p2::WasiCtx,
    /// WASI 资源表。
    pub table: wasmtime::component::ResourceTable,
    /// 当前挂载是否有效（fiber active 期间为 true）。
    pub mounted: Cell<bool>,
}

impl ComponentHostState {
    fn new(caps: Capabilities) -> Self {
        let wasi = caps.build_wasi_ctx();
        ComponentHostState {
            caps,
            log: Vec::new(),
            listeners: Vec::new(),
            wasi,
            table: wasmtime::component::ResourceTable::new(),
            mounted: Cell::new(false),
        }
    }

    /// 取当前 Cordis（apply/事件期间）。
    fn ctx(&self) -> Option<Cordis> {
        if !self.mounted.get() {
            return None;
        }
        CURRENT_CTX.with(|c| c.borrow().clone())
    }
}

// WASI preview2 视图：`add_to_linker_sync` 需要 `WasiView`（含 `IoView: Send`）。
impl wasmtime_wasi::p2::IoView for ComponentHostState {
    fn table(&mut self) -> &mut wasmtime::component::ResourceTable {
        &mut self.table
    }
}

impl wasmtime_wasi::p2::WasiView for ComponentHostState {
    fn ctx(&mut self) -> &mut wasmtime_wasi::p2::WasiCtx {
        &mut self.wasi
    }
}

/// 宿主能力实现（组件模型形态；能力位在此检查）。
impl dsh::plugin::host_api::Host for ComponentHostState {
    fn log(&mut self, message: wasmtime::component::__internal::String) {
        self.log.push(message.to_string());
    }

    fn emit(&mut self, payload: wasmtime::component::__internal::Vec<u8>) {
        if !self.caps.allows(CAPS_EMIT) {
            self.log.push("host_emit denied (capability)".to_string());
            return;
        }
        let Some(ctx) = self.ctx() else { return };
        let payload: Value = serde_json::from_slice(&payload).unwrap_or(Value::Null);
        ctx.emit("wasm", vec![payload]);
    }

    fn on(&mut self, event: wasmtime::component::__internal::String) {
        self.listeners.push(event.to_string());
    }

    fn provide(
        &mut self,
        service: wasmtime::component::__internal::String,
        value: wasmtime::component::__internal::Vec<u8>,
    ) -> i32 {
        if !self.caps.allows(CAPS_PROVIDE) {
            self.log
                .push("host_provide denied (capability)".to_string());
            return -1;
        }
        let Some(ctx) = self.ctx() else { return -1 };
        let value: Value = serde_json::from_slice(&value).unwrap_or(Value::Null);
        match ctx.provide(&service, Arc::new(value)) {
            Ok(_) => 0,
            Err(e) => {
                self.log.push(format!("host_provide failed: {e}"));
                -1
            }
        }
    }

    fn get(
        &mut self,
        service: wasmtime::component::__internal::String,
    ) -> wasmtime::component::__internal::Vec<u8> {
        if !self.caps.allows(CAPS_GET) {
            self.log.push("host_get denied (capability)".to_string());
            return Vec::new();
        }
        let Some(ctx) = self.ctx() else { return Vec::new() };
        ctx.get(&service)
            .and_then(|v| v.downcast::<Value>().ok())
            .and_then(|v| serde_json::to_vec(&*v).ok())
            .unwrap_or_default()
    }
}

/// 一个组件实例（Store + 组件接口句柄）。
pub struct ComponentRuntime {
    pub store: Store<ComponentHostState>,
    pub plugin: DshPlugin,
}

/// WASM 组件插件（dsh-core `Plugin` 适配）。
pub struct WasmComponentPlugin {
    name: &'static str,
    caps: Capabilities,
    engine: Engine,
    component: Component,
    rt: Rc<RefCell<Option<ComponentRuntime>>>,
}

impl WasmComponentPlugin {
    pub fn new(name: &'static str, bytes: &[u8], caps: Capabilities) -> Result<Self, CordisError> {
        let engine = Engine::default();
        let component = Component::from_binary(&engine, bytes)
            .map_err(|e| CordisError::Internal(format!("wasm component: {e}")))?;
        Ok(WasmComponentPlugin {
            name,
            caps,
            engine,
            component,
            rt: Rc::new(RefCell::new(None)),
        })
    }

    fn instantiate(&self) -> Result<ComponentRuntime, CordisError> {
        let mut store = Store::new(&self.engine, ComponentHostState::new(self.caps));
        let mut linker = Linker::<ComponentHostState>::new(&self.engine);

        // 注册 WASI preview2 能力（组件可能 import wasi:* 接口）
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
            .map_err(|e| CordisError::Internal(format!("linker wasi: {e}")))?;

        // 注册宿主能力（Host trait 实现；HasSelf 包装 Store data 类型）
        dsh::plugin::host_api::add_to_linker::<
            ComponentHostState,
            wasmtime::component::HasSelf<ComponentHostState>,
        >(&mut linker, |state| state)
        .map_err(|e| CordisError::Internal(format!("linker host api: {e}")))?;

        let plugin = DshPlugin::instantiate(&mut store, &self.component, &linker)
            .map_err(|e| CordisError::Internal(format!("instantiate component: {e}")))?;
        Ok(ComponentRuntime { store, plugin })
    }

    fn runtime(&self) -> Result<std::cell::RefMut<'_, Option<ComponentRuntime>>, CordisError> {
        let mut rt = self.rt.borrow_mut();
        if rt.is_none() {
            *rt = Some(self.instantiate()?);
        }
        Ok(rt)
    }
}

impl Plugin for WasmComponentPlugin {
    fn name(&self) -> &'static str {
        self.name
    }

    fn apply(&self, ctx: &Cordis, config: Value) -> Result<EffectOutcome, CordisError> {
        let mut rt = self.runtime()?;
        {
            let runtime = rt.as_mut().expect("instance ready");
            // 挂载：thread_local 注入当前 Cordis，host 回调可访问
            CURRENT_CTX.with(|c| *c.borrow_mut() = Some(ctx.clone()));
            runtime.store.data_mut().mounted.set(true);
            runtime.store.data_mut().listeners.clear();
            let config_bytes = serde_json::to_vec(&config)
                .map_err(|e| CordisError::Internal(format!("config encode: {e}")))?;
            let code = runtime
                .plugin
                .dsh_plugin_plugin_api()
                .call_apply(&mut runtime.store, &config_bytes)
                .map_err(|e| CordisError::Internal(format!("component apply: {e}")))?;
            if code != 0 {
                return Err(CordisError::Internal(format!(
                    "wasm component plugin {name} apply failed with code {code}",
                    name = self.name
                )));
            }
        }
        // 统一注册 wasm 内 host_on 记录的事件监听（当前 fiber 仍有效）
        let events: Vec<String> = rt
            .as_ref()
            .map(|r| r.store.data().listeners.clone())
            .unwrap_or_default();
        for event in events {
            let rt = self.rt.clone();
            let event_for_closure = event.clone();
            let _ = ctx.on(
                &event,
                Arc::new(move |_ctx, args, _next| {
                    let payload = args.first().cloned().unwrap_or(Value::Null);
                    let payload_bytes = serde_json::to_vec(&payload).unwrap_or_default();
                    let mut guard = rt.borrow_mut();
                    if let Some(runtime) = guard.as_mut() {
                        let _ = runtime
                            .plugin
                            .dsh_plugin_plugin_api()
                            .call_handle_event(&mut runtime.store, &event_for_closure, &payload_bytes)
                            .map_err(|e| {
                                let _ = e;
                            });
                    }
                    HookResult::Continue
                }),
            );
        }
        // 卸载时调用插件 dispose（副作用随 fiber 卸载回滚）
        let rt = self.rt.clone();
        ctx.effect(
            "wasm-component-dispose",
            Box::new(move |_ctx| {
                Ok(EffectOutcome::One(Rc::new(move |_ctx| {
                    let mut guard = rt.borrow_mut();
                    if let Some(runtime) = guard.as_mut() {
                        let _ = runtime
                            .plugin
                            .dsh_plugin_plugin_api()
                            .call_dispose(&mut runtime.store)
                            .map_err(|e| {
                                let _ = e;
                            });
                        runtime.store.data_mut().mounted.set(false);
                    }
                })))
            }),
        )?;
        Ok(EffectOutcome::None)
    }
}

impl WasmComponentPlugin {
    /// 插件日志（host_log 记录，测试/诊断用）。
    pub fn logs(&self) -> Vec<String> {
        self.rt
            .borrow()
            .as_ref()
            .map(|r| r.store.data().log.clone())
            .unwrap_or_default()
    }
}

/// WASM 组件 world（按**导出接口**判别的 ABI 事实，非配置推断）。
/// 插件包解析据此选适配器（loop→`WasmLoopPlugin` / plugin→`WasmComponentPlugin`）；
/// `Unknown` = 组件编译失败或非 dsh world（装配层 fail-loud）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentKind {
    /// dsh-plugin world：导出 `dsh:plugin/plugin-api`（通用 apply）。
    Plugin,
    /// dsh-loop world：导出 `dsh:dsh/agent-loop`（turn 驱动，run_turn 具体类型）。
    Loop,
    /// 非 dsh world 或组件编译失败。
    Unknown,
}

/// 预检组件字节的 world（wasmtime 34 `types::Component::exports` 遍历导出名）。
/// loop 优先（dsh-loop 导出 `agent-loop` / `tools-handler`；dsh-plugin 导出 `plugin-api`）。
pub fn detect_component_kind(bytes: &[u8]) -> ComponentKind {
    let engine = Engine::default();
    let component = match Component::from_binary(&engine, bytes) {
        Ok(c) => c,
        Err(_) => return ComponentKind::Unknown,
    };
    let mut has_plugin = false;
    let mut has_loop = false;
    for (name, _) in component.component_type().exports(&engine) {
        if name.contains("agent-loop") {
            has_loop = true;
        }
        if name.contains("plugin-api") {
            has_plugin = true;
        }
    }
    if has_loop {
        return ComponentKind::Loop;
    }
    if has_plugin {
        return ComponentKind::Plugin;
    }
    ComponentKind::Unknown
}

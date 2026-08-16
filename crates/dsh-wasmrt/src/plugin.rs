//! `WasmPlugin`：把 wasm32 插件适配为 dsh-core `Plugin`。
//!
//! 每个 `WasmPlugin` 持有一个懒实例化的 wasmtime 实例（`Rc<RefCell<Option<WasmRuntime>>>`）。
//! 双向桥接：
//! - 宿主 → 插件：`plugin_apply`（配置 JSON 字节）、`plugin_handle_event`（事件转发）、`plugin_dispose`
//! - 插件 → 宿主（import）：`host_log` / `host_emit` / `host_on` / `host_provide` / `host_get`
//!
//! wasmtime 的 `func_wrap` 闭包要求 `Send + Sync`，因此宿主 import 闭包**只**通过
//! `caller.data()`（`WasmHostState`）读写：`host_on` 仅把事件名记入 state，监听器的
//! 真实注册由 `apply` 在 wasm 调用返回后（当前 fiber 仍有效）统一完成，避免闭包捕获
//! 非 Send 的运行时句柄。副作用（provide/on）经 dsh-core fiber 注册，随 fiber 卸载自动回滚。
//! 能力检查在 host import 侧进行，被拒返回错误码并记日志。

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use dsh_core::*;
use wasmtime::{Caller, Engine, Instance, Linker, Memory, Module, Store};

use crate::abi::*;

/// 运行时状态（host import 可访问）。
pub struct WasmHostState {
    /// 当前挂载上下文（apply 时设置，供 import 桥接回 Cordis）。
    pub ctx: Option<Cordis>,
    /// 能力集合。
    pub caps: Capabilities,
    /// 插件日志。
    pub log: Vec<String>,
    /// apply 期间注册的事件监听名（宿主在 apply 返回后统一注册）。
    pub listeners: Vec<String>,
}

impl WasmHostState {
    fn new(caps: Capabilities) -> Self {
        WasmHostState {
            ctx: None,
            caps,
            log: Vec::new(),
            listeners: Vec::new(),
        }
    }
}

/// 一个 wasmtime 实例（Store + Instance）。
pub struct WasmRuntime {
    pub store: Store<WasmHostState>,
    pub instance: Instance,
}

/// WASM 插件（dsh-core `Plugin` 适配）。
pub struct WasmPlugin {
    name: &'static str,
    caps: Capabilities,
    engine: Engine,
    module: Module,
    rt: Rc<RefCell<Option<WasmRuntime>>>,
}

impl WasmPlugin {
    pub fn new(name: &'static str, bytes: &[u8], caps: Capabilities) -> Result<Self, CordisError> {
        let engine = Engine::default();
        let module = Module::new(&engine, bytes)
            .map_err(|e| CordisError::Internal(format!("wasm module: {e}")))?;
        Ok(WasmPlugin {
            name,
            caps,
            engine,
            module,
            rt: Rc::new(RefCell::new(None)),
        })
    }

    fn runtime(&self) -> Result<std::cell::RefMut<'_, Option<WasmRuntime>>, CordisError> {
        let mut rt = self.rt.borrow_mut();
        if rt.is_none() {
            *rt = Some(self.instantiate()?);
        }
        Ok(rt)
    }

    fn instantiate(&self) -> Result<WasmRuntime, CordisError> {
        let mut store = Store::new(&self.engine, WasmHostState::new(self.caps));
        let mut linker = Linker::<WasmHostState>::new(&self.engine);

        linker
            .func_wrap("env", IMPORT_LOG, |mut caller: Caller<'_, WasmHostState>, ptr: i32, len: i32| {
                let text = read_str(&mut caller, ptr, len);
                caller.data_mut().log.push(text);
            })
            .map_err(|e| CordisError::Internal(format!("linker log: {e}")))?;

        linker
            .func_wrap(
                "env",
                IMPORT_EMIT,
                |mut caller: Caller<'_, WasmHostState>, ptr: i32, len: i32| {
                    let payload = read_json(&mut caller, ptr, len);
                    if !caller.data().caps.allows(CAPS_EMIT) {
                        caller
                            .data_mut()
                            .log
                            .push("host_emit denied (capability)".to_string());
                        return;
                    }
                    let Some(ctx) = caller.data().ctx.clone() else { return };
                    ctx.emit("wasm", vec![payload]);
                },
            )
            .map_err(|e| CordisError::Internal(format!("linker emit: {e}")))?;

        // host_on：仅记录事件名；监听器由 apply 返回后统一注册（func_wrap 闭包需 Send）。
        linker
            .func_wrap(
                "env",
                IMPORT_ON,
                |mut caller: Caller<'_, WasmHostState>, ptr: i32, len: i32| {
                    let event = read_str(&mut caller, ptr, len);
                    caller.data_mut().listeners.push(event);
                },
            )
            .map_err(|e| CordisError::Internal(format!("linker on: {e}")))?;

        linker
            .func_wrap(
                "env",
                IMPORT_PROVIDE,
                |mut caller: Caller<'_, WasmHostState>, sptr: i32, slen: i32, vptr: i32, vlen: i32| -> i32 {
                    if !caller.data().caps.allows(CAPS_PROVIDE) {
                        caller
                            .data_mut()
                            .log
                            .push("host_provide denied (capability)".to_string());
                        return -1;
                    }
                    let service = read_str(&mut caller, sptr, slen);
                    let value = read_json(&mut caller, vptr, vlen);
                    let Some(ctx) = caller.data().ctx.clone() else { return -1 };
                    match ctx.provide(&service, Arc::new(value)) {
                        Ok(_) => 0,
                        Err(e) => {
                            caller
                                .data_mut()
                                .log
                                .push(format!("host_provide failed: {e}"));
                            -1
                        }
                    }
                },
            )
            .map_err(|e| CordisError::Internal(format!("linker provide: {e}")))?;

        linker
            .func_wrap(
                "env",
                IMPORT_GET,
                |mut caller: Caller<'_, WasmHostState>, sptr: i32, slen: i32, out_ptr: i32, out_len_ptr: i32| -> i32 {
                    if !caller.data().caps.allows(CAPS_GET) {
                        caller
                            .data_mut()
                            .log
                            .push("host_get denied (capability)".to_string());
                        return -1;
                    }
                    let service = read_str(&mut caller, sptr, slen);
                    let Some(ctx) = caller.data().ctx.clone() else { return -1 };
                    let value: Option<Value> = ctx
                        .get(&service)
                        .and_then(|v| v.downcast::<Value>().ok())
                        .map(|v| (*v).clone());
                    let bytes = match value {
                        Some(v) => serde_json::to_vec(&v).unwrap_or_default(),
                        None => Vec::new(),
                    };
                    let mem = memory(&mut caller);
                    let data = mem.data_mut(&mut caller);
                    let (optr, olen) = (out_ptr as usize, out_len_ptr as usize);
                    if optr + bytes.len() <= data.len() && olen + 4 <= data.len() {
                        data[optr..optr + bytes.len()].copy_from_slice(&bytes);
                        data[olen..olen + 4].copy_from_slice(&(bytes.len() as i32).to_le_bytes());
                    }
                    0
                },
            )
            .map_err(|e| CordisError::Internal(format!("linker get: {e}")))?;

        let instance = linker
            .instantiate(&mut store, &self.module)
            .map_err(|e| CordisError::Internal(format!("instantiate: {e}")))?;
        Ok(WasmRuntime { store, instance })
    }
}

impl Plugin for WasmPlugin {
    fn name(&self) -> &'static str {
        self.name
    }

    fn apply(&self, ctx: &Cordis, config: Value) -> Result<EffectOutcome, CordisError> {
        let mut rt = self.runtime()?;
        {
            let runtime = rt.as_mut().expect("instance ready");
            runtime.store.data_mut().ctx = Some(ctx.clone());
            runtime.store.data_mut().listeners.clear();
            let config_bytes = serde_json::to_vec(&config)
                .map_err(|e| CordisError::Internal(format!("config encode: {e}")))?;
            let code = call_apply(runtime, &config_bytes)?;
            if code != 0 {
                return Err(CordisError::Internal(format!(
                    "wasm plugin {name} apply failed with code {code}",
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
                        let _ = call_handle_event(runtime, &event_for_closure, &payload_bytes);
                    }
                    HookResult::Continue
                }),
            );
        }
        // 卸载时调用插件 dispose（副作用随 fiber 卸载回滚）
        let rt = self.rt.clone();
        ctx.effect(
            "wasm-dispose",
            Box::new(move |_ctx| {
                Ok(EffectOutcome::One(Rc::new(move |_ctx| {
                    let mut guard = rt.borrow_mut();
                    if let Some(runtime) = guard.as_mut() {
                        let _ = call_dispose(runtime);
                    }
                })))
            }),
        )?;
        Ok(EffectOutcome::None)
    }
}

impl WasmPlugin {
    /// 插件日志（host_log 记录，测试/诊断用）。
    pub fn logs(&self) -> Vec<String> {
        self.rt
            .borrow()
            .as_ref()
            .map(|r| r.store.data().log.clone())
            .unwrap_or_default()
    }
}

// ---- wasmtime 调用辅助 ----

fn call_apply(rt: &mut WasmRuntime, config: &[u8]) -> Result<i32, CordisError> {
    let store = &mut rt.store;
    let mem = rt
        .instance
        .get_memory(&mut *store, "memory")
        .ok_or_else(|| CordisError::Internal("no memory export".into()))?;
    let alloc = rt
        .instance
        .get_typed_func::<(i32,), (i32,)>(&mut *store, EXPORT_ALLOC)
        .map_err(|e| CordisError::Internal(e.to_string()))?;
    let apply = rt
        .instance
        .get_typed_func::<(i32, i32), (i32,)>(&mut *store, EXPORT_APPLY)
        .map_err(|e| CordisError::Internal(e.to_string()))?;
    let dealloc = rt
        .instance
        .get_typed_func::<(i32, i32), ()>(&mut *store, EXPORT_DEALLOC)
        .map_err(|e| CordisError::Internal(e.to_string()))?;
    let len = config.len() as i32;
    let (ptr,) = alloc
        .call(&mut *store, (len,))
        .map_err(|e| CordisError::Internal(e.to_string()))?;
    mem.data_mut(&mut *store)[ptr as usize..(ptr + len) as usize].copy_from_slice(config);
    let (code,) = apply
        .call(&mut *store, (ptr, len))
        .map_err(|e| CordisError::Internal(e.to_string()))?;
    let _ = dealloc.call(&mut *store, (ptr, len));
    Ok(code)
}

fn call_handle_event(rt: &mut WasmRuntime, event: &str, payload: &[u8]) -> Result<i32, CordisError> {
    let store = &mut rt.store;
    let mem = rt
        .instance
        .get_memory(&mut *store, "memory")
        .ok_or_else(|| CordisError::Internal("no memory export".into()))?;
    let alloc = rt
        .instance
        .get_typed_func::<(i32,), (i32,)>(&mut *store, EXPORT_ALLOC)
        .map_err(|e| CordisError::Internal(e.to_string()))?;
    let handle = rt
        .instance
        .get_typed_func::<(i32, i32, i32, i32), (i32,)>(&mut *store, EXPORT_HANDLE_EVENT)
        .map_err(|e| CordisError::Internal(e.to_string()))?;
    let dealloc = rt
        .instance
        .get_typed_func::<(i32, i32), ()>(&mut *store, EXPORT_DEALLOC)
        .map_err(|e| CordisError::Internal(e.to_string()))?;

    let elen = event.len() as i32;
    let plen = payload.len() as i32;
    let (eptr,) = alloc
        .call(&mut *store, (elen,))
        .map_err(|e| CordisError::Internal(e.to_string()))?;
    let (pptr,) = alloc
        .call(&mut *store, (plen,))
        .map_err(|e| CordisError::Internal(e.to_string()))?;
    {
        let data = mem.data_mut(&mut *store);
        data[eptr as usize..(eptr + elen) as usize].copy_from_slice(event.as_bytes());
        data[pptr as usize..(pptr + plen) as usize].copy_from_slice(payload);
    }
    let (code,) = handle
        .call(&mut *store, (eptr, elen, pptr, plen))
        .map_err(|e| CordisError::Internal(e.to_string()))?;
    let _ = dealloc.call(&mut *store, (eptr, elen));
    let _ = dealloc.call(&mut *store, (pptr, plen));
    Ok(code)
}

fn call_dispose(rt: &mut WasmRuntime) -> Result<i32, CordisError> {
    let store = &mut rt.store;
    let dispose = rt
        .instance
        .get_typed_func::<(), (i32,)>(&mut *store, EXPORT_DISPOSE)
        .map_err(|e| CordisError::Internal(e.to_string()))?;
    let (code,) = dispose
        .call(&mut *store, ())
        .map_err(|e| CordisError::Internal(e.to_string()))?;
    Ok(code)
}

fn memory(caller: &mut Caller<'_, WasmHostState>) -> Memory {
    caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
        .expect("memory export")
}

fn read_str(caller: &mut Caller<'_, WasmHostState>, ptr: i32, len: i32) -> String {
    let mem = memory(caller);
    let data = mem.data(caller);
    let (ptr, len) = (ptr as usize, len as usize);
    if ptr + len <= data.len() {
        String::from_utf8_lossy(&data[ptr..ptr + len]).to_string()
    } else {
        String::new()
    }
}

fn read_json(caller: &mut Caller<'_, WasmHostState>, ptr: i32, len: i32) -> Value {
    let text = read_str(caller, ptr, len);
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

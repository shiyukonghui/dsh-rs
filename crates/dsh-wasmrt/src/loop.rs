//! DSH 层 loop 宿主：把实现 `dsh-loop` world 的 WASM 组件适配为 dsh-core `Plugin`。
//!
//! 架构（第一性原理）：「loop 本身可替换」的正确分层是
//! - **缝**（session/tools/llm/agent-loop）= WIT（`wit-dsh/dsh-loop.wit`），类型化契约；
//! - **loop** = WASM 插件（如 `echo-loop`），实现 `agent-loop` 缝；
//! - **缝的承载** = 宿主 Host 实现（本模块）——session/tools/llm 的 Host trait
//!   是**宿主的职责**（如同 WASI 的 Host 实现），不是「native DSH 层参考插件」。
//!
//! `WasmLoopPlugin` 把 WASM loop 组件适配为 [`Plugin`]：apply 时实例化组件
//! （session/tools/llm Host + WASI 一并注册），返回 disposer（卸载时清理）。
//! 宿主经 `run_turn` 调用 WASM 内的 loop 驱动；session 事件由宿主缝记录。

use std::cell::RefCell;
use std::rc::Rc;

use dsh_core::*;
use wasmtime::component::{Component, Linker};
use wasmtime::{Engine, Store};

use crate::abi::Capabilities;

// 使 `tools::Host::list_tools` 等方法可经 `.list_tools()` 直接调用。
#[allow(unused_imports)]
use dsh::dsh::tools::Host as _;

// 编译期绑定 dsh-loop world（host 侧；生成顶层 DshLoop + dsh::dsh::* 模块）。
wasmtime::component::bindgen!({
    path: "wit-dsh/dsh-loop.wit",
    world: "dsh-loop",
    async: false,
});

// apply 期间当前挂载上下文（host 回调经此访问 Cordis；单线程内安全）。
thread_local! {
    static CURRENT_CTX: RefCell<Option<Cordis>> = const { RefCell::new(None) };
}

/// 宿主缝实现（session/tools/llm 的 Host trait + WASI 视图）。
///
/// 桥接：当 Cordis 提供了 `sessions`（[`SessionLog`]）与 `tools`（[`ToolRegistry`]）
/// 服务时，WASM loop 经缝的写入/调用落入这些服务（宿主可经 `ctx` 查询）；
/// 未提供时回退到内存记录（`appends`/`tool_calls`）供断言。
pub struct LoopHost {
    /// session::append 收到的 (kind, payload)（内存回退记录）。
    pub appends: Vec<(String, Vec<u8>)>,
    /// tools::execute 收到的 (name, arguments)（内存回退记录）。
    pub tool_calls: Vec<(String, Vec<u8>)>,
    /// llm::generate 收到的 messages 字节。
    pub llm_calls: Vec<Vec<u8>>,
    /// WASI preview2 上下文与资源表（组件为 wasip1 构建）。
    wasi: Option<wasmtime_wasi::p2::WasiCtx>,
    table: Option<wasmtime::component::ResourceTable>,
}

impl LoopHost {
    /// 构建宿主缝实现（WASI 上下文按能力集精细授予）。
    pub fn new(caps: Capabilities) -> Self {
        LoopHost {
            appends: Vec::new(),
            tool_calls: Vec::new(),
            llm_calls: Vec::new(),
            wasi: Some(caps.build_wasi_ctx()),
            table: Some(wasmtime::component::ResourceTable::new()),
        }
    }

    /// 当前 Cordis（apply 期间注入；thread_local 桥接 Send 约束）。
    fn ctx(&self) -> Option<Cordis> {
        CURRENT_CTX.with(|c| c.borrow().clone())
    }

    /// 向 Cordis `sessions` 服务追加（若提供）；否则内存记录。
    fn append_session(&mut self, kind: &str, payload: Vec<u8>) -> u64 {
        self.appends.push((kind.to_string(), payload.clone()));
        if let Some(ctx) = self.ctx() {
            if let Some(handle) = ctx.get_typed::<SessionHandle>("sessions") {
                return handle.lock().unwrap().append(kind, payload);
            }
        }
        (self.appends.len() - 1) as u64
    }

    /// 经 Cordis `tools` 服务执行（若提供）；否则回退内存 add 工具。
    fn execute_tool(&mut self, name: &str, arguments: Value) -> Value {
        self.tool_calls
            .push((name.to_string(), serde_json::to_vec(&arguments).unwrap_or_default()));
        if let Some(ctx) = self.ctx() {
            if let Some(handle) = ctx.get_typed::<ToolRegistryHandle>("tools") {
                return handle.lock().unwrap().execute(name, arguments);
            }
        }
        // 内存回退：add 工具
        if name == "add" {
            let a = arguments.get("a").and_then(|v| v.as_i64()).unwrap_or(0);
            let b = arguments.get("b").and_then(|v| v.as_i64()).unwrap_or(0);
            return serde_json::json!({"sum": a + b});
        }
        serde_json::json!({"error": format!("tool \"{name}\" not registered")})
    }

    /// 取已记录的 session 事件种类序列（诊断/断言用）。
    pub fn event_kinds(&self) -> Vec<String> {
        self.appends.iter().map(|(k, _)| k.clone()).collect()
    }

    /// 解析第 `i` 个 session 事件的 payload（JSON）。
    pub fn event_payload(&self, i: usize) -> Value {
        self.appends
            .get(i)
            .and_then(|(_, p)| serde_json::from_slice(p).ok())
            .unwrap_or(Value::Null)
    }

    /// 投影模型历史（对应 deepseek-harness `Session.deriveMessages` /
    /// `deriveEventMessage`，M34 对齐生产 `Message` 形状）：
    /// 从 session 事件流提取 user/message、assistant/message、tool/result 的
    /// 消息序列（按事件顺序）。宿主侧最小投影，供断言与诊断。
    /// - `user/message` → data 逐字透传（data 本身即完整 `Message` 对象）；
    /// - `assistant/message` → `data.message`（content 空数组跳过）；
    /// - `tool/result` → `data.message`。
    pub fn derive_messages(&self) -> Vec<Value> {
        let mut out = Vec::new();
        for (kind, payload) in &self.appends {
            let v: Value = serde_json::from_slice(payload).unwrap_or(Value::Null);
            match kind.as_str() {
                "user/message" => out.push(v),
                "assistant/message" => {
                    let Some(msg) = v.get("message").cloned() else {
                        continue;
                    };
                    if msg
                        .get("content")
                        .and_then(|c| c.as_array())
                        .map(|a| a.is_empty())
                        .unwrap_or(false)
                    {
                        continue;
                    }
                    out.push(msg);
                }
                "tool/result" => {
                    if let Some(msg) = v.get("message").cloned() {
                        out.push(msg);
                    }
                }
                _ => {}
            }
        }
        out
    }
}

impl Default for LoopHost {
    fn default() -> Self {
        Self::new(Capabilities::all())
    }
}

impl wasmtime_wasi::p2::IoView for LoopHost {
    fn table(&mut self) -> &mut wasmtime::component::ResourceTable {
        self.table.as_mut().expect("wasi table")
    }
}

impl wasmtime_wasi::p2::WasiView for LoopHost {
    fn ctx(&mut self) -> &mut wasmtime_wasi::p2::WasiCtx {
        self.wasi.as_mut().expect("wasi ctx")
    }
}

// dsh-loop 缝的宿主实现（session/tools/llm）。
impl dsh::dsh::session::Host for LoopHost {
    fn append(&mut self, kind: String, payload: Vec<u8>) -> u32 {
        self.append_session(&kind, payload) as u32
    }
    fn derive_messages(&mut self) -> Vec<u8> {
        // 优先取 Cordis sessions 服务的投影；否则内存回退（LoopHost::derive_messages）。
        if let Some(ctx) = self.ctx() {
            if let Some(handle) = ctx.get_typed::<SessionHandle>("sessions") {
                let log = handle.lock().unwrap();
                return serde_json::to_vec(&log.derive_messages()).unwrap_or_default();
            }
        }
        serde_json::to_vec(&LoopHost::derive_messages(self)).unwrap_or_default()
    }
}
impl dsh::dsh::tools::Host for LoopHost {
    fn execute(&mut self, name: String, arguments: Vec<u8>) -> Vec<u8> {
        let args: Value = serde_json::from_slice(&arguments).unwrap_or(Value::Null);
        let result = self.execute_tool(&name, args);
        serde_json::to_vec(&result).unwrap_or_default()
    }
    fn register(&mut self, _name: String, _schema: Vec<u8>, _handler: u32) -> u32 {
        0
    }
    /// 枚举已注册工具（WIT `tools::list-tools` → `list_tools`）。
    fn list_tools(&mut self) -> Vec<u8> {
        let items: Vec<(String, Value)> = if let Some(ctx) = self.ctx() {
            if let Some(handle) = ctx.get_typed::<ToolRegistryHandle>("tools") {
                handle.lock().unwrap().list()
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };
        serde_json::to_vec(&items).unwrap_or_default()
    }
}
impl dsh::dsh::llm::Host for LoopHost {
    fn generate(&mut self, provider: String, messages: Vec<u8>, tools: Vec<u8>) -> Vec<u8> {
        self.llm_calls.push(messages.clone());
        // 桥接 Cordis llm 服务（ctx.llm；按 provider 选适配器）；
        // 未提供时回退：回显首条消息。
        if let Some(ctx) = self.ctx() {
            if let Some(handle) = ctx.get_typed::<LlmHandle>("llm") {
                let msgs: Vec<Value> = serde_json::from_slice(&messages).unwrap_or_default();
                let tool_schemas: Vec<Value> = serde_json::from_slice(&tools).unwrap_or_default();
                let provider = if provider.is_empty() {
                    None
                } else {
                    Some(provider.as_str())
                };
                let result = handle.lock().unwrap().generate(provider, msgs, tool_schemas);
                return serde_json::to_vec(&result).unwrap_or_default();
            }
        }
        // 内存回退：把 messages 原样返回（WASM loop 的 echo-loop 不调用 llm 缝）。
        messages
    }
}

/// 一个 dsh-loop 组件实例（Store + 组件接口句柄）。
pub struct LoopRuntime {
    pub store: Store<LoopHost>,
    pub plugin: DshLoop,
}

/// DSH 层 loop 宿主插件（dsh-core `Plugin` 适配）。
pub struct WasmLoopPlugin {
    name: &'static str,
    caps: Capabilities,
    engine: Engine,
    component: Component,
    rt: Rc<RefCell<Option<LoopRuntime>>>,
}

impl WasmLoopPlugin {
    /// 从 dsh-loop world 组件字节构造插件。
    pub fn new(name: &'static str, bytes: &[u8], caps: Capabilities) -> Result<Self, CordisError> {
        Self::new_owned(name, bytes, caps)
    }

    /// 从 dsh-loop world 组件字节构造插件（运行时名字；泄漏换取 `&'static str`）。
    pub fn new_owned(name: &str, bytes: &[u8], caps: Capabilities) -> Result<Self, CordisError> {
        let engine = Engine::default();
        let component = Component::from_binary(&engine, bytes)
            .map_err(|e| CordisError::Internal(format!("wasm loop component: {e}")))?;
        let name: &'static str = Box::leak(name.to_string().into_boxed_str());
        Ok(WasmLoopPlugin {
            name,
            caps,
            engine,
            component,
            rt: Rc::new(RefCell::new(None)),
        })
    }

    fn instantiate(&self) -> Result<LoopRuntime, CordisError> {
        let mut store = Store::new(&self.engine, LoopHost::new(self.caps));
        let mut linker = Linker::<LoopHost>::new(&self.engine);

        // WASI preview2（组件为 wasip1 构建，import wasi:* 接口）
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
            .map_err(|e| CordisError::Internal(format!("linker wasi: {e}")))?;

        // 注册 dsh-loop 三缝的宿主实现
        dsh::dsh::session::add_to_linker::<LoopHost, wasmtime::component::HasSelf<LoopHost>>(
            &mut linker,
            |s| s,
        )
        .map_err(|e| CordisError::Internal(format!("linker session: {e}")))?;
        dsh::dsh::tools::add_to_linker::<LoopHost, wasmtime::component::HasSelf<LoopHost>>(
            &mut linker,
            |s| s,
        )
        .map_err(|e| CordisError::Internal(format!("linker tools: {e}")))?;
        dsh::dsh::llm::add_to_linker::<LoopHost, wasmtime::component::HasSelf<LoopHost>>(
            &mut linker,
            |s| s,
        )
        .map_err(|e| CordisError::Internal(format!("linker llm: {e}")))?;

        let plugin = DshLoop::instantiate(&mut store, &self.component, &linker)
            .map_err(|e| CordisError::Internal(format!("instantiate dsh-loop: {e}")))?;
        Ok(LoopRuntime { store, plugin })
    }

    fn runtime(&self) -> Result<std::cell::RefMut<'_, Option<LoopRuntime>>, CordisError> {
        let mut rt = self.rt.borrow_mut();
        if rt.is_none() {
            *rt = Some(self.instantiate()?);
        }
        Ok(rt)
    }

    /// 驱动一个 turn：调用 WASM loop 的 `run_turn`（input 为 JSON 字节）。
    /// `ctx` 用于桥接 Cordis 服务（sessions/tools）；返回 WASM 插件的结果（JSON）。
    pub fn run_turn(&self, ctx: &Cordis, input: &Value) -> Result<Value, CordisError> {
        // 注入当前上下文（host 回调经 thread_local 访问 Cordis）
        CURRENT_CTX.with(|c| *c.borrow_mut() = Some(ctx.clone()));
        let result = self.run_turn_inner(input);
        CURRENT_CTX.with(|c| *c.borrow_mut() = None);
        result
    }

    fn run_turn_inner(&self, input: &Value) -> Result<Value, CordisError> {
        let mut rt = self.runtime()?;
        let runtime = rt.as_mut().expect("loop runtime ready");
        let input_bytes = serde_json::to_vec(input)
            .map_err(|e| CordisError::Internal(format!("input encode: {e}")))?;
        let result = runtime
            .plugin
            .dsh_dsh_agent_loop()
            .call_run_turn(&mut runtime.store, &input_bytes, 0)
            .map_err(|e| CordisError::Internal(format!("run_turn: {e}")))?;
        serde_json::from_slice(&result)
            .map_err(|e| CordisError::Internal(format!("result decode: {e}")))
    }

    /// 宿主缝记录的 session 事件种类（诊断/断言）。
    pub fn event_kinds(&self) -> Vec<String> {
        self.rt
            .borrow()
            .as_ref()
            .map(|r| r.store.data().event_kinds())
            .unwrap_or_default()
    }

    /// 宿主缝投影的模型历史（user/assistant/tool 消息序列）。
    pub fn derive_messages(&self) -> Vec<Value> {
        self.rt
            .borrow()
            .as_ref()
            .map(|r| r.store.data().derive_messages())
            .unwrap_or_default()
    }

    /// 枚举宿主已注册工具 `(name, schema)` 对（JSON 字节）。
    /// 直接驱动 host 侧 `tools::list-tools` 桥接（注入 ctx 后调 `LoopHost`）；
    /// 供测试验证 WIT `list-tools` 缝的 host 实现。
    pub fn list_tools(&self, ctx: &Cordis) -> Vec<u8> {
        let _ = self.runtime(); // 确保实例化
        CURRENT_CTX.with(|c| *c.borrow_mut() = Some(ctx.clone()));
        let out = self
            .rt
            .borrow_mut()
            .as_mut()
            .map(|r| r.store.data_mut().list_tools())
            .unwrap_or_default();
        CURRENT_CTX.with(|c| *c.borrow_mut() = None);
        out
    }
}

impl Plugin for WasmLoopPlugin {
    fn name(&self) -> &'static str {
        self.name
    }

    fn apply(&self, ctx: &Cordis, _config: Value) -> Result<EffectOutcome, CordisError> {
        // 注入当前上下文（apply 期间 host 回调可访问 Cordis）
        CURRENT_CTX.with(|c| *c.borrow_mut() = Some(ctx.clone()));
        let outcome = (|| {
            // 实例化组件（懒）；提供 run_turn 服务供宿主调用
            self.runtime()?;
            // 卸载时清理实例
            let rt = self.rt.clone();
            ctx.effect(
                "wasm-loop-dispose",
                Box::new(move |_ctx| {
                    Ok(EffectOutcome::One(Rc::new(move |_ctx| {
                        *rt.borrow_mut() = None;
                    })))
                }),
            )?;
            Ok(EffectOutcome::None)
        })();
        CURRENT_CTX.with(|c| *c.borrow_mut() = None);
        outcome
    }
}

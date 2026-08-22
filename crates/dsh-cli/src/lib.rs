//! DSH 层启动器核心：从 cordis.yml 形态配置组装运行时并驱动 loop。
//!
//! 对应 deepseek-harness 的 app-boot（profile → bundle → cordis.patch → 挂载）：
//! 1. 读 YAML 入口列表（services + loop entries），叠加 profile overlays
//!    （同 id entry 后者覆盖 config——bundle/patch 语义）；
//! 2. 注册插件仓库：`dsh:services`（缝的承载）+ WASM loop 插件（按 entry 的
//!    `config.wasm` 指明组件目录或 `.wasm` 文件路径构建）；
//! 3. `Include` 挂载；
//! 4. 宿主经 `run_turn` 驱动 WASM loop（输入来自调用方）。

// 同 dsh-core：单线程运行时，`Arc` 仅共享所有权。
#![allow(clippy::arc_with_non_send_sync)]

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use dsh_core::*;
use dsh_loader::{EntryOptions, Include, Loader};
use dsh_wasmrt::{Capabilities, DshServicesPlugin, WasmLoopPlugin};

/// `dsh web`——服务 DeepSeek Harness 前端 + `/api` RPC（M70）。
pub mod web;

/// M3a host 目录方法面（listDirectory/createDirectory 真实 fs 实现，可差分单测）。
pub mod host_dir;

/// M1e SessionHost：把 WASM loop 的 SessionLog 事件 adopt 进 dsh-session store，
/// 并挂载持久化（dsh-persistence coordinator event 回调）。
pub mod session_host;

/// 启动结果：运行时上下文 + loop 插件句柄（供驱动）。
pub struct Boot {
    pub ctx: Cordis,
    /// M58：loop 插件句柄（`Rc<RefCell<>>`——HMR refresh 换 loop 组件时替换）。
    pub loop_plugin: Rc<std::cell::RefCell<Arc<WasmLoopPlugin>>>,
    /// 可用服务句柄（诊断）。
    pub sessions: dsh_core::SessionHandle,
    /// M1e：llm 服务句柄（`llm.providers`/`llm.models` 目录来源）。
    pub llm: dsh_core::LlmHandle,
    /// HMR refresh 回调：重读主配置 + overlays → 重新挂载（watch 模式用；
    /// 对应 Cordis Include 插件的 `internal/update → refresh` 路径）。
    pub refresh: Rc<dyn Fn() -> Result<(), CordisError>>,
    /// M2g：可选的 Rust AgentLoopHost（装配了真实 agent-loop 服务；Some 时
    /// `session.prompt`/`agent.run` 改驱 Rust loop，None 保留 M1 WASM loop 路径）。
    pub agent_loop: Option<Rc<dsh_agent_loop::AgentLoopHost>>,
    /// M3b：settings 能力缝（namespace 注册 + describe/update/replace/mutate + 文件）。
    /// `Rc<RefCell>`——web RPC 只持 `&Boot`，跨请求共享可变状态。
    pub settings: Rc<std::cell::RefCell<dsh_settings::SettingsProvider>>,
    /// M3c：credentials 能力缝（env/file 分层 + set/unset + 文件）。
    pub credentials: Rc<std::cell::RefCell<dsh_credentials::CredentialProvider>>,
}

/// M56：转储生效配置（对齐生产 `dsh --dump-config`）——读主配置 + overlays
/// 合并（同 id 后者覆盖 config、新 id 追加），序列化为 YAML；**不 boot loop**
/// （纯配置查看）。
pub fn dump_config(config_path: &Path, overlays: &[PathBuf]) -> Result<String, CordisError> {
    let mut entries = read_entries(config_path)?;
    for overlay in overlays {
        let layer = read_entries(overlay)?;
        entries = merge_entries(entries, layer);
    }
    serde_yaml::to_string(&entries)
        .map_err(|e| CordisError::Internal(format!("dump-config serialize: {e}")))
}

/// 从 cordis.yml 形态的 YAML 配置启动。
///
/// - `config_path`：主配置（YAML 入口列表）。
/// - `overlays`：profile 叠加层（按顺序应用；同 id entry 后者覆盖 config——
///   bundle/patch 语义，对应 DSH 的 overlay 层）。
/// - `wasm_base`：WASM loop 组件的解析基址。entry `config.wasm` 两种形态：
///   - 目录名（如 `echo-loop`）→ `<wasm_base>/<dir>/target/wasm32-wasip1/debug/<dir>_plugin.wasm`
///   - 直接 `.wasm` 文件路径（相对 wasm_base 或绝对）。
pub fn boot(
    config_path: &Path,
    overlays: &[PathBuf],
    wasm_base: &Path,
) -> Result<Boot, CordisError> {
    // 读主配置 + 叠加层，合并 entries（同 id 后者覆盖）
    let mut entries = read_entries(config_path)?;
    for overlay in overlays {
        let layer = read_entries(overlay)?;
        entries = merge_entries(entries, layer);
    }

    let cordis = Cordis::new();
    let loader = Loader::new(&cordis)?;

    // 注册插件仓库：services + 每个非 services entry 按 config.wasm 构建 WASM loop
    loader.register_plugin("dsh:services", Arc::new(DshServicesPlugin::all()));
    let mut loop_plugin: Option<Arc<WasmLoopPlugin>> = None;
    for entry in &entries {
        if entry.name == "dsh:services" {
            continue;
        }
        let wasm = entry
            .config
            .get("wasm")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                CordisError::Internal(format!(
                    "boot: entry \"{}\" needs config.wasm (component dir or .wasm path)",
                    entry.id
                ))
            })?;
        let bytes = load_component(wasm_base, wasm)?;
        // loop 是 dsh-loop world 组件：直接构造 WasmLoopPlugin（run_turn 需具体类型）。
        // 能力按 entry 配置授予（`config.caps` 数组；缺省 = ABI 能力，无 WASI）。
        let caps = Capabilities::from_json(entry.config.get("caps"));
        let plugin = Arc::new(WasmLoopPlugin::new_owned(&entry.name, &bytes, caps)?);
        let dyn_plugin: Arc<dyn Plugin> = plugin.clone();
        loader.register_plugin(&entry.name, dyn_plugin);
        loop_plugin = Some(plugin);
    }
    let loop_plugin = loop_plugin.ok_or_else(|| {
        CordisError::Internal("boot: no loop entry (non-services) in cordis.yml".into())
    })?;
    // M58：可变 loop 句柄（HMR refresh 换组件时替换）
    let loop_cell: Rc<std::cell::RefCell<Arc<WasmLoopPlugin>>> =
        Rc::new(std::cell::RefCell::new(loop_plugin.clone()));

    // Include 挂载（用合并后的配置；临时写回供 loader.sync）
    let merged = merge_path_for_include(config_path, &entries)?;
    let include = Include::new(&loader, &merged, vec![]);
    include.load()?;

    let sessions: dsh_core::SessionHandle = cordis
        .get_typed::<dsh_core::SessionHandle>("sessions")
        .ok_or_else(|| CordisError::Internal("boot: sessions service missing".into()))?
        .as_ref()
        .clone();

    // M1e：llm 服务句柄（真实模型适配器注册处；web `llm.providers`/`llm.models`
    // 的目录来源）。sessions 服务必有，但 llm 可能未注册——缺省给空服务。
    let llm: dsh_core::LlmHandle = cordis
        .get_typed::<dsh_core::LlmHandle>("llm")
        .map(|a| a.as_ref().clone())
        .unwrap_or_else(dsh_core::new_llm);

    // HMR refresh：重读主配置 + overlays → 重新挂载（async 事务 `load_async` →
    // `sync_async` allSettled + 整事务回滚；经 current_thread runtime block_on 驱动）。
    let refresh_loader = loader.clone();
    let refresh_config = config_path.to_path_buf();
    let refresh_overlays = overlays.to_vec();
    let refresh_wasm_base = wasm_base.to_path_buf();
    let refresh_loop_cell = loop_cell.clone();
    let refresh: Rc<dyn Fn() -> Result<(), CordisError>> = Rc::new(move || {
        let entries = read_entries(&refresh_config)?;
        let mut merged = entries;
        for overlay in &refresh_overlays {
            let layer = read_entries(overlay)?;
            merged = merge_entries(merged, layer);
        }
        let tmp = merge_path_for_include(&refresh_config, &merged)?;
        let include = Include::new(&refresh_loader, &tmp, vec![]);
        // async 事务：全部入口都尝试 create/update（一个失败不阻断其他）、
        // 失败整事务回滚——对应 Cordis `EntryGroup.update(config)`。
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| CordisError::Internal(format!("hmr refresh runtime: {e}")))?;
        rt.block_on(include.load_async()).map_err(|agg| {
            // AggregateError → 首个失败（消息含数量）
            CordisError::Internal(format!(
                "hmr refresh failed ({} errors)",
                agg.errors.len()
            ))
        })?;
        // M58：HMR 换 loop 组件——按合并后 loop entry 的 config.wasm 重建
        // WasmLoopPlugin 并替换句柄（config.wasm 变化时新组件生效）。
        if let Some(loop_entry) = merged.iter().find(|e| e.name != "dsh:services") {
            let wasm = loop_entry
                .config
                .get("wasm")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    CordisError::Internal("hmr refresh: loop entry needs config.wasm".into())
                })?;
            let bytes = load_component(&refresh_wasm_base, wasm)?;
            let caps = Capabilities::from_json(loop_entry.config.get("caps"));
            let plugin = Arc::new(WasmLoopPlugin::new_owned(&loop_entry.name, &bytes, caps)?);
            *refresh_loop_cell.borrow_mut() = plugin;
        }
        Ok(())
    });

    let settings = Rc::new(std::cell::RefCell::new(
        dsh_settings::SettingsProvider::memory(),
    ));
    // M3d：注册 LLM 连接 namespace（对齐 TS `llm` 插件注册集）。schema 覆盖
    // provider/model/baseURL/apiKey(secret)；用户写入即落到本地文档。
    {
        let mut sp = settings.borrow_mut();
        let mut dict = std::collections::HashMap::new();
        dict.insert(
            "provider".to_string(),
            dsh_schema::Schema::with_default(&dsh_schema::Schema::string(), serde_json::json!("dsh")),
        );
        dict.insert(
            "model".to_string(),
            dsh_schema::Schema::with_default(&dsh_schema::Schema::string(), serde_json::json!("echo")),
        );
        dict.insert("baseURL".to_string(), dsh_schema::Schema::string());
        dict.insert(
            "apiKey".to_string(),
            dsh_schema::Schema::secret(&dsh_schema::Schema::string()),
        );
        sp.register("llm", &dsh_schema::Schema::object(dict), None, dsh_settings::Applies::Restart);
    }

    Ok(Boot {
        ctx: cordis,
        loop_plugin: loop_cell,
        sessions,
        llm,
        refresh,
        agent_loop: None,
        settings,
        credentials: Rc::new(std::cell::RefCell::new(
            dsh_credentials::CredentialProvider::memory(),
        )),
    })
}

/// 读取 YAML 入口列表。
fn read_entries(path: &Path) -> Result<Vec<EntryOptions>, CordisError> {    let text = std::fs::read_to_string(path).map_err(|e| {
        CordisError::Internal(format!("boot read {}: {e}", path.display()))
    })?;
    let value: Value = serde_yaml::from_str(&text)
        .map_err(|e| CordisError::Internal(format!("boot parse yaml: {e}")))?;
    match value {
        Value::Array(items) => items
            .iter()
            .map(|v| serde_json::from_value(v.clone()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| CordisError::Internal(format!("boot entries invalid: {e}"))),
        _ => Err(CordisError::Internal(
            "cordis.yml must be a top-level array".into(),
        )),
    }
}

/// 合并两层 entries（同 id 后者覆盖 config/name；base 顺序保留，新 id 追加）。
fn merge_entries(base: Vec<EntryOptions>, overlay: Vec<EntryOptions>) -> Vec<EntryOptions> {
    let mut out = base;
    for layer in overlay {
        match out.iter_mut().find(|e| e.id == layer.id) {
            Some(existing) => {
                existing.name = layer.name;
                if !layer.config.is_null() && layer.config.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
                    existing.config = layer.config;
                }
                existing.disabled = layer.disabled;
            }
            None => out.push(layer),
        }
    }
    out
}

/// 把合并后的 entries 写为临时 YAML 供 Include 挂载（loader.sync 读文件）。
fn merge_path_for_include(config_path: &Path, entries: &[EntryOptions]) -> Result<PathBuf, CordisError> {
    // 文件名含地址唯一性（多 boot 并行时避免覆盖）
    let unique = format!("{:p}", entries.as_ptr()).replace("0x", "");
    let tmp = std::env::temp_dir().join(format!(
        "dsh-boot-{}-{}-{}",
        config_path.file_name().and_then(|s| s.to_str()).unwrap_or("cfg"),
        std::process::id(),
        unique
    ));
    let yaml = serde_yaml::to_string(entries)
        .map_err(|e| CordisError::Internal(format!("boot serialize merged: {e}")))?;
    std::fs::write(&tmp, yaml)
        .map_err(|e| CordisError::Internal(format!("boot write merged: {e}")))?;
    Ok(tmp)
}

/// 读取 WASM loop 组件字节。
///
/// `spec` 两种形态：
/// - 目录名（如 `echo-loop`）→ `<wasm_base>/<dir>/target/wasm32-wasip1/debug/<dir>_plugin.wasm`
/// - 以 `.wasm` 结尾 → 相对 wasm_base 或绝对路径直接读取。
fn load_component(wasm_base: &Path, spec: &str) -> Result<Vec<u8>, CordisError> {
    let path = if spec.ends_with(".wasm") {
        let p = PathBuf::from(spec);
        if p.is_absolute() {
            p
        } else {
            wasm_base.join(p)
        }
    } else {
        let wasm_name = format!("{}_plugin.wasm", spec.replace('-', "_"));
        wasm_base
            .join(spec)
            .join("target/wasm32-wasip1/debug")
            .join(wasm_name)
    };
    std::fs::read(&path).map_err(|e| {
        CordisError::Internal(format!(
            "boot: read wasm component {}: {e}",
            path.display()
        ))
    })
}

/// 驱动一个 turn（宿主侧：注入 ctx → run_turn）。
/// M58：经 `Rc<RefCell<>>` 读当前 loop 插件（HMR refresh 换组件后生效）。
pub fn run_turn(boot: &Boot, input: &Value) -> Result<Value, CordisError> {
    let plugin = boot.loop_plugin.borrow().clone();
    plugin.run_turn(&boot.ctx, input)
}

/// M2g：把一条 user 文本驱动进 Rust AgentLoopHost 的配置 agent。
/// - 目标 agent：配置项中 `sessionId == session_id` 或默认 `agent-{id} == session_id` 者；
/// - agent 懒装配（ensure_agent 幂等）；事件直接写 AgentLoopHost 持有的共享 store
///   （web 侧与 SessionHost 同店 → 前端读模型/下链/持久化同一事实源）；
/// - 无 host 或无可路由 agent → Err（fail loud）。
pub fn run_rust_loop(boot: &Boot, session_id: &str, content: &str) -> Result<(), CordisError> {
    let host = boot.agent_loop.as_ref().ok_or_else(|| {
        CordisError::Internal("no Rust AgentLoopHost assembled in this boot".into())
    })?;
    let configured = host
        .config
        .agents
        .iter()
        .find(|a| {
            a.session_id.as_deref() == Some(session_id)
                || a.resume_session_id.as_deref() == Some(session_id)
                || format!("agent-{}", a.id) == session_id
        })
        .cloned()
        .ok_or_else(|| {
            CordisError::Internal(format!(
                "no configured agent maps to session \"{session_id}\""
            ))
        })?;
    host.ensure_agent(&configured)
        .map_err(|e| CordisError::Internal(format!("agent-loop host: {e}")))?;
    let message = dsh_llm::Message::user(
        dsh_llm::MessageId::from_raw(format!("prompt-{session_id}")),
        vec![dsh_llm::ContentBlock::text(content)],
    );
    host.followup(&configured.id, message)
        .map_err(|e| CordisError::Internal(format!("agent-loop host: {e}")))
}

/// headless 单发任务的结果（对齐 DSH `dsh --profile headless "job"`：
/// 从 session 事件推导最终答案与 turn 结束原因）。
#[derive(Debug, Clone)]
pub struct HeadlessResult {
    /// 最后一个非空 assistant 文本（`data.message.content[0].text`）。
    pub answer: String,
    /// 最终 turn/end 的 `data.reason`（completed/blocked/max-tokens/...）。
    pub reason: String,
}

/// M45：headless 单发模式——提交一个任务（user 消息），驱动 loop，从
/// **session 事件流**推导最终答案（而非 loop 返回值——任何 loop 都可用）。
/// - 取最后一条 `assistant/message` 的 `data.message.content[0].text`
///   （M34 生产 Message 形状；空 content 的助手消息被 `derive_messages`
///   跳过，此处同样跳过空文本）；
/// - 取最后 `turn/end` 的 `data.reason`；
/// - 无 assistant 消息 → Err（fail loud）。
pub fn run_headless(boot: &Boot, task: &str) -> Result<HeadlessResult, CordisError> {
    run_turn(boot, &json!({"content": task}))?;
    let log = boot.sessions.lock().unwrap();
    derive_headless(log.events())
}

/// M48：恢复会话（`--session-in`）——从 JSONL 加载历史事件并导入
/// `boot.sessions`（append 重放 events + surface；`session_history()` 投影
/// 含前轮消息 → 多轮共享上下文，对齐 DSH resume 语义）。
pub fn restore_session(boot: &Boot, path: &std::path::Path) -> Result<(), CordisError> {
    let loaded = dsh_core::SessionLog::load_from(path)?;
    let mut log = boot.sessions.lock().unwrap();
    for e in loaded.events() {
        log.append(&e.kind, e.payload.clone());
    }
    Ok(())
}

/// 从 session 事件流推导 headless 结果（独立函数：可单测错误路径）。
pub(crate) fn derive_headless(events: &[dsh_core::SessionEvent]) -> Result<HeadlessResult, CordisError> {
    let mut answer: Option<String> = None;
    let mut reason: Option<String> = None;
    for e in events {
        let v = e.payload_value();
        match e.kind.as_str() {
            "assistant/message" => {
                // data = {turn, step, message: {id, role, content: [...], source}}
                let text = v
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array())
                    .and_then(|blocks| {
                        blocks
                            .iter()
                            .find(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
                            .and_then(|b| b.get("text").and_then(|t| t.as_str()))
                    })
                    .unwrap_or("");
                if !text.is_empty() {
                    answer = Some(text.to_string());
                }
            }
            "turn/end" => {
                if let Some(r) = v.get("reason").and_then(|r| r.as_str()) {
                    reason = Some(r.to_string());
                }
            }
            _ => {}
        }
    }
    let answer = answer.ok_or_else(|| {
        CordisError::Internal("headless: no assistant answer in session".into())
    })?;
    let reason = reason.unwrap_or_else(|| "completed".to_string());
    Ok(HeadlessResult { answer, reason })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M45：derive_headless 错误路径——无 assistant 文本 → Err。
    #[test]
    fn derive_headless_no_answer_fails() {
        let events = vec![
            dsh_core::SessionEvent {
                seq: 0,
                kind: "turn/start".into(),
                payload: serde_json::to_vec(&json!({"turn": 1})).unwrap(),
            },
            dsh_core::SessionEvent {
                seq: 1,
                kind: "turn/end".into(),
                payload: serde_json::to_vec(&json!({"turn": 1, "reason": "completed"})).unwrap(),
            },
        ];
        let err = derive_headless(&events).unwrap_err();
        assert!(err.to_string().contains("no assistant answer"), "{err}");
    }

    /// M45：derive_headless 跳过空文本助手消息（对齐 derive_messages）。
    #[test]
    fn derive_headless_skips_empty_assistant() {
        let empty = dsh_core::SessionEvent {
            seq: 0,
            kind: "assistant/message".into(),
            payload: serde_json::to_vec(&json!({
                "turn": 1, "step": 1,
                "message": {
                    "id": "a1", "role": "assistant",
                    "content": [],
                    "source": {"kind": "model", "provider": "mock", "model": "mock"},
                },
            }))
            .unwrap(),
        };
        let real = dsh_core::SessionEvent {
            seq: 1,
            kind: "assistant/message".into(),
            payload: serde_json::to_vec(&json!({
                "turn": 1, "step": 1,
                "message": {
                    "id": "a2", "role": "assistant",
                    "content": [{"type": "text", "text": "real answer"}],
                    "source": {"kind": "model", "provider": "mock", "model": "mock"},
                },
            }))
            .unwrap(),
        };
        let end = dsh_core::SessionEvent {
            seq: 2,
            kind: "turn/end".into(),
            payload: serde_json::to_vec(&json!({"turn": 1, "reason": "completed"})).unwrap(),
        };
        let r = derive_headless(&[empty, real, end]).expect("real answer");
        assert_eq!(r.answer, "real answer");
        assert_eq!(r.reason, "completed");
    }

    /// M52：`merge_entries`（`--overlay`/`--patch` 的合并语义）——
    /// 同 id 完整 config 替换（对齐生产 patch「替换整行 config」）+ 新 id
    /// 追加插入。
    #[test]
    fn merge_entries_replaces_config_and_inserts() {
        use dsh_loader::EntryOptions;
        let mut base = vec![
            EntryOptions::new("services", "dsh:services"),
            EntryOptions::new("loop", "echo-loop"),
        ];
        base[0].config = json!({"services": ["sessions"]});
        base[1].config = json!({"wasm": "echo-loop"});

        // patch 层 1：替换 loop 的完整 config（换 tool-loop）+ 插入新 entry
        let mut p1 = EntryOptions::new("loop", "tool-loop");
        p1.config = json!({"wasm": "tool-loop"});
        let p2 = EntryOptions::new("extra", "dsh:extra");
        let merged = merge_entries(base, vec![p1, p2]);

        assert_eq!(merged.len(), 3, "inserted new entry");
        let loop_entry = merged.iter().find(|e| e.id == "loop").unwrap();
        assert_eq!(loop_entry.name, "tool-loop");
        assert_eq!(loop_entry.config, json!({"wasm": "tool-loop"}), "config fully replaced");
        assert!(merged.iter().any(|e| e.id == "extra"), "new id appended");
        // 未命中的 services 保留
        assert!(merged.iter().any(|e| e.id == "services" && e.config == json!({"services": ["sessions"]})));
    }
}

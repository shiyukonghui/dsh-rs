//! D-115-Web（D2/阶段 A）：真实宿主投影器（`RemoteServiceProjector`）。
//!
//! 为 wasm remote 端点（`wasm-plugins/host-remote`，经 `host-services.get/set` 反查
//! 宿主）提供**真实数据源**——全部来自 Rust 运行时真实状态，无占位/假值：
//! - `loader` / `dynamicPlugins`：真实 `dsh-loader` 条目（含动态创建；Boot.loader）。
//! - `sessionIdentity` / `sessionMessages` / `sessionCandidates`：真实会话事件投影
//!   （SessionLog.events()）。
//! - `workspaceFiles` / `agentWorkspace`：真实工作区注册表 + 文件系统扫描。
//! - `time`：真实墙钟 epoch ms。
//! - `newVersion`：真实 uuid v4（verson 源）。
//! - `kv`：真实持久 KV（进程内 map；SQLite 后端在 serve 装配时经 `kv` store 投影——
//!   本实现缺省内存，serve 可换）。未知服务 → 规范化错误（fail-loud）。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use dsh_session::types::{EventKind, SessionEvent};
use dsh_wasmrt::RemoteServiceProjector;

/// 真实宿主投影器：持 live 运行时句柄（会话事件源 / loader / 工作区）。
pub struct RemoteHost {
    /// 真实会话事件源（`(session_id, event)` 平坦流，含 session 维度）。serve 装配
    /// `SessionHost.sink`（Arc 共享）→ 消息 id / 会话候选/身份 的真实投影。
    pub events: Option<crate::session_host::EventSink>,
    /// 真实动态装配器 `dsh-loader`（entries/create/update/dispose/fiber）。
    pub loader: Option<dsh_loader::Loader>,
    /// 真实工作区注册表（workspaceFiles / agentWorkspace 数据源）。
    pub workspaces: Option<Rc<RefCell<crate::workspace_host::WorkspaceRegistry>>>,
    /// `kv` 的进程内后端（messageFeedback 持久；serve 可换成 SQLite）。
    pub kv: Rc<RefCell<HashMap<String, serde_json::Value>>>,
    /// 阶段 B/C：动态插件包注册表（pluginId → 包定义）。serve/测试注入真实 wasm
    /// 组件字节；`runHostHalf` 按 packageId 装配进 loader（真实 fiber）。
    pub dynamic_packages: Rc<RefCell<HashMap<String, DynamicPackage>>>,
    /// 每次 handle 调用通过 host-services 反查的记账（诊断）。
    pub calls: Rc<RefCell<Vec<String>>>,
    /// D-192：真实 settings 提供者（panel-settings 只读投影数据源；None → 诚实报错）。
    pub settings: Option<Rc<RefCell<dsh_settings::SettingsProvider>>>,
}

/// 一个动态 wasm 组件包（阶段 B/C）：真实装配单位。
#[derive(Clone)]
pub struct DynamicPackage {
    /// 稳定插件 id（UI pluginId）。
    pub plugin_id: String,
    /// 包版本 id（packageId；对齐 CordisDynamicPackageId）。
    pub package_id: String,
    /// 包名（UI label）。
    pub name: String,
    /// 用途（UI purpose，如 "run"）。
    pub purpose: String,
    /// dsh-plugin world 组件字节（组件模型——禁 C ABI）。
    pub bytes: Vec<u8>,
    /// 是否有宿主半（Rust 装配 = 宿主半真实启动）。
    pub has_host_half: bool,
    /// 是否有客户端半（Rust 无 dynamic client 包 → 恒 false）。
    pub has_client_half: bool,
}

impl RemoteHost {
    pub fn new(
        events: Option<crate::session_host::EventSink>,
        loader: Option<dsh_loader::Loader>,
        workspaces: Option<Rc<RefCell<crate::workspace_host::WorkspaceRegistry>>>,
        settings: Option<Rc<RefCell<dsh_settings::SettingsProvider>>>,
    ) -> Self {
        RemoteHost {
            events,
            loader,
            workspaces,
            kv: Rc::new(RefCell::new(HashMap::new())),
            dynamic_packages: Rc::new(RefCell::new(HashMap::new())),
            calls: Rc::new(RefCell::new(Vec::new())),
            settings,
        }
    }

    /// 注册一个动态包（阶段 B/C；serve/测试注入）。
    pub fn register_dynamic_package(&self, pkg: DynamicPackage) -> Option<DynamicPackage> {
        self.dynamic_packages
            .borrow_mut()
            .insert(pkg.plugin_id.clone(), pkg)
    }

    fn log(&self, service: &str) {
        self.calls.borrow_mut().push(service.to_string());
    }

    fn err_json(code: &str, message: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "ok": false,
            "error": {"code": code, "message": message},
        }))
        .unwrap_or_default()
    }

    /// 某 session 的全部事件（sink 平坦流按 session_id 过滤；无 sink → 空，诚实）。
    fn session_events(&self, session_id: &str) -> Vec<(String, SessionEvent)> {
        let Some(sink) = self.events.as_ref() else {
            return Vec::new();
        };
        sink.lock()
            .unwrap()
            .iter()
            .filter(|(sid, _)| sid == session_id)
            .cloned()
            .collect()
    }

    /// 由真实会话事件投影 message ids（user/assistant message 事件）。
    /// 真实事件形状（history 实证）：`user/message` / `assistant/message` 事件
    /// data = `{id, role, content, source}`——**messageId 在 data.id**。
    fn session_message_ids(&self, session_id: &str) -> Vec<String> {
        let mut ids = Vec::new();
        for (_, e) in self.session_events(session_id) {
            match e.kind {
                EventKind::UserMessage | EventKind::AssistantMessage => {
                    let id = e
                        .data
                        .get("id")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    let id = id.or_else(|| {
                        e.data
                            .get("message")
                            .and_then(|m| m.get("id"))
                            .and_then(|i| i.as_str())
                            .map(|s| s.to_string())
                    });
                    if let Some(id) = id {
                        ids.push(id);
                    }
                }
                _ => {}
            }
        }
        ids
    }

    /// 全部已出现 session id（sink 平坦流去重；保持出现序）。
    fn all_session_ids(&self) -> Vec<String> {
        let Some(sink) = self.events.as_ref() else {
            return Vec::new();
        };
        let mut seen = std::collections::HashSet::new();
        let mut ids = Vec::new();
        for (sid, _) in sink.lock().unwrap().iter() {
            if seen.insert(sid.clone()) {
                ids.push(sid.clone());
            }
        }
        ids
    }

    fn loader_entries_json(&self) -> Vec<serde_json::Value> {
        let Some(loader) = self.loader.as_ref() else {
            return Vec::new();
        };
        loader
            .entries()
            .into_iter()
            .map(|e| {
                serde_json::json!({
                    "id": e.id,
                    "name": e.name,
                    "disabled": e.disabled,
                    "group": e.group,
                    "fiber": e.fiber.map(|_| serde_json::json!({"state": "active"})),
                })
            })
            .collect()
    }

    /// 默认工作区 cwd（`default` 工作区 path）。
    fn default_cwd(&self) -> String {
        self.workspaces
            .as_ref()
            .and_then(|ws| ws.borrow().get("default"))
            .map(|w| w.path.clone())
            .unwrap_or_default()
    }

    // ---- 阶段 B：真实动态装配（dynamicCordisRunner run/stop/undefine 的宿主后端）----

    /// 把动态包装配为真实 loader entry（fiber 启动）。
    /// 返回 `(pluginRunId, message)` 或 Err（诚实失败）。
    pub fn dynamic_activate(&self, plugin_id: &str, _package_id: &str) -> Result<(String, String), String> {
        let Some(loader) = self.loader.as_ref() else {
            return Err("no loader assembled (dynamic activation unavailable)".to_string());
        };
        let pkg = {
            let map = self.dynamic_packages.borrow();
            map.get(plugin_id).cloned()
        };
        let Some(pkg) = pkg else {
            return Err(format!("dynamic plugin {plugin_id} is not defined"));
        };
        // 组件模型（禁 C ABI）：dsh-plugin world 组件 → WasmComponentPlugin。
        let plugin = dsh_wasmrt::WasmComponentPlugin::new(
            Box::leak(pkg.name.clone().into_boxed_str()),
            &pkg.bytes,
            dsh_wasmrt::Capabilities::all(),
        )
        .map_err(|e| format!("dynamic plugin {plugin_id} component load: {e}"))?;
        // 注册 + 创建 entry（真实启动 fiber）。
        loader.register_plugin(&pkg.name, std::sync::Arc::new(plugin));
        let entry_id = format!("dyn:{plugin_id}");
        loader
            .create(dsh_loader::EntryOptions::new(&entry_id, &pkg.name))
            .map_err(|e| format!("dynamic plugin {plugin_id} activate: {e}"))?;
        Ok((entry_id, format!("dynamic plugin {plugin_id} activated ({})", pkg.package_id)))
    }

    /// 停跑一处动态插件（真实 dispose + 移除 entry，保留包定义）。
    pub fn dynamic_stop(&self, plugin_id: &str) -> Result<bool, String> {
        let Some(loader) = self.loader.as_ref() else {
            return Err("no loader assembled".to_string());
        };
        let entry_id = format!("dyn:{plugin_id}");
        if !loader.entries().iter().any(|e| e.id == entry_id) {
            // 未在跑 → 诚实 not-running（对齐 TS stop 语义）。
            return Ok(false);
        }
        loader
            .remove(&entry_id)
            .map_err(|e| format!("dynamic plugin {plugin_id} stop: {e}"))?;
        Ok(true)
    }

    /// 从注册表移除一处动态插件（真卸载定义 + 停跑）。
    pub fn dynamic_undefine(&self, plugin_id: &str) -> Result<bool, String> {
        let stopped = self.dynamic_stop(plugin_id)?;
        let removed = self.dynamic_packages.borrow_mut().remove(plugin_id).is_some();
        Ok(stopped || removed)
    }

    /// 动态包注册表投影（阶段 C inventory 数据源：真实可装配包清单）。
    fn dynamic_registry_json(&self) -> Vec<serde_json::Value> {
        self.dynamic_packages
            .borrow()
            .values()
            .map(|p| {
                serde_json::json!({
                    "pluginId": p.plugin_id,
                    "packageId": p.package_id,
                    "name": p.name,
                    "purpose": p.purpose,
                    "hasHostHalf": p.has_host_half,
                    "hasClientHalf": p.has_client_half,
                })
            })
            .collect()
    }
}

impl RemoteServiceProjector for RemoteHost {
    fn get(&self, service: &str, payload: &[u8]) -> Vec<u8> {
        self.log(service);
        let payload: serde_json::Value =
            serde_json::from_slice(payload).unwrap_or(serde_json::Value::Null);
        match service {
            // pluginInventory 数据源：真实 loader 条目（当前已组合插件）。
            "loader" => serde_json::to_vec(&serde_json::json!({
                "ok": true,
                "entries": self.loader_entries_json(),
            }))
            .unwrap_or_default(),
            // dynamicCordisRunner/inventory 数据源（阶段 C 真实语义）：dynamic 注册表的
            // 包定义（packages = 该插件已定义的全部不可变版本，对齐 TS define order）+
            // 装配状态（entry 是否 active → latestRun）。无包在跑 → 诚实无 latestRun/activeRun。
            "dynamicPlugins" => {
                let mut plugins: Vec<serde_json::Value> = Vec::new();
                let entries: Vec<String> = self
                    .loader
                    .as_ref()
                    .map(|l| l.entries().into_iter().map(|e| e.id).collect())
                    .unwrap_or_default();
                for pkg in self.dynamic_packages.borrow().values() {
                    let entry_id = format!("dyn:{}", pkg.plugin_id);
                    let running = entries.iter().any(|e| e == &entry_id);
                    // 基本行（pluginId/agentId/packages 必填；activeRun/latestRun 运行时才附——
                    // optional 键 undefined 放行，null 可能被 zod 拒）。
                    let mut row = serde_json::json!({
                        "pluginId": pkg.plugin_id,
                        "agentId": "default",
                        "packages": [{
                            "packageId": pkg.package_id,
                            "name": pkg.name,
                            "purpose": pkg.purpose,
                            "hasHostHalf": pkg.has_host_half,
                            "hasClientHalf": pkg.has_client_half,
                        }],
                        "currentPackageId": pkg.package_id,
                    });
                    if running {
                        row["activeRun"] = serde_json::json!({
                            "pluginRunId": entry_id, "packageId": pkg.package_id
                        });
                        row["latestRun"] = serde_json::json!({
                            "pluginRunId": entry_id,
                            "packageId": pkg.package_id,
                            "mode": "run",
                            "status": "running",
                            "host": {"status": "running", "waitingFor": []},
                            "client": {"status": "absent", "waitingFor": []},
                        });
                    }
                    plugins.push(row);
                }
                serde_json::to_vec(&serde_json::json!({"ok": true, "plugins": plugins}))
                    .unwrap_or_default()
            }
            // messageFeedback target 校验：会话真实消息 id 列表。
            "sessionMessages" => {
                let sid = payload.get("sessionId").and_then(|s| s.as_str()).unwrap_or("");
                let ids = self.session_message_ids(sid);
                serde_json::to_vec(&serde_json::json!({"ok": true, "messageIds": ids}))
                    .unwrap_or_default()
            }
            // messageFeedback 会话身份（createdAt 从首事件 time 投影）。
            "sessionIdentity" => {
                let sid = payload.get("sessionId").and_then(|s| s.as_str()).unwrap_or("");
                let evs = self.session_events(sid);
                let identity = if evs.is_empty() {
                    serde_json::Value::Null
                } else {
                    let created = evs.iter().map(|(_, e)| e.time).min().unwrap_or(0);
                    serde_json::json!({"createdAt": created, "cwd": self.default_cwd()})
                };
                serde_json::to_vec(&serde_json::json!({"ok": true, "identity": identity})).unwrap_or_default()
            }
            // sessionReferenceResolver 数据源：真实会话候选（每会话一个，无标题 → label=id）。
            "sessionCandidates" => {
                let mut candidates: Vec<serde_json::Value> = Vec::new();
                for sid in self.all_session_ids() {
                    let evs = self.session_events(&sid);
                    let created = evs.iter().map(|(_, e)| e.time).min().unwrap_or(0);
                    candidates.push(serde_json::json!({
                        "sessionId": sid,
                        "label": sid,
                        "createdAt": created,
                    }));
                }
                serde_json::to_vec(&serde_json::json!({"ok": true, "candidates": candidates})).unwrap_or_default()
            }
            // fileReferences：真实工作区 cwd（默认工作区 path）。
            "agentWorkspace" => {
                let cwd = self.default_cwd();
                serde_json::to_vec(&serde_json::json!({"ok": true, "cwd": cwd})).unwrap_or_default()
            }
            // fileReferences：真实 fs 扫描（cwd 下匹配 query 前缀的路径）。
            "workspaceFiles" => {
                let cwd = payload.get("cwd").and_then(|c| c.as_str()).unwrap_or(".");
                let query = payload.get("query").and_then(|q| q.as_str()).unwrap_or("");
                let mut paths: Vec<String> = Vec::new();
                if let Ok(rd) = std::fs::read_dir(cwd) {
                    for entry in rd.flatten() {
                        let p = entry.path();
                        let text = p.to_string_lossy().to_string();
                        let name = entry.file_name().to_string_lossy().to_string();
                        if query.is_empty()
                            || text.contains(query)
                            || name.contains(query)
                        {
                            paths.push(text);
                        }
                    }
                }
                paths.sort();
                serde_json::to_vec(&serde_json::json!({"ok": true, "paths": paths})).unwrap_or_default()
            }
            // 真实墙钟 epoch ms。
            "time" => {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                serde_json::to_vec(&serde_json::json!({"ok": true, "epochMs": now})).unwrap_or_default()
            }
            // 真实 uuid v4。
            "newVersion" => {
                let u = uuid::Uuid::new_v4().to_string();
                serde_json::to_vec(&serde_json::json!({"ok": true, "uuid": u})).unwrap_or_default()
            }
            // 持久 KV（messageFeedback 后端）。
            "kv" => {
                let key = payload.get("key").and_then(|k| k.as_str()).unwrap_or("");
                let value = self.kv.borrow().get(key).cloned().unwrap_or(serde_json::Value::Null);
                serde_json::to_vec(&serde_json::json!({"ok": true, "value": value})).unwrap_or_default()
            }
            // 阶段 B/C：动态包注册表（真实可装配包清单）。
            "dynamicRegistry" => serde_json::to_vec(&serde_json::json!({
                "ok": true,
                "plugins": self.dynamic_registry_json(),
            }))
            .unwrap_or_default(),
            // D-192（panel-settings 只读投影）：与原生 settings.describe **同形状**——
            // 复用 namespace_view（一个视图函数两处用，杜绝双源漂移）。
            "settingsDescribe" => match &self.settings {
                None => Self::err_json("no-settings", "no settings provider assembled"),
                Some(sp) => {
                    let mut sp = sp.borrow_mut();
                    let namespaces: Vec<serde_json::Value> = sp
                        .describe_all()
                        .into_iter()
                        .map(crate::web::namespace_view)
                        .collect();
                    let has_document = sp.has_document();
                    serde_json::to_vec(&serde_json::json!({
                        "ok": true,
                        "value": {
                            "writable": true,
                            "hasDocument": has_document,
                            "namespaces": namespaces,
                        }
                    }))
                    .unwrap_or_default()
                }
            },
            _ => Self::err_json("unknown-service", &format!("no service {service}")),
        }
    }

    fn set(&self, service: &str, payload: &[u8]) -> Vec<u8> {
        self.log(&format!("set:{service}"));
        match service {
            "kv" => {
                let payload: serde_json::Value =
                    serde_json::from_slice(payload).unwrap_or(serde_json::Value::Null);
                let key = payload.get("key").and_then(|k| k.as_str()).unwrap_or("").to_string();
                let value = payload.get("value").cloned().unwrap_or(serde_json::Value::Null);
                self.kv.borrow_mut().insert(key, value.clone());
                serde_json::to_vec(&serde_json::json!({"ok": true, "value": value})).unwrap_or_default()
            }
            // 阶段 B：动态激活（runHostHalf 宿主后端）——真实装配进 loader。
            "dynamicActivate" => {
                let payload: serde_json::Value =
                    serde_json::from_slice(payload).unwrap_or(serde_json::Value::Null);
                let plugin_id = payload.get("pluginId").and_then(|v| v.as_str()).unwrap_or("");
                let package_id = payload.get("packageId").and_then(|v| v.as_str()).unwrap_or("");
                match self.dynamic_activate(plugin_id, package_id) {
                    Ok((run_id, msg)) => serde_json::to_vec(&serde_json::json!({
                        "ok": true,
                        "pluginRunId": run_id,
                        "message": msg,
                    }))
                    .unwrap_or_default(),
                    Err(e) => Self::err_json("internal", &e),
                }
            }
            // 阶段 B：停跑（stopFromPanel 宿主后端）。
            "dynamicStop" => {
                let payload: serde_json::Value =
                    serde_json::from_slice(payload).unwrap_or(serde_json::Value::Null);
                let plugin_id = payload.get("pluginId").and_then(|v| v.as_str()).unwrap_or("");
                match self.dynamic_stop(plugin_id) {
                    Ok(true) => serde_json::to_vec(&serde_json::json!({"ok": true})).unwrap_or_default(),
                    Ok(false) => Self::err_json("internal", &format!("dynamic plugin {plugin_id} is not running")),
                    Err(e) => Self::err_json("internal", &e),
                }
            }
            // 阶段 B：卸载（undefineFromPanel 宿主后端）。
            "dynamicUndefine" => {
                let payload: serde_json::Value =
                    serde_json::from_slice(payload).unwrap_or(serde_json::Value::Null);
                let plugin_id = payload.get("pluginId").and_then(|v| v.as_str()).unwrap_or("");
                match self.dynamic_undefine(plugin_id) {
                    Ok(_) => serde_json::to_vec(&serde_json::json!({"ok": true})).unwrap_or_default(),
                    Err(e) => Self::err_json("internal", &e),
                }
            }
            _ => Self::err_json("read-only", &format!("service {service} is read-only")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// D-192（panel-settings）：`settingsDescribe` 投影与原生 settings.describe 同形状
    /// （复用 namespace_view 的字段面：ns/applies/revision…）。
    #[test]
    fn settings_describe_projection_matches_native_shape() {
        let settings = Rc::new(RefCell::new(dsh_settings::SettingsProvider::memory()));
        crate::register_host_settings(&mut settings.borrow_mut());
        let host = RemoteHost::new(None, None, None, Some(settings));
        let out: serde_json::Value =
            serde_json::from_slice(&host.get("settingsDescribe", b"{}")).unwrap();
        assert_eq!(out["ok"], true, "{out}");
        assert_eq!(out["value"]["writable"], true);
        let namespaces = out["value"]["namespaces"].as_array().expect("namespaces");
        assert!(!namespaces.is_empty(), "注册过产品偏好命名空间必有投影");
        let theme = namespaces
            .iter()
            .find(|n| n["ns"] == "ui-theme")
            .unwrap_or_else(|| panic!("ui-theme 应在投影里: {namespaces:?}"));
        assert_eq!(theme["applies"], "live");
        assert!(theme["revision"].is_number(), "revision 齐（乐观锁面）");
    }

    /// 缺依赖诚实报错——不伪造空命名空间表。
    #[test]
    fn settings_describe_without_reference_is_honest() {
        let host = RemoteHost::new(None, None, None, None);
        let out: serde_json::Value =
            serde_json::from_slice(&host.get("settingsDescribe", b"{}")).unwrap();
        assert_eq!(out["ok"], false, "缺 settings 引用不得报成功: {out}");
        assert_eq!(out["error"]["code"], "no-settings");
    }
}

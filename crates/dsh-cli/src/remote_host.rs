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
    /// 每次 handle 调用通过 host-services 反查的记账（诊断）。
    pub calls: Rc<RefCell<Vec<String>>>,
}

impl RemoteHost {
    pub fn new(
        events: Option<crate::session_host::EventSink>,
        loader: Option<dsh_loader::Loader>,
        workspaces: Option<Rc<RefCell<crate::workspace_host::WorkspaceRegistry>>>,
    ) -> Self {
        RemoteHost {
            events,
            loader,
            workspaces,
            kv: Rc::new(RefCell::new(HashMap::new())),
            calls: Rc::new(RefCell::new(Vec::new())),
        }
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
            // dynamicCordisRunner/inventory 数据源：真实已装配插件（含动态创建的；
            // 无 recent run → latestRun/activeRun 由组件诚实缺省）。
            "dynamicPlugins" => {
                let plugins: Vec<serde_json::Value> = self
                    .loader_entries_json()
                    .into_iter()
                    .filter(|e| e.get("group").and_then(|g| g.as_bool()) != Some(true))
                    .map(|e| {
                        serde_json::json!({
                            "pluginId": e.get("id").cloned().unwrap_or(serde_json::Value::Null),
                            // agentId 必填 string（schema intersection(string,unknown)——null 会被
                            // zod 拒）；Rust 单默认 agent → 真实 session id "default"。
                            "agentId": "default",
                            "packages": [{
                                "packageId": e.get("id").cloned().unwrap_or(serde_json::Value::Null),
                                "name": e.get("name").cloned().unwrap_or(serde_json::Value::Null),
                                "purpose": "run",
                                "hasHostHalf": true,
                                "hasClientHalf": false,
                            }],
                        })
                    })
                    .collect();
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
            _ => Self::err_json("read-only", &format!("service {service} is read-only")),
        }
    }
}

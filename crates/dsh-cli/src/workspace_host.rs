//! D-100：真实 in-process 工作区注册表（对齐 TS `packages/workspace/workspace` 的
//! 本会话语义；持久化域见 DECISIONS D-100 已知限制①，另行立项）。
//!
//! 语义来源（TS 权威）：
//! - `workspace.create`：canonicalize 路径（不存在/非目录 → 错误）；同 canonical path
//!   幂等返回既有工作区（`created:false`，不改 title）；新 path 铸**全新 id**（进程内
//!   不复用）+ `title = basename(path)` + `created:true` + prepend 到 registry order。
//! - `sessionIds` 由 `session.create{workspaceId}` 的 attach 维护（attach_session）。
//! - `archivedSessionIds` 是注册表级全局归档集。
//! - view 字段对齐 `workspaceViewSchema`：workspaceId/path/title/sessionIds/createdAt/
//!   updatedAt。
//!
//! 单线程 `Rc<RefCell>` 纪律（对齐 M4h settings/goal，D-004/D-006）：全部 RPC 处理在
//! serve 单线程 accept 循环，无需锁。跨线程（SSE/WS 事件推送）不读取注册表——host 帧
//! 经 `HostEventsLog`（Arc<Mutex<Vec<Value>>>）转发。

use std::collections::HashMap;
use std::path::Path;

/// 一个已注册工作区的当前投影行。
#[derive(Debug, Clone)]
pub struct WorkspaceRecord {
    pub workspace_id: String,
    pub path: String,
    pub title: String,
    pub session_ids: Vec<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

/// `workspace.create` 的结果：命中/新建的工作区 id + 是否真的新建。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateOutcome {
    pub id: String,
    pub created: bool,
}

/// 工作区注册表（registry order + 记录 + 全局归档集 + id 计数器）。
#[derive(Default)]
pub struct WorkspaceRegistry {
    order: Vec<String>,
    by_id: HashMap<String, WorkspaceRecord>,
    archived: Vec<String>,
    next_id: u64,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn canonical_path(path: &str) -> Result<String, String> {
    let raw = std::fs::canonicalize(path).map_err(|e| format!("workspace path '{}': {e}", path))?;
    let canonical = raw.to_string_lossy().to_string();
    if !raw.is_dir() {
        return Err(format!(
            "cannot create a workspace at '{}': path is not a directory",
            canonical
        ));
    }
    Ok(canonical)
}

fn basename(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| path.to_string())
}

impl WorkspaceRegistry {
    /// 注册 boot `default` 工作区（id `default`、path = canonical cwd、sessionIds
    /// `["default"]`）——保持既有 web UI 基线不变（D-100）。
    pub fn new() -> Self {
        let mut reg = Self::default();
        let now = now_ms();
        let cwd = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let path = canonical_path(&cwd).unwrap_or(cwd);
        reg.order.push("default".to_string());
        reg.by_id.insert(
            "default".to_string(),
            WorkspaceRecord {
                workspace_id: "default".to_string(),
                path,
                title: "default".to_string(),
                session_ids: vec!["default".to_string()],
                created_at: now,
                updated_at: now,
            },
        );
        reg
    }

    /// 铸造进程内绝不复用的工作区 id（高 64 位 = 进程级自增计数器 → 每次铸造唯一）。
    fn mint_id(&mut self) -> String {
        self.next_id += 1;
        let delta = now_ms() as u128;
        let high = (self.next_id as u128) << 64;
        format!("{:032x}", high | delta)
    }

    /// `workspace.create`：幂等去重 or 新铸。见模块文档语义。
    pub fn create(&mut self, path: &str) -> Result<CreateOutcome, String> {
        let canonical = canonical_path(path)?;
        if let Some(existing) = self.by_id.values().find(|r| r.path == canonical) {
            return Ok(CreateOutcome {
                id: existing.workspace_id.clone(),
                created: false,
            });
        }
        let id = self.mint_id();
        let now = now_ms();
        let title = basename(&canonical);
        self.order.insert(0, id.clone());
        self.by_id.insert(
            id.clone(),
            WorkspaceRecord {
                workspace_id: id.clone(),
                path: canonical,
                title,
                session_ids: Vec::new(),
                created_at: now,
                updated_at: now,
            },
        );
        Ok(CreateOutcome { id, created: true })
    }

    /// registry order（新建 prepend，`default` 排在最后除非被重排）。
    pub fn order(&self) -> Vec<String> {
        self.order.clone()
    }

    /// 完整列表（registry order）。
    pub fn list(&self) -> Vec<WorkspaceRecord> {
        self.order
            .iter()
            .filter_map(|id| self.by_id.get(id).cloned())
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<WorkspaceRecord> {
        self.by_id.get(id).cloned()
    }

    /// 当前 archivedSessionIds（归档顺序）。
    pub fn archived_session_ids(&self) -> Vec<String> {
        self.archived.clone()
    }

    /// `workspace.rename`：改 title（trim 非空）并 bump updated_at。
    pub fn rename(&mut self, id: &str, title: &str) -> Option<WorkspaceRecord> {
        let title = title.trim();
        if title.is_empty() {
            return None;
        }
        let record = self.by_id.get_mut(id)?;
        record.title = title.to_string();
        record.updated_at = now_ms();
        Some(record.clone())
    }

    /// `workspace.delete`：移除注册（保留目录/会话，同 TS 语义）。未知 id 为幂等 no-op。
    pub fn delete(&mut self, id: &str) -> bool {
        if self.by_id.remove(id).is_none() {
            return false;
        }
        self.order.retain(|candidate| candidate != id);
        true
    }

    /// `workspace.insertBefore`：DOM-insertBefore 式重排；无锚点 append。
    pub fn insert_before(&mut self, id: &str, before: Option<&str>) -> Result<Vec<String>, String> {
        if !self.by_id.contains_key(id) {
            return Err(format!(
                "workspace-order-invalid: unknown workspace '{}'",
                id
            ));
        }
        if let Some(b) = before {
            if !self.by_id.contains_key(b) {
                return Err(format!("workspace-order-invalid: unknown anchor '{}'", b));
            }
            if b == id {
                return Ok(self.order.clone());
            }
        }
        self.order.retain(|candidate| candidate != id);
        let at = match before {
            Some(b) => self
                .order
                .iter()
                .position(|candidate| candidate == b)
                .unwrap_or(self.order.len()),
            None => self.order.len(),
        };
        self.order.insert(at, id.to_string());
        Ok(self.order.clone())
    }

    /// `session.create{workspaceId}` 的 attach：把新会话追加进工作区 sessionIds
    /// （去重）并 bump updated_at。未知工作区返回 None。
    pub fn attach_session(
        &mut self,
        workspace_id: &str,
        session_id: &str,
    ) -> Option<WorkspaceRecord> {
        let record = self.by_id.get_mut(workspace_id)?;
        if !record.session_ids.iter().any(|s| s == session_id) {
            record.session_ids.push(session_id.to_string());
        }
        record.updated_at = now_ms();
        Some(record.clone())
    }

    /// `workspace.insertSessionBefore`：工作区内手动顺序重排（无锚点 append）。
    pub fn insert_session_before(
        &mut self,
        workspace_id: &str,
        session_id: &str,
        before: Option<&str>,
    ) -> Option<WorkspaceRecord> {
        let record = self.by_id.get_mut(workspace_id)?;
        if !record.session_ids.iter().any(|s| s == session_id) {
            return None;
        }
        if let Some(b) = before {
            if !record.session_ids.iter().any(|s| s == b) {
                return None;
            }
            if b == session_id {
                return Some(record.clone());
            }
        }
        record.session_ids.retain(|s| s != session_id);
        let at = match before {
            Some(b) => record
                .session_ids
                .iter()
                .position(|s| s == b)
                .unwrap_or(record.session_ids.len()),
            None => record.session_ids.len(),
        };
        record.session_ids.insert(at, session_id.to_string());
        record.updated_at = now_ms();
        Some(record.clone())
    }

    /// `workspace.archiveSession`：加入/保持注册表级归档集（幂等）。
    pub fn archive_session(&mut self, session_id: &str) -> Vec<String> {
        if !self.archived.iter().any(|s| s == session_id) {
            self.archived.push(session_id.to_string());
        }
        self.archived.clone()
    }
}

/// 对齐 `workspaceViewSchema` 的 wire 视图。
pub fn workspace_view(record: &WorkspaceRecord) -> serde_json::Value {
    serde_json::json!({
        "workspaceId": record.workspace_id,
        "path": record.path,
        "title": record.title,
        "sessionIds": record.session_ids,
        "createdAt": record.created_at.to_string(),
        "updatedAt": record.updated_at.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "dsh-ws-test-{}-{}-{}",
            tag,
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn new_registers_boot_default() {
        let reg = WorkspaceRegistry::new();
        let items = reg.list();
        assert_eq!(items.len(), 1);
        let boot = &items[0];
        assert_eq!(boot.workspace_id, "default");
        assert_eq!(boot.title, "default");
        assert_eq!(boot.session_ids, vec!["default".to_string()]);
        let cwd = std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap();
        assert_eq!(boot.path, cwd.to_string_lossy());
    }

    #[test]
    fn create_new_path_mints_unique_id_and_prepends() {
        let mut reg = WorkspaceRegistry::new();
        let dir = temp_dir("new");
        let out = reg.create(dir.to_str().unwrap()).unwrap();
        assert!(out.created);
        assert_ne!(
            out.id, "default",
            "new workspace must not collide with boot default"
        );
        // id 唯一：再建一个同前缀目录 → 不同 id。
        let dir2 = temp_dir("new2");
        let out2 = reg.create(dir2.to_str().unwrap()).unwrap();
        assert!(out2.created);
        assert_ne!(out2.id, out.id);
        // prepend：新工作区排在最前。
        let order = reg.order();
        assert_eq!(order[0], out2.id);
        assert_eq!(order[1], out.id);
        assert_eq!(order[2], "default");
        // title = basename。
        let record = reg.get(&out.id).unwrap();
        assert_eq!(
            record.title,
            dir.file_name().unwrap().to_string_lossy(),
            "title must be the path basename"
        );
        assert!(record.session_ids.is_empty());
        // view 字段对齐 schema。
        let view = workspace_view(&record);
        assert_eq!(view["workspaceId"], out.id);
        assert_eq!(view["path"], record.path);
        assert_eq!(view["title"], record.title);
        assert_eq!(view["sessionIds"], serde_json::json!([]));
        assert!(view["createdAt"].is_string());
        assert!(view["updatedAt"].is_string());
    }

    #[test]
    fn create_same_path_is_idempotent_keeps_title() {
        let mut reg = WorkspaceRegistry::new();
        let dir = temp_dir("idem");
        let first = reg.create(dir.to_str().unwrap()).unwrap();
        assert!(first.created);
        let second = reg.create(dir.to_str().unwrap()).unwrap();
        assert!(
            !second.created,
            "same canonical path must resolve created:false"
        );
        assert_eq!(
            second.id, first.id,
            "idempotent hit must return the same id"
        );
        assert_eq!(reg.list().len(), 2, "no duplicate registration");
        // 幂等命中不改 title。
        let _ = reg.rename(&first.id, "renamed");
        let again = reg.create(dir.to_str().unwrap()).unwrap();
        assert!(!again.created);
        assert_eq!(reg.get(&first.id).unwrap().title, "renamed");
    }

    #[test]
    fn create_missing_and_non_directory_err() {
        let mut reg = WorkspaceRegistry::new();
        let missing = format!("Z:\\dsh-no-such-{}", now_ms());
        assert!(reg.create(&missing).is_err());
        let file = temp_dir("file").join("a.txt");
        std::fs::write(&file, "x").unwrap();
        assert!(reg.create(file.to_str().unwrap()).is_err());
    }

    #[test]
    fn rename_bumps_title_and_updated_at() {
        let mut reg = WorkspaceRegistry::new();
        let dir = temp_dir("rename");
        let out = reg.create(dir.to_str().unwrap()).unwrap();
        let before = reg.get(&out.id).unwrap().updated_at;
        let renamed = reg.rename(&out.id, "  My Project  ").unwrap();
        assert_eq!(renamed.title, "My Project", "title must be trimmed");
        assert!(renamed.updated_at >= before);
        // 空 title 拒绝。
        assert!(reg.rename(&out.id, "   ").is_none());
        assert_eq!(reg.get(&out.id).unwrap().title, "My Project");
        // 未知 id → None。
        assert!(reg.rename("nope", "x").is_none());
    }

    #[test]
    fn delete_removes_registration() {
        let mut reg = WorkspaceRegistry::new();
        let dir = temp_dir("del");
        let out = reg.create(dir.to_str().unwrap()).unwrap();
        assert!(reg.delete(&out.id));
        assert!(reg.get(&out.id).is_none());
        assert_eq!(reg.list().len(), 1);
        assert!(!reg.delete(&out.id), "unknown id is a no-op");
    }

    #[test]
    fn insert_before_reorders_and_validates() {
        let mut reg = WorkspaceRegistry::new();
        let a = reg.create(temp_dir("a").to_str().unwrap()).unwrap();
        let b = reg.create(temp_dir("b").to_str().unwrap()).unwrap();
        // order: [b, a, default] → 把 a 移到 b 前 → [a, b, default]。
        let order = reg.insert_before(&a.id, Some(&b.id)).unwrap();
        assert_eq!(
            order,
            vec![a.id.clone(), b.id.clone(), "default".to_string()]
        );
        // 无锚点 append → [b...] 到末尾：a 已在前 → [b, default, a]？不，a 开头。
        let order2 = reg.insert_before(&a.id, None).unwrap();
        assert_eq!(order2[order2.len() - 1], a.id);
        // 未知 id/锚点 → Err。
        assert!(reg.insert_before("nope", None).is_err());
        assert!(reg.insert_before(&a.id, Some("nope")).is_err());
        // 锚点 == 自身 → no-op 成功。
        let order3 = reg.insert_before(&a.id, Some(&a.id)).unwrap();
        assert_eq!(order3, order2);
    }

    #[test]
    fn attach_session_appends_without_dup() {
        let mut reg = WorkspaceRegistry::new();
        let dir = temp_dir("attach");
        let out = reg.create(dir.to_str().unwrap()).unwrap();
        let rec = reg.attach_session(&out.id, "s-new").unwrap();
        assert_eq!(rec.session_ids, vec!["s-new".to_string()]);
        let rec2 = reg.attach_session(&out.id, "s-new").unwrap();
        assert_eq!(
            rec2.session_ids,
            vec!["s-new".to_string()],
            "no duplicate attach"
        );
        let rec3 = reg.attach_session(&out.id, "s-other").unwrap();
        assert_eq!(
            rec3.session_ids,
            vec!["s-new".to_string(), "s-other".to_string()]
        );
        // 未知工作区 → None。
        assert!(reg.attach_session("nope", "s-x").is_none());
    }

    #[test]
    fn insert_session_before_moves_within_workspace() {
        let mut reg = WorkspaceRegistry::new();
        let dir = temp_dir("isb");
        let out = reg.create(dir.to_str().unwrap()).unwrap();
        reg.attach_session(&out.id, "s-a");
        reg.attach_session(&out.id, "s-b");
        reg.attach_session(&out.id, "s-c");
        // [s-a, s-b, s-c] → c 前移 b 前 → [s-a, s-c, s-b]。
        let rec = reg
            .insert_session_before(&out.id, "s-c", Some("s-b"))
            .unwrap();
        assert_eq!(rec.session_ids, vec!["s-a", "s-c", "s-b"]);
        // 未知锚点/会话/工作区 → None。
        assert!(reg
            .insert_session_before(&out.id, "s-a", Some("nope"))
            .is_none());
        assert!(reg.insert_session_before(&out.id, "nope", None).is_none());
        assert!(reg.insert_session_before("nope", "s-a", None).is_none());
    }

    #[test]
    fn archive_session_collects_uniquely() {
        let mut reg = WorkspaceRegistry::new();
        let a = reg.archive_session("s-1");
        assert_eq!(a, vec!["s-1"]);
        let b = reg.archive_session("s-1");
        assert_eq!(b, vec!["s-1"], "already archived is idempotent");
        let c = reg.archive_session("s-2");
        assert_eq!(c, vec!["s-1", "s-2"]);
        assert_eq!(reg.archived_session_ids(), vec!["s-1", "s-2"]);
    }
}

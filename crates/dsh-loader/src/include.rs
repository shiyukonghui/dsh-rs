//! Include 文件加载器（对应 PLAN §1.9，M3 子集）。
//!
//! 从 YAML/JSON 文件读取入口列表，应用 patch，装载到 Loader 根组；
//! 支持写回与手动 `refresh()`（文件热更；M3 不做文件 watcher）。
//!
//! 已知 M3 差异：`!!js` YAML 标签不支持，用 `{"__jsExpr": "..."}` 对象代替；
//! patch 仅作用于根层（Cordis 支持向 group 内 insert，M4 补齐）。

use std::path::{Path, PathBuf};

use dsh_core::{CordisError, Value};

use crate::entry::EntryOptions;
use crate::loader::Loader;

/// 运行时 patch（Cordis `PatchOptions` 的 M3 子集）。
#[derive(Debug, Clone, Default)]
pub struct Patch {
    pub id: Option<String>,
    pub insert: Option<Vec<EntryOptions>>,
    pub name: Option<String>,
    pub config: Option<Value>,
    pub disabled: Option<bool>,
    pub group: Option<bool>,
}

/// 应用 patch 列表到入口列表（Cordis `applyEntryPatches` 的 M3 子集）。
/// 输入不变，返回脱离副本；insert 后立即重建索引，后续 patch 可命中新行。
pub fn apply_entry_patches(data: &[EntryOptions], patches: &[Patch]) -> Vec<EntryOptions> {
    let mut data: Vec<EntryOptions> = data.to_vec();
    for patch in patches {
        if let Some(insert) = &patch.insert {
            data.extend(insert.clone());
            continue;
        }
        let Some(id) = &patch.id else { continue };
        let idx = data.iter().position(|e| &e.id == id);
        let Some(idx) = idx else { continue };
        if let Some(name) = &patch.name {
            if data[idx].name != *name {
                continue; // name mismatch：跳过
            }
        }
        if let Some(c) = &patch.config {
            data[idx].config = c.clone();
        }
        if let Some(d) = &patch.disabled {
            data[idx].disabled = *d;
        }
        if let Some(g) = &patch.group {
            data[idx].group = *g;
        }
    }
    data
}

/// Include 文件加载器。
pub struct Include {
    pub loader: Loader,
    pub path: PathBuf,
    pub patches: Vec<Patch>,
    pub initial: Option<Vec<EntryOptions>>,
}

impl Include {
    pub fn new(loader: &Loader, path: impl AsRef<Path>, patches: Vec<Patch>) -> Self {
        Include {
            loader: loader.clone(),
            path: path.as_ref().to_path_buf(),
            patches,
            initial: None,
        }
    }

    /// 读取并装载文件内容。
    pub fn load(&self) -> Result<(), CordisError> {
        let entries = self.read()?;
        self.loader.sync(&entries)
    }

    /// 手动刷新（重读文件 → 应用 patch → 同步树）。
    pub fn refresh(&self) -> Result<(), CordisError> {
        self.load()
    }

    /// 把当前根组入口写回文件（JSON 或 YAML）。
    pub fn write_back(&self) -> Result<(), CordisError> {
        let entries = self.current_entries();
        let text = match self.path.extension().and_then(|e| e.to_str()) {
            Some("json") => serde_json::to_string_pretty(&entries)
                .map_err(|e| CordisError::Internal(format!("include write: {e}")))?,
            _ => serde_yaml::to_string(&entries)
                .map_err(|e| CordisError::Internal(format!("include write: {e}")))?,
        };
        std::fs::write(&self.path, text)
            .map_err(|e| CordisError::Internal(format!("include write {}: {e}", self.path.display())))
    }

    fn current_entries(&self) -> Vec<EntryOptions> {
        let root = {
            let st = self.loader.state.borrow();
            st.root_group.clone()
        };
        let ids: Vec<String> = {
            let st = self.loader.state.borrow();
            st.groups.get(&root).map(|g| g.data.clone()).unwrap_or_default()
        };
        let st = self.loader.state.borrow();
        ids.iter()
            .filter_map(|id| st.entries.get(id).map(|e| e.options.clone()))
            .collect()
    }

    fn read(&self) -> Result<Vec<EntryOptions>, CordisError> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // 首次：写入 initial 再读
                if let Some(initial) = &self.initial {
                    let s = serde_yaml::to_string(initial)
                        .map_err(|e| CordisError::Internal(format!("include initial: {e}")))?;
                    std::fs::write(&self.path, s).map_err(|e| {
                        CordisError::Internal(format!("include write {}: {e}", self.path.display()))
                    })?;
                    std::fs::read_to_string(&self.path).map_err(|e| {
                        CordisError::Internal(format!("include read {}: {e}", self.path.display()))
                    })?
                } else {
                    return Err(CordisError::Internal(format!(
                        "include read {}: {e}",
                        self.path.display()
                    )));
                }
            }
            Err(e) => {
                return Err(CordisError::Internal(format!(
                    "include read {}: {e}",
                    self.path.display()
                )))
            }
        };
        let value: Value = match self.path.extension().and_then(|e| e.to_str()) {
            Some("json") => serde_json::from_str(&text)
                .map_err(|e| CordisError::Internal(format!("include parse json: {e}")))?,
            _ => serde_yaml::from_str(&text)
                .map_err(|e| CordisError::Internal(format!("include parse yaml: {e}")))?,
        };
        let raw: Vec<EntryOptions> = match value {
            Value::Array(items) => items
                .iter()
                .map(|v| serde_json::from_value(v.clone()))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| CordisError::Internal(format!("include entries invalid: {e}")))?,
            _ => return Err(CordisError::Internal("include file must be a top-level array".into())),
        };
        Ok(apply_entry_patches(&raw, &self.patches))
    }
}

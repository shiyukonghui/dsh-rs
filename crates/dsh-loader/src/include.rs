//! Include 文件加载器（对应 PLAN §1.9，M3 子集）。
//!
//! 从 YAML/JSON 文件读取入口列表，应用 patch，装载到 Loader 根组；
//! 支持写回与手动 `refresh()`（文件热更；M3 不做文件 watcher）。
//!
//! 已知 M3 差异：`!!js` YAML 标签不支持，用 `{"__jsExpr": "..."}` 对象代替；
//! patch 仅作用于根层（Cordis 支持向 group 内 insert，M4 补齐）。

use std::path::{Path, PathBuf};

use dsh_core::{AggregateError, CordisError, Value};

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

/// 应用 patch 列表到入口列表（对齐 Cordis `applyEntryPatches`）。
/// 输入不变，返回脱离副本；insert 后立即重建索引，后续 patch 可命中新行。
/// - `insert` 带 `id` → 向该 id 的 **group** config 数组插入（对齐 TS：目标
///   必须是 group，否则跳过）；无 id → 顶层追加。
/// - `id` patch 命中**嵌套**入口（含 group 子入口；对齐 TS entryMap 含子入口）。
///
/// M39：静默版（无 warn sink，跳过不诊断）——委托 [`apply_entry_patches_with_warn`]。
pub fn apply_entry_patches(data: &[EntryOptions], patches: &[Patch]) -> Vec<EntryOptions> {
    apply_entry_patches_with_warn(data, patches, &mut |_| {})
}

/// M39：带 warn sink 的 patch 应用（对齐 Cordis `applyEntryPatches(data,
/// patches, warn)`）——patch 未命中（id 找不到/非 group/name mismatch/缺 id）
/// 时调用 `warn(message)`（Cordis printf 风格 `%C` 在此为格式化好的字符串）；
/// 否则静默跳过。warn 是**诊断 sink**（logger/收集器），不影响结果。
pub fn apply_entry_patches_with_warn(
    data: &[EntryOptions],
    patches: &[Patch],
    warn: &mut dyn FnMut(String),
) -> Vec<EntryOptions> {
    let mut data: Vec<EntryOptions> = data.to_vec();
    for patch in patches {
        if let Some(insert) = &patch.insert {
            match &patch.id {
                // 向 group 的 config 数组插入（目标必须存在且是 group）
                Some(id) => {
                    let (out, warned) = patch_insert_into_group(data, id, insert);
                    if let Some(w) = warned {
                        warn(w);
                    }
                    data = out;
                }
                None => data.extend(insert.clone()),
            }
            continue;
        }
        let Some(id) = &patch.id else {
            warn("patch: id is required for non-insert patches".into());
            continue;
        };
        // 先找目标（含嵌套），name mismatch 检查需要目标 name
        let target_name = find_entry_name(&data, id);
        match target_name {
            None => {
                warn(format!("patch: entry {id} not found"));
                continue;
            }
            Some(name_of_target) => {
                if let Some(name) = &patch.name {
                    if name_of_target != *name {
                        warn(format!(
                            "patch: name mismatch for {id} (expected {}, got {}), skipping",
                            name_of_target, name
                        ));
                        continue;
                    }
                }
            }
        }
        data = patch_update(data, id, &|target| {
            if let Some(c) = &patch.config {
                target.config = c.clone();
            }
            if let Some(d) = &patch.disabled {
                target.disabled = *d;
            }
            if let Some(g) = &patch.group {
                target.group = *g;
            }
        });
    }
    data
}

/// 在入口列表（含嵌套 group 子入口）中查找 id；返回其 name（命中时）。
fn find_entry_name(data: &[EntryOptions], id: &str) -> Option<String> {
    for e in data {
        if e.id == id {
            return Some(e.name.clone());
        }
        if e.group {
            if let Value::Array(items) = &e.config {
                let children: Vec<EntryOptions> = items
                    .iter()
                    .filter_map(|v| serde_json::from_value(v.clone()).ok())
                    .collect();
                if let Some(name) = find_entry_name(&children, id) {
                    return Some(name);
                }
            }
        }
    }
    None
}

/// 向 id（必须是 group）的 config 数组插入子入口；未命中/非 group → 原样。
/// 返回 (结果, 可选的 warn 消息)——对齐 TS：id 不存在 → warn "entry not found"；
/// 目标非 group → warn "entry is not a group"；命中 → None。
fn patch_insert_into_group(
    data: Vec<EntryOptions>,
    id: &str,
    insert: &[EntryOptions],
) -> (Vec<EntryOptions>, Option<String>) {
    let mut warned: Option<String> = None;
    let mut found = false;
    let out: Vec<EntryOptions> = data
        .into_iter()
        .map(|mut e| {
            if e.id == id {
                found = true;
                if e.group {
                    let mut items = match e.config {
                        Value::Array(items) => items,
                        _ => Vec::new(),
                    };
                    for child in insert {
                        items.push(serde_json::to_value(child.clone()).unwrap_or(Value::Null));
                    }
                    e.config = Value::Array(items);
                } else {
                    warned = Some(format!("patch insert: entry {id} is not a group"));
                }
                return e;
            }
            // 递归 group 子入口
            if e.group {
                if let Value::Array(items) = &e.config {
                    let children: Vec<EntryOptions> = items
                        .iter()
                        .filter_map(|v| serde_json::from_value(v.clone()).ok())
                        .collect();
                    let (patched, inner_warn) = patch_insert_into_group(children, id, insert);
                    if warned.is_none() {
                        warned = inner_warn;
                    }
                    let encoded: Vec<Value> = patched
                        .iter()
                        .map(|c| serde_json::to_value(c.clone()).unwrap_or(Value::Null))
                        .collect();
                    e.config = Value::Array(encoded);
                }
            }
            e
        })
        .collect();
    if !found && warned.is_none() {
        warned = Some(format!("patch insert: entry {id} not found"));
    }
    (out, warned)
}

/// 按 id 更新入口（含嵌套 group 子入口）；`update` 闭包修改目标。
fn patch_update(
    data: Vec<EntryOptions>,
    id: &str,
    update: &dyn Fn(&mut EntryOptions),
) -> Vec<EntryOptions> {
    data.into_iter()
        .map(|mut e| {
            if e.id == id {
                update(&mut e);
                return e;
            }
            // 递归 group 子入口
            if e.group {
                if let Value::Array(items) = &e.config {
                    let children: Vec<EntryOptions> = items
                        .iter()
                        .filter_map(|v| serde_json::from_value(v.clone()).ok())
                        .collect();
                    let patched = patch_update(children, id, update);
                    let encoded: Vec<Value> = patched
                        .iter()
                        .map(|c| serde_json::to_value(c.clone()).unwrap_or(Value::Null))
                        .collect();
                    e.config = Value::Array(encoded);
                }
            }
            e
        })
        .collect()
}

/// Include 文件加载器。
#[derive(Clone)]
pub struct Include {
    pub loader: Loader,
    pub path: PathBuf,
    pub patches: Vec<Patch>,
    pub initial: Option<Vec<EntryOptions>>,
    /// M39：最近一次 read 的 patch 警告（对齐 Cordis `applyEntryPatches`
    /// 的 warn sink——logger 输出；此处收集供宿主查询/诊断）。
    warns: std::cell::RefCell<Vec<String>>,
}

impl Include {
    pub fn new(loader: &Loader, path: impl AsRef<Path>, patches: Vec<Patch>) -> Self {
        Include {
            loader: loader.clone(),
            path: path.as_ref().to_path_buf(),
            patches,
            initial: None,
            warns: std::cell::RefCell::new(Vec::new()),
        }
    }

    /// 取走最近一次 read 的 patch 警告（M39：对齐 Cordis warn sink 的诊断）。
    pub fn take_warns(&self) -> Vec<String> {
        std::mem::take(&mut *self.warns.borrow_mut())
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

    /// 读取并装载文件内容（async 事务：allSettled + 回滚，对应 Cordis
    /// `EntryGroup.update(config)`——Include 插件的 `internal/update` 路径）。
    pub async fn load_async(&self) -> Result<(), AggregateError> {
        let entries = self
            .read()
            .map_err(|e| AggregateError { errors: vec![e] })?;
        self.loader.sync_async(&entries).await
    }

    /// 手动刷新（async 事务版本）。
    pub async fn refresh_async(&self) -> Result<(), AggregateError> {
        self.load_async().await
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
        // M39：带 warn sink 应用 patch（未命中诊断收集到 warns；结果不变）
        let mut warns = Vec::new();
        let out = apply_entry_patches_with_warn(&raw, &self.patches, &mut |w| warns.push(w));
        *self.warns.borrow_mut() = warns;
        Ok(out)
    }
}

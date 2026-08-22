//! dsh-settings：用户配置能力缝（M3b，见 M3-REQUIREMENTS.md）。
//!
//! 权威参考：`@deepseek-ai/dsh-settings` + `settings-file`。
//! M3b 交付：namespace 注册、分层 resolve（defaults→base→user）、redactSecrets、
//! update/replace/mutate + revision conflict、YAML 文件持久化（原子写）。
//! 不引入 OS watch / 注释保真 leaf-diff（D-037 非目标）。

mod merge;
mod redact;

use crate::merge::apply_path_ops;
use crate::redact::walk_redact;
use dsh_schema::{resolve, ResolveOptions, SchemaRef};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

pub use crate::redact::SecretSlot;

/// 变更生效时机。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applies {
    Live,
    Restart,
}

/// settings 能力缝错误。
#[derive(Debug)]
pub enum SettingsError {
    /// revision 冲突（wire 映射为 SETTINGS_CONFLICT）。
    Conflict { ns: String, expected: u64, actual: u64 },
    /// 参数/JSON 形状/校验拒绝（wire 映射为 settings-rejected）。
    Invalid { message: String },
    /// namespace 未注册。
    NotRegistered(String),
}

/// describe 返回的 namespace 描述（wire `SettingsNamespaceView` 的领域侧数据）。
#[derive(Debug)]
pub struct NamespaceDescriptor {
    pub ns: String,
    pub schema: Value,
    pub value: Value,
    pub base: Option<Value>,
    pub user: Option<Value>,
    pub applies: Applies,
    pub secrets: Vec<SecretSlot>,
    pub revision: u64,
}

/// 配置提供者：注册 namespace + describe/update/replace/mutate + 文件持久化。
pub struct SettingsProvider {
    /// 注册表（保序）。
    registrations: Vec<Registration>,
    /// 每个 namespace 的用户 section（raw，持久化后刷新）。
    document: HashMap<String, Value>,
    /// 每个 namespace 的 revision。
    revision: HashMap<String, u64>,
    /// 可选本地文档路径（内存模式为 None）。
    document_path: Option<PathBuf>,
}

struct Registration {
    ns: String,
    schema: SchemaRef,
    base: Option<Value>,
    applies: Applies,
}

impl SettingsProvider {
    /// 纯内存提供者（测试/无文件环境）。
    pub fn memory() -> Self {
        SettingsProvider {
            registrations: Vec::new(),
            document: HashMap::new(),
            revision: HashMap::new(),
            document_path: None,
        }
    }

    /// 文件提供者（YAML `{ns: section}`，原子写）。
    pub fn file(path: PathBuf) -> Self {
        let doc = load_document(&path);
        let mut p = SettingsProvider::memory();
        p.document_path = Some(path);
        if let Some(doc) = doc {
            p.document = doc;
        }
        p
    }

    /// 注册一个 namespace。
    pub fn register(
        &mut self,
        ns: &str,
        schema: &SchemaRef,
        base: Option<Value>,
        applies: Applies,
    ) {
        if self.registrations.iter().any(|r| r.ns == ns) {
            return;
        }
        self.registrations.push(Registration {
            ns: ns.to_string(),
            schema: schema.clone(),
            base,
            applies,
        });
        // 不预先插入空 section：document 无该键 ≡ 未写（user 层省略，对齐 TS
        // `section()` undefined）。revision 从 0 开始。
        self.revision.entry(ns.to_string()).or_insert(0);
    }

    fn registration(&self, ns: &str) -> Result<&Registration, SettingsError> {
        self.registrations
            .iter()
            .find(|r| r.ns == ns)
            .ok_or_else(|| SettingsError::NotRegistered(ns.to_string()))
    }

    fn describe_inner(&mut self, ns: &str, reg: &Registration) -> Result<NamespaceDescriptor, SettingsError> {
        let user = self.document.get(ns).cloned();
        let value = resolve_layers(&reg.schema, reg.base.as_ref(), user.as_ref()).map_err(|e| {
            SettingsError::Invalid { message: e.to_string() }
        })?;
        // redact 三个层：value 恒 redact（wire 形态）；base/user 存在才 redact。
        let base = reg.base.clone();
        let redacted_value = walk_redact(&reg.schema, &value);
        let redacted_base = base.as_ref().map(|b| walk_redact(&reg.schema, b).0);
        let redacted_user = user.as_ref().map(|u| walk_redact(&reg.schema, u).0);
        let secrets = redacted_value.1;
        Ok(NamespaceDescriptor {
            ns: ns.to_string(),
            schema: reg.schema.to_json(),
            value: redacted_value.0,
            base: redacted_base,
            user: redacted_user,
            applies: reg.applies,
            secrets,
            revision: *self.revision.get(ns).unwrap_or(&0),
        })
    }

    pub fn describe(&mut self, ns: &str) -> Result<NamespaceDescriptor, SettingsError> {
        let reg = self.registration(ns)?.clone_reg();
        self.describe_inner(ns, &reg)
    }

    /// 列出所有已注册 namespace 的描述（注册顺序）。
    pub fn describe_all(&mut self) -> Vec<NamespaceDescriptor> {
        let regs = self
            .registrations
            .iter()
            .map(|r| r.clone_reg())
            .collect::<Vec<_>>();
        regs.iter()
            .filter_map(|r| self.describe_inner(&r.ns, r).ok())
            .collect()
    }

    /// 是否绑定本地文档（file 模式）。
    pub fn has_document(&self) -> bool {
        self.document_path.is_some()
    }

    pub fn update(
        &mut self,
        ns: &str,
        patch: &Value,
        expected_revision: Option<u64>,
    ) -> Result<NamespaceDescriptor, SettingsError> {
        if patch.as_object().is_none() {
            return Err(SettingsError::Invalid {
                message: format!("settings update for \"{ns}\" must be a plain object"),
            });
        }
        let reg = self.registration(ns)?.clone_reg();
        self.check_revision(ns, expected_revision)?;
        let current = self.document.get(ns).cloned().unwrap_or_else(|| Value::Object(Default::default()));
        let next = merge_layers(&current, patch);
        self.commit_section(ns, &reg, next)
    }

    pub fn replace(
        &mut self,
        ns: &str,
        section: &Value,
        expected_revision: Option<u64>,
    ) -> Result<NamespaceDescriptor, SettingsError> {
        if section.as_object().is_none() {
            return Err(SettingsError::Invalid {
                message: format!("settings replace for \"{ns}\" must be a plain object"),
            });
        }
        let reg = self.registration(ns)?.clone_reg();
        self.check_revision(ns, expected_revision)?;
        self.commit_section(ns, &reg, section.clone())
    }

    pub fn mutate(
        &mut self,
        ns: &str,
        ops: &Value,
        expected_revision: Option<u64>,
    ) -> Result<NamespaceDescriptor, SettingsError> {
        let reg = self.registration(ns)?.clone_reg();
        self.check_revision(ns, expected_revision)?;
        let current = self.document.get(ns).cloned().unwrap_or_else(|| Value::Object(Default::default()));
        let next = apply_path_ops(&current, ops).map_err(|m| SettingsError::Invalid { message: m })?;
        self.commit_section(ns, &reg, next)
    }

    fn check_revision(&self, ns: &str, expected: Option<u64>) -> Result<(), SettingsError> {
        if let Some(expected) = expected {
            let actual = *self.revision.get(ns).unwrap_or(&0);
            if expected != actual {
                return Err(SettingsError::Conflict {
                    ns: ns.to_string(),
                    expected,
                    actual,
                });
            }
        }
        Ok(())
    }

    /// 校验 → 持久化 → bump revision → 返回新 describe。
    fn commit_section(
        &mut self,
        ns: &str,
        reg: &Registration,
        section: Value,
    ) -> Result<NamespaceDescriptor, SettingsError> {
        // 校验：schema resolve 必须成功（非 strict，允许额外键但键值类型必须对）。
        let _ = resolve_layers(&reg.schema, reg.base.as_ref(), Some(&section))
            .map_err(|e| SettingsError::Invalid { message: e.to_string() })?;
        self.persist_section(ns, &section)?;
        self.document.insert(ns.to_string(), section);
        let rev = self.revision.entry(ns.to_string()).or_insert(0);
        *rev += 1;
        self.describe_inner(ns, reg)
    }

    /// 持久化一个 namespace 的 section（file 模式写整个 `{ns: section}` YAML）。
    fn persist_section(&self, ns: &str, section: &Value) -> Result<(), SettingsError> {
        let Some(path) = &self.document_path else {
            return Ok(());
        };
        let mut root: serde_json::Map<String, Value> = serde_json::Map::new();
        for reg in &self.registrations {
            let current = if reg.ns == ns {
                section.clone()
            } else {
                self.document.get(&reg.ns).cloned().unwrap_or_else(|| Value::Object(Default::default()))
            };
            root.insert(reg.ns.clone(), current);
        }
        let yaml = serde_yaml::to_string(&Value::Object(root))
            .map_err(|e| SettingsError::Invalid { message: format!("yaml: {e}") })?;
        dsh_persistence::fs_atomic::atomic_write(path, yaml.as_bytes())
            .map_err(|e| SettingsError::Invalid { message: format!("persist: {e}") })
    }

    /// 当前内存 section（供测试/诊断）。
    pub fn raw_user(&self, ns: &str) -> Option<&Value> {
        self.document.get(ns)
    }
}

impl Registration {
    fn clone_reg(&self) -> Registration {
        Registration {
            ns: self.ns.clone(),
            schema: self.schema.clone(),
            base: self.base.clone(),
            applies: self.applies,
        }
    }
}

/// 分层 resolve：schema 校验/补默认值 + base + user 合并。
fn resolve_layers(
    schema: &SchemaRef,
    base: Option<&Value>,
    user: Option<&Value>,
) -> Result<Value, dsh_schema::ValidationError> {
    let zero = Value::Object(Default::default());
    let mut merged = merge_layers(&zero, base.unwrap_or(&zero));
    merged = merge_layers(&merged, user.unwrap_or(&zero));
    resolve(&merged, schema, &ResolveOptions::default())
}

/// 深合并（对齐 TS mergeLayers）：plain object 递归，其它值整取代（数组含）。
pub fn merge_layers(under: &Value, over: &Value) -> Value {
    if over.is_null() || (!under.is_object() || !over.is_object()) {
        return over.clone();
    }
    let mut merged = under.clone();
    if let (Some(u), Some(o)) = (merged.as_object_mut(), over.as_object()) {
        for (k, v) in o {
            match u.get(k) {
                Some(existing) => {
                    let m = merge_layers(existing, v);
                    u.insert(k.clone(), m);
                }
                None => {
                    u.insert(k.clone(), v.clone());
                }
            }
        }
    }
    merged
}

/// 从 YAML 文档读回 `{ns: section}` 映射（文件缺失/损坏 → 空）。
fn load_document(path: &PathBuf) -> Option<HashMap<String, Value>> {
    let text = std::fs::read_to_string(path).ok()?;
    let root: Value = serde_yaml::from_str(&text).ok()?;
    let mut doc = HashMap::new();
    if let Some(obj) = root.as_object() {
        for (k, v) in obj {
            doc.insert(k.clone(), v.clone());
        }
    }
    Some(doc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merge_layers_recursive_and_replace() {
        // object 深合并 + 数组整取代（对齐 TS mergeLayers）。
        let (under, over) = (
            json!({"a": {"x": 1, "y": 2}, "list": [1, 2]}),
            json!({"a": {"y": 9}, "list": [3], "new": true}),
        );
        let merged = merge_layers(&under, &over);
        assert_eq!(merged["a"]["x"], 1, "unmentioned nested key preserved");
        assert_eq!(merged["a"]["y"], 9, "matches override");
        assert_eq!(merged["list"], json!([3]), "array replaced wholesale");
        assert_eq!(merged["new"], true);
    }
}

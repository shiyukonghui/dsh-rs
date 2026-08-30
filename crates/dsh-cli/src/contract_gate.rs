//! D-216 P2 协商关：mount-sync 实例化前的纯函数协商 + 结构化报告。
//!
//! 语义（设计稿 §3）：无声明的单元零扰动（照挂）；有声明 → 对 host catalog 协商，
//! 不兼容 → **不挂载**并把 issues 记入报告（经 RPC `contract/negotiationReport`
//! 与 inventory 行展示——不静默、不半挂）。报告每次现扫目录现算（无缓存=恒反映真身）。

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use dsh_contract::catalog::{Catalog, Definition, NegotiationReport, RequireEntry, SupportEntry, Severity};
use dsh_contract::declaration::declaration_from_value;
use serde_json::{json, Value};

/// host 提供的能力坐标种子表（与 dispatch/投影面人工对账；新增服务面时同步补行）。
/// 纪律：kind 大驼峰；apiVersion=标准文法正字；核心不推断版本兼容。
const HOST_SERVICES: &[(&str, &str)] = &[
    ("dsh.settings/v1", "Settings"),
    ("dsh.session-log/v1", "SessionLog"),
    ("dsh.llm/v1", "Llm"),
    ("dsh.schedule/v1", "Schedule"),
    ("dsh.jobs/v1", "Jobs"),
    ("dsh.approvals/v1", "Approvals"),
    ("dsh.workspace-files/v1", "WorkspaceFiles"),
    ("dsh.runtime-status/v1", "RuntimeStatus"),
    ("dsh.plugin-inventory/v1", "PluginInventory"),
    ("dsh.dynamic-plugins/v1", "DynamicPlugins"),
    ("dsh.loader/v1", "Loader"),
];

struct SeedDef {
    kind: &'static str,
    version: &'static str,
}
impl Definition for SeedDef {
    fn kind(&self) -> &str {
        self.kind
    }
    fn accepts(&self) -> Vec<String> {
        vec![self.version.to_string()]
    }
}

/// 宿主 catalog（每调用新建——纯构造，零状态）。
pub fn host_catalog() -> Catalog {
    let mut c = Catalog::new("dsh-host", env!("CARGO_PKG_VERSION"));
    for (v, kind) in HOST_SERVICES {
        let def = SeedDef { kind, version: v };
        // 种子表内同 kind 重复=装配期 bug，panic 于测试期即炸（不接受静默）。
        c.register(std::rc::Rc::new(def)).expect("host catalog seed duplicate kind");
    }
    c
}

/// 宿主支持面（participant 恒 `dsh-host`）。
pub fn host_supports() -> Vec<SupportEntry> {
    HOST_SERVICES
        .iter()
        .map(|(v, k)| SupportEntry {
            participant: "dsh-host".to_string(),
            reference: dsh_contract::declaration::ApiReference { api_version: (*v).to_string(), kind: (*k).to_string() },
        })
        .collect()
}

/// 单单元协商：`plugin.json` **无 participant 且无 requires/supports** → `None`
/// （老单元零声明零扰=照挂）。有任一声明键 → 协商（声明坏了也出报告——诚实可见）。
pub fn negotiate_unit(name: &str, plugin_json: &Value) -> Option<NegotiationReport> {
    let declared = plugin_json.get("participant").is_some()
        || plugin_json.get("requires").is_some()
        || plugin_json.get("supports").is_some();
    if !declared {
        return None;
    }
    // 归一：participant 缺省=目录名。
    let mut normalized = plugin_json.clone();
    if normalized.get("participant").is_none() {
        normalized
            .as_object_mut()
            .map(|o| o.insert("participant".to_string(), json!(name)));
    }
    let dec = match declaration_from_value(&normalized) {
        Ok(d) => d,
        Err(msg) => {
            // 声明自身非法：直接产 error 报告（非法声明绝不静默挂载）。
            let mut c = Catalog::new("dsh-host", env!("CARGO_PKG_VERSION"));
            for (v, kind) in HOST_SERVICES {
                let _ = c.register(std::rc::Rc::new(SeedDef { kind, version: v }));
            }
            return Some(NegotiationReport {
                api_version: "dsh.negotiation-report/v1alpha1".into(),
                evaluator: format!("dsh-host {}", env!("CARGO_PKG_VERSION")),
                compatible: false,
                protocols: vec![],
                issues: vec![dsh_contract::catalog::Issue {
                    code: "declaration-invalid".into(),
                    severity: Severity::Error,
                    participant: Some(name.to_string()),
                    message: msg,
                }],
            });
        }
    };
    let requires: Vec<RequireEntry> = dec
        .requires
        .iter()
        .map(|r| RequireEntry { participant: dec.participant.clone(), reference: r.reference.clone(), optional: r.optional })
        .collect();
    let mut supports = host_supports();
    supports.extend(dec.supports.iter().map(|s| SupportEntry { participant: dec.participant.clone(), reference: s.reference.clone() }));
    Some(dsh_contract::catalog::negotiate(&host_catalog(), &requires, &supports))
}

/// 报告基准目录（serve 启动一次性写入；测试可覆盖——报告面唯一路径来源）。
static REPORT_BASE: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();

pub fn set_report_base(dir: &Path) {
    *REPORT_BASE.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some(dir.to_path_buf());
}

fn report_base() -> Option<PathBuf> {
    REPORT_BASE.get_or_init(|| Mutex::new(None)).lock().unwrap().clone()
}

/// 目录级报告（值形）：扫 `<base>/*/plugin.json`，逐单元一行（未声明=declared:false，
/// 透明呈现）。`value.compatible` = 全部已声明单元兼容。
pub fn report_value() -> Value {
    let Some(base) = report_base() else {
        return json!({"ok": true, "value": {
            "apiVersion": "dsh.negotiation-report/v1alpha1",
            "evaluator": format!("dsh-host {}", env!("CARGO_PKG_VERSION")),
            "compatible": true, "units": []
        }});
    };
    report_value_in(&base)
}

/// 纯目录版（测试与 report_value 共用）。
pub fn report_value_in(base: &Path) -> Value {
    let mut units: Vec<Value> = Vec::new();
    let mut rd = match std::fs::read_dir(base) {
        Ok(rd) => rd,
        Err(_) => return json!({"ok": true, "value": {"apiVersion": "dsh.negotiation-report/v1alpha1", "compatible": true, "units": []}}),
    };
    let mut names: Vec<String> = rd
        .by_ref()
        .flatten()
        .filter(|de| de.path().is_dir())
        .map(|de| de.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    for name in names {
        let mj = base.join(&name).join("plugin.json");
        let Ok(text) = std::fs::read_to_string(&mj) else { continue };
        let Ok(j) = serde_json::from_str::<Value>(&text) else { continue };
        if j.get("world").and_then(|w| w.as_str()) != Some("remote") {
            continue;
        }
        match negotiate_unit(&name, &j) {
            None => units.push(json!({"unit": name, "declared": false})),
            Some(rep) => units.push(json!({
                "unit": name,
                "declared": true,
                "compatible": rep.compatible,
                "issues": rep.issues.iter().map(|i| json!({
                    "code": i.code, "severity": format!("{:?}", i.severity).to_lowercase(),
                    "message": i.message
                })).collect::<Vec<_>>(),
            })),
        }
    }
    let compatible = units.iter().all(|u| u["compatible"].as_bool().unwrap_or(true));
    json!({
        "ok": true,
        "value": {
            "apiVersion": "dsh.negotiation-report/v1alpha1",
            "evaluator": format!("dsh-host {}", env!("CARGO_PKG_VERSION")),
            "compatible": compatible,
            "units": units,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undeclared_unit_is_untouched() {
        assert!(negotiate_unit("panel-x", &json!({"world": "remote"})).is_none(), "零声明=零扰动");
    }

    #[test]
    fn satisfied_requires_mounts() {
        let rep = negotiate_unit(
            "plan-unit",
            &json!({"world": "remote", "participant": "plan-unit",
                    "requires": [{"apiVersion": "dsh.session-log/v1", "kind": "SessionLog"}]}),
        )
        .expect("declared");
        assert!(rep.compatible, "{:?}", rep.issues);
    }

    #[test]
    fn unknown_kind_blocks_with_reason() {
        let rep = negotiate_unit(
            "ghost-unit",
            &json!({"world": "remote", "requires": [{"apiVersion": "dsh.ghost/v1", "kind": "Ghost"}]}),
        )
        .expect("declared");
        assert!(!rep.compatible);
        assert_eq!(rep.issues[0].code, "requirement-unsupported");
    }

    #[test]
    fn invalid_declaration_is_visible_error() {
        let rep = negotiate_unit(
            "bad-unit",
            &json!({"world": "remote", "requires": [{"apiVersion": "dsh/ghost/v1", "kind": "Ghost"}]}),
        )
        .expect("declared");
        assert!(!rep.compatible);
        assert_eq!(rep.issues[0].code, "declaration-invalid");
    }

    #[test]
    fn optional_gap_is_compatible_with_warning() {
        let rep = negotiate_unit(
            "soft-unit",
            &json!({"world": "remote", "requires": [{"apiVersion": "dsh.nope/v1", "kind": "Nope", "optional": true}]}),
        )
        .expect("declared");
        assert!(rep.compatible, "optional 未满足不阻断");
        assert_eq!(rep.issues[0].severity, Severity::Warning);
    }

    #[test]
    fn report_scans_directory_transparently() {
        let tmp = std::env::temp_dir().join(format!("dsh-gate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("plain-unit")).unwrap();
        std::fs::write(tmp.join("plain-unit/plugin.json"), r#"{"world":"remote"}"#).unwrap();
        std::fs::create_dir_all(tmp.join("bad-unit")).unwrap();
        std::fs::write(
            tmp.join("bad-unit/plugin.json"),
            r#"{"world":"remote","requires":[{"apiVersion":"dsh.ghost/v1","kind":"Ghost"}]}"#,
        )
        .unwrap();
        std::fs::create_dir_all(tmp.join("not-remote")).unwrap();
        std::fs::write(tmp.join("not-remote/plugin.json"), r#"{"world":"loop"}"#).unwrap();
        let v = report_value_in(&tmp);
        let units = v["value"]["units"].as_array().unwrap();
        assert_eq!(units.len(), 2, "非 remote 不进报告");
        assert_eq!(v["value"]["compatible"], json!(false));
        let bad = units.iter().find(|u| u["unit"] == "bad-unit").unwrap();
        assert_eq!(bad["issues"][0]["code"], "requirement-unsupported");
        std::fs::remove_dir_all(&tmp).ok();
    }
}

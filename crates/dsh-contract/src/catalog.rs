//! 目录与协商：每协议坐标的**定义**登记 `accepts` 版本集（核心不推断兼容），
//! `negotiate` 纯函数吃声明、吐结构化报告（issue code 机读，UI/CI/日志共用）。

use std::collections::HashMap;
use std::rc::Rc;

use serde::Serialize;

use crate::declaration::{Declaration, ApiReference};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Issue {
    pub code: String,
    pub severity: Severity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub participant: Option<String>,
    pub message: String,
}

/// 参与者的支持面拍平行（宿主 catalog 侧或声明侧均可产出）。
#[derive(Debug, Clone)]
pub struct SupportEntry {
    pub participant: String,
    pub reference: ApiReference,
}

#[derive(Debug, Clone)]
pub struct RequireEntry {
    pub participant: String,
    pub reference: ApiReference,
    pub optional: bool,
}

/// 协议定义：拥有该 kind 的接受版本集（显式，不推断）。
pub trait Definition {
    fn kind(&self) -> &str;
    /// 该定义接受的 apiVersion 字符串全集。
    fn accepts(&self) -> Vec<String>;
}

pub struct Catalog {
    pub evaluator_name: String,
    pub evaluator_version: String,
    defs: HashMap<String, Rc<dyn Definition>>,
}

impl Catalog {
    pub fn new(evaluator_name: impl Into<String>, evaluator_version: impl Into<String>) -> Self {
        Self { evaluator_name: evaluator_name.into(), evaluator_version: evaluator_version.into(), defs: HashMap::new() }
    }
    /// 注册定义（同 kind 重复注册=装配错误，拒）。
    pub fn register(&mut self, def: Rc<dyn Definition>) -> Result<(), String> {
        let kind = def.kind().to_string();
        if self.defs.contains_key(&kind) {
            return Err(format!("protocol definition for kind {kind} already registered"));
        }
        self.defs.insert(kind, def);
        Ok(())
    }
    pub fn accepts(&self, kind: &str, api_version: &str) -> bool {
        self.defs.get(kind).is_some_and(|d| d.accepts().iter().any(|v| v == api_version))
    }
    pub fn has_definition(&self, kind: &str) -> bool {
        self.defs.contains_key(kind)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReportProtocol {
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    pub kind: String,
    pub participants: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<Issue>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NegotiationReport {
    /// 报告自身也活在文法下（文法干净版报告 id；见 version.rs 对 dsh-std
    /// README 三段示例的勘误注记）。
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    pub evaluator: String,
    pub compatible: bool,
    pub protocols: Vec<ReportProtocol>,
    pub issues: Vec<Issue>,
}

/// 协商：逐条 require 找支持面。判定纪律（对齐 dsh-std）——
/// ①kind 无任何定义 → `definition-missing`(error)；
/// ②kind 有支持但精确版本不在「支持集 ∪ 定义 accepts」→ `version-mismatch`(error/optional→warning)；
/// ③kind 全无支持 → `requirement-unsupported`(error/optional→warning)。
pub fn negotiate(catalog: &Catalog, requires: &[RequireEntry], supports: &[SupportEntry]) -> NegotiationReport {
    let mut issues: Vec<Issue> = Vec::new();
    let mut by_kind: HashMap<String, ReportProtocol> = HashMap::new();
    let mut touch = |kind: &str, participant: &str, issue: Option<Issue>, ok: bool| {
        let e = by_kind.entry(kind.to_string()).or_insert_with(|| ReportProtocol {
            api_version: String::new(),
            kind: kind.to_string(),
            participants: Vec::new(),
            issues: Vec::new(),
        });
        if ok && !e.participants.iter().any(|p| p == participant) {
            e.participants.push(participant.to_string());
        }
        if let Some(i) = issue {
            e.issues.push(i);
        }
    };
    for r in requires {
        let kind = &r.reference.kind;
        let sev = if r.optional { Severity::Warning } else { Severity::Error };
        let matches: Vec<&SupportEntry> = supports.iter().filter(|s| s.reference.kind == *kind).collect();
        if matches.is_empty() {
            touch(kind, &r.participant, Some(Issue {
                code: "requirement-unsupported".into(),
                severity: sev,
                participant: Some(r.participant.clone()),
                message: format!("no participant supports kind {kind}"),
            }), false);
            continue;
        }
        if !catalog.has_definition(kind) {
            touch(kind, &r.participant, Some(Issue {
                code: "definition-missing".into(),
                severity: Severity::Error,
                participant: Some(r.participant.clone()),
                message: format!("kind {kind} has supports but no registered definition"),
            }), false);
            continue;
        }
        let exact = matches.iter().any(|s| s.reference.api_version == r.reference.api_version);
        let accepted = catalog.accepts(kind, &r.reference.api_version);
        if exact || accepted {
            // 参与者=双方（需求方 + 供得起的支持方，排序保确定性）。
            touch(kind, &r.participant, None, true);
            for s in &matches {
                if s.reference.api_version == r.reference.api_version || catalog.accepts(kind, &s.reference.api_version) {
                    touch(kind, &s.participant, None, true);
                }
            }
        } else {
            touch(kind, &r.participant, Some(Issue {
                code: "version-mismatch".into(),
                severity: sev,
                participant: Some(r.participant.clone()),
                message: format!("required {} not satisfied by kind {kind} (accepts-exact or catalog accepts)", r.reference.api_version),
            }), false);
        }
    }
    let mut protocols: Vec<ReportProtocol> = by_kind.into_values().collect();
    protocols.sort_by(|a, b| a.kind.cmp(&b.kind));
    for p in protocols.iter_mut() {
        p.participants.sort();
    }
    issues.extend(protocols.iter().flat_map(|p| p.issues.iter().cloned()));
    let compatible = !issues.iter().any(|i| i.severity == Severity::Error);
    NegotiationReport {
        api_version: "dsh.negotiation-report/v1alpha1".into(),
        evaluator: format!("{} {}", catalog.evaluator_name, catalog.evaluator_version),
        compatible,
        protocols,
        issues,
    }
}

/// 声明拍平成协商输入（便捷组合子）。
pub fn declarations_to_inputs(declarations: &[Declaration]) -> (Vec<RequireEntry>, Vec<SupportEntry>) {
    let mut requires = Vec::new();
    let mut supports = Vec::new();
    for d in declarations {
        for r in &d.requires {
            requires.push(RequireEntry { participant: d.participant.clone(), reference: r.reference.clone(), optional: r.optional });
        }
        for s in &d.supports {
            supports.push(SupportEntry { participant: d.participant.clone(), reference: s.reference.clone() });
        }
    }
    (requires, supports)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::declaration::ApiReference;

    struct Def {
        kind: &'static str,
        accepts: Vec<&'static str>,
    }
    impl Definition for Def {
        fn kind(&self) -> &str {
            self.kind
        }
        fn accepts(&self) -> Vec<String> {
            self.accepts.iter().map(|s| s.to_string()).collect()
        }
    }

    fn ref_of(v: &str, k: &str) -> ApiReference {
        ApiReference { api_version: v.into(), kind: k.into() }
    }
    fn catalog() -> Catalog {
        let mut c = Catalog::new("dsh-host", "0.1");
        c.register(Rc::new(Def { kind: "SessionLog", accepts: vec!["dsh.session-log/v1"] })).unwrap();
        assert!(c.register(Rc::new(Def { kind: "SessionLog", accepts: vec![] })).is_err(), "重复注册拒绝");
        c
    }

    #[test]
    fn satisfied_when_supports_exact_version() {
        let c = catalog();
        let req = vec![RequireEntry { participant: "plan".into(), reference: ref_of("dsh.session-log/v1", "SessionLog"), optional: false }];
        let sup = vec![SupportEntry { participant: "host".into(), reference: ref_of("dsh.session-log/v1", "SessionLog") }];
        let r = negotiate(&c, &req, &sup);
        assert!(r.compatible, "{r:?}");
        assert!(r.issues.is_empty());
        assert_eq!(r.protocols[0].participants, vec!["host".to_string(), "plan".to_string()], "协议参与者=供需双方");
    }

    #[test]
    fn missing_support_is_error_or_warning() {
        let c = catalog();
        let req = |opt: bool| vec![RequireEntry { participant: "plan".into(), reference: ref_of("dsh.session-log/v1", "SessionLog"), optional: opt }];
        let hard = negotiate(&c, &req(false), &[]);
        assert!(!hard.compatible);
        assert_eq!(hard.issues[0].code, "requirement-unsupported");
        assert_eq!(hard.issues[0].severity, Severity::Error);
        let soft = negotiate(&c, &req(true), &[]);
        assert!(soft.compatible, "optional 未满足不阻断");
        assert_eq!(soft.issues[0].severity, Severity::Warning);
    }

    #[test]
    fn version_mismatch_only_when_neither_exact_nor_accepts() {
        let c = catalog();
        let req = vec![RequireEntry { participant: "plan".into(), reference: ref_of("dsh.session-log/v2", "SessionLog"), optional: false }];
        let sup = vec![SupportEntry { participant: "host".into(), reference: ref_of("dsh.session-log/v1", "SessionLog") }];
        let r = negotiate(&c, &req, &sup);
        assert!(!r.compatible, "v2 不在精确集也不在 accepts → 不兼容（核心不推断）");
        assert_eq!(r.issues[0].code, "version-mismatch");
    }

    #[test]
    fn definition_missing_blocked_when_support_without_def() {
        let c = Catalog::new("h", "1");
        let req = vec![RequireEntry { participant: "u".into(), reference: ref_of("dsh.ghost/v1", "Ghost"), optional: false }];
        let sup = vec![SupportEntry { participant: "x".into(), reference: ref_of("dsh.ghost/v1", "Ghost") }];
        let r = negotiate(&c, &req, &sup);
        assert!(!r.compatible);
        assert_eq!(r.issues[0].code, "definition-missing");
    }

    #[test]
    fn report_serializes_stable_shape() {
        let c = catalog();
        let (req, sup) = declarations_to_inputs(&[]);
        let r = negotiate(&c, &req, &sup);
        let j = serde_json::to_value(&r).unwrap();
        assert_eq!(j["apiVersion"], "dsh.negotiation-report/v1alpha1");
        assert_eq!(j["compatible"], true);
    }
}

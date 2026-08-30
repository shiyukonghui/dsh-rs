//! 协议声明：participant + requires[] + supports[]（dsh-std `ProtocolDeclaration`
//! 同款形态，从 plugin.json 的 JSON 视图解析——声明是视图，消费面是真身）。

use serde::Serialize;
use serde_json::Value;

use crate::version::{parse_api_version, validate_kind};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ApiReference {
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    pub kind: String,
}

impl ApiReference {
    /// 协议坐标（同 dsh-std protocolKey：同 apiVersion+kind 才算同协议）。
    pub fn key(&self) -> String {
        format!("{}\0{}", self.api_version, self.kind)
    }
    pub fn validate(&self) -> Result<(), String> {
        parse_api_version(&self.api_version)?;
        validate_kind(&self.kind)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Requirement {
    #[serde(flatten)]
    pub reference: ApiReference,
    #[serde(default)]
    pub optional: bool,
    #[serde(default)]
    pub spec: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Support {
    #[serde(flatten)]
    pub reference: ApiReference,
    #[serde(default)]
    pub spec: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Declaration {
    pub participant: String,
    #[serde(default)]
    pub requires: Vec<Requirement>,
    #[serde(default)]
    pub supports: Vec<Support>,
}

fn ref_from(v: &Value, label: &str, i: usize) -> Result<ApiReference, String> {
    let obj = v.as_object().ok_or_else(|| format!("{label}[{i}] must be an object"))?;
    let r = ApiReference {
        api_version: obj
            .get("apiVersion")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("{label}[{i}].apiVersion must be a string"))?
            .to_string(),
        kind: obj
            .get("kind")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("{label}[{i}].kind must be a string"))?
            .to_string(),
    };
    r.validate()?;
    Ok(r)
}

/// 从 plugin.json 的 JSON 视图解析声明（形状/文法/去重三重校验；错误信息带路径）。
/// 无 requires/supports 键 → 空集（老单元零声明零扰——P2 挂载序语义）。
pub fn declaration_from_value(v: &Value) -> Result<Declaration, String> {
    let participant = v
        .get("participant")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "declaration.participant must be a non-empty string".to_string())?;
    let mut requires = Vec::new();
    if let Some(arr) = v.get("requires") {
        let arr = arr.as_array().ok_or_else(|| "declaration.requires must be an array".to_string())?;
        for (i, row) in arr.iter().enumerate() {
            let reference = ref_from(row, "declaration.requires", i)?;
            let optional = match row.get("optional") {
                Some(v) => v.as_bool().ok_or_else(|| format!("declaration.requires[{i}].optional must be boolean"))?,
                None => false,
            };
            requires.push(Requirement { reference, optional, spec: row.get("spec").cloned() });
        }
    }
    let mut supports = Vec::new();
    if let Some(arr) = v.get("supports") {
        let arr = arr.as_array().ok_or_else(|| "declaration.supports must be an array".to_string())?;
        for (i, row) in arr.iter().enumerate() {
            let reference = ref_from(row, "declaration.supports", i)?;
            supports.push(Support { reference, spec: row.get("spec").cloned() });
        }
    }
    // 去重（同 key 两行无意义且协商结果不定——dsh-std validateRows 同款守卫）。
    let req_keys: Vec<String> = requires.iter().map(|r| r.reference.key()).collect();
    let sup_keys: Vec<String> = supports.iter().map(|s| s.reference.key()).collect();
    for (label, keys) in [("declaration.requires", req_keys), ("declaration.supports", sup_keys)] {
        let mut seen: std::collections::HashSet<String> = Default::default();
        for k in keys {
            if !seen.insert(k.clone()) {
                return Err(format!("{label} contains duplicate protocol row"));
            }
        }
    }
    Ok(Declaration { participant: participant.to_string(), requires, supports })
}

/// 顶层校验入口（宿主挂载解析 plugin.json 用）。
pub fn validate_declaration_value(v: &Value) -> Result<Declaration, String> {
    declaration_from_value(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_shape_with_requires_supports() {
        let d = declaration_from_value(&json!({
            "participant": "plan-unit",
            "requires": [
                {"apiVersion": "dsh.session-log/v1", "kind": "SessionLog"},
                {"apiVersion": "dsh.settings/v1", "kind": "Settings", "optional": true, "spec": {"mode": "ro"}}
            ],
            "supports": [{"apiVersion": "dsh.plan/v1", "kind": "Plan"}]
        }))
        .unwrap();
        assert_eq!(d.requires.len(), 2);
        assert!(d.requires[1].optional);
        assert_eq!(d.requires[1].spec.as_ref().unwrap()["mode"], "ro");
        assert_eq!(d.supports[0].reference.kind, "Plan");
    }

    #[test]
    fn empty_declarations_are_legal() {
        // 老单元零声明零扰。
        let d = declaration_from_value(&json!({"participant": "x"})).unwrap();
        assert!(d.requires.is_empty() && d.supports.is_empty());
    }

    #[test]
    fn rejects_bad_rows() {
        assert!(declaration_from_value(&json!({})).is_err(), "缺 participant");
        assert!(declaration_from_value(&json!({"participant": ""})).is_err(), "空 participant");
        assert!(declaration_from_value(&json!({"participant": "p", "requires": [{"apiVersion": "dsh/a/v2x", "kind": "A"}]})).is_err(), "非法 apiVersion");
        assert!(declaration_from_value(&json!({"participant": "p", "requires": [{"apiVersion": "dsh.a/v1", "kind": "a"}]})).is_err(), "非法 kind");
        assert!(declaration_from_value(&json!({"participant": "p", "requires": [{"apiVersion": "dsh.a/v1", "kind": "A", "optional": "yes"}]})).is_err(), "optional 非 bool");
        // 同 key 两行 → 拒。
        assert!(declaration_from_value(&json!({"participant": "p", "supports": [
            {"apiVersion": "dsh.a/v1", "kind": "A"},
            {"apiVersion": "dsh.a/v1", "kind": "A"}
        ]}))
        .is_err());
    }

    #[test]
    fn same_key_needs_same_version_and_kind() {
        let a = ApiReference { api_version: "dsh.a/v1".into(), kind: "A".into() };
        let b = ApiReference { api_version: "dsh.a/v1".into(), kind: "B".into() };
        assert_ne!(a.key(), b.key(), "kind 参与坐标");
    }
}

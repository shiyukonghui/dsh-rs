//! apiVersion 文法：`^[a-z][a-z0-9.-]*/v[1-9][0-9]*((alpha|beta)[1-9][0-9]*)?$`
//! （与 dsh-std `version.js`/`protocol.js` 同一正则语义；kind=大驼峰另列）。

use serde::Serialize;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Stability {
    Stable,
    Alpha,
    Beta,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApiVersion {
    pub group: String,
    pub major: u32,
    pub stability: Stability,
    /// stable 恒 0；alpha/beta 为其修订号。
    pub revision: u32,
}

/// 严格解析（非法即 Err(String)，错误信息对齐 dsh-std 的 invalid apiVersion 语义）。
pub fn parse_api_version(value: &str) -> Result<ApiVersion, String> {
    let invalid = |why: &str| Err(format!("invalid apiVersion {value:?}: {why}"));
    // 恰好一个 '/' 分隔 group 与版本段（group 文法不含 '/'）。
    let mut parts = value.split('/');
    let group = parts.next().unwrap_or_default();
    let rest = match parts.next() {
        Some(r) => r,
        None => return invalid("missing /v segment"),
    };
    if parts.next().is_some() {
        return invalid("multiple segments");
    }
    if group.is_empty() || !group.starts_with(|c: char| c.is_ascii_lowercase()) {
        return invalid("group must start with lowercase letter");
    }
    if !group
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-')
    {
        return invalid("group has illegal characters");
    }
    let rest = rest.strip_prefix('v').ok_or_else(|| format!("invalid apiVersion {value:?}: version must start with 'v'"))?;
    let digits_end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    let (digits, suffix) = rest.split_at(digits_end);
    if digits.is_empty() || (digits.len() > 1 && digits.starts_with('0')) || digits.starts_with('0') {
        return invalid("major must be >=1 without leading zero");
    }
    let major: u32 = digits.parse().map_err(|_| format!("invalid apiVersion {value:?}: major overflow"))?;
    if major < 1 {
        return invalid("major must be >=1");
    }
    if suffix.is_empty() {
        return Ok(ApiVersion { group: group.to_string(), major, stability: Stability::Stable, revision: 0 });
    }
    // alphaN / betaN（N≥1）；其余一律非法（不猜）。
    for (tag, st) in [("alpha", Stability::Alpha), ("beta", Stability::Beta)] {
        if let Some(n) = suffix.strip_prefix(tag) {
            if n.is_empty()
                || n.starts_with('0')
                || !n.chars().all(|c| c.is_ascii_digit())
            {
                return invalid("stability revision must be >=1 digits");
            }
            let revision: u32 = n.parse().map_err(|_| format!("invalid apiVersion {value:?}: revision overflow"))?;
            return Ok(ApiVersion { group: group.to_string(), major, stability: st, revision });
        }
    }
    invalid("unknown stability tag")
}

impl fmt::Display for ApiVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.stability {
            Stability::Stable => write!(f, "{}/v{}", self.group, self.major),
            Stability::Alpha => write!(f, "{}/v{}alpha{}", self.group, self.major, self.revision),
            Stability::Beta => write!(f, "{}/v{}beta{}", self.group, self.major, self.revision),
        }
    }
}

/// kind 文法：大驼峰 `^[A-Z][A-Za-z0-9]*$`（dsh-std 同款）。
pub fn validate_kind(kind: &str) -> Result<(), String> {
    if kind.is_empty() || !kind.starts_with(|c: char| c.is_ascii_uppercase()) {
        return Err(format!("invalid kind {kind:?}"));
    }
    if !kind.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(format!("invalid kind {kind:?}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_official_shapes() {
        // P0 新契约串（我们的标准文法正字）。
        let v = parse_api_version("dsh.panel-ui/v2").unwrap();
        assert_eq!((v.group.as_str(), v.major, v.stability), ("dsh.panel-ui", 2, Stability::Stable));
        let v = parse_api_version("dsh.session-log/v10beta7").unwrap();
        assert_eq!((v.group.as_str(), v.major, v.stability, v.revision), ("dsh.session-log", 10, Stability::Beta, 7));
        let v = parse_api_version("core.dsh-negotiation/v1alpha1").unwrap();
        assert_eq!((v.group.as_str(), v.major, v.stability, v.revision), ("core.dsh-negotiation", 1, Stability::Alpha, 1));
    }

    #[test]
    fn rejects_everything_else() {
        // 旧方言串（双斜杠）必须非法——这就是 P0 硬切的文法根据。
        // 注：dsh-std README 的 "core.dsh/report/v1alpha1" 同样三段——被其自家
        // protocol.ts 正则拒绝（双实现互认发现的规范不自洽，采文法弃示例）。
        for bad in [
            "dsh/plugin-ui/v2", "dsh/plugin-ui/v1", "core.dsh/report/v1alpha1",
            "", "v2", "dsh.x/V2", "dsh.x/v0", "dsh.x/v01",
            "dsh.x/v2gamma1", "dsh.x/v2alpha0", "Dsh.x/v1", "dsh.x/v", "dsh_x/v2alpha",
            "dsh..x/v1x",
        ] {
            assert!(parse_api_version(bad).is_err(), "必须非法: {bad}");
        }
        // 注意：下划线不在 group 文法（dsh-std 一致）→ 上列 dsh_x 非法。
    }

    #[test]
    fn roundtrip_display_grammar() {
        for s in ["core.dsh-negotiation/v1alpha1", "dsh.panel-ui/v2", "a/v1beta3", "dash-ed.v2x/v12"] {
            let v = parse_api_version(s).unwrap();
            assert_eq!(v.to_string(), s, "parse→format 必须恒等");
        }
    }

    #[test]
    fn kind_grammar() {
        assert!(validate_kind("SessionLog").is_ok());
        assert!(validate_kind("PanelUi9").is_ok());
        for bad in ["", "sessionLog", "Session-Log", "9Session"] {
            assert!(validate_kind(bad).is_err(), "kind 必须非法: {bad}");
        }
    }
}

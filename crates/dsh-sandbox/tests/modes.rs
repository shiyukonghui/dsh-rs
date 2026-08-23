//! dsh-sandbox：SandboxMode 阶梯契约（M5-DESIGN §3.1）。
//!
//! 参考 `sandbox/sandbox/src/types.ts` + `index.ts`/`escalation.ts`：
//! `SandboxMode = 'read-only'|'workspace-write'|'danger-full-access'`（kebab 序列化）；
//! 升级只允许「严格更宽」target；read-only 永不可作为升级 target。

use dsh_sandbox::{wider_modes, wider_modes_map, SandboxMode, ESCALATION_TARGETS};

#[test]
fn mode_kebab_serialization_roundtrip() {
    for (mode, expect) in [
        (SandboxMode::ReadOnly, "read-only"),
        (SandboxMode::WorkspaceWrite, "workspace-write"),
        (SandboxMode::DangerFullAccess, "danger-full-access"),
    ] {
        assert_eq!(mode.as_str(), expect, "as_str");
        assert_eq!(format!("{mode}"), expect, "Display");
        let parsed: SandboxMode = expect.parse().unwrap();
        assert_eq!(parsed, mode, "FromStr {expect}");
    }
}

#[test]
fn mode_parse_rejects_unknown() {
    assert!("read_only".parse::<SandboxMode>().is_err());
    assert!("danger".parse::<SandboxMode>().is_err());
    assert!("".parse::<SandboxMode>().is_err());
}

#[test]
fn escalation_targets_are_only_wider_modes() {
    assert_eq!(
        ESCALATION_TARGETS.to_vec(),
        vec![SandboxMode::WorkspaceWrite, SandboxMode::DangerFullAccess]
    );
}

#[test]
fn wider_modes_is_strict_wider_ladder() {
    let w = wider_modes_map();
    assert_eq!(
        w[&SandboxMode::ReadOnly],
        vec![SandboxMode::WorkspaceWrite, SandboxMode::DangerFullAccess]
    );
    assert_eq!(w[&SandboxMode::WorkspaceWrite], vec![SandboxMode::DangerFullAccess]);
    assert!(w[&SandboxMode::DangerFullAccess].is_empty());
    // 严格单向：read-only 永不在任何模式的 wider 集合里作为 target
    assert!(w.values().all(|targets| !targets.contains(&SandboxMode::ReadOnly)));
}

#[test]
fn wider_modes_function_matches_map() {
    let w = wider_modes_map();
    for m in [SandboxMode::ReadOnly, SandboxMode::WorkspaceWrite, SandboxMode::DangerFullAccess] {
        assert_eq!(wider_modes(m), w[&m], "fn == map for {m:?}");
    }
}

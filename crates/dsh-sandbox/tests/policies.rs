//! dsh-sandbox：escalation 校验 + writableRoots + 标记词汇（M5-DESIGN §3.1/§3.2）。
//!
//! 参考 `escalation.ts`/`roots.ts` 逐字：validateEscalationArgs（同现 + justification 非空）、
//! sandboxDenialMarker / escalationHintMarker、writableRoots（[workspaceRoot, /tmp, tmpdir()]
//! canonical 去重；read-only → []）。

use dsh_sandbox::{
    canonical_path, escalation_hint_marker, sandbox_denial_marker, validate_escalation_args,
    writable_roots, SandboxMode,
};

#[test]
fn escalation_validates_together_or_neither() {
    // 同现且 justification 非空 → 放行
    validate_escalation_args(Some("workspace-write"), Some("需要写工作区文件")).expect("valid pair");
    validate_escalation_args(None, None).expect("neither ok");

    // 缺一方 → 拒绝
    assert!(validate_escalation_args(Some("workspace-write"), None).is_err());
    assert!(validate_escalation_args(None, Some("justification")).is_err());

    // justification 全空白 → 拒绝
    assert!(validate_escalation_args(Some("workspace-write"), Some("   ")).is_err());
}

#[test]
fn escalation_error_messages_match_reference() {
    let err = validate_escalation_args(Some("workspace-write"), None).unwrap_err();
    assert!(err.contains("sandbox_permissions requires a justification"), "got {err}");

    let err = validate_escalation_args(None, Some("j")).unwrap_err();
    assert!(err.contains("justification is only valid together with sandbox_permissions"), "got {err}");

    let err = validate_escalation_args(Some("workspace-write"), Some("  ")).unwrap_err();
    assert!(err.contains("expected a non-empty sentence"), "got {err}");
}

#[test]
fn denial_marker_matches_reference() {
    assert_eq!(
        sandbox_denial_marker(SandboxMode::ReadOnly),
        "[sandbox: file access denied under read-only mode]"
    );
    assert_eq!(
        sandbox_denial_marker(SandboxMode::WorkspaceWrite),
        "[sandbox: file access denied under workspace-write mode]"
    );
}

#[test]
fn escalation_hint_marker_matches_reference() {
    let hint = escalation_hint_marker("write");
    assert!(hint.starts_with("[sandbox: escalation available — retry this exact write once with sandbox_permissions"), "got {hint}");
    assert!(hint.contains("+ justification"), "got {hint}");
}

#[test]
fn readable_roots_empty_under_read_only() {
    // read-only → 空根
    let roots = writable_roots(SandboxMode::ReadOnly, None);
    assert!(roots.is_empty(), "read-only allows nothing, got {roots:?}");
    // danger-full-access 同样不产名单（直通语义）
    let roots = writable_roots(
        SandboxMode::DangerFullAccess,
        Some(std::env::temp_dir().join("ws")),
    );
    assert!(roots.is_empty(), "danger yields no roots list, got {roots:?}");
}

#[test]
fn workspace_write_roots_dedupe_canonical() {
    let roots = writable_roots(SandboxMode::WorkspaceWrite, Some(std::env::temp_dir().join("ws")));
    // [canonical(ws), /tmp or (windows 无), tmpdir()] 去重。
    let mut unique: Vec<String> = roots.iter().map(|s| s.to_string_lossy().into_owned()).collect();
    unique.sort();
    unique.dedup();
    assert_eq!(roots.len(), unique.len(), "no dup roots: {roots:?}");
    assert!(roots.iter().any(|r| canonical_path(&r.to_string_lossy()).to_string_lossy().contains("ws")), "workspace root present: {roots:?}");
}

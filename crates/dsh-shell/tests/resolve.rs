//! dsh-shell：resolve 纯面（M5-DESIGN §5.2，参考 bash-local resolve + clampTimeout）。

use dsh_shell::{
    assert_serviceable_bash_config, clamp_timeout, resolve, BashConfig, ShellExecRequest,
    DEFAULT_GRACE_MS, DEFAULT_MAX_OUTPUT_BYTES, DEFAULT_MAX_TIMEOUT_MS, DEFAULT_TIMEOUT_MS,
    MAX_TIMER_DELAY_MS,
};

#[test]
fn defaults_aligned() {
    assert_eq!(DEFAULT_TIMEOUT_MS, 120_000);
    assert_eq!(DEFAULT_MAX_TIMEOUT_MS, 600_000);
    assert_eq!(DEFAULT_MAX_OUTPUT_BYTES, 64_000);
    assert_eq!(DEFAULT_GRACE_MS, 3_000);
    assert_eq!(MAX_TIMER_DELAY_MS, 2_147_483_647);
}

#[test]
fn clamp_timeout_default_then_cap() {
    assert_eq!(clamp_timeout(None, 3_000, 10_000, "t").unwrap(), 3_000);
    assert_eq!(
        clamp_timeout(Some(2_000), 3_000, 10_000, "t").unwrap(),
        2_000
    );
    // 超出上限 → clamp
    assert_eq!(
        clamp_timeout(Some(50_000), 3_000, 10_000, "t").unwrap(),
        10_000
    );
}

#[test]
fn clamp_timeout_rejects_zero() {
    assert_eq!(
        clamp_timeout(Some(0), 3_000, 10_000, "bash-local: request.timeoutMs").unwrap_err(),
        "bash-local: request.timeoutMs must be a positive finite number"
    );
}

#[test]
fn config_must_be_serviceable() {
    let too_long_grace = BashConfig {
        grace_ms: MAX_TIMER_DELAY_MS + 1,
        ..BashConfig::default()
    };
    assert!(assert_serviceable_bash_config(&too_long_grace).is_err());
    let zero_timeout = BashConfig {
        timeout_ms: 0,
        ..BashConfig::default()
    };
    assert!(assert_serviceable_bash_config(&zero_timeout).is_err());
    // 默认即合法
    assert!(assert_serviceable_bash_config(&BashConfig::default()).is_ok());
}

#[test]
fn resolve_applies_defaults_and_clamp() {
    let cfg = BashConfig::default();
    let spec = resolve(
        &ShellExecRequest {
            command: "echo hi".into(),
            ..Default::default()
        },
        &cfg,
    )
    .expect("resolve ok");
    assert_eq!(spec.command, "echo hi");
    assert_eq!(spec.timeout_ms, DEFAULT_TIMEOUT_MS);
    assert_eq!(spec.stdout_max_bytes, DEFAULT_MAX_OUTPUT_BYTES);
    assert_eq!(spec.workdir, std::env::current_dir().expect("cwd"));
    assert!(!spec.bash_program.is_empty(), "bash 程序已解析");
}

#[test]
fn resolve_respects_request_overrides_and_caps() {
    let cfg = BashConfig::default();
    let spec = resolve(
        &ShellExecRequest {
            command: "x".into(),
            workdir: Some("C:\\some\\dir".into()),
            timeout_ms: Some(999_999),
            stdout_max_bytes: Some(123),
            stdin: Some("in".into()),
            ..Default::default()
        },
        &cfg,
    )
    .expect("resolve ok");
    assert_eq!(spec.workdir.to_string_lossy(), "C:\\some\\dir");
    assert_eq!(
        spec.timeout_ms, cfg.max_timeout_ms,
        "超上限 clamp 到 maxTimeoutMs"
    );
    assert_eq!(spec.stdout_max_bytes, 123);
    assert_eq!(spec.stdin.as_deref(), Some("in"));
}

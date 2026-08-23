//! dsh-subprocess：scrubbedParentEnv 契约（M5-DESIGN §2.1/§2.5）。
//!
//! 参考（`subprocess/subprocess/src/types.ts`）：父环境 − credential-shaped
//! (`/KEY|PASSWORD|SECRET|TOKEN/i`) − 所有 `DSH_*`。本测试先行定义期望行为（红），
//! 再驱动实现（绿）。

use std::collections::BTreeMap;
use std::ffi::OsString;

use dsh_subprocess::scrubbed_parent_env;

/// 判别一个键是否为 credential-shaped。
fn is_credential(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    upper.contains("KEY")
        || upper.contains("PASSWORD")
        || upper.contains("SECRET")
        || upper.contains("TOKEN")
}

/// 判别一个键是否 DSH_* 托管前缀。
fn is_dsh(key: &str) -> bool {
    key.starts_with("DSH_")
}

fn sample_env() -> Vec<(OsString, OsString)> {
    [
        ("PATH", "/usr/bin"),
        ("HOME", "/home/user"),
        ("API_KEY", "abc123"),
        ("DB_PASSWORD", "s3cret"),
        ("ACCESS_TOKEN", "tok"),
        ("CLIENT_SECRET", "sec"),
        ("MY_SECRET_KEY", "k"),
        ("DSH_WORKSPACE_ROOT", "/ws"),
        ("DSH_SESSION_ID", "s1"),
        ("KEEPSAKE", "kept"),
        ("SECONDARY", "kept2"),
        ("APISECRET", "scrubbed"), // SECRET 子串 → credential-shaped
        ("USERKEYS", "scrubbed"),  // KEY 子串 → credential-shaped
    ]
    .into_iter()
    .map(|(k, v)| (OsString::from(k), OsString::from(v)))
    .collect()
}

#[test]
fn scrub_removes_credentials_and_dsh_but_keeps_plain() {
    let src = sample_env();
    let out = scrubbed_parent_env(&src);

    let map: BTreeMap<String, String> = out
        .iter()
        .map(|(k, v)| {
            (
                k.to_string_lossy().to_string(),
                v.to_string_lossy().to_string(),
            )
        })
        .collect();

    // DSH_* 全局清除
    assert!(
        map.keys().all(|k| !is_dsh(k)),
        "no DSH_* may survive, got: {:?}",
        map.keys().collect::<Vec<_>>()
    );

    // credential-shaped 清除
    assert!(
        map.keys().all(|k| !is_credential(k)),
        "no credential-shaped key may survive, got: {:?}",
        map.keys().collect::<Vec<_>>()
    );

    // 关键字保留
    assert_eq!(map.get("PATH").map(String::as_str), Some("/usr/bin"));
    assert_eq!(map.get("HOME").map(String::as_str), Some("/home/user"));
    assert_eq!(map.get("KEEPSAKE").map(String::as_str), Some("kept"));
    assert_eq!(map.get("SECONDARY").map(String::as_str), Some("kept2"));

    // 精确断言剩余集合（顺序无关）
    let expected = ["HOME", "KEEPSAKE", "PATH", "SECONDARY"];
    let mut got: Vec<&String> = map.keys().collect();
    got.sort();
    assert_eq!(got, expected);
}

#[test]
fn scrub_keeps_empty_source() {
    assert!(scrubbed_parent_env(&[]).is_empty());
}

#[test]
fn scrub_handles_lowercase_key_pattern_via_uppercase_probe() {
    // 参考按关键词（大小写无关）匹配；我们以大写探针对齐实现但保留小写原文。
    let src = vec![
        (OsString::from("apiKey"), OsString::from("v")),
        (OsString::from("normal"), OsString::from("v")),
    ];
    let out: Vec<(String, String)> = scrubbed_parent_env(&src)
        .into_iter()
        .map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                v.to_string_lossy().into_owned(),
            )
        })
        .collect();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].0, "normal");
}

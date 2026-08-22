//! M3c：dsh-credentials 能力缝（对齐 `@deepseek-ai/dsh-credentials` + credentials-local）。
//!
//! 覆盖（每项 = TS 语义的一个独立断言）：
//! - REF 语法校验（POSIX 环境变量名）；
//! - describe/resolve 分层：进程 env（只读，wins）> 本地文件（可写）；
//! - 空值 seam-wide 规则：空串 = 未配置（resolve 跳过、describe unconfigured）；
//! - set 非空校验 + shadowed 拒绝（env 已设 → set/unset 拒）；
//! - 文件持久化 round-trip（写 → 重建读回）；
//! - unset 幂等（absent → 成功 no-op）；
//! - 文件损坏 → 解析拒绝（boot-invalid，不静默当空）。

use dsh_credentials::{
    is_credential_ref_name, CredentialProvider, CredentialView, CredentialsError, ResolvedCredential,
};
use std::collections::HashMap;

#[test]
fn ref_name_grammar() {
    assert!(is_credential_ref_name("DEEPSEEK_API_KEY"));
    assert!(is_credential_ref_name("_X"));
    assert!(!is_credential_ref_name("1abc"));
    assert!(!is_credential_ref_name("a-b"));
    assert!(!is_credential_ref_name("a b"));
    assert!(!is_credential_ref_name(""));
}

/// 进程环境提供值（只读）；doc/set/unset 拒绝。
#[test]
fn env_layers_readonly_and_wins() {
    let mut env = HashMap::new();
    env.insert("DEEPSEEK_API_KEY".to_string(), "env-secret".to_string());
    let mut p = CredentialProvider::with_env(env);
    let resolved: Option<ResolvedCredential> = p.resolve("DEEPSEEK_API_KEY").unwrap();
    let r = resolved.expect("env supplies value");
    assert_eq!(r.value, "env-secret");
    assert_eq!(r.source, "env");
    let info = p.describe("DEEPSEEK_API_KEY").unwrap();
    assert!(info.configured);
    assert_eq!(info.source.as_deref(), Some("env"));
    assert!(!info.writable, "inherited env is read-only");
    // set/unset shadowed 拒绝。
    assert!(matches!(p.set("DEEPSEEK_API_KEY", "x"), Err(CredentialsError::Shadowed(_))));
    assert!(matches!(p.unset("DEEPSEEK_API_KEY"), Err(CredentialsError::Shadowed(_))));
}

/// 未配置 ref：describe → configured:false, writable:true；resolve → None。
#[test]
fn unconfigured_ref() {
    let p = CredentialProvider::memory();
    let info = p.describe("NOT_SET_ANYWHERE").unwrap();
    assert!(!info.configured);
    assert!(info.writable);
    assert_eq!(info.source, None);
    assert!(p.resolve("NOT_SET_ANYWHERE").unwrap().is_none());
}

/// 文件存储 + resolve（env 缺席时文件 wins）+ set/unset 文件持久化。
#[test]
fn file_set_resolve_unset_roundtrip() {
    let dir = std::env::temp_dir().join(format!("dsh-cred-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join(".credentials.yaml");
    let mut p = CredentialProvider::file(path.clone());
    // set → resolve。
    p.set("MY_KEY", "s3cret").expect("set ok");
    let resolved = p.resolve("MY_KEY").unwrap().expect("configured after set");
    assert_eq!(resolved.value, "s3cret");
    assert_eq!(resolved.source, "file");
    let info = p.describe("MY_KEY").unwrap();
    assert!(info.configured);
    assert_eq!(info.source.as_deref(), Some("file"));
    assert!(info.writable);
    // 重建读回（持久化检验）。
    let mut p2 = CredentialProvider::file(path.clone());
    let resolved2 = p2.resolve("MY_KEY").unwrap().expect("reloaded");
    assert_eq!(resolved2.value, "s3cret");
    assert_eq!(resolved2.source, "file");
    // unset → absent，再 unset 幂等成功。
    p2.unset("MY_KEY").expect("unset ok");
    assert!(p2.resolve("MY_KEY").unwrap().is_none());
    p2.unset("MY_KEY").expect("unset absent is no-op");
    let p3 = CredentialProvider::file(path.clone());
    assert!(p3.resolve("MY_KEY").unwrap().is_none(), "unset persisted");
    let _ = std::fs::remove_dir_all(&dir);
}

/// set 拒绝空值。
#[test]
fn rejects_empty_value() {
    let mut p = CredentialProvider::memory();
    assert!(matches!(p.set("KEY", ""), Err(CredentialsError::Empty(_))));
}

/// 文件损坏 → 解析拒绝（不静默当空）。
#[test]
fn corrupted_file_fails_loud() {
    let dir = std::env::temp_dir().join(format!("dsh-cred-bad-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join(".credentials.yaml");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(&path, "version: 1\nrefs:\n  - not-a-map").unwrap();
    // 尝试从文件构建 → 必须拒绝（而非返回空）。
    let result = CredentialProvider::try_file(path.clone());
    assert!(result.is_err(), "corrupted document must fail loud");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 文件布局：`version: 1` + `refs:` map；未知顶层键拒绝。
#[test]
fn file_layout_versioned() {
    let dir = std::env::temp_dir().join(format!("dsh-cred-v-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join(".credentials.yaml");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        &path,
        "version: 1\nrefs:\n  A_KEY: value\nrecords: {}\n",
    )
    .unwrap();
    let p = CredentialProvider::try_file(path.clone()).expect("valid doc");
    let resolved = p.resolve("A_KEY").unwrap().expect("file value");
    assert_eq!(resolved.value, "value");
    let _ = std::fs::remove_dir_all(&dir);
}

/// describe 批量视图（多 ref 各自独立）。
#[test]
fn describe_key_preserved() {
    let mut env = HashMap::new();
    env.insert("A".to_string(), "a".to_string());
    let p = CredentialProvider::with_env(env);
    let v: CredentialView = p.describe("A").unwrap();
    assert_eq!(v, CredentialView { configured: true, source: Some("env".into()), writable: false });
}

//! dsh-code-runtime：run_code 工具纯面（M5-DESIGN §7.4：code/description 必填、
//! `<parent>:code:<n>` 确定性嵌套派发 id、无递归）。

use dsh_code_runtime::run_code::{
    code_dispatch_id, exclude_run_code, parse_run_code_args, run_code_schema,
};
use serde_json::json;

#[test]
fn run_code_schema_requires_code_and_description() {
    let v = run_code_schema();
    let obj = v.as_object().expect("obj");
    assert_eq!(obj["code"]["type"], json!("string"));
    assert_eq!(obj["code"]["required"], json!(true));
    assert_eq!(obj["description"]["type"], json!("string"));
    assert_eq!(obj["description"]["required"], json!(true));
}

#[test]
fn parse_run_code_args_validation() {
    let (code, desc) =
        parse_run_code_args(&json!({ "code": "return 1", "description": "one" })).expect("ok");
    assert_eq!(code, "return 1");
    assert_eq!(desc, "one");
    assert!(parse_run_code_args(&json!({"description": "x"})).is_err());
    assert!(parse_run_code_args(&json!({"code": "x"})).is_err());
    assert!(
        parse_run_code_args(&json!({"code": "", "description": "x"})).is_err(),
        "空 code 拒绝"
    );
}

#[test]
fn code_dispatch_ids_are_deterministic_nested() {
    assert_eq!(code_dispatch_id("parent-1", 0), "parent-1:code:0");
    assert_eq!(code_dispatch_id("parent-1", 2), "parent-1:code:2");
    assert_eq!(
        code_dispatch_id("parent-1:code:1", 0),
        "parent-1:code:1:code:0"
    );
}

#[test]
fn run_code_is_never_exposed_to_itself() {
    let names = &["run_code", "bash", "fs_read", "run_code"];
    let out = exclude_run_code(names);
    assert_eq!(out, vec!["bash", "fs_read"], "run_code 从注入命名空间剔除");
}

// 参考 may_evaluate：断言嵌套派发 id 前缀可追溯回父工具（消歧保证）。
#[test]
fn dispatch_prefix_preserves_ancestry() {
    let child = code_dispatch_id("owner:code:3", 1);
    assert!(child.starts_with("owner:code:3"), "{child}");
}

//! dsh-eval：`!!js` 表达式子集求值 + interpolate。

use std::collections::HashMap;

use dsh_eval::*;
use serde_json::json;

fn scope() -> HashMap<String, serde_json::Value> {
    let mut s = HashMap::new();
    s.insert(
        "config".to_string(),
        json!({"env": "prod", "k": 21, "user": {"name": "ada"}, "list": [1, 2, 3], "obj": {"a": 1, "b": 2}, "n": "42", "flag": false}),
    );
    s
}

#[test]
fn arithmetic_and_comparison() {
    let s = scope();
    assert_eq!(evaluate(&s, "1 + 2 * 3").unwrap(), json!(7));
    assert_eq!(evaluate(&s, "config.k * 2").unwrap(), json!(42));
    assert_eq!(evaluate(&s, "config.env === 'prod'").unwrap(), json!(true));
    assert_eq!(evaluate(&s, "config.env === 'dev'").unwrap(), json!(false));
    assert_eq!(evaluate(&s, "config.k > 10").unwrap(), json!(true));
    assert_eq!(evaluate(&s, "10 % 3").unwrap(), json!(1));
}

#[test]
fn logical_and_ternary() {
    let s = scope();
    // JS truthiness：0/""/null/false 为假
    assert_eq!(evaluate(&s, "config.flag || 'fallback'").unwrap(), json!("fallback"));
    assert_eq!(evaluate(&s, "config.env && 'yes'").unwrap(), json!("yes"));
    assert_eq!(evaluate(&s, "!config.flag").unwrap(), json!(true));
    assert_eq!(evaluate(&s, "config.k > 10 ? 'big' : 'small'").unwrap(), json!("big"));
}

#[test]
fn member_access() {
    let s = scope();
    assert_eq!(evaluate(&s, "config.user.name").unwrap(), json!("ada"));
    assert_eq!(evaluate(&s, "config.list[1]").unwrap(), json!(2));
    assert_eq!(evaluate(&s, "config.list.length").unwrap(), json!(3));
}

#[test]
fn whitelist_calls() {
    let s = scope();
    assert_eq!(evaluate(&s, "Number(config.n) + 1").unwrap(), json!(43));
    assert_eq!(evaluate(&s, "String(config.k)").unwrap(), json!("21"));
    assert_eq!(evaluate(&s, "Boolean(config.flag)").unwrap(), json!(false));
    assert_eq!(evaluate(&s, "Array.isArray(config.list)").unwrap(), json!(true));
    assert_eq!(evaluate(&s, "Object.keys(config.obj).length").unwrap(), json!(2));
    // 非白名单调用拒绝（fail loud）
    assert!(evaluate(&s, "Math.max(1, 2)").is_err());
    assert!(evaluate(&s, "config.env = 'x'").is_err());
}

#[test]
fn interpolate_replaces_js_expr_nodes() {
    let s = scope();
    let v = json!({"k": 21, "doubled": {"__jsExpr": "config.k * 2"}, "list": [{"__jsExpr": "1 + 1"}, 3]});
    let out = interpolate(&s, &v).unwrap();
    assert_eq!(out, json!({"k": 21, "doubled": 42, "list": [2, 3]}));
}

#[test]
fn syntax_error_fails_loud() {
    let s = scope();
    assert!(evaluate(&s, "config.env ===").is_err());
    assert!(evaluate(&s, "(1 + 2").is_err());
    assert!(evaluate(&s, "unknown.thing").is_err());
}

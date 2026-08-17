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

/// M50：可选链 `?.`——null 安全成员访问（`a?.b` 当 a 为 null/undefined 时
/// 返回 null 而非报错；非 null 时等价 `a.b`）。
#[test]
fn optional_chaining_null_safe() {
    let s = scope();
    // 非 null 对象：等价普通成员访问
    assert_eq!(evaluate(&s, "config?.k").unwrap(), json!(21));
    assert_eq!(evaluate(&s, "config?.user?.name").unwrap(), json!("ada"));
    // null 基对象：短路返回 null
    assert_eq!(evaluate(&s, "config?.missing?.deep").unwrap(), json!(null));
    // 作用域内不存在的标识符：`?.` 也短路（JS：undefined?.b → undefined）
    assert_eq!(evaluate(&s, "ghost?.deep").unwrap(), json!(null));
    // 数组索引 + 可选链
    assert_eq!(evaluate(&s, "config?.list?.[0]").unwrap(), json!(1));
    // 普通 `.` 在 null 上仍报错（fail loud，与 JS 一致）
    assert!(evaluate(&s, "ghost.deep").is_err());
}

/// M51：nullish coalescing `??`——仅当左侧为 null 时取右侧（与 `||` 的
/// truthiness 短路不同：0/''/false 保留左侧）。
#[test]
fn nullish_coalescing() {
    let s = scope();
    // null 左侧 → 取右侧
    assert_eq!(evaluate(&s, "config?.missing ?? 'fallback'").unwrap(), json!("fallback"));
    assert_eq!(evaluate(&s, "null ?? 'r'").unwrap(), json!("r"));
    // 非 null falsy 值 → 保留左侧（与 || 不同）
    assert_eq!(evaluate(&s, "0 ?? 'r'").unwrap(), json!(0));
    assert_eq!(evaluate(&s, "'' ?? 'r'").unwrap(), json!(""));
    assert_eq!(evaluate(&s, "false ?? 'r'").unwrap(), json!(false));
    // 与 || 对比：0 || 'r' → 'r'（truthiness）
    assert_eq!(evaluate(&s, "0 || 'r'").unwrap(), json!("r"));
    // 链式
    assert_eq!(evaluate(&s, "null ?? null ?? 'deep'").unwrap(), json!("deep"));
    // 优先级：与 || 同级（左结合）
    assert_eq!(evaluate(&s, "config.k ?? config.n ?? 0").unwrap(), json!(21));
}

/// M53：`typeof` 一元运算符——返回 JS 类型字符串（"string"/"number"/
/// "boolean"/"object"/"null"——Rust Value 无 undefined，未定义标识符 →
/// "undefined"）。
#[test]
fn typeof_operator() {
    let s = scope();
    assert_eq!(evaluate(&s, "typeof 'abc'").unwrap(), json!("string"));
    assert_eq!(evaluate(&s, "typeof config.k").unwrap(), json!("number"));
    assert_eq!(evaluate(&s, "typeof config.flag").unwrap(), json!("boolean"));
    assert_eq!(evaluate(&s, "typeof config.user").unwrap(), json!("object"));
    assert_eq!(evaluate(&s, "typeof null").unwrap(), json!("object")); // JS 遗留
    assert_eq!(evaluate(&s, "typeof ghost").unwrap(), json!("undefined")); // 未定义
    // 与 === 组合（守卫模式）
    assert_eq!(evaluate(&s, "typeof config.k === 'number'").unwrap(), json!(true));
    assert_eq!(evaluate(&s, "typeof config.k === 'string'").unwrap(), json!(false));
    // 优先级：typeof 高于二元
    assert_eq!(evaluate(&s, "typeof config.k === 'number' ? 'yes' : 'no'").unwrap(), json!("yes"));
}

/// M54：模板字符串——反引号 + `${expr}` 插值（段拼接，非字符串段转字符串）。
#[test]
fn template_strings() {
    let s = scope();
    // 纯字面量
    assert_eq!(evaluate(&s, "`hello`").unwrap(), json!("hello"));
    // 单插值
    assert_eq!(evaluate(&s, "`k=${config.k}`").unwrap(), json!("k=21"));
    // 多段 + 表达式（成员访问 / 算术 / 字符串拼接）
    assert_eq!(
        evaluate(&s, "`${config.user.name} is ${config.k + 1}`").unwrap(),
        json!("ada is 22")
    );
    // 表达式内字符串
    assert_eq!(evaluate(&s, "`${'a' + 'b'}`").unwrap(), json!("ab"));
    // 数字转字符串（JS String() 语义）
    assert_eq!(evaluate(&s, "`n=${config.n}`").unwrap(), json!("n=42"));
    // 空插值
    assert_eq!(evaluate(&s, "`x${config.k}y`").unwrap(), json!("x21y"));
}

/// M55：`in` 运算符——`'key' in obj` 键存在性检查（含数组索引/字符串键）。
#[test]
fn in_operator() {
    let s = scope();
    // 对象键存在性
    assert_eq!(evaluate(&s, "'k' in config").unwrap(), json!(true));
    assert_eq!(evaluate(&s, "'nope' in config").unwrap(), json!(false));
    assert_eq!(evaluate(&s, "'user' in config").unwrap(), json!(true));
    // 数组索引（JS：'0' in [1,2] → true）
    assert_eq!(evaluate(&s, "'0' in config.list").unwrap(), json!(true));
    assert_eq!(evaluate(&s, "'5' in config.list").unwrap(), json!(false));
    // 与 === 组合（守卫模式）
    assert_eq!(evaluate(&s, "'k' in config && config.k > 10").unwrap(), json!(true));
    // 非对象右侧 → 报错（fail loud）
    assert!(evaluate(&s, "'x' in 42").is_err());
    // 非键左侧 → 报错（fail loud；JS 中 false in {} 也是 TypeError）
    assert!(evaluate(&s, "false in config").is_err());
}

/// M59：`?.()` 可选调用——callee 为 null/未定义时短路返回 null（不调用）；
/// 否则等价普通调用。白名单函数（String 等）可经 `?.()` 调用。
#[test]
fn optional_call_short_circuits() {
    let s = scope();
    // 白名单函数经 `?.()` 正常调用
    assert_eq!(evaluate(&s, "String?.(42)").unwrap(), json!("42"));
    assert_eq!(evaluate(&s, "Number?.('7')").unwrap(), json!(7));
    // 链式：config?.fn?.()——fn 不存在（缺失成员）→ 短路 null
    assert_eq!(evaluate(&s, "config?.handler?.()").unwrap(), json!(null));
    // 未定义标识符 + 可选调用 → 短路 null
    assert_eq!(evaluate(&s, "ghost?.()").unwrap(), json!(null));
    // 基对象 null → 短路
    assert_eq!(evaluate(&s, "config?.missing?.()").unwrap(), json!(null));
    // 普通调用在缺失成员上仍报错（fail loud）
    assert!(evaluate(&s, "config.handler()").is_err());
}

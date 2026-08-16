//! M4：dsh-schema 组合子语义（default/autofix/union/intersect/transform/lazy/错误消息）。

use std::collections::HashMap;
use std::rc::Rc;

use dsh_schema::*;
use serde_json::json;

fn resolve_ok(schema: &SchemaRef, data: serde_json::Value) -> serde_json::Value {
    resolve(&data, schema, &ResolveOptions::default()).unwrap()
}

fn resolve_err(schema: &SchemaRef, data: serde_json::Value) -> String {
    resolve(&data, schema, &ResolveOptions::default())
        .err()
        .map(|e| e.to_string())
        .expect("expected validation error")
}

/// object：default 填充、缺省键省略、required 报错、路径消息。
#[test]
fn object_defaults_and_required() {
    let mut dict = HashMap::new();
    dict.insert("a".to_string(), Schema::with_default(&Schema::number(), json!(1)));
    dict.insert("b".to_string(), Schema::string());
    let obj = Schema::object(dict);

    // default 填充：a=1；b 缺省（无默认 → 省略）
    assert_eq!(resolve_ok(&obj, json!({})), json!({"a": 1}));
    assert_eq!(resolve_ok(&obj, json!({"b": "x"})), json!({"a": 1, "b": "x"}));

    // required 缺失（即便整体为空对象也报错，路径 $.c）
    let mut d2 = HashMap::new();
    d2.insert("c".to_string(), Schema::required(&Schema::boolean()));
    let obj2 = Schema::object(d2);
    let msg = resolve_err(&obj2, json!({}));
    assert!(msg.contains("missing required value"), "{msg}");
    assert!(msg.contains("$.c"), "{msg}");
}

/// object：autofix 删除无效项并回退默认。
#[test]
fn object_autofix_strips_invalid() {
    let mut dict = HashMap::new();
    dict.insert("a".to_string(), Schema::number());
    dict.insert("b".to_string(), Schema::with_default(&Schema::number(), json!(7)));
    let obj = Schema::object(dict);

    let opts = ResolveOptions {
        autofix: true,
        ..ResolveOptions::default()
    };
    let out = resolve(&json!({"a": "bad", "b": "also-bad", "c": "kept"}), &obj, &opts).unwrap();
    // a 无效且无默认 → 删除；b 无效 → 回退默认 7；c 是多余键（非 strict）→ 保留
    assert_eq!(out, json!({"b": 7, "c": "kept"}));

    // 非 autofix：直接报错，路径指向 $.a
    let msg = resolve_err(&obj, json!({"a": "bad"}));
    assert!(msg.contains("expected number but got bad"), "{msg}");
    assert!(msg.contains("$.a"), "{msg}");
}

/// string/number/natural/percent 约束。
#[test]
fn scalar_constraints() {
    // pattern + 长度范围
    let s = Schema::pattern(&Schema::string(), "^[a-z]+$", "");
    assert_eq!(resolve_ok(&s, json!("abc")), json!("abc"));
    assert!(resolve_err(&s, json!("ABC")).contains("regexp"));

    let s = Schema::min(&Schema::string(), 3.0);
    assert!(resolve_err(&s, json!("ab")).contains("string length >= 3"));

    // number 范围
    let n = Schema::max(&Schema::number(), 10.0);
    assert!(resolve_err(&n, json!(11)).contains("number <= 10"));

    // natural：step 1 + min 0
    let nat = Schema::natural();
    assert_eq!(resolve_ok(&nat, json!(5)), json!(5));
    assert!(resolve_err(&nat, json!(-1)).contains("number >= 0"));
    assert!(resolve_err(&nat, json!(2.5)).contains("multiple of 1"));

    // percent：0..=1 step 0.01
    let pct = Schema::percent();
    assert_eq!(resolve_ok(&pct, json!(0.5)), json!(0.5));
    assert!(resolve_err(&pct, json!(1.5)).contains("number <= 1"));
}

/// array/tuple：长度约束与逐项校验。
#[test]
fn array_and_tuple() {
    let arr = Schema::array(Schema::number());
    assert_eq!(resolve_ok(&arr, json!([1, 2, 3])), json!([1, 2, 3]));
    assert!(resolve_err(&arr, json!([1, "x"])).contains("expected number but got x"));
    let msg = resolve_err(&arr, json!([1, "x"]));
    assert!(msg.contains("[1]"), "{msg}");

    let arr2 = Schema::max(&arr, 2.0);
    assert!(resolve_err(&arr2, json!([1, 2, 3])).contains("array length <= 2"));

    let tup = Schema::tuple(vec![Schema::number(), Schema::string()]);
    assert_eq!(resolve_ok(&tup, json!([1, "x"])), json!([1, "x"]));
    // 非 strict：多余项追加
    assert_eq!(resolve_ok(&tup, json!([1, "x", true])), json!([1, "x", true]));
}

/// dict：键经 sKey 校验。
#[test]
fn dict_with_key_schema() {
    let d = Schema::dict(Schema::number(), Schema::string());
    assert_eq!(resolve_ok(&d, json!({"a": 1, "b": 2})), json!({"a": 1, "b": 2}));
    assert!(resolve_err(&d, json!({"a": "x"})).contains("expected number but got x"));
}

/// union：逐个尝试，全部失败聚合错误。
#[test]
fn union_fallback_and_message() {
    let u = Schema::union(vec![Schema::number(), Schema::boolean()]);
    assert_eq!(resolve_ok(&u, json!(3)), json!(3));
    assert_eq!(resolve_ok(&u, json!(true)), json!(true));
    let msg = resolve_err(&u, json!("nope"));
    assert!(msg.contains("number"), "{msg}");
    assert!(msg.contains("boolean"), "{msg}");
    assert!(msg.contains("got \"nope\""), "{msg}");
}

/// intersect：合并对象。
#[test]
fn intersect_merges_objects() {
    let mut d1 = HashMap::new();
    d1.insert("a".to_string(), Schema::number());
    let mut d2 = HashMap::new();
    d2.insert("b".to_string(), Schema::string());
    let inter = Schema::intersect(vec![Schema::object(d1), Schema::object(d2)]);
    let out = resolve_ok(&inter, json!({"a": 1, "b": "x"}));
    assert_eq!(out, json!({"a": 1, "b": "x"}));
}

/// transform：先校验 inner 再回调。
#[test]
fn transform_callback() {
    let cb: TransformFn = Rc::new(|v| {
        let n = v.as_i64().unwrap_or(0);
        Ok(json!(n * 2))
    });
    let t = Schema::transform(Schema::number(), false, cb);
    assert_eq!(resolve_ok(&t, json!(21)), json!(42));
    assert!(resolve_err(&t, json!("x")).contains("expected number but got x"));
}

/// const/never/any。
#[test]
fn const_never_any() {
    let c = Schema::const_value(json!("prod"));
    assert_eq!(resolve_ok(&c, json!("prod")), json!("prod"));
    assert!(resolve_err(&c, json!("dev")).contains("expected \"prod\" but got dev"));

    let n = Schema::never();
    assert!(resolve_err(&n, json!(1)).contains("expected nullable but got 1"));

    assert_eq!(resolve_ok(&Schema::any(), json!({"a": [1]})), json!({"a": [1]}));
}

/// lazy：递归结构（树节点）。
#[test]
fn lazy_recursion() {
    let node = Schema::lazy(Rc::new(|| {
        let mut dict = HashMap::new();
        dict.insert("value".to_string(), Schema::number());
        dict.insert(
            "children".to_string(),
            Schema::array(Schema::lazy(Rc::new(|| {
                let mut d2 = HashMap::new();
                d2.insert("value".to_string(), Schema::number());
                Schema::object(d2)
            }))),
        );
        Schema::object(dict)
    }));
    let out = resolve_ok(&node, json!({"value": 1, "children": [{"value": 2}, {"value": 3}]}));
    assert_eq!(out, json!({"value": 1, "children": [{"value": 2}, {"value": 3}]}));
    assert!(resolve_err(&node, json!({"children": [{"value": "x"}]})).contains("expected number"));
}

/// 元数据链与 toString。
#[test]
fn meta_and_tostring() {
    let s = Schema::role(&Schema::description(&Schema::string(), "desc"), "textarea");
    assert_eq!(s.meta.role.as_deref(), Some("textarea"));
    assert_eq!(s.meta.description.as_deref(), Some("desc"));

    let mut dict = HashMap::new();
    dict.insert("a".to_string(), Schema::number());
    let obj = Schema::object(dict);
    assert_eq!(schema_to_string(&obj), "{ a?: number }");
    let u = Schema::union(vec![Schema::string(), Schema::number()]);
    assert_eq!(schema_to_string(&u), "string | number");
}

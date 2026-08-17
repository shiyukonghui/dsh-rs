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

fn resolve_strict(schema: &SchemaRef, data: serde_json::Value) -> serde_json::Value {
    let opts = ResolveOptions {
        strict: true,
        ..ResolveOptions::default()
    };
    resolve(&data, schema, &opts).unwrap()
}

/// M25 strict：object 不合并多余键（丢弃）。
#[test]
fn strict_object_drops_extra_keys() {
    let mut dict = HashMap::new();
    dict.insert("a".to_string(), Schema::number());
    let obj = Schema::object(dict);
    // 非 strict：多余键保留
    assert_eq!(resolve_ok(&obj, json!({"a": 1, "b": 2})), json!({"a": 1, "b": 2}));
    // strict：多余键丢弃
    assert_eq!(resolve_strict(&obj, json!({"a": 1, "b": 2})), json!({"a": 1}));
}

/// M25 strict：tuple 不追加多余项。
#[test]
fn strict_tuple_drops_extra_items() {
    let tup = Schema::tuple(vec![Schema::number(), Schema::string()]);
    // 非 strict：多余项追加
    assert_eq!(resolve_ok(&tup, json!([1, "x", true])), json!([1, "x", true]));
    // strict：多余项丢弃
    assert_eq!(resolve_strict(&tup, json!([1, "x", true])), json!([1, "x"]));
}

/// M25 strict：intersect 不合并剩余对象键。
#[test]
fn strict_intersect_drops_extra_keys() {
    let mut d1 = HashMap::new();
    d1.insert("a".to_string(), Schema::number());
    let mut d2 = HashMap::new();
    d2.insert("b".to_string(), Schema::string());
    let inter = Schema::intersect(vec![Schema::object(d1), Schema::object(d2)]);
    // 非 strict：多余键 c 合并
    assert_eq!(
        resolve_ok(&inter, json!({"a": 1, "b": "x", "c": true})),
        json!({"a": 1, "b": "x", "c": true})
    );
    // strict：多余键 c 丢弃
    assert_eq!(
        resolve_strict(&inter, json!({"a": 1, "b": "x", "c": true})),
        json!({"a": 1, "b": "x"})
    );
}

/// M25 strict：dict 的 sKey 校验失败——strict 跳过该键；非 strict 抛错。
#[test]
fn strict_dict_skips_invalid_key() {
    // sKey = 仅接受 "x" 的模式（pattern 约束）
    let s_key = Schema::pattern(&Schema::string(), "^x$", "");
    let d = Schema::dict(Schema::number(), s_key);

    // 非 strict：非法键抛错
    let msg = resolve_err(&d, json!({"a": 1}));
    assert!(msg.contains("regexp"), "{msg}");

    // strict：非法键跳过
    let out = resolve_strict(&d, json!({"a": 1, "x": 2}));
    assert_eq!(out, json!({"x": 2}), "invalid key dropped in strict mode");
}

/// M26：regex flags——`i`/`m`/`s` 生效；JS 的 `u`（Unicode）与 `g`/`y` 被
/// 安全处理（`u` 为 Rust regex 默认；`g`/`y` 对 test 无意义忽略）。
#[test]
fn regex_flags_behavior() {
    // i：大小写不敏感
    let ci = Schema::pattern(&Schema::string(), "^abc$", "i");
    assert_eq!(resolve_ok(&ci, json!("ABC")), json!("ABC"));
    assert!(resolve_err(&ci, json!("abd")).contains("regexp"));

    // m：多行（^ 匹配行首）
    let multi = Schema::pattern(&Schema::string(), "^b$", "m");
    assert_eq!(resolve_ok(&multi, json!("a\nb")), json!("a\nb"));
    // 无 m：^ 只匹配串首
    let single = Schema::pattern(&Schema::string(), "^b$", "");
    assert!(resolve_err(&single, json!("a\nb")).contains("regexp"));

    // s：点匹配换行
    let dotall = Schema::pattern(&Schema::string(), "^a.b$", "s");
    assert_eq!(resolve_ok(&dotall, json!("a\nb")), json!("a\nb"));
    let no_dotall = Schema::pattern(&Schema::string(), "^a.b$", "");
    assert!(resolve_err(&no_dotall, json!("a\nb")).contains("regexp"));

    // u（Unicode 默认）+ g/y（忽略，不报错）
    let uni = Schema::pattern(&Schema::string(), "^\\p{L}+$", "u");
    assert_eq!(resolve_ok(&uni, json!("中文")), json!("中文"));
    let gy = Schema::pattern(&Schema::string(), "^abc$", "gy");
    assert_eq!(resolve_ok(&gy, json!("abc")), json!("abc"));
    assert!(resolve_err(&gy, json!("abd")).contains("regexp"));
}

/// M26：date 组合子——字符串经 RFC3339 校验原样返回；非法日期报错。
#[test]
fn date_combinator_validates_rfc3339() {
    let d = Schema::date();
    // 合法 RFC3339 → 原样返回
    assert_eq!(resolve_ok(&d, json!("2026-01-15T10:30:00Z")), json!("2026-01-15T10:30:00Z"));
    assert_eq!(resolve_ok(&d, json!("2026-01-15T10:30:00+08:00")), json!("2026-01-15T10:30:00+08:00"));
    // 非法 → union 聚合错误（Date | string；transform 分支的 invalid date 被聚合）
    let msg = resolve_err(&d, json!("not-a-date"));
    assert!(msg.contains("Date") && msg.contains("string"), "{msg}");
    // 数字也失败（is(Date) 恒失败 + string 分支拒绝）
    assert!(resolve_err(&d, json!(123)).contains("expected"));
}

/// M26：regExp 组合子——字符串校验可编译；非法正则报错。
#[test]
fn regexp_combinator_validates_source() {
    let r = Schema::reg_exp("");
    assert_eq!(resolve_ok(&r, json!("^[a-z]+$")), json!("^[a-z]+$"));
    let msg = resolve_err(&r, json!("[unclosed"));
    assert!(!msg.is_empty(), "invalid regexp rejected: {msg}");

    // 带 flag：i 生效（源字符串经 build_regex 带 (?i) 编译）
    let ri = Schema::reg_exp("i");
    assert_eq!(resolve_ok(&ri, json!("^abc$")), json!("^abc$"));
}

/// M57：`Schema.extend` 自定义类型注册（对齐 Schemastery `Schema.extend(type,
/// resolve)`）——全局注册表，`Schema::custom(type)` 构造节点，resolve 查表。
#[test]
fn schema_extend_custom_type() {
    // 注册自定义类型 "duration"：数字校验 >= 0，输出保留原值
    Schema::extend("duration", |data, _schema, _opts| {
        let n = data.as_f64().ok_or_else(|| ValidationError::new("expected a number", &[]))?;
        if n < 0.0 {
            return Err(ValidationError::new("duration must be >= 0", &[]));
        }
        Ok(data.clone())
    });

    let custom = Schema::custom("duration");
    assert_eq!(resolve_ok(&custom, json!(42)), json!(42));
    assert_eq!(resolve_ok(&custom, json!(0)), json!(0));
    let msg = resolve_err(&custom, json!(-1));
    assert!(msg.contains(">= 0"), "{msg}");
    let msg2 = resolve_err(&custom, json!("not-a-number"));
    assert!(msg2.contains("number"), "{msg2}");

    // 未注册类型 → unsupported
    let unknown = Schema::custom("nope");
    let msg3 = resolve_err(&unknown, json!(1));
    assert!(msg3.contains("unsupported"), "{msg3}");
}

/// M57：`Schema.extend` 自定义类型可参与组合（object 内、union 分支）。
#[test]
fn schema_extend_composes() {
    Schema::extend("positive", |data, _schema, _opts| {
        let n = data.as_f64().ok_or_else(|| ValidationError::new("expected number", &[]))?;
        if n <= 0.0 {
            return Err(ValidationError::new("must be positive", &[]));
        }
        Ok(data.clone())
    });
    let positive = Schema::custom("positive");

    // object 组合
    let mut dict = HashMap::new();
    dict.insert("n".to_string(), positive.clone());
    let obj = Schema::object(dict);
    assert_eq!(resolve_ok(&obj, json!({"n": 5})), json!({"n": 5}));
    assert!(resolve(&json!({"n": -5}), &obj, &ResolveOptions::default()).is_err());

    // union 分支
    let u = Schema::union(vec![Schema::string(), positive]);
    assert_eq!(resolve_ok(&u, json!("str")), json!("str"));
    assert_eq!(resolve_ok(&u, json!(7)), json!(7));
    assert!(resolve(&json!(-7), &u, &ResolveOptions::default()).is_err());
}

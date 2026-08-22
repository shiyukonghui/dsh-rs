//! M3b：`Schema::to_json()`——Schemastery `Schema.prototype.toJSON` 的 Rust 等价。
//!
//! wire 形状：`{uid, refs:{ "<uid-str>": <node json>, ... }}`，其中：
//! - 每个 schema 节点序列化为 `{type, meta, ...结构性字段}`（不含 uid 本身——
//!   TS 的 uid 是 `defineProperty(enumerable:false)`，`{...this}` 展开不含它）；
//! - 结构性字段的嵌套 schema 引用以 **uid 数字** 占位（前端 `new Schema({uid, refs})`
//!   rehydrate 时用 refs[uid] 恢复引用）；
//! - meta 字段与 TS `schema.meta` 对齐（default/required/min/max/step/role/hidden/
//!   collapse/disabled/description/link/comment/badges/loose/pattern{source,flags}/extra）。

use dsh_schema::*;
use serde_json::{json, Value};
use std::collections::HashMap;

fn to_json(schema: &SchemaRef) -> Value {
    schema.to_json()
}

#[test]
fn primitive_string_shape() {
    let s = Schema::string();
    let v = to_json(&s);
    let refs = v["refs"].as_object().expect("refs map");
    assert!(refs.len() == 1, "one node for a leaf: {}", refs.len());
    let node = refs.values().next().unwrap();
    assert_eq!(node["type"], "string");
    // meta 存在（可为空对象）。
    assert!(node["meta"].is_object());
    // 根 uid 与 refs 唯一键一致。
    let root_uid = v["uid"].as_u64().unwrap();
    assert!(refs.contains_key(&root_uid.to_string()));
}

#[test]
fn object_with_property_refs_by_uid() {
    let mut dict = HashMap::new();
    dict.insert(
        "baseURL".to_string(),
        Schema::with_default(&Schema::string(), json!("https://api.example.com")),
    );
    dict.insert(
        "key".to_string(),
        Schema::description(&Schema::secret(&Schema::string()), "write-only"),
    );
    let obj = Schema::object(dict);
    let v = to_json(&obj);
    let refs = v["refs"].as_object().unwrap();
    // root + 2 children = 3 节点。
    assert_eq!(refs.len(), 3, "object + 2 properties");
    let root = &refs[v["uid"].as_u64().unwrap().to_string().as_str()];
    assert_eq!(root["type"], "object");
    let props = root["dict"].as_object().unwrap();
    assert_eq!(props.len(), 2);
    // property 引用是 uid 数字。
    let base_ref = props["baseURL"].as_u64().expect("reference by uid");
    let base_node = &refs[base_ref.to_string().as_str()];
    assert_eq!(base_node["type"], "string");
    assert_eq!(
        base_node["meta"]["default"],
        json!("https://api.example.com")
    );
    let key_ref = props["key"].as_u64().unwrap();
    let key_node = &refs[key_ref.to_string().as_str()];
    assert_eq!(key_node["meta"]["role"], "secret");
    assert_eq!(key_node["meta"]["description"], "write-only");
}

#[test]
fn number_meta_full_rendered() {
    let s = Schema::percent(); // number().step(0.01).min(0).max(1).role('slider')
    let v = to_json(&s);
    let refs = v["refs"].as_object().unwrap();
    let node = refs.values().next().unwrap();
    assert_eq!(node["type"], "number");
    let meta = &node["meta"];
    assert_eq!(meta["step"], json!(0.01));
    assert_eq!(meta["min"], json!(0.0));
    assert_eq!(meta["max"], json!(1.0));
    assert_eq!(meta["role"], "slider");
}

#[test]
fn pattern_renders_pair_object() {
    let s = Schema::pattern(&Schema::string(), "^[a-z]+$", "i");
    let v = to_json(&s);
    let refs = v["refs"].as_object().unwrap();
    let node = refs.values().next().unwrap();
    let pat = &node["meta"]["pattern"];
    assert_eq!(pat["source"], "^[a-z]+$");
    assert_eq!(pat["flags"], "i");
}

#[test]
fn nested_containers_trace_shared_root() {
    // array(object{name:string}, with required)
    let mut inner = HashMap::new();
    inner.insert("name".to_string(), Schema::required(&Schema::string()));
    let arr = Schema::array(Schema::object(inner));
    let v = to_json(&arr);
    let refs = v["refs"].as_object().unwrap();
    assert_eq!(refs.len(), 3, "root array + object + string");
    let root = &refs[v["uid"].as_u64().unwrap().to_string().as_str()];
    assert_eq!(root["type"], "array");
    let obj_ref = root["inner"].as_u64().unwrap();
    let obj_node = &refs[obj_ref.to_string().as_str()];
    assert_eq!(obj_node["type"], "object");
    let str_ref = obj_node["dict"]["name"].as_u64().unwrap();
    let str_node = &refs[str_ref.to_string().as_str()];
    assert_eq!(str_node["type"], "string");
    assert_eq!(str_node["meta"]["required"], true);
}

#[test]
fn union_and_tuple_use_list_arrays() {
    let u = Schema::union(vec![Schema::string(), Schema::number()]);
    let v = to_json(&u);
    let refs = v["refs"].as_object().unwrap();
    let root = &refs[v["uid"].as_u64().unwrap().to_string().as_str()];
    assert_eq!(root["type"], "union");
    assert_eq!(root["list"].as_array().unwrap().len(), 2);

    let t = Schema::tuple(vec![Schema::boolean(), Schema::const_value(json!(1))]);
    let v = to_json(&t);
    let refs = v["refs"].as_object().unwrap();
    let root = &refs[v["uid"].as_u64().unwrap().to_string().as_str()];
    assert_eq!(root["type"], "tuple");
    let list = root["list"].as_array().unwrap();
    assert_eq!(list.len(), 2);
    let const_ref = list[1].as_u64().unwrap();
    assert_eq!(refs[const_ref.to_string().as_str()]["type"], "const");
}

#[test]
fn const_serializes_value() {
    let s = Schema::const_value(json!({"a": [1, 2]}));
    let v = to_json(&s);
    let refs = v["refs"].as_object().unwrap();
    let node = refs.values().next().unwrap();
    assert_eq!(node["type"], "const");
    assert_eq!(node["value"], json!({"a": [1, 2]}));
}

#[test]
fn bitset_serializes_bits() {
    let mut bits = HashMap::new();
    bits.insert("read".to_string(), 1);
    bits.insert("write".to_string(), 2);
    let s = Schema::bitset(bits);
    let v = to_json(&s);
    let refs = v["refs"].as_object().unwrap();
    let node = refs.values().next().unwrap();
    assert_eq!(node["type"], "bitset");
    assert_eq!(node["bits"]["read"], 1);
    assert_eq!(node["bits"]["write"], 2);
}

#[test]
fn transform_follows_inner() {
    let cb: TransformFn = std::rc::Rc::new(|v| Ok(v.clone()));
    let s = Schema::transform(Schema::number(), true, cb);
    let v = to_json(&s);
    let refs = v["refs"].as_object().unwrap();
    let root = &refs[v["uid"].as_u64().unwrap().to_string().as_str()];
    assert_eq!(root["type"], "transform");
    let inner_ref = root["inner"].as_u64().unwrap();
    assert_eq!(refs[inner_ref.to_string().as_str()]["type"], "number");
}

#[test]
fn custom_type_name() {
    let s = Schema::custom("color");
    let v = to_json(&s);
    let refs = v["refs"].as_object().unwrap();
    let node = refs.values().next().unwrap();
    assert_eq!(node["type"], "color");
}

#[test]
fn required_and_extra_meta() {
    let s = Schema::extra(&Schema::string(), "ui", json!({"hint": "x"}));
    let v = to_json(&s);
    let refs = v["refs"].as_object().unwrap();
    let node = refs.values().next().unwrap();
    assert_eq!(node["meta"]["ui"], json!({"hint": "x"}));
}

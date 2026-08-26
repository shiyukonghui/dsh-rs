//! B4 config simplify 回写 unparse：插件声明 config_schema 时，**运行时配置更新**
//! （`update_with(fid, config, false)` → `internal/update` write_back）按 schemastery
//! `Schema.prototype.simplify` 语义简化存回 `e.options.config`（cordis
//! `Config['simplify'](config)`，index.ts:106-107）——内存=落盘形态一致；
//! 无 schema 插件写回原样；create 不简化（cordis `_patchContext` 的 `fiber.update(cfg, true)`
//! 带 noSave=true，write_back 跳过——同径）。
#![allow(clippy::arc_with_non_send_sync)]

mod common;
use common::*;

use std::collections::HashMap;
use std::sync::Arc;

use dsh_core::*;
use dsh_loader::*;

fn obj(fields: &[(&str, dsh_schema::SchemaRef)]) -> dsh_schema::SchemaRef {
    let mut m = HashMap::new();
    for (k, s) in fields {
        m.insert((*k).to_string(), s.clone());
    }
    dsh_schema::Schema::object(m)
}

/// B4（T1，红核心）：运行时更新先写回简化 config——schema {def:5, other:7}，
/// `update_with(fid, {def:5, other:2}, false)` → 写回 `{other:2}`（def==默认删；
/// other!=默认留）。缺 simplify 则存原样 {def:5,other:2}。
#[test]
fn runtime_update_simplifies_write_back() {
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    let schema = obj(&[
        (
            "def",
            dsh_schema::Schema::with_default(&dsh_schema::Schema::natural(), json!(5)),
        ),
        (
            "other",
            dsh_schema::Schema::with_default(&dsh_schema::Schema::natural(), json!(7)),
        ),
    ]);
    loader
        .register_plugin("sp", Arc::new(FnPlugin::noop("sp").with_config_schema(schema)));
    let mut o = EntryOptions::new("a", "sp");
    o.config = json!({"def": 5, "other": 1});
    loader.create(o).unwrap();
    // create 阶段：cordis 同径不简化（_patchContext noSave=true）——原样
    assert_eq!(
        loader.entry_options()[0].config,
        json!({"def": 5, "other": 1}),
        "create persists raw (cordis _patchContext skip)"
    );

    // 运行时更新（noSave=false）→ write_back 简化写回
    let fid = loader.fiber("a").expect("a fiber");
    cordis
        .update_with(fid, json!({"def": 5, "other": 2}), false)
        .unwrap();
    assert_eq!(
        loader.entry_options()[0].config,
        json!({"other": 2}),
        "B4: runtime update must simplify default-equal key away"
    );
}

/// B4（T2）：无 schema 插件 → 运行时更新写回 config 原样。
#[test]
fn runtime_update_raw_without_schema() {
    let cordis = Cordis::new();
    let loader = Loader::new(&cordis).unwrap();
    loader.register_plugin("np", Arc::new(FnPlugin::noop("np")));
    let mut o = EntryOptions::new("a", "np");
    o.config = json!({"k": 1});
    loader.create(o).unwrap();

    let fid = loader.fiber("a").expect("a fiber");
    cordis
        .update_with(fid, json!({"k": 2}), false)
        .unwrap();
    assert_eq!(
        loader.entry_options()[0].config,
        json!({"k": 2}),
        "no schema → config stays raw on update"
    );
}

/// B4（T3）：`dsh_schema::simplify` 逐分支语义（schemastery 对齐）。
#[test]
fn simplify_branch_semantics() {
    // 嵌套对象：子全默认 → 空对象 → == 默认 {} → Null（schemastery deepEqual(result, {})）
    let nested = obj(&[(
        "a",
        obj(&[(
            "x",
            dsh_schema::Schema::with_default(&dsh_schema::Schema::natural(), json!(5)),
        )]),
    )]);
    assert_eq!(
        dsh_schema::simplify(&nested, &json!({"a": {"x": 5}})),
        Value::Null,
        "all-default nested object collapses to Null ({{}} == default {{}})"
    );
    // object：未声明键删（None 分支 → Null → 删）；全删 → Null
    let decl = obj(&[(
        "def",
        dsh_schema::Schema::with_default(&dsh_schema::Schema::natural(), json!(5)),
    )]);
    assert_eq!(
        dsh_schema::simplify(&decl, &json!({"def": 5, "other": 1})),
        Value::Null,
        "undeclared key dropped; empty result == default {{}} → Null"
    );
    // object：有存活键 → 保（README 形态）
    let both = obj(&[
        (
            "foo",
            dsh_schema::Schema::with_default(&dsh_schema::Schema::string(), json!("")),
        ),
        (
            "bar",
            dsh_schema::Schema::with_default(&dsh_schema::Schema::natural(), json!(0)),
        ),
    ]);
    assert_eq!(
        dsh_schema::simplify(&both, &json!({"foo": "", "bar": 1})),
        json!({"bar": 1}),
        "README: default-equal foo dropped, bar kept"
    );
    // dict：inner 默认相等 → null 项**保留**（dict 分支不删）
    let dict = dsh_schema::Schema::dict(
        dsh_schema::Schema::with_default(&dsh_schema::Schema::natural(), json!(0)),
        dsh_schema::Schema::string(),
    );
    assert_eq!(
        dsh_schema::simplify(&dict, &json!({"k": 0})),
        json!({"k": null}),
        "dict keeps null-valued item"
    );
    // array：inner 默认相等 → null 项保留（逐项映射）
    let arr = dsh_schema::Schema::array(dsh_schema::Schema::with_default(
        &dsh_schema::Schema::natural(),
        json!(0),
    ));
    assert_eq!(
        dsh_schema::simplify(&arr, &json!([0, 5])),
        json!([null, 5]),
        "array maps each item"
    );
    // union：第一个可解析成员简化（5 == number 默认 → null）
    let un = dsh_schema::Schema::union(vec![
        dsh_schema::Schema::string(),
        dsh_schema::Schema::with_default(&dsh_schema::Schema::natural(), json!(5)),
    ]);
    assert_eq!(
        dsh_schema::simplify(&un, &json!(5)),
        json!(null),
        "union picks first resolvable member"
    );
    // 顶层与默认深等 → null
    let d = dsh_schema::Schema::with_default(&dsh_schema::Schema::natural(), json!(5));
    assert_eq!(dsh_schema::simplify(&d, &json!(5)), Value::Null);
    // null 透传
    assert_eq!(dsh_schema::simplify(&obj(&[]), &Value::Null), Value::Null);
    // 无默认的原始类型 → 原值
    assert_eq!(dsh_schema::simplify(&dsh_schema::Schema::string(), &json!("x")), json!("x"));
}

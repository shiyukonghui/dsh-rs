//! M66：`tools::list` 动态工具 schema —— `ToolRegistry` 记录工具 schema，
//! 支持枚举（名 + schema），供 `llm.generate` 构造工具列表 / WASM `tools::list` 缝。
//!
//! Cordis 侧 `ctx.tools`（DSH 生产 `ToolRegistry`）注册工具参数 schema 并枚举；
//! 本测试锁定 Rust `ToolRegistry` 的 schema 承载能力（阶段 A，无 WIT 依赖）。

use dsh_core::ToolRegistry;

use serde_json::json;

/// 注册时带 schema：`list()` 返回 (name, schema) 对，按名排序。
#[test]
fn list_returns_name_and_schema() {
    let mut reg = ToolRegistry::new();
    reg.register_with_schema("add", json!({"type":"object","properties":{"a":{"type":"number"},"b":{"type":"number"}}}), |a| json!({"sum": a.get("a").and_then(|v| v.as_i64()).unwrap_or(0)}));
    reg.register_with_schema("echo", json!({"type":"object"}), |a| json!({"echo": a}));

    let list = reg.list();
    // 按名排序：add, echo。
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].0, "add");
    assert_eq!(list[1].0, "echo");
    // schema 原样保留。
    assert_eq!(list[0].1, json!({"type":"object","properties":{"a":{"type":"number"},"b":{"type":"number"}}}));
    assert_eq!(list[1].1, json!({"type":"object"}));
}

/// 无 schema 的 `register` 委托：schema 记为空对象（`Null` 归一为 `{}`），
/// 仍可枚举且可执行（现有调用点零破坏）。
#[test]
fn register_without_schema_lists_and_executes() {
    let mut reg = ToolRegistry::new();
    reg.register("blink", |a| json!({"on": a.get("n").cloned().unwrap_or(json!(0))}));

    let list = reg.list();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].0, "blink");
    // 无 schema 时记为空对象（仍可列举）。
    assert_eq!(list[0].1, json!({}));

    // 执行不受影响。
    let r = reg.execute("blink", json!({"n": 7}));
    assert_eq!(r, json!({"on": 7}));
}

/// `schema(name)` 单查；未注册返回 None。
#[test]
fn schema_lookup() {
    let mut reg = ToolRegistry::new();
    reg.register_with_schema("x", json!({"type":"string"}), |_| json!(null));
    assert_eq!(reg.schema("x"), Some(json!({"type":"string"})));
    assert_eq!(reg.schema("missing"), None);
}

/// 重复注册同名：后者覆盖（名 + schema），`list()` 只有一条。
#[test]
fn re_register_replaces() {
    let mut reg = ToolRegistry::new();
    reg.register_with_schema("a", json!({"v":1}), |_| json!(1));
    reg.register_with_schema("a", json!({"v":2}), |_| json!(2));
    let list = reg.list();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].0, "a");
    assert_eq!(list[0].1, json!({"v":2}));
}

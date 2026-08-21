//! M2b-3a: dsh-tools TS SDK 代码生成测试（移植 TS ts-types.spec 的可观察行为）。
//! 覆盖 jsonSchemaToTs 映射表（标量/const/enum/oneOf/array/object/unknown）与
//! renderToolsSdk 完整输出（固定说明文本 + 声明）。

use dsh_tools::{
    json_schema_to_ts, render_tools_sdk, ToolSdkSchema, SDK_INSTRUCTIONS,
};
use serde_json::json;

fn ts(schema: serde_json::Value) -> String {
    json_schema_to_ts(&schema, 0)
}

// ---------------------------------------------------------------------------
// jsonSchemaToTs 映射表
// ---------------------------------------------------------------------------

#[test]
fn ts_scalar_types() {
    assert_eq!(ts(json!({ "type": "string" })), "string");
    assert_eq!(ts(json!({ "type": "number" })), "number");
    // integer → number（broad）
    assert_eq!(ts(json!({ "type": "integer" })), "number");
    assert_eq!(ts(json!({ "type": "boolean" })), "boolean");
    assert_eq!(ts(json!({ "type": "null" })), "null");
    // 无 type 无 oneOf → JsonValue
    assert_eq!(ts(json!({})), "JsonValue");
}

#[test]
fn ts_const_and_enum() {
    // const → 单字面量
    assert_eq!(ts(json!({ "type": "string", "const": "a" })), "\"a\"");
    assert_eq!(ts(json!({ "type": "integer", "const": 5 })), "5");
    assert_eq!(ts(json!({ "type": "boolean", "const": true })), "true");
    // enum → literal union
    assert_eq!(
        ts(json!({ "type": "string", "enum": ["a", "b"] })),
        "\"a\" | \"b\""
    );
    assert_eq!(
        ts(json!({ "type": "integer", "enum": [1, 2, 3] })),
        "1 | 2 | 3"
    );
}

#[test]
fn ts_one_of_union() {
    assert_eq!(
        ts(json!({ "oneOf": [{ "type": "string" }, { "type": "null" }] })),
        "string | null"
    );
    assert_eq!(
        ts(json!({ "oneOf": [{ "type": "integer" }, { "type": "string" }] })),
        "number | string"
    );
}

#[test]
fn ts_array_items() {
    // 无 items → JsonValue[]
    assert_eq!(ts(json!({ "type": "array" })), "JsonValue[]");
    // 简单 items
    assert_eq!(
        ts(json!({ "type": "array", "items": { "type": "string" } })),
        "string[]"
    );
    // union items → 括号
    assert_eq!(
        ts(json!({ "type": "array", "items": { "oneOf": [{ "type": "string" }, { "type": "integer" }] } })),
        "(string | number)[]"
    );
    // 嵌套数组
    assert_eq!(
        ts(json!({ "type": "array", "items": { "type": "array", "items": { "type": "integer" } } })),
        "number[][]"
    );
}

#[test]
fn ts_object_shapes() {
    // 无属性 open → Record<string, JsonValue>；closed → Record<string, never>
    assert_eq!(ts(json!({ "type": "object" })), "Record<string, JsonValue>");
    assert_eq!(
        ts(json!({ "type": "object", "additionalProperties": false })),
        "Record<string, never>"
    );
    // 有属性 → 多行对象字面量；required 无 ?，可选有 ?
    assert_eq!(
        ts(json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["a"],
            "properties": {
                "a": { "type": "string" },
                "b": { "type": "integer" },
            },
        })),
        "{\n  a: string;\n  b?: number;\n}"
    );
    // open → 尾部 & Record
    assert_eq!(
        ts(json!({
            "type": "object",
            "properties": { "a": { "type": "string" } },
        })),
        "{\n  a?: string;\n} & Record<string, JsonValue>"
    );
    // 多行缩进（嵌套 open object 追加 & Record 再分号）
    assert_eq!(
        ts(json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["x"],
            "properties": { "x": { "type": "object", "properties": { "y": { "type": "string" } } } },
        })),
        "{\n  x: {\n    y?: string;\n  } & Record<string, JsonValue>;\n}"
    );
}

#[test]
fn ts_object_doc_comment_and_key_quoting() {
    // description → JSDoc；`*/` 被转义
    assert_eq!(
        ts(json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["k"],
            "properties": { "k": { "type": "string", "description": "a */ b" } },
        })),
        "{\n  /** a *\\/ b */\n  k: string;\n}"
    );
    // 非标识符键 → JSON 引号
    assert_eq!(
        ts(json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["my-tool"],
            "properties": { "my-tool": { "type": "string" } },
        })),
        "{\n  \"my-tool\": string;\n}"
    );
}

#[test]
fn ts_unsupported_degrades_to_unknown() {
    assert_eq!(ts(json!({ "type": "bogus" })), "unknown");
    assert_eq!(ts(json!({ "minLength": 1 })), "unknown");
}

#[test]
fn ts_array_of_open_objects_parenthesized() {
    // open object 含 '&' → 父含 union/intersection → 数组项加括号
    assert_eq!(
        ts(json!({
            "type": "array",
            "items": { "type": "object", "properties": { "a": { "type": "string" } } },
        })),
        "({\n  a?: string;\n} & Record<string, JsonValue>)[]"
    );
    // closed object 数组 → 括号化（因 & 只在 open 出现；closed 无 &）
    assert_eq!(
        ts(json!({
            "type": "array",
            "items": { "type": "object", "additionalProperties": false, "properties": { "a": { "type": "string" } } },
        })),
        "{\n  a?: string;\n}[]"
    );
}

// ---------------------------------------------------------------------------
// renderToolsSdk
// ---------------------------------------------------------------------------

fn sample_schemas() -> Vec<ToolSdkSchema> {
    vec![
        ToolSdkSchema::new(
            "web_search",
            "Search the web for a query.",
            crate_node(json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["query"],
                "properties": { "query": { "type": "string" } },
            })),
            crate_node(json!({ "type": "array", "items": { "type": "string" } })),
        ),
        ToolSdkSchema::new(
            "add",
            "Add two integers.",
            crate_node(json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["a", "b"],
                "properties": {
                    "a": { "type": "integer" },
                    "b": { "type": "integer" },
                },
            })),
            crate_node(json!({ "type": "integer" })),
        ),
    ]
}

fn crate_node(v: serde_json::Value) -> dsh_tools::JsonSchemaNode {
    dsh_tools::json_schema::parse_asserted_schema(&v)
}

#[test]
fn render_tools_sdk_sorts_and_declares_each_tool() {
    let out = render_tools_sdk(&sample_schemas());
    // 固定说明在前
    assert!(out.starts_with(SDK_INSTRUCTIONS));
    assert!(out.contains("```ts\n"));
    assert!(out.contains("type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue }"));
    // 声明包含两个工具（字典序 add 在前）
    let add_idx = out.find("add: ").expect("add declared");
    let search_idx = out.find("web_search: ").expect("web_search declared");
    assert!(add_idx < search_idx, "lexicographic order");
    assert!(out.contains("interface ToolArgsMap {"));
    assert!(out.contains("interface ToolOutputMap {"));
    assert!(out.contains("type ToolName = keyof ToolOutputMap"));
    assert!(out.contains("declare class ToolCallError extends Error {"));
    assert!(out.contains("readonly toolName: ToolName;"));
    assert!(out.contains("declare const tools: {"));
    assert!(out.contains("[K in ToolName]: (args: ToolArgsMap[K]) => Promise<ToolOutputMap[K]>;"));
    assert!(out.ends_with("```\n") || out.ends_with('`'));
}

#[test]
fn render_tools_sdk_empty_set_is_empty_interfaces() {
    let out = render_tools_sdk(&[]);
    assert!(out.contains("interface ToolArgsMap {}"));
    assert!(out.contains("interface ToolOutputMap {}"));
    assert!(!out.contains("raw"));
}

#[test]
fn render_tools_sdk_anchors_byte_exact_prose() {
    let out = render_tools_sdk(&[]);
    // 关键逐字行（无尾随空白；`\`` 反引号原样）
    assert!(out.contains("- Call tools as `await tools.name(args)` — quoted access for exotic names: `tools[\"my-tool\"](args)`. Every call resolves to the tool's typed canonical JSON value. Tool arguments must be lossless JSON."));
    assert!(out.contains("- A FAILED tool call rejects with `ToolCallError`, whose `toolName` identifies the failed tool and whose `message` is human-readable — `try/catch` it to handle and continue."));
}

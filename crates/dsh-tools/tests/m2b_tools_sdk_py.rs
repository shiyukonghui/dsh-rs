//! M2b-3b: dsh-tools Python SDK 代码生成测试（移植 TS py-types.spec 的可观察行为）。
//! 覆盖 jsonSchemaToPy 映射表（str/float/int/bool/None/Literal/enum/array/oneOf/
//! object 降级/非法 Any）与 renderToolsSdkPy（命名 TypedDict、协议、imports 顺序、
//! 子脚本路径、固定说明）。

use dsh_tools::{
    json_schema_to_py, render_tools_sdk_py, ToolSdkSchema,
};
use serde_json::json;

fn py(schema: serde_json::Value) -> String {
    json_schema_to_py(&schema)
}

// ---------------------------------------------------------------------------
// jsonSchemaToPy 映射表
// ---------------------------------------------------------------------------

#[test]
fn py_scalar_types() {
    assert_eq!(py(json!({ "type": "string" })), "str");
    assert_eq!(py(json!({ "type": "number" })), "float");
    assert_eq!(py(json!({ "type": "integer" })), "int");
    assert_eq!(py(json!({ "type": "boolean" })), "bool");
    assert_eq!(py(json!({ "type": "null" })), "None");
    assert_eq!(py(json!({})), "Any");
}

#[test]
fn py_const_and_enum() {
    assert_eq!(py(json!({ "type": "string", "const": "a" })), "Literal[\"a\"]");
    assert_eq!(py(json!({ "type": "integer", "const": 5 })), "Literal[5]");
    assert_eq!(py(json!({ "type": "boolean", "const": true })), "Literal[True]");
    assert_eq!(py(json!({ "type": "boolean", "const": false })), "Literal[False]");
    assert_eq!(
        py(json!({ "type": "string", "enum": ["a", "b"] })),
        "Literal[\"a\", \"b\"]"
    );
    assert_eq!(
        py(json!({ "type": "integer", "enum": [1, 2, 3] })),
        "Literal[1, 2, 3]"
    );
}

#[test]
fn py_one_of_and_array() {
    assert_eq!(
        py(json!({ "oneOf": [{ "type": "string" }, { "type": "null" }] })),
        "str | None"
    );
    assert_eq!(
        py(json!({ "oneOf": [{ "type": "integer" }, { "type": "string" }] })),
        "int | str"
    );
    assert_eq!(py(json!({ "type": "array" })), "list[Any]");
    assert_eq!(py(json!({ "type": "array", "items": { "type": "integer" } })), "list[int]");
    assert_eq!(
        py(json!({ "type": "array", "items": { "type": "string", "enum": ["a", "b"] } })),
        "list[Literal[\"a\", \"b\"]]"
    );
    assert_eq!(
        py(json!({ "type": "array", "items": { "type": "array", "items": { "type": "integer" } } })),
        "list[list[int]]"
    );
}

#[test]
fn py_context_free_object_degrades() {
    // 上下文无关入口：对象 → dict[str, Any]（无命名上下文）
    assert_eq!(
        py(json!({
            "type": "object",
            "properties": { "query": { "type": "string" } },
        })),
        "dict[str, Any]"
    );
    assert_eq!(py(json!({ "type": "object" })), "dict[str, Any]");
}

#[test]
fn py_unsupported_degrades_to_any() {
    assert_eq!(py(json!({ "type": "bogus" })), "Any");
    assert_eq!(py(json!({ "minLength": 1 })), "Any");
}

// ---------------------------------------------------------------------------
// renderToolsSdkPy
// ---------------------------------------------------------------------------

fn sample_schemas() -> Vec<ToolSdkSchema> {
    vec![
        ToolSdkSchema::new(
            "get_weather",
            "Get the weather for a city.",
            node(json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["city"],
                "properties": {
                    "city": { "type": "string", "description": "The city name." },
                    "units": { "type": "string", "enum": ["celsius", "fahrenheit"] },
                },
            })),
            node(json!({ "type": "object", "additionalProperties": false, "properties": {} })),
        ),
        ToolSdkSchema::new(
            "sum",
            "",
            node(json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["values"],
                "properties": { "values": { "type": "array", "items": { "type": "number" } } },
            })),
            node(json!({ "type": "number" })),
        ),
    ]
}

fn node(v: serde_json::Value) -> dsh_tools::JsonSchemaNode {
    dsh_tools::json_schema::parse_asserted_schema(&v)
}

#[test]
fn render_tools_sdk_py_structure() {
    let out = render_tools_sdk_py(&sample_schemas());
    assert!(out.starts_with(dsh_tools::py_types::SDK_INSTRUCTIONS));
    assert!(out.contains("```python\n"));

    // 排序：get_weather 在 sum 前
    let gw = out.find("get_weather").expect("get_weather declared");
    let sum = out.find("sum(").expect("sum declared");
    assert!(gw < sum, "lexicographic order");

    // 命名 TypedDict 类（嵌套先于引用）
    // city 字段注释 + required 无 NotRequired；units 可选 → NotRequired
    assert!(out.contains("class GetWeatherArgs(TypedDict):"));
    assert!(out.contains("    # The city name."));
    assert!(out.contains("    city: str"));
    assert!(out.contains("    units: NotRequired[Literal[\"celsius\", \"fahrenheit\"]]"));
    // closed 空对象 → 空 TypedDict（带 pass）
    assert!(out.contains("class GetWeatherOutput(TypedDict):"));
    assert!(out.contains("    pass"));

    // 方法流：get_weather 有描述 → docstring 版；sum 无描述 → `...` 版
    assert!(out.contains("    async def get_weather(self, args: GetWeatherArgs) -> GetWeatherOutput:\n        \"\"\"Get the weather for a city.\"\"\""));
    assert!(out.contains("    async def sum(self, args: SumArgs) -> float: ..."));

    // imports 顺序按 TYPING_ORDER（Any,Literal,NotRequired,Protocol,TypedDict）；
    // 第一行是固定说明，import 行紧跟 ```python
    let fence = out.find("```python\n").unwrap();
    let declaration = &out[fence + "```python\n".len()..];
    assert_eq!(
        declaration.lines().next().unwrap(),
        "from typing import Literal, NotRequired, Protocol, TypedDict"
    );

    // 错误声明 + 协议面
    assert!(out.contains("class ToolCallError(Exception):\n    toolName: str"));
    assert!(out.contains("class Tools(Protocol):"));
    assert!(out.contains("\ntools: Tools"));
}

#[test]
fn render_tools_sdk_py_subscript_path_for_exotic_name() {
    let out = render_tools_sdk_py(&[ToolSdkSchema::new(
        "my-tool",
        "Does things.",
        node(json!({
            "type": "object",
            "additionalProperties": false,
            "properties": { "x": { "type": "integer" } },
        })),
        node(json!({ "type": "string" })),
    )]);
    // 下标注释流（非方法流）；命名 TypedDict 照常生成（camelCase 名）
    assert!(out.contains("class MyToolArgs(TypedDict):"));
    assert!(out.contains("    x: NotRequired[int]"));
    assert!(out.contains("# tools[\"my-tool\"](args: MyToolArgs) -> str"));
    assert!(out.contains("#   Does things."));
    // body 只有注释 → 需要 pass
    assert!(out.contains("class Tools(Protocol):\n    pass\n    # tools[\"my-tool\"]"));
}

#[test]
fn render_tools_sdk_py_imports_any_for_open_json_output() {
    let out = render_tools_sdk_py(&[ToolSdkSchema::new(
        "raw",
        "",
        node(json!({ "type": "object", "additionalProperties": false, "properties": {} })),
        node(json!({ "type": "json", "description": "anything" })),
    )]);
    // json 输出 → Any；args 为 closed 空对象 → 空 TypedDict
    assert!(out.contains("from typing import Any, Protocol, TypedDict"));
    assert!(out.contains("class RawArgs(TypedDict):\n    pass"));
    assert!(out.contains("async def raw(self, args: RawArgs) -> Any: ..."));
}

#[test]
fn render_tools_sdk_py_empty_set() {
    let out = render_tools_sdk_py(&[]);
    assert!(out.contains("from typing import Protocol"));
    assert!(out.contains("class Tools(Protocol):\n    pass\n\ntools: Tools"));
}

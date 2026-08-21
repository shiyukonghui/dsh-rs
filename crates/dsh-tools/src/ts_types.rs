//! TS SDK 代码生成（镜像 TS `@deepseek-ai/dsh-tools/ts-types.ts`）：把注册工具的
//! schema 投影为 `tools:sdk` 提示区里的 `declare const tools` API 文本（Code Mode 下
//! 是模型读取工具签名的唯一来源）。
//!
//! 语义对齐点：
//! - `json_schema_to_ts` total：`assertSupportedJsonSchema` 失败即返回 `"unknown"`，
//!   绝不抛。
//! - 对象键：`render_key` 仅 ASCII 裸标识符，否则 JSON 引号化（沿用 D-014：数字/串
//!   展示用 serde_json，合法 Unicode 串下等价 JS `JSON.stringify`）。
//! - `contains_union_or_intersection` 用与 TS 相同的「可组合文档」结构传递（字符串段
//!   扫描 `|`/`&`，文档段用子标志）——精确认同 TS typeDocumentFrom 的逐段判定。
//! - 排序按 name 字典序（Rust `str`s cmp ≤ JS UTF-16 lexicographic，极稀有 astral/
//!   surrogate 差异记入 D-027）。

use crate::json_schema::{parse_asserted_schema, JsonSchemaNode, JsonSchemaScalar, JsonSchemaType};
use serde_json::Value;

/// Code Mode 下模型面对的工具 schema：模型可见参数 schema + canonical 输出 schema。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSdkSchema {
    pub name: String,
    pub description: String,
    pub parameters: JsonSchemaNode,
    pub output: JsonSchemaNode,
}

impl ToolSdkSchema {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: JsonSchemaNode,
        output: JsonSchemaNode,
    ) -> Self {
        ToolSdkSchema {
            name: name.into(),
            description: description.into(),
            parameters,
            output,
        }
    }
}

/// 逐字节固定（源：deepseek-harness/packages/core/tools/src/ts-types.ts SDK_INSTRUCTIONS）。
pub const SDK_INSTRUCTIONS: &str = r#"## Writing code for run_code

`run_code` takes two required arguments: `code` — the body of an async TypeScript function (erasable syntax only — no `enum` or namespaces; type annotations are advisory, the code runs type-stripped) — and `description`, a short summary of what the program does. Inside the program:

- Call tools as `await tools.name(args)` — quoted access for exotic names: `tools["my-tool"](args)`. Every call resolves to the tool's typed canonical JSON value. Tool arguments must be lossless JSON.
- A FAILED tool call rejects with `ToolCallError`, whose `toolName` identifies the failed tool and whose `message` is human-readable — `try/catch` it to handle and continue.
- Independent read-only calls MAY overlap under `Promise.all` (safe calls run concurrently; mutating calls run alone, in submission order). Sequence dependent work with `await`.
- Emit results with `return` and/or `console.log(...)`. Only what you print or return is program output. A successful tool result containing an image is attached after the run so you can inspect it on the next step; every other intermediate result stays out of the conversation, so extract just what you need.

The available tools:"#;

// ---------------------------------------------------------------------------
// 可组合类型文档（镜像 TS TypeDocument）
// ---------------------------------------------------------------------------

#[derive(Clone)]
enum Part {
    Text(String),
    Doc(Box<TypeDoc>),
}

#[derive(Clone)]
struct TypeDoc {
    parts: Vec<Part>,
    contains_union_or_intersection: bool,
}

fn doc_from(parts: Vec<Part>) -> TypeDoc {
    let cu = parts.iter().any(|part| match part {
        Part::Text(s) => s.contains('|') || s.contains('&'),
        Part::Doc(d) => d.contains_union_or_intersection,
    });
    TypeDoc {
        parts,
        contains_union_or_intersection: cu,
    }
}

fn doc(parts: Vec<Part>) -> TypeDoc {
    doc_from(parts)
}

fn flatten(d: &TypeDoc) -> String {
    let mut out = String::new();
    for part in &d.parts {
        match part {
            Part::Text(s) => out.push_str(s),
            Part::Doc(child) => out.push_str(&flatten(child)),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 词法原语
// ---------------------------------------------------------------------------

/// `^[A-Za-z_$][A-Za-z0-9_$]*$`（ASCII 裸 TS 标识符）。
fn is_ascii_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '$' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

fn render_key(name: &str) -> String {
    if is_ascii_identifier(name) {
        name.to_string()
    } else {
        serde_json::to_string(name).unwrap_or_else(|_| "\"\"".to_string())
    }
}

fn pad(indent: usize) -> String {
    "  ".repeat(indent)
}

/// 单行 JSDoc 条目（TS `docLines`：折叠空白、转义 `*/`）。
fn doc_lines(description: Option<&str>, indent: usize) -> Vec<String> {
    match description {
        None => Vec::new(),
        Some(d) if d.trim().is_empty() => Vec::new(),
        Some(d) => {
            let collapsed: String = d.split_whitespace().collect::<Vec<_>>().join(" ");
            // JS replaceAll('*/', '*\\/')：`*/` → `*\/`
            let escaped = collapsed.replace("*/", "*\\/");
            vec![format!("{}/** {escaped} */", pad(indent))]
        }
    }
}

// ---------------------------------------------------------------------------
// 标量渲染
// ---------------------------------------------------------------------------

/// JS `JSON.stringify(scalar)` 等价展示。数字用 serde_json Number 字面展示；合法
/// Unicode 串与 JS 无差异；超大整数（serde_json 可保精确 u64/i64）会比 JS 更精确，
/// 记 D-027。
fn render_scalar(s: &JsonSchemaScalar) -> String {
    match s {
        JsonSchemaScalar::Str(t) => serde_json::to_string(t).unwrap_or_else(|_| "\"\"".to_string()),
        JsonSchemaScalar::Num(n) => n.to_string(),
        JsonSchemaScalar::Bool(b) => {
            if *b {
                "true".to_string()
            } else {
                "false".to_string()
            }
        }
        JsonSchemaScalar::Null => "null".to_string(),
    }
}

/// 标量 `const`/`enum` 渲染（枚举 `a | b`），否则返回 broad 类型。
fn render_constrained_scalar(node: &JsonSchemaNode, r#type: &str) -> String {
    let broad = if r#type == "integer" { "number" } else { r#type };
    if let Some(v) = &node.const_ {
        return render_scalar(v);
    }
    if let Some(enums) = &node.r#enum {
        return enums.iter().map(render_scalar).collect::<Vec<_>>().join(" | ");
    }
    broad.to_string()
}

// ---------------------------------------------------------------------------
// schema → TS 渲染
// ---------------------------------------------------------------------------

/// 渲染一个 raw schema 为 TS 类型文本（total：断言/解析失败即 `"unknown"`，绝不抛）。
pub fn json_schema_to_ts(schema: &Value, indent: usize) -> String {
    if crate::json_schema::assert_supported_json_schema(schema).is_err() {
        return "unknown".to_string();
    }
    let node = parse_asserted_schema(schema);
    flatten(&render_supported(&node, indent))
}

/// 渲染一个已验证的 node 为文档。
fn render_supported(node: &JsonSchemaNode, indent: usize) -> TypeDoc {
    if let Some(branches) = &node.one_of {
        let mut parts: Vec<Part> = Vec::new();
        for (i, branch) in branches.iter().enumerate() {
            if i > 0 {
                parts.push(Part::Text(" | ".to_string()));
            }
            parts.push(Part::Doc(Box::new(render_supported(branch, indent))));
        }
        return doc_from(parts);
    }
    match node.r#type {
        None => doc(vec![Part::Text("JsonValue".to_string())]),
        Some(t) => match t {
            JsonSchemaType::Object => render_object(node, indent),
            JsonSchemaType::Array => render_array(node),
            JsonSchemaType::String
            | JsonSchemaType::Number
            | JsonSchemaType::Integer
            | JsonSchemaType::Boolean
            | JsonSchemaType::Null => {
                let s = render_constrained_scalar(node, t.as_str());
                doc(vec![Part::Text(s)])
            }
        },
    }
}

fn render_array(node: &JsonSchemaNode) -> TypeDoc {
    let items = match &node.items {
        None => return doc(vec![Part::Text("JsonValue[]".to_string())]),
        Some(b) => b,
    };
    let child = render_supported(items, 0);
    if child.contains_union_or_intersection {
        doc(vec![
            Part::Text("(".to_string()),
            Part::Doc(Box::new(child)),
            Part::Text(")[]".to_string()),
        ])
    } else {
        doc(vec![Part::Doc(Box::new(child)), Part::Text("[]".to_string())])
    }
}

fn render_object(node: &JsonSchemaNode, indent: usize) -> TypeDoc {
    let open = node.additional_properties != Some(false);
    let entries: Vec<(&String, &JsonSchemaNode)> = node
        .properties
        .as_ref()
        .map(|m| m.iter().collect())
        .unwrap_or_default();
    if entries.is_empty() {
        return if open {
            doc(vec![Part::Text("Record<string, JsonValue>".to_string())])
        } else {
            doc(vec![Part::Text("Record<string, never>".to_string())])
        };
    }
    let required: std::collections::BTreeSet<&String> =
        node.required.iter().flatten().collect();
    let mut parts: Vec<Part> = vec![Part::Text("{".to_string())];
    for (name, prop) in &entries {
        for line in doc_lines(prop.description.as_deref(), indent + 1) {
            parts.push(Part::Text("\n".to_string()));
            parts.push(Part::Text(line));
        }
        let opt = if required.contains(name) { "" } else { "?" };
        parts.push(Part::Text(format!(
            "\n{}{}{opt}: ",
            pad(indent + 1),
            render_key(name)
        )));
        parts.push(Part::Doc(Box::new(render_supported(prop, indent + 1))));
        parts.push(Part::Text(";".to_string()));
    }
    parts.push(Part::Text(format!("\n{}}}", pad(indent))));
    let declared = doc_from(parts);
    if open {
        doc(vec![
            Part::Doc(Box::new(declared)),
            Part::Text(" & Record<string, JsonValue>".to_string()),
        ])
    } else {
        declared
    }
}

// ---------------------------------------------------------------------------
// renderToolsSdk
// ---------------------------------------------------------------------------

/// 渲染完整 `tools:sdk` 提示区：固定使用说明 + 每个工具一份声明接口。
/// 工具按 name 字典序稳定排序；`run_code` 由调用方排除。
pub fn render_tools_sdk(schemas: &[ToolSdkSchema]) -> String {
    let mut sorted: Vec<&ToolSdkSchema> = schemas.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));

    let mut args_members: Vec<String> = Vec::new();
    let mut output_members: Vec<String> = Vec::new();
    for schema in &sorted {
        args_members.extend(doc_lines(Some(&schema.description), 1));
        let params = json_schema_to_ts(&schema.parameters.to_json(), 1);
        let output = json_schema_to_ts(&schema.output.to_json(), 1);
        args_members.push(format!("{}{}: {params};", pad(1), render_key(&schema.name)));
        output_members.push(format!("{}{}: {output};", pad(1), render_key(&schema.name)));
    }

    let args_map = if args_members.is_empty() {
        "interface ToolArgsMap {}".to_string()
    } else {
        format!("interface ToolArgsMap {{\n{}\n}}", args_members.join("\n"))
    };
    let output_map = if output_members.is_empty() {
        "interface ToolOutputMap {}".to_string()
    } else {
        format!("interface ToolOutputMap {{\n{}\n}}", output_members.join("\n"))
    };
    let declaration = [
        args_map,
        output_map,
        "type ToolName = keyof ToolOutputMap".to_string(),
        [
            "declare class ToolCallError extends Error {",
            "  readonly name: \"ToolCallError\";",
            "  readonly toolName: ToolName;",
            "}",
        ]
        .join("\n"),
        [
            "declare const tools: {",
            "  [K in ToolName]: (args: ToolArgsMap[K]) => Promise<ToolOutputMap[K]>;",
            "}",
        ]
        .join("\n"),
    ]
    .join("\n\n");
    let json_value =
        "type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue }";
    format!("{SDK_INSTRUCTIONS}\n\n```ts\n{json_value}\n\n{declaration}\n```")
}

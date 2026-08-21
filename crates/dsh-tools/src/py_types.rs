//! Python SDK 代码生成（镜像 TS `@deepseek-ai/dsh-tools/py-types.ts`）：把注册工具
//! schema 投影为 Python 味的 `tools:sdk` 提示区（一个 `Tools(Protocol)` + 每工具
//! 的命名 `TypedDict`）。
//!
//! Unicode 面用 `unicode-ident`（XID_Start/XID_Continue 表）+ `unicode-normalization`
//!（NFKC）实现 `is_bare_identifier`/`camel_case`。**已知偏斜**（D-026/D-027）：
//! TS/CPython 各自的 Unicode 表版本与 Rust 捆绑表不同——astral/近期赋码位在
//! `camelCase` 头大写映射、UTF-16 码元计数的类名上限处可能与本机 CPython 行为有差；
//! 本实现保证「对 ASCCI/通用 BMP 输入与 TS 逐字节一致」，并把涉及不同表版本的边角
//! 显式记录而非静默猜对。字段渲染顺序为 BTreeMap 字典序（D-014 规范序），不等同 JS
//! 插入序（仅影响 Python 类字段排版，不影响可解析性）。

use crate::json_schema::{parse_asserted_schema, JsonSchemaNode, JsonSchemaScalar, JsonSchemaType};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use unicode_ident::{is_xid_continue, is_xid_start};
use unicode_normalization::UnicodeNormalization;

/// 逐字节固定（源：deepseek-harness/packages/core/tools/src/py-types.ts SDK_INSTRUCTIONS）。
pub const SDK_INSTRUCTIONS: &str = r#"## Writing code for run_code

`run_code` takes two required arguments: `code` — the body of an async Python function (top-level `await` and `return` both work) — and `description`, a short summary of what the program does. At run time exactly two of the names declared below are bound: `tools` and `ToolCallError`. Everything else is a STATIC STUB describing argument and return types — in particular the `TypedDict` classes do NOT exist at run time, so build arguments as plain `dict`/`list` JSON values: `await tools.name({"field": 1})`, never `FooArgs(field=1)`, which raises `NameError`. Inside the program:

- Call tools as `await tools.name(args)` — subscript access for exotic, reserved, or underscore-leading names: `await tools["my-tool"](args)`. Every call resolves to the tool's typed canonical JSON value (each method's return type below). Tool arguments must be lossless JSON.
- A FAILED tool call raises `ToolCallError`, whose `toolName` identifies the failed tool and whose message is human-readable — wrap in `try/except` to handle and continue.
- Independent read-only calls MAY overlap under `asyncio.gather` (safe calls run concurrently; mutating calls run alone, in submission order). Sequence dependent work with `await`.
- Emit the run's answer with `print(...)` and/or a top-level `return <value>`; the returned value must be lossless JSON. Only what you print and return is program output. A successful tool result containing an image is attached after the run so you can inspect it on the next step; every other intermediate result stays out of the conversation, so extract just what you need.

The available tools:"#;

/// Code Mode 下模型面对的工具 schema（TS `ToolSdkSchema` 的 Python 侧同型）。
pub use crate::ts_types::ToolSdkSchema;

// ---------------------------------------------------------------------------
// 词法/常量
// ---------------------------------------------------------------------------

/// Python 硬保留词 + `__debug__`（TS `RESERVED`；软关键字刻意缺席）。
const RESERVED: [&str; 34] = [
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class",
    "continue", "def", "del", "elif", "else", "except", "finally", "for", "from", "global",
    "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise",
    "return", "try", "while", "with",
];

const __RESERVED_TAIL: &str = "__debug__";

/// `typing` 符号确定性导入序。
const TYPING_ORDER: [&str; 5] = ["Any", "Literal", "NotRequired", "Protocol", "TypedDict"];

/// 深处 `list[…]` 嵌套上限（TS `MAX_LIST_NESTING`：CPython 200 括号上限留余量）。
const MAX_LIST_NESTING: usize = 180;

/// 类名基上限（TS `MAX_CLASS_NAME_BASE`，UTF-16 码元计）。
const MAX_CLASS_NAME_BASE: usize = 120;

fn pad(indent: usize) -> String {
    "    ".repeat(indent)
}

fn nfkc(s: &str) -> String {
    s.nfkc().collect()
}

/// `^[\p{XID_Start}_]\p{XID_Continue}*$` + NFKC 稳定（TS `isBareIdentifier`）。
fn is_bare_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if is_xid_start(c) || c == '_' => {}
        _ => return false,
    }
    if !chars.all(is_xid_continue) {
        return false;
    }
    let normalized = nfkc(name);
    name == normalized
}

/// 折叠空白 + 转义 C0/C1/DEL + trim（TS `describe`；lone surrogate 在合法 UTF-8 里
/// 不存在，省略 `\uNNNN` 分支）。
fn describe_text(description: Option<&str>) -> Option<String> {
    let description = description?;
    let collapsed: String = description.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::with_capacity(collapsed.len());
    for c in collapsed.chars() {
        let cp = c as u32;
        if (cp <= 0x08) || (0x0e..=0x1f).contains(&cp) || (0x7f..=0x9f).contains(&cp) {
            out.push_str(&format!("\\x{cp:02x}"));
        } else {
            out.push(c);
        }
    }
    let trimmed = out.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// 单行 docstring（TS `docLines`：描述 → `"""…"""`，反斜杠/引号转义）。
fn doc_lines(description: Option<&str>, indent: usize) -> Vec<String> {
    match describe_text(description) {
        None => Vec::new(),
        Some(c) => {
            let escaped = c.replace('\\', "\\\\").replace('"', "\\\"");
            vec![format!("{}\"\"\"{escaped}\"\"\"", pad(indent))]
        }
    }
}

/// 渲染一个标量（TS `pyScalar`）：True/False、JSON 字符串、整数（超安全范围精确
/// 十进制）、否则 f64 字面。
fn py_scalar(s: &JsonSchemaScalar) -> String {
    match s {
        JsonSchemaScalar::Str(t) => serde_json::to_string(t).unwrap_or_else(|_| "\"\"".to_string()),
        JsonSchemaScalar::Num(n) => {
            const SAFE: i64 = 9007199254740991; // 2^53 - 1
            if let Some(i) = n.as_i64() {
                if !(-SAFE..=SAFE).contains(&i) {
                    return i.to_string();
                }
            }
            if let Some(u) = n.as_u64() {
                if u > SAFE as u64 {
                    return u.to_string();
                }
            }
            n.to_string()
        }
        JsonSchemaScalar::Bool(b) => {
            if *b {
                "True".to_string()
            } else {
                "False".to_string()
            }
        }
        JsonSchemaScalar::Null => "None".to_string(),
    }
}

/// 标量 `const`/`enum` → `Literal[...]`，否则 broad 类型。
fn render_constrained_scalar(node: &JsonSchemaNode, broad: &str, state: &mut RenderState) -> String {
    if let Some(v) = &node.const_ {
        state.typing.insert("Literal");
        return format!("Literal[{}]", py_scalar(v));
    }
    if let Some(enums) = &node.r#enum {
        state.typing.insert("Literal");
        return format!(
            "Literal[{}]",
            enums.iter().map(py_scalar).collect::<Vec<_>>().join(", ")
        );
    }
    broad.to_string()
}

/// 类名基上限（JS `slice(0,120)` 计数 UTF-16 码元；跨界 astral 整体丢弃以对齐
/// 上游的 high-surrogate 正则回退）。
fn cap_class_name_base(base: &str) -> String {
    let mut units = 0usize;
    let mut chars_out: Vec<char> = Vec::new();
    for c in base.chars() {
        if units >= MAX_CLASS_NAME_BASE {
            break;
        }
        let w = c.len_utf16();
        if units + w > MAX_CLASS_NAME_BASE {
            break; // 跨界：JS 会把高低代理对整体丢弃
        }
        units += w;
        chars_out.push(c);
    }
    chars_out.into_iter().collect()
}

/// 碰撞计数后缀分配（TS `allocateClassName`）。
fn allocate_class_name(base: &str, state: &mut RenderState) -> String {
    let capped = cap_class_name_base(base);
    let name = if state.used_class_names.contains(&capped) {
        let mut n = *state.next_class_counter.get(&capped).unwrap_or(&2);
        while state.used_class_names.contains(&format!("{capped}{n}")) {
            n += 1;
        }
        state.next_class_counter.insert(capped.clone(), n + 1);
        format!("{capped}{n}")
    } else {
        capped
    };
    state.used_class_names.insert(name.clone());
    name
}

/// 追加子段（TS `childClassName`：拼接 NFKC 后封顶）。
fn child_class_name(base: &str, segment: &str) -> String {
    cap_class_name_base(&nfkc(&format!("{base}{segment}")))
}

/// CamelCase（TS `camelCase`：XID_Continue 非 `_` 段、头大写、NFKC、非标识符头加
/// `Tool` 前缀、再次 NFKC）。
fn camel_case(raw: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut current = String::new();
    for c in raw.chars() {
        if is_xid_continue(c) && c != '_' {
            current.push(c);
        } else {
            if !current.is_empty() {
                parts.push(std::mem::take(&mut current));
            }
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    let joined: String = parts
        .iter()
        .map(|p| {
            let mut it = p.chars();
            let head = it.next().expect("non-empty part");
            let head_up: String = head.to_uppercase().collect();
            format!("{head_up}{}", it.as_str())
        })
        .collect::<String>();
    let joined = nfkc(&joined);
    let starts_ok = joined.chars().next().map(is_xid_start).unwrap_or(false);
    let result = if starts_ok { joined } else { format!("Tool{joined}") };
    nfkc(&result)
}

// ---------------------------------------------------------------------------
// 渲染状态
// ---------------------------------------------------------------------------

#[derive(Default)]
struct RenderState {
    classes: Vec<String>,
    used_class_names: BTreeSet<String>,
    next_class_counter: BTreeMap<String, usize>,
    typing: BTreeSet<&'static str>,
}

// ---------------------------------------------------------------------------
// renderType
// ---------------------------------------------------------------------------

/// 渲染一个已验证 node 为 Python 类型表达式（收集 TypedDict 声明与 typing 符号）。
fn render_type(
    node: &JsonSchemaNode,
    class_name: &str,
    state: &mut RenderState,
    list_depth: usize,
) -> String {
    if let Some(one_of) = &node.one_of {
        let mut union = String::new();
        for (i, branch) in one_of.iter().enumerate() {
            let child = child_class_name(class_name, &(i + 1).to_string());
            let t = render_type(branch, &child, state, list_depth);
            union = if i == 0 { t } else { format!("{union} | {t}") };
        }
        return union;
    }
    match node.r#type {
        None => {
            state.typing.insert("Any");
            "Any".to_string()
        }
        Some(JsonSchemaType::String) => render_constrained_scalar(node, "str", state),
        Some(JsonSchemaType::Number) => render_constrained_scalar(node, "float", state),
        Some(JsonSchemaType::Integer) => render_constrained_scalar(node, "int", state),
        Some(JsonSchemaType::Boolean) => render_constrained_scalar(node, "bool", state),
        Some(JsonSchemaType::Null) => "None".to_string(),
        Some(JsonSchemaType::Array) => {
            let Some(items) = &node.items else {
                state.typing.insert("Any");
                return "list[Any]".to_string();
            };
            if list_depth >= MAX_LIST_NESTING {
                state.typing.insert("Any");
                return "Any".to_string();
            }
            format!(
                "list[{}]",
                render_type(items, class_name, state, list_depth + 1)
            )
        }
        Some(JsonSchemaType::Object) => {
            let entries: Vec<(&String, &JsonSchemaNode)> = node
                .properties
                .as_ref()
                .map(|m| m.iter().collect())
                .unwrap_or_default();
            let valid_name = |name: &str| {
                is_bare_identifier(name)
                    && !is_reserved(name)
                    && (!name.starts_with("__") || name.ends_with("__"))
            };
            if class_name.is_empty() || !entries.iter().all(|(n, _)| valid_name(n)) {
                state.typing.insert("Any");
                return "dict[str, Any]".to_string();
            }
            let open = node.additional_properties != Some(false);
            if entries.is_empty() && open {
                state.typing.insert("Any");
                return "dict[str, Any]".to_string();
            }
            let allocated = allocate_class_name(class_name, state);
            state.typing.insert("TypedDict");
            let required: BTreeSet<&String> = node.required.iter().flatten().collect();
            let mut lines = vec![format!("class {allocated}(TypedDict):")];
            for (field, field_schema) in &entries {
                if let Some(desc) = describe_text(field_schema.description.as_deref()) {
                    lines.push(format!("{}# {desc}", pad(1)));
                }
                let field_type = render_type(
                    field_schema,
                    &child_class_name(&allocated, &camel_case(field)),
                    state,
                    1,
                );
                if required.contains(field) {
                    lines.push(format!("{}{field}: {field_type}", pad(1)));
                } else {
                    state.typing.insert("NotRequired");
                    lines.push(format!("{}{field}: NotRequired[{field_type}]", pad(1)));
                }
            }
            if open {
                lines.push(format!("{}# Additional keys beyond those declared are allowed.", pad(1)));
            }
            if lines.len() == 1 {
                lines.push(format!("{}pass", pad(1)));
            }
            state.classes.push(lines.join("\n"));
            allocated
        }
    }
}

fn is_reserved(name: &str) -> bool {
    RESERVED.contains(&name) || name == __RESERVED_TAIL
}

// ---------------------------------------------------------------------------
// 公开入口
// ---------------------------------------------------------------------------

/// 上下文无关映射（total）：对象含属性 → `dict[str, Any]`（无命名上下文）；非法 → `Any`。
pub fn json_schema_to_py(schema: &Value) -> String {
    if crate::json_schema::assert_supported_json_schema(schema).is_err() {
        return "Any".to_string();
    }
    let node = parse_asserted_schema(schema);
    let mut state = RenderState::default();
    render_type(&node, "", &mut state, 0)
}

/// 渲染完整 Python 味 `tools:sdk` 提示区（字典序 + 命名 TypedDict + Tools 协议）。
pub fn render_tools_sdk_py(schemas: &[ToolSdkSchema]) -> String {
    let mut sorted: Vec<&ToolSdkSchema> = schemas.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    let mut state = RenderState::default();
    state.typing.insert("Protocol");

    let mut members: Vec<String> = Vec::new();
    let mut statements = 0usize;
    for schema in &sorted {
        let cc = camel_case(&schema.name);
        let arg_type = render_type(&schema.parameters, &format!("{cc}Args"), &mut state, 0);
        let output_type = render_type(&schema.output, &format!("{cc}Output"), &mut state, 0);
        if is_bare_identifier(&schema.name)
            && !is_reserved(&schema.name)
            && !schema.name.starts_with('_')
        {
            let doc = doc_lines(Some(&schema.description), 2);
            if doc.is_empty() {
                members.push(format!(
                    "{}async def {}(self, args: {arg_type}) -> {output_type}: ...",
                    pad(1),
                    schema.name
                ));
            } else {
                members.push(format!(
                    "{}async def {}(self, args: {arg_type}) -> {output_type}:",
                    pad(1),
                    schema.name
                ));
                members.extend(doc);
            }
            statements += 1;
        } else {
            let quoted = serde_json::to_string(&schema.name).unwrap_or_else(|_| "\"\"".to_string());
            members.push(format!(
                "{}# tools[{quoted}](args: {arg_type}) -> {output_type}",
                pad(1)
            ));
            if let Some(desc) = describe_text(Some(&schema.description)) {
                members.push(format!("{}#   {desc}", pad(1)));
            }
        }
    }
    let body_lines = if statements > 0 {
        members
    } else {
        let mut v = vec![format!("{}pass", pad(1))];
        v.extend(members);
        v
    };
    let body = body_lines.join("\n");
    let imports = TYPING_ORDER
        .iter()
        .filter(|s| state.typing.contains(*s))
        .copied()
        .collect::<Vec<_>>()
        .join(", ");
    let class_block = if state.classes.is_empty() {
        String::new()
    } else {
        format!("{}\n\n", state.classes.join("\n\n"))
    };
    let error_declaration = "class ToolCallError(Exception):\n    toolName: str";
    let declaration = format!(
        "from typing import {imports}\n\n{error_declaration}\n\n{class_block}class Tools(Protocol):\n{body}\n\ntools: Tools"
    );
    format!("{SDK_INSTRUCTIONS}\n\n```python\n{declaration}\n```")
}

//! 强制 JSON Schema 子集（镜像 TS `@deepseek-ai/dsh-tools/json-schema.ts`）。
//!
//! 子集接受任意 JSON 根、annotation-only schema（无约束 JSON）、单一标量 `type`、
//! object `properties`/`required`/布尔 `additionalProperties`、array `items`、
//! type-correct 标量 `enum`/`const`、以及 exact-one `oneOf`。不支持或放错位置的
//! 关键词一律拒绝；需 object 根的消费者用 [`assert_object_json_schema`]。

use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// 标量 JSON 值（`enum`/`const` 可用）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(untagged)]
pub enum JsonSchemaScalar {
    Str(String),
    /// 用 serde_json::Number 保真数字字面表示；相等按 f64 语义（对齐 JS `===`）。
    Num(serde_json::Number),
    Bool(bool),
    Null,
}

impl JsonSchemaScalar {
    fn as_value(&self) -> Value {
        match self {
            JsonSchemaScalar::Str(s) => Value::String(s.clone()),
            JsonSchemaScalar::Num(n) => Value::Number(n.clone()),
            JsonSchemaScalar::Bool(b) => Value::Bool(*b),
            JsonSchemaScalar::Null => Value::Null,
        }
    }

    /// JS `JSON.stringify` 语义展示（字符串带引号，数字/布尔/null 原样）。
    fn num_display(&self) -> String {
        serde_json::to_string(&self.as_value()).unwrap_or_default()
    }
}

impl PartialEq for JsonSchemaScalar {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (JsonSchemaScalar::Num(a), JsonSchemaScalar::Num(b)) => {
                a.as_f64() == b.as_f64()
            }
            (JsonSchemaScalar::Str(a), JsonSchemaScalar::Str(b)) => a == b,
            (JsonSchemaScalar::Bool(a), JsonSchemaScalar::Bool(b)) => a == b,
            (JsonSchemaScalar::Null, JsonSchemaScalar::Null) => true,
            _ => false,
        }
    }
}

/// 子集接受的单一 type 关键词。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JsonSchemaType {
    Object,
    Array,
    String,
    Number,
    Integer,
    Boolean,
    Null,
}

impl JsonSchemaType {
    pub const ALL: [JsonSchemaType; 7] = [
        JsonSchemaType::Object,
        JsonSchemaType::Array,
        JsonSchemaType::String,
        JsonSchemaType::Number,
        JsonSchemaType::Integer,
        JsonSchemaType::Boolean,
        JsonSchemaType::Null,
    ];

    pub fn parse(s: &str) -> Option<JsonSchemaType> {
        Some(match s {
            "object" => JsonSchemaType::Object,
            "array" => JsonSchemaType::Array,
            "string" => JsonSchemaType::String,
            "number" => JsonSchemaType::Number,
            "integer" => JsonSchemaType::Integer,
            "boolean" => JsonSchemaType::Boolean,
            "null" => JsonSchemaType::Null,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            JsonSchemaType::Object => "object",
            JsonSchemaType::Array => "array",
            JsonSchemaType::String => "string",
            JsonSchemaType::Number => "number",
            JsonSchemaType::Integer => "integer",
            JsonSchemaType::Boolean => "boolean",
            JsonSchemaType::Null => "null",
        }
    }
}

/// 单位标量型 wrapper（enum/const 的类型域）。
#[derive(Clone, Copy)]
struct JsonSchemaScalarType(JsonSchemaType);

/// 强制子集里一个 JSON Schema 节点。
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonSchemaNode {
    /// 缺失 = 无约束（任意 JSON），或配 `one_of`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#type: Option<JsonSchemaType>,
    /// exact-one 联合，至少 2 分支。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub one_of: Option<Vec<JsonSchemaNode>>,
    /// 嵌套属性 schema（仅 object）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<BTreeMap<String, JsonSchemaNode>>,
    /// 必填属性名；每个必须出现在 properties。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<Vec<String>>,
    /// false=拒绝未声明键；缺省/true=开放。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub additional_properties: Option<bool>,
    /// 元素 schema（仅 array）；缺省=任意项。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Box<JsonSchemaNode>>,
    /// 标量允许值。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#enum: Option<Vec<JsonSchemaScalar>>,
    /// 标量唯一允许值。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub const_: Option<JsonSchemaScalar>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub examples: Option<Value>,
}

impl JsonSchemaNode {
    /// 转换回规范 JSON（键序为字段序 + BTreeMap 字典序；对齐 D-014 canonical）。
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).expect("JsonSchemaNode serializes to JSON")
    }
}

/// object 根约束的 schema。
pub type ObjectJsonSchema = JsonSchemaNode;

/// 强制子集外 schema 抛出的错误（`HarnessError`-style：message + code + violations）。
#[derive(Debug, Clone, PartialEq)]
pub struct JsonSchemaError {
    pub message: String,
    pub violations: Vec<String>,
}

impl JsonSchemaError {
    pub const CODE: &'static str = "UNSUPPORTED_SCHEMA";

    pub fn new(violations: Vec<String>) -> Self {
        JsonSchemaError {
            message: format!("unsupported JSON schema: {}", violations.join("; ")),
            violations,
        }
    }
}

impl std::fmt::Display for JsonSchemaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", Self::CODE, self.message)
    }
}

impl std::error::Error for JsonSchemaError {}

/// 约束关键词名（放错位置即拒绝）。
const CONSTRAINT_KEYWORDS: [&str; 8] = [
    "type",
    "oneOf",
    "properties",
    "required",
    "additionalProperties",
    "items",
    "enum",
    "const",
];

/// 标注关键词名（不参与校验，但须 lossless JSON）。
const ANNOTATION_KEYWORDS: [&str; 4] = ["description", "title", "default", "examples"];

/// `oneOf` 旁禁止的兄弟关键词。
const ONE_OF_SIBLING_KEYWORDS: [&str; 6] = [
    "properties",
    "required",
    "additionalProperties",
    "items",
    "enum",
    "const",
];

const SCHEMA_TYPES_JOIN: &str = "object/array/string/number/integer/boolean/null";

/// 值是否为 lossless JSON（Rust 内 serde_json::Value 恒真；占位对齐 TS 边界）。
fn is_json_value_lossless(_v: &Value) -> bool {
    true
}

/// 数字是否 finite 且非 -0。
fn is_finite_json_number(n: &serde_json::Number) -> bool {
    match n.as_f64() {
        Some(f) => f.is_finite(),
        None => true, // i64/u64 整型恒 finite
    }
}

fn is_negative_zero(n: &serde_json::Number) -> bool {
    n.as_f64() == Some(-0.0)
}

/// 单位数 candidate 是否匹配一个声明标量型（ffinity TS `scalarMatches`）。
fn scalar_matches(ty: JsonSchemaScalarType, value: &Value) -> bool {
    match ty.0 {
        JsonSchemaType::String => matches!(value, Value::String(_)),
        JsonSchemaType::Number => match value {
            Value::Number(n) => is_finite_json_number(n) && !is_negative_zero(n),
            _ => false,
        },
        JsonSchemaType::Integer => match value {
            Value::Number(n) => {
                if n.is_i64() || n.is_u64() {
                    true
                } else {
                    n.as_f64().map(|f| f.is_finite() && f.fract() == 0.0).unwrap_or(false)
                }
            }
            _ => false,
        },
        JsonSchemaType::Boolean => matches!(value, Value::Bool(_)),
        JsonSchemaType::Null => value.is_null(),
        _ => false,
    }
}

/// 候选 value 与 unit 标量是否 JS `===` 相等。
fn scalar_eq(value: &Value, scalar: &JsonSchemaScalar) -> bool {
    match (value, scalar) {
        (Value::String(a), JsonSchemaScalar::Str(b)) => a == b,
        (Value::Number(a), JsonSchemaScalar::Num(b)) => a.as_f64() == b.as_f64(),
        (Value::Bool(a), JsonSchemaScalar::Bool(b)) => a == b,
        (Value::Null, JsonSchemaScalar::Null) => true,
        _ => false,
    }
}

/// 两个 JSON 值 JS 语义相等（数字按 f64；字符串/布尔/null 按值）。用于 schema
/// 里 `const ∈ enum` 的判定（两者都是 Value）。
fn value_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        _ => a == b,
    }
}

/// 转换 Value 到字典的 plain record 视图（非 object 返回 None）。
fn as_object(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object()
}

/// —— 强制子集断言（checkSchemaNode 的递归直译）——
///
/// TS 用显式栈避免深 schema 爆栈；schema 经断言后无环（seen 集），Rust 递归深度
/// 受 schema 深度约束，安全。
fn check_schema_node(
    root: &Value,
    path: &str,
    violations: &mut Vec<String>,
    seen: &mut Vec<*const Value>,
) {
    let ptr = root as *const Value;
    let is_record = root.is_object();
    if !is_record {
        violations.push(format!("{path} must be a schema object"));
        return;
    }
    if seen.contains(&ptr) {
        violations.push(format!("{path} is circular"));
        return;
    }
    seen.push(ptr);
    let obj = root.as_object().expect("is_object checked");

    for key in obj.keys() {
        let v = &obj[key];
        if CONSTRAINT_KEYWORDS.contains(&key.as_str()) {
            continue;
        }
        if ANNOTATION_KEYWORDS.contains(&key.as_str()) {
            if !is_json_value_lossless(v) {
                violations.push(format!("{path}.{key} annotation must be lossless JSON data"));
            }
            continue;
        }
        violations.push(format!(
            "{path}.{key} is not a supported keyword (subset: type/oneOf/properties/required/additionalProperties/items/enum/const + annotations)"
        ));
    }
    if let Some(v) = obj.get("description") {
        if !v.is_string() {
            violations.push(format!("{path}.description must be a string"));
        }
    }
    if let Some(v) = obj.get("title") {
        if !v.is_string() {
            violations.push(format!("{path}.title must be a string"));
        }
    }

    let has_type = obj.contains_key("type");
    let has_one_of = obj.contains_key("oneOf");
    if has_type && has_one_of {
        violations.push(format!("{path} cannot declare both type and oneOf"));
        seen.pop();
        return;
    }
    if !has_type && !has_one_of {
        for key in ONE_OF_SIBLING_KEYWORDS {
            if obj.contains_key(key) {
                violations.push(format!("{path}.{key} requires type or oneOf"));
            }
        }
        seen.pop();
        return;
    }

    if has_one_of {
        for key in ONE_OF_SIBLING_KEYWORDS {
            if obj.contains_key(key) {
                violations.push(format!("{path}.{key} is not supported beside oneOf"));
            }
        }
        let one_of = &obj["oneOf"];
        if !one_of.is_array() || one_of.as_array().is_some_and(|a| a.len() < 2) {
            violations.push(format!(
                "{path}.oneOf must be an array of at least two schemas"
            ));
        } else {
            let arr = one_of.as_array().expect("array checked");
            for (i, branch) in arr.iter().enumerate() {
                check_schema_node(branch, &format!("{path}.oneOf[{i}]"), violations, seen);
            }
        }
        seen.pop();
        return;
    }

    let type_value = &obj["type"];
    let schema_type = match JsonSchemaType::parse(type_value.as_str().unwrap_or_default()) {
        Some(t) => t,
        None => {
            if type_value.is_array() {
                violations.push(format!(
                    "{path}.type must be a single type string (type arrays are not supported)"
                ));
            } else {
                violations.push(format!("{path}.type must be one of {SCHEMA_TYPES_JOIN}"));
            }
            seen.pop();
            return;
        }
    };

    let allowed_for: [(&str, &[JsonSchemaType]); 6] = [
        ("properties", &[JsonSchemaType::Object]),
        ("required", &[JsonSchemaType::Object]),
        ("additionalProperties", &[JsonSchemaType::Object]),
        ("items", &[JsonSchemaType::Array]),
        (
            "enum",
            &[
                JsonSchemaType::String,
                JsonSchemaType::Number,
                JsonSchemaType::Integer,
                JsonSchemaType::Boolean,
                JsonSchemaType::Null,
            ],
        ),
        (
            "const",
            &[
                JsonSchemaType::String,
                JsonSchemaType::Number,
                JsonSchemaType::Integer,
                JsonSchemaType::Boolean,
                JsonSchemaType::Null,
            ],
        ),
    ];
    for (key, types) in allowed_for {
        if obj.contains_key(key) && !types.contains(&schema_type) {
            violations.push(format!(
                "{path}.{key} is not supported on type \"{}\"",
                schema_type.as_str()
            ));
        }
    }

    match schema_type {
        JsonSchemaType::Object => {
            if let Some(properties) = obj.get("properties") {
                if !properties.is_object() {
                    violations.push(format!("{path}.properties must be an object of schemas"));
                } else {
                    let pobj = properties.as_object().expect("object checked");
                    for (key, child) in pobj {
                        check_schema_node(child, &format!("{path}.properties.{key}"), violations, seen);
                    }
                }
            }
            // object-tail：required 数组 + 名字都在 properties + additionalProperties bool
            if let Some(required) = obj.get("required") {
                if !required.is_array()
                    || !required.as_array().is_some_and(|a| a.iter().all(|e| e.is_string()))
                {
                    violations.push(format!("{path}.required must be an array of strings"));
                } else if let Some(arr) = required.as_array() {
                    // TS：declared = properties 若为 record，否则 {}（无 properties 时
                    // 任何 required 名都视为“不在 properties”）。
                    for item in arr {
                        let key = item.as_str().unwrap_or_default();
                        let in_props = obj
                            .get("properties")
                            .and_then(Value::as_object)
                            .is_some_and(|m| m.contains_key(key));
                        if !in_props {
                            violations.push(format!(
                                "{path}.required names \"{key}\" which is not in properties"
                            ));
                        }
                    }
                }
            }
            if let Some(v) = obj.get("additionalProperties") {
                if !v.is_boolean() {
                    violations.push(format!("{path}.additionalProperties must be a boolean"));
                }
            }
        }
        JsonSchemaType::Array => {
            if let Some(items) = obj.get("items") {
                check_schema_node(items, &format!("{path}.items"), violations, seen);
            }
        }
        JsonSchemaType::String | JsonSchemaType::Number | JsonSchemaType::Integer | JsonSchemaType::Boolean | JsonSchemaType::Null => {
            let st = JsonSchemaScalarType(schema_type);
            let has_enum = obj.contains_key("enum");
            let enum_valid = obj
                .get("enum")
                .map(|e| {
                    e.is_array()
                        && !e.as_array().is_some_and(|a| a.is_empty())
                        && e.as_array().is_some_and(|a| a.iter().all(|x| scalar_matches(st, x)))
                })
                .unwrap_or(false);
            if has_enum && !enum_valid {
                violations.push(format!(
                    "{path}.enum must be a non-empty array of {} values",
                    schema_type.as_str()
                ));
            }
            let has_const = obj.contains_key("const");
            let const_valid = obj
                .get("const")
                .map(|c| scalar_matches(st, c))
                .unwrap_or(false);
            if has_const {
                if !const_valid {
                    violations.push(format!(
                        "{path}.const must be a {} value",
                        schema_type.as_str()
                    ));
                } else if enum_valid {
                    let allowed = obj["enum"].as_array().expect("enum_valid implies array");
                    let declared = &obj["const"];
                    if !allowed.iter().any(|x| value_eq(x, declared)) {
                        violations.push(format!(
                            "{path}.const must be one of {path}.enum when both are declared"
                        ));
                    }
                }
            }
        }
    }
    seen.pop();
}

/// 断言任意 raw schema 属于强制子集。
pub fn assert_supported_json_schema(schema: &Value) -> Result<(), JsonSchemaError> {
    let mut violations = Vec::new();
    let mut seen = Vec::new();
    check_schema_node(schema, "schema", &mut violations, &mut seen);
    if violations.is_empty() {
        Ok(())
    } else {
        Err(JsonSchemaError::new(violations))
    }
}

/// 断言子集 + object 根约束。
pub fn assert_object_json_schema(schema: &Value) -> Result<(), JsonSchemaError> {
    let mut violations = Vec::new();
    let mut seen = Vec::new();
    check_schema_node(schema, "schema", &mut violations, &mut seen);
    if violations.is_empty()
        && !(schema.is_object() && schema.get("type").and_then(Value::as_str) == Some("object"))
    {
        violations.push("schema.type must be \"object\" (structured output is object-rooted)".to_string());
    }
    if violations.is_empty() {
        Ok(())
    } else {
        Err(JsonSchemaError::new(violations))
    }
}

/// 把已断言的 raw schema 解析为强类型节点（断言通过后调用；重复校验无害）。
pub fn parse_asserted_schema(schema: &Value) -> JsonSchemaNode {
    fn walk(node: &Value) -> JsonSchemaNode {
        let mut out = JsonSchemaNode::default();
        let obj = node.as_object().expect("asserted schema node is a record");
        if let Some(t) = obj.get("type").and_then(Value::as_str).and_then(JsonSchemaType::parse) {
            out.r#type = Some(t);
        }
        if let Some(one_of) = obj.get("oneOf").and_then(Value::as_array) {
            out.one_of = Some(one_of.iter().map(walk).collect());
        }
        if let Some(props) = obj.get("properties").and_then(Value::as_object) {
            let mut map = BTreeMap::new();
            for (k, v) in props {
                map.insert(k.clone(), walk(v));
            }
            out.properties = Some(map);
        }
        if let Some(req) = obj.get("required").and_then(Value::as_array) {
            out.required = Some(
                req.iter().filter_map(Value::as_str).map(String::from).collect(),
            );
        }
        if let Some(ap) = obj.get("additionalProperties").and_then(Value::as_bool) {
            out.additional_properties = Some(ap);
        }
        if let Some(items) = obj.get("items") {
            out.items = Some(Box::new(walk(items)));
        }
        if let Some(enum_arr) = obj.get("enum").and_then(Value::as_array) {
            out.r#enum = Some(enum_arr.iter().map(scalar_from_value).collect());
        }
        if let Some(c) = obj.get("const") {
            out.const_ = Some(scalar_from_value(c));
        }
        if let Some(s) = obj.get("description").and_then(Value::as_str) {
            out.description = Some(s.to_string());
        }
        if let Some(s) = obj.get("title").and_then(Value::as_str) {
            out.title = Some(s.to_string());
        }
        if let Some(d) = obj.get("default") {
            out.default = Some(d.clone());
        }
        if let Some(e) = obj.get("examples") {
            out.examples = Some(e.clone());
        }
        out
    }
    // 断言已在调用方做；此处仅转换。极深的 schema 递归由 Rust 栈承受（断言时已同样递归）。
    walk(schema)
}

/// 从 JSON 值构造单位标量（断言后的 enum/const 成员）。
fn scalar_from_value(v: &Value) -> JsonSchemaScalar {
    match v {
        Value::String(s) => JsonSchemaScalar::Str(s.clone()),
        Value::Number(n) => JsonSchemaScalar::Num(n.clone()),
        Value::Bool(b) => JsonSchemaScalar::Bool(*b),
        Value::Null => JsonSchemaScalar::Null,
        other => JsonSchemaScalar::Str(serde_json::to_string(other).unwrap_or_default()),
    }
}

/// —— 值校验（validateJsonSchemaValue 直译；递归 —— schema 断言后无环）——
///
/// 返回路径限定的违规列表（walk 序）；空 = 合法。全程 total：任意候选值都不 panic。
pub fn validate_json_schema_value(schema: &JsonSchemaNode, value: &Value, path: &str) -> Vec<String> {
    /// 根感知诊断路径（空哨兵 → "arguments"）。
    fn diag(path: &str) -> &str {
        if path.is_empty() {
            "arguments"
        } else {
            path
        }
    }

    fn prop_path(path: &str, key: &str) -> String {
        if path.is_empty() {
            key.to_string()
        } else {
            format!("{path}.{key}")
        }
    }

    /// 一个合法 schema 节点的通用 lossless 诊断。
    fn lossless_violation(path: &str) -> String {
        format!("\"{}\" must be a lossless JSON value", diag(path))
    }

    /// 标量节点在 primitive type 检查后的字面约束诊断。
    fn check_scalar_value(node: &JsonSchemaNode, value: &Value, path: &str) -> Vec<String> {
        if let Some(allowed) = &node.r#enum {
            let hit = allowed.iter().any(|s| scalar_eq(value, s));
            if !hit {
                let shown = allowed
                    .iter()
                    .map(|s| s.num_display())
                    .collect::<Vec<_>>()
                    .join(",");
                return vec![format!("\"{}\" must be one of [{}]", diag(path), shown)];
            }
        }
        if let Some(c) = &node.const_ {
            if !scalar_eq(value, c) {
                return vec![format!("\"{}\" must be {}", diag(path), c.num_display())];
            }
        }
        Vec::new()
    }

    fn check_value(node: &JsonSchemaNode, value: &Value, path: &str) -> Vec<String> {
        let has_type = node.r#type.is_some();
        let node_type = node.r#type;
        // TS：nodeType 非白名单成员时该 frame `catches=false`——但断言过的 schema
        // type 必在白名单，故此处恒 true；保留为语义占位。
        let _catches = has_type;

        if let Some(branches) = &node.one_of {
            let matches = branches
                .iter()
                .filter(|b| check_value(b, value, path).is_empty())
                .count();
            return if matches == 1 {
                Vec::new()
            } else {
                vec![format!(
                    "\"{}\" must match exactly one oneOf branch (matched {matches})",
                    diag(path)
                )]
            };
        }

        match node_type {
            None => {
                if is_json_value_lossless(value) {
                    Vec::new()
                } else {
                    vec![lossless_violation(path)]
                }
            }
            Some(JsonSchemaType::Object) => {
                let Some(obj) = as_object(value) else {
                    return vec![format!("\"{}\" must be an object", diag(path))];
                };
                let mut violations = Vec::new();
                let required = node.required.clone().unwrap_or_default();
                for key in &required {
                    // TS：key 缺失或值为 undefined 视为缺失；serde 无 undefined，
                    // 存在（含 null）即满足。
                    if !obj.contains_key(key) {
                        violations.push(format!(
                            "missing required property \"{}\"",
                            prop_path(path, key)
                        ));
                    }
                }
                let mut children: Vec<(&JsonSchemaNode, &Value, String)> = Vec::new();
                if let Some(props) = &node.properties {
                    for (key, child) in props {
                        if !obj.contains_key(key) {
                            continue;
                        }
                        children.push((child, &obj[key], prop_path(path, key)));
                    }
                }
                let mut tail = Vec::new();
                if node.additional_properties == Some(false) {
                    let declared = |key: &str| {
                        node.properties
                            .as_ref()
                            .is_some_and(|m| m.contains_key(key))
                    };
                    for key in obj.keys() {
                        if !declared(key) {
                            tail.push(format!(
                                "\"{}\" is not a declared property (additionalProperties: false)",
                                prop_path(path, key)
                            ));
                        }
                    }
                }
                for (child, v, p) in children {
                    violations.extend(check_value(child, v, &p));
                }
                violations.extend(tail);
                if !violations.is_empty() {
                    violations
                } else {
                    Vec::new() // lossless 恒真
                }
            }
            Some(JsonSchemaType::Array) => {
                let Some(arr) = value.as_array() else {
                    return vec![format!("\"{}\" must be an array", diag(path))];
                };
                let mut violations = Vec::new();
                if let Some(items) = &node.items {
                    for (i, entry) in arr.iter().enumerate() {
                        violations.extend(check_value(items, entry, &format!("{path}[{i}]")));
                    }
                }
                violations
            }
            Some(JsonSchemaType::String) => {
                if value.is_string() {
                    check_scalar_value(node, value, path)
                } else {
                    vec![format!("\"{}\" must be a string", diag(path))]
                }
            }
            Some(JsonSchemaType::Number) => match value {
                Value::Number(n) if !is_finite_json_number(n) => {
                    vec![format!("\"{}\" must be a finite JSON number", diag(path))]
                }
                Value::Number(_) => check_scalar_value(node, value, path),
                _ => vec![format!("\"{}\" must be a number", diag(path))],
            },
            Some(JsonSchemaType::Integer) => {
                let is_int = match value {
                    Value::Number(n) => {
                        if n.is_i64() || n.is_u64() {
                            true
                        } else {
                            n.as_f64().map(|f| f.is_finite() && f.fract() == 0.0).unwrap_or(false)
                        }
                    }
                    _ => false,
                };
                if is_int {
                    check_scalar_value(node, value, path)
                } else {
                    vec![format!("\"{}\" must be an integer", diag(path))]
                }
            }
            Some(JsonSchemaType::Boolean) => {
                if value.is_boolean() {
                    check_scalar_value(node, value, path)
                } else {
                    vec![format!("\"{}\" must be a boolean", diag(path))]
                }
            }
            Some(JsonSchemaType::Null) => {
                if value.is_null() {
                    check_scalar_value(node, value, path)
                } else {
                    vec![format!("\"{}\" must be null", diag(path))]
                }
            }
        }
    }

    check_value(schema, value, path)
}

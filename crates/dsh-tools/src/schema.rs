//! 作者面 schema DSL 编译 + 参数校验 + typed 工具 helper
//! （镜像 TS `@deepseek-ai/dsh-tools/schema.ts`）。
//!
//! DSL 输入与产出统一为 `serde_json::Value`（D-014 规范化键序）。编译期错误立即
//! 抛出单条 `JsonSchemaError`（TS `authorError`）；产物再经
//! [`assert_supported_json_schema`](crate::json_schema) 全量断言。

use serde_json::{Map, Value};
use std::sync::Arc;

use crate::json_schema::{
    assert_supported_json_schema, parse_asserted_schema, JsonSchemaError, JsonSchemaNode,
    validate_json_schema_value,
};
use crate::types::{
    ToolDefinition, ToolFailureData, ToolOutputDefinition, ToolResult, ToolRunContext,
    CODE_INVALID_ARGS, ToolExecute, ToolFinalize, ToolIsConcurrencySafe, ToolPresentCall,
    ToolPresentResult, ToolPresentationMeta, ToolRender,
};

/// 引子注解键（所有作者节点共有）。
const ANNOTATIONS: [&str; 4] = ["description", "title", "default", "examples"];

fn author_error(message: String) -> JsonSchemaError {
    JsonSchemaError::new(vec![message])
}

fn copy_annotations(src: &Map<String, Value>, dst: &mut Map<String, Value>) {
    for key in ANNOTATIONS {
        if let Some(v) = src.get(key) {
            dst.insert(key.to_string(), v.clone());
        }
    }
}

/// 检查作者键白名单；不支持的键即报 DSL 特异性错误。
fn assert_author_keys(
    obj: &Map<String, Value>,
    path: &str,
    allowed: Vec<&str>,
) -> Result<(), JsonSchemaError> {
    for key in obj.keys() {
        if !allowed.iter().any(|a| *a == key) {
            return Err(author_error(format!(
                "{path}.{key} is not supported by the value schema DSL"
            )));
        }
    }
    Ok(())
}

/// 编译一个隐式属性 map：返回 `(properties, required)`。
fn compile_property_map(input: &Value, path: &str) -> Result<(Map<String, Value>, Vec<String>), JsonSchemaError> {
    if !input.is_object() {
        return Err(author_error(format!("{path} must be an object of value schemas")));
    }
    let obj = input.as_object().unwrap();
    let mut properties = Map::new();
    let mut required = Vec::new();
    for (key, prop) in obj {
        if !prop.is_object() {
            return Err(author_error(format!("{path}.{key} must be a value schema object")));
        }
        let pobj = prop.as_object().unwrap();
        if let Some(r) = pobj.get("required") {
            if r != &Value::Bool(true) {
                return Err(author_error(format!(
                    "{path}.{key}.required must be true when present"
                )));
            }
            required.push(key.clone());
        }
        let node = compile_node(prop, &format!("{path}.{key}"), true)?;
        properties.insert(key.clone(), Value::Object(node));
    }
    Ok((properties, required))
}

/// 编译一个作者节点（未套消费者根限制）。
fn compile_node(input: &Value, path: &str, allow_required: bool) -> Result<Map<String, Value>, JsonSchemaError> {
    if !input.is_object() {
        return Err(author_error(format!("{path} must be a value schema object")));
    }
    let obj = input.as_object().unwrap();
    let mut node = Map::new();

    let mut author_keys: Vec<&str> = ANNOTATIONS.to_vec();
    if allow_required {
        author_keys.push("required");
    }

    if obj.contains_key("oneOf") {
        let mut allowed = author_keys.clone();
        allowed.push("oneOf");
        if obj.contains_key("type") {
            allowed.push("type");
        }
        assert_author_keys(obj, path, allowed)?;
        if obj.contains_key("type") {
            return Err(author_error(format!("{path} cannot declare both type and oneOf")));
        }
        let one_of = &obj["oneOf"];
        if !matches!(one_of, Value::Array(a) if a.len() >= 2) {
            return Err(author_error(format!(
                "{path}.oneOf must be an array of at least two value schemas"
            )));
        }
        copy_annotations(obj, &mut node);
        let branches: Vec<Value> = one_of
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .map(|(i, b)| Ok(Value::Object(compile_node(b, &format!("{path}.oneOf[{i}]"), false)?)))
            .collect::<Result<_, JsonSchemaError>>()?;
        node.insert("oneOf".to_string(), Value::Array(branches));
        return Ok(node);
    }

    let input_type = obj.get("type").map(|v| v.as_str().unwrap_or_default());

    match input_type {
        Some("json") => {
            let mut allowed = author_keys.clone();
            allowed.push("type");
            assert_author_keys(obj, path, allowed)?;
            copy_annotations(obj, &mut node);
        }
        Some("object") => {
            let mut allowed = author_keys.clone();
            allowed.push("type");
            allowed.push("properties");
            allowed.push("additionalProperties");
            assert_author_keys(obj, path, allowed)?;
            let ap = obj.get("additionalProperties");
            match ap {
                Some(Value::Bool(b)) => {
                    node.insert("type".to_string(), Value::String("object".to_string()));
                    copy_annotations(obj, &mut node);
                    node.insert("additionalProperties".to_string(), Value::Bool(*b));
                    if let Some(props) = obj.get("properties") {
                        let (properties, required) = compile_property_map(props, &format!("{path}.properties"))?;
                        node.insert("properties".to_string(), Value::Object(properties));
                        if !required.is_empty() {
                            node.insert(
                                "required".to_string(),
                                Value::Array(required.into_iter().map(Value::String).collect()),
                            );
                        }
                    }
                }
                _ => {
                    return Err(author_error(format!(
                        "{path}.additionalProperties must be explicitly true or false"
                    )));
                }
            }
        }
        Some("array") => {
            let mut allowed = author_keys.clone();
            allowed.push("type");
            allowed.push("items");
            assert_author_keys(obj, path, allowed)?;
            node.insert("type".to_string(), Value::String("array".to_string()));
            copy_annotations(obj, &mut node);
            if let Some(items) = obj.get("items") {
                node.insert(
                    "items".to_string(),
                    Value::Object(compile_node(items, &format!("{path}.items"), false)?),
                );
            }
        }
        Some("string") | Some("number") | Some("integer") | Some("boolean") | Some("null") => {
            let mut allowed = author_keys.clone();
            allowed.push("type");
            allowed.push("enum");
            allowed.push("const");
            assert_author_keys(obj, path, allowed)?;
            node.insert("type".to_string(), Value::String(input_type.unwrap().to_string()));
            copy_annotations(obj, &mut node);
            if let Some(enum_v) = obj.get("enum") {
                if !enum_v.is_array() {
                    return Err(author_error(format!(
                        "{path}.enum must be a non-empty array of scalar values"
                    )));
                }
                node.insert(
                    "enum".to_string(),
                    Value::Array(enum_v.as_array().unwrap().clone()),
                );
            }
            if let Some(c) = obj.get("const") {
                node.insert("const".to_string(), c.clone());
            }
        }
        _ => {
            return Err(author_error(format!(
                "{path}.type must be string/number/integer/boolean/null/array/object/json, or use oneOf"
            )));
        }
    }
    Ok(node)
}

/// 编译一个作者 value schema 到强制 raw JSON Schema 子集（`json` → annotation-only）。
pub fn value_schema_spec_to_json_schema(spec: &Value) -> Result<JsonSchemaNode, JsonSchemaError> {
    let raw = Value::Object(compile_node(spec, "schema", false)?);
    assert_supported_json_schema(&raw)?;
    Ok(parse_asserted_schema(&raw))
}

/// 编译隐式开放参数对象到 object 根 raw JSON Schema。
pub fn parameter_schema_spec_to_json_schema(
    spec: &Value,
) -> Result<JsonSchemaNode, JsonSchemaError> {
    let (properties, required) = compile_property_map(spec, "parameters")?;
    let mut schema = Map::new();
    schema.insert("type".to_string(), Value::String("object".to_string()));
    schema.insert("properties".to_string(), Value::Object(properties));
    if !required.is_empty() {
        schema.insert(
            "required".to_string(),
            Value::Array(required.into_iter().map(Value::String).collect()),
        );
    }
    let raw = Value::Object(schema);
    assert_supported_json_schema(&raw)?;
    Ok(parse_asserted_schema(&raw))
}

/// 无效模型参数（镜像 TS `ToolArgsError`：`invalid arguments: {join}`，code
/// `INVALID_ARGS`，name `ToolArgsError`，携带 violations）。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolArgsError {
    pub message: String,
    pub violations: Vec<String>,
}

impl ToolArgsError {
    pub const CODE: &'static str = CODE_INVALID_ARGS;

    pub fn new(violations: Vec<String>) -> Self {
        ToolArgsError {
            message: format!("invalid arguments: {}", violations.join("; ")),
            violations,
        }
    }
}

impl std::fmt::Display for ToolArgsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", Self::CODE, self.message)
    }
}

impl std::error::Error for ToolArgsError {}

/// 校验模型产出的参数（对任意形态的候选值 total；空 = 合法）。
pub fn validate_args(spec: &Value, args: &Value) -> Vec<String> {
    let schema = parameter_schema_spec_to_json_schema(spec)
        .expect("validateArgs precondition: parameter spec must compile");
    validate_json_schema_value(&schema, args, "")
}

/// `define_tool` 的定义期错误：超时参数或 schema 编译失败。
#[derive(Debug, Clone, PartialEq)]
pub enum ToolDefinitionError {
    /// `defineTool(${name}): timeoutMs must be a positive finite number`。
    Timeout(String),
    Schema(JsonSchemaError),
}

impl std::fmt::Display for ToolDefinitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ToolDefinitionError::Timeout(m) => write!(f, "{m}"),
            ToolDefinitionError::Schema(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ToolDefinitionError {}

/// `defineTool` 的选项（对齐 TS `DefineToolOptions`；函数体以 `Rc` 闭包承载）。
pub struct DefineToolOptions {
    pub name: String,
    pub description: String,
    /// 作者面参数 DSL（object of value schemas）。
    pub parameters: Value,
    /// 作者面输出 schema（value schema）。
    pub output_schema: Value,
    /// 纯模型渲染：`(validated args, canonical value)` → ContentBlock。
    pub render: ToolRender,
    /// 直接顶层调用的纯呈现元数据。
    pub presentation_meta: Option<ToolPresentationMeta>,
    /// 正有限毫秒预算；非法即定义错误。
    pub timeout_ms: Option<f64>,
    /// 参数校验后的本体执行（同步）。
    pub execute: ToolExecute,
    /// 每个归一化结果的最后内容变换。
    pub finalize_content: Option<ToolFinalize>,
    /// 仅 `true` 才允许并行（软校验：参数非法 → false）。
    pub is_concurrency_safe: Option<ToolIsConcurrencySafe>,
    /// 待执行态呈现（软校验：参数非法 → None）。
    pub present_call: Option<ToolPresentCall>,
    /// 已完成态呈现（软校验：参数非法 → None）。
    pub present_result: Option<ToolPresentResult>,
}

impl std::fmt::Debug for DefineToolOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DefineToolOptions")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("parameters", &self.parameters)
            .field("output_schema", &self.output_schema)
            .field("timeout_ms", &self.timeout_ms)
            .finish_non_exhaustive()
    }
}

impl Default for DefineToolOptions {
    fn default() -> Self {
        DefineToolOptions {
            name: String::new(),
            description: String::new(),
            parameters: Value::Object(Map::new()),
            output_schema: Value::Object(Map::new()),
            render: Arc::new(|_, _| Vec::new()),
            presentation_meta: None,
            timeout_ms: None,
            execute: Arc::new(|_, _| Err(ToolFailureData::new("unimplemented", CODE_INVALID_ARGS, "Error"))),
            finalize_content: None,
            is_concurrency_safe: None,
            present_call: None,
            present_result: None,
        }
    }
}

/// 定义带推断参数与严格执行校验的一手工具（镜像 TS `defineTool`）。
///
/// 呈现只用于 replay，可能遇到任意旧日志参数——**软校验**：参数不合法时回退
/// `None`/`false`（通用呈现），而不是 execute 路径的硬 `ToolArgsError`。
pub fn define_tool(options: DefineToolOptions) -> Result<ToolDefinition, ToolDefinitionError> {
    if let Some(t) = options.timeout_ms {
        if !t.is_finite() || t <= 0.0 {
            return Err(ToolDefinitionError::Timeout(format!(
                "defineTool({}): timeoutMs must be a positive finite number",
                options.name
            )));
        }
    }
    let parameters_node = parameter_schema_spec_to_json_schema(&options.parameters)
        .map_err(ToolDefinitionError::Schema)?;
    let output_schema = value_schema_spec_to_json_schema(&options.output_schema)
        .map_err(ToolDefinitionError::Schema)?;

    let parameters_json = parameters_node.to_json();

    let execute = options.execute;
    let present_call = options.present_call;
    let present_result = options.present_result;
    let is_concurrency_safe = options.is_concurrency_safe;

    let execute_node = parameters_node.clone();
    let wrapped_execute = Arc::new(move |args: &Value, ctx: &ToolRunContext| {
        let violations = validate_json_schema_value(&execute_node, args, "");
        if !violations.is_empty() {
            return Err(ToolFailureData::new(
                ToolArgsError::new(violations).message,
                CODE_INVALID_ARGS,
                "ToolArgsError",
            ));
        }
        execute(args, ctx)
    });

    let wrapped_present_call = present_call.map(|f| {
        let node = parameters_node.clone();
        let out: ToolPresentCall = Arc::new(move |args: &Value| {
            if !validate_json_schema_value(&node, args, "").is_empty() {
                return None;
            }
            f(args)
        });
        out
    });
    let wrapped_present_result = present_result.map(|f| {
        let node = parameters_node.clone();
        let out: ToolPresentResult = Arc::new(move |args: &Value, result: &ToolResult| {
            if !validate_json_schema_value(&node, args, "").is_empty() {
                return None;
            }
            f(args, result)
        });
        out
    });
    let wrapped_is_concurrency_safe = is_concurrency_safe.map(|f| {
        let node = parameters_node.clone();
        let out: ToolIsConcurrencySafe = Arc::new(move |args: &Value| {
            if !validate_json_schema_value(&node, args, "").is_empty() {
                return false;
            }
            f(args)
        });
        out
    });

    Ok(ToolDefinition {
        name: options.name,
        description: options.description,
        parameters: parameters_json,
        output: ToolOutputDefinition {
            schema: output_schema,
            render: options.render,
            presentation_meta: options.presentation_meta,
        },
        timeout_ms: options.timeout_ms,
        execute: wrapped_execute,
        finalize_content: options.finalize_content,
        is_concurrency_safe: wrapped_is_concurrency_safe,
        present_call: wrapped_present_call,
        present_result: wrapped_present_result,
    })
}

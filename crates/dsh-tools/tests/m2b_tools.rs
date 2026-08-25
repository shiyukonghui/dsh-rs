//! M2b: dsh-tools 纯语义层测试（移植 TS schema.spec/json-schema.spec 的可观察行为）。
//! 覆盖：作者 DSL 编译产物与错误消息、强制子集断言消息、值校验消息、validateArgs、
//! define_tool（超时/参数校验/软呈现）。

use dsh_tools::{
    assert_object_json_schema, assert_supported_json_schema, define_tool, json_schema,
    parameter_schema_spec_to_json_schema, validate_args, validate_json_schema_value,
    value_schema_spec_to_json_schema, DefineToolOptions, JsonSchemaError, ToolArgsError,
    ToolRunContext,
};
use serde_json::{json, Value};
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// schema 编译产物
// ---------------------------------------------------------------------------

#[test]
fn schema_compiles_primitives_and_annotations() {
    let node = value_schema_spec_to_json_schema(&json!({
        "type": "string",
        "description": "a name",
        "title": "Name",
        "default": "x",
        "examples": ["a", "b"],
    }))
    .unwrap();
    let v = node.to_json();
    assert_eq!(v["type"], json!("string"));
    assert_eq!(v["description"], json!("a name"));
    assert_eq!(v["title"], json!("Name"));
    assert_eq!(v["default"], json!("x"));
    assert_eq!(v["examples"], json!(["a", "b"]));
}

#[test]
fn schema_compiles_array_without_items_to_open_items() {
    let node = value_schema_spec_to_json_schema(&json!({ "type": "array" })).unwrap();
    let v = node.to_json();
    assert_eq!(v["type"], json!("array"));
    assert!(v.get("items").is_none(), "omitted items stays absent");
}

#[test]
fn schema_compiles_object_with_required_and_openness() {
    let node = value_schema_spec_to_json_schema(&json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "a": { "type": "string", "required": true },
            "b": { "type": "number" },
        },
    }))
    .unwrap();
    let v = node.to_json();
    assert_eq!(v["type"], json!("object"));
    assert_eq!(v["additionalProperties"], json!(false));
    assert_eq!(v["properties"]["a"]["type"], json!("string"));
    assert_eq!(
        v["properties"]["a"].get("required"),
        None,
        "property-level required:true is consumed into the parent list"
    );
    assert_eq!(v["required"], json!(["a"]), "required collected in property order");
}

#[test]
fn schema_compiles_object_additional_properties_true() {
    let node = value_schema_spec_to_json_schema(&json!({
        "type": "object",
        "additionalProperties": true,
    }))
    .unwrap();
    assert_eq!(node.to_json()["additionalProperties"], json!(true));
}

#[test]
fn schema_json_node_becomes_annotation_only() {
    let node = value_schema_spec_to_json_schema(&json!({ "type": "json", "description": "any" })).unwrap();
    let v = node.to_json();
    assert!(v.get("type").is_none(), "json node has no type keyword");
    assert_eq!(v["description"], json!("any"));
}

#[test]
fn schema_one_of_compiles_branches() {
    let node = value_schema_spec_to_json_schema(&json!({
        "oneOf": [
            { "type": "string" },
            { "type": "integer" },
        ],
    }))
    .unwrap();
    let v = node.to_json();
    let branches = v["oneOf"].as_array().unwrap();
    assert_eq!(branches.len(), 2);
    assert_eq!(branches[0]["type"], json!("string"));
    assert_eq!(branches[1]["type"], json!("integer"));
}

#[test]
fn schema_enum_and_const_compile() {
    let node = value_schema_spec_to_json_schema(&json!({
        "type": "string",
        "enum": ["a", "b"],
        "const": "a",
    }))
    .unwrap();
    let v = node.to_json();
    assert_eq!(v["enum"], json!(["a", "b"]));
    assert_eq!(v["const"], json!("a"));
}

#[test]
fn parameter_schema_compilation_sets_object_root() {
    let node = parameter_schema_spec_to_json_schema(&json!({
        "code": { "type": "string", "required": true },
        "reason": { "type": "string" },
    }))
    .unwrap();
    let v = node.to_json();
    assert_eq!(v["type"], json!("object"));
    assert_eq!(v["required"], json!(["code"]));
}

// ---------------------------------------------------------------------------
// schema 编译错误（TS authorError → JsonSchemaError 单条）
// ---------------------------------------------------------------------------

#[test]
fn schema_errors_report_exact_messages() {
    let cases: Vec<(Value, &str)> = vec![
        (json!({}), "schema.type must be string/number/integer/boolean/null/array/object/json, or use oneOf"),
        (json!({ "type": "foo" }), "schema.type must be string/number/integer/boolean/null/array/object/json, or use oneOf"),
        (json!({ "type": "object" }), "schema.additionalProperties must be explicitly true or false"),
        (json!({ "type": "string", "bogus": 1 }), "schema.bogus is not supported by the value schema DSL"),
        (json!({ "type": "string", "enum": "nope" }), "schema.enum must be a non-empty array of scalar values"),
        (json!({ "oneOf": [] }), "schema.oneOf must be an array of at least two value schemas"),
        (json!({ "oneOf": [{ "type": "string" }] }), "schema.oneOf must be an array of at least two value schemas"),
        (json!({ "type": "string", "oneOf": [{ "type": "string" }, { "type": "number" }] }), "schema cannot declare both type and oneOf"),
    ];
    for (spec, expected) in cases {
        let err = value_schema_spec_to_json_schema(&spec).unwrap_err();
        assert_eq!(err.message, format!("unsupported JSON schema: {expected}"), "spec={spec}");
        assert_eq!(err.violations, vec![expected.to_string()]);
    }
}

#[test]
fn schema_parameter_errors_report_exact_messages() {
    let err = parameter_schema_spec_to_json_schema(&json!({
        "a": { "type": "string", "required": false },
    }))
    .unwrap_err();
    assert_eq!(
        err.violations,
        vec!["parameters.a.required must be true when present".to_string()]
    );

    let err = parameter_schema_spec_to_json_schema(&json!({ "a": "plain" })).unwrap_err();
    assert_eq!(
        err.violations,
        vec!["parameters.a must be a value schema object".to_string()]
    );

    let err = parameter_schema_spec_to_json_schema(&json!([])).unwrap_err();
    assert!(err.violations[0].contains("parameters must be an object of value schemas"));
}

#[test]
fn schema_nested_paths_in_messages() {
    let err = value_schema_spec_to_json_schema(&json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "a": { "type": "array", "items": { "type": "wat" } },
        },
    }))
    .unwrap_err();
    assert_eq!(
        err.violations,
        vec!["schema.properties.a.items.type must be string/number/integer/boolean/null/array/object/json, or use oneOf".to_string()]
    );
}

// ---------------------------------------------------------------------------
// assertSupportedJsonSchema / assertObjectJsonSchema
// ---------------------------------------------------------------------------

#[test]
fn supported_schema_accepts_various_roots() {
    assert_supported_json_schema(&json!({})).unwrap();
    assert_supported_json_schema(&json!({ "type": "object", "properties": {}, "additionalProperties": true })).unwrap();
    assert_supported_json_schema(&json!({
        "type": "array",
        "items": { "type": "string", "enum": ["a"] },
    }))
    .unwrap();
    assert_supported_json_schema(&json!({
        "oneOf": [
            { "type": "null" },
            { "type": "boolean", "const": true },
        ],
    }))
    .unwrap();
}

#[test]
fn supported_schema_rejects_unknown_and_misplaced_keywords() {
    let cases: Vec<(Value, &str)> = vec![
        (json!({ "type": "string", "maxLength": 3 }),
            "schema.maxLength is not a supported keyword (subset: type/oneOf/properties/required/additionalProperties/items/enum/const + annotations)"),
        (json!({ "properties": {} }),
            "schema.properties requires type or oneOf"),
        (json!({ "type": "string", "properties": {} }),
            "schema.properties is not supported on type \"string\""),
        (json!({ "type": ["string"] }),
            "schema.type must be a single type string (type arrays are not supported)"),
        (json!({ "type": "wat" }),
            "schema.type must be one of object/array/string/number/integer/boolean/null"),
        (json!({ "oneOf": [{}], "type": "string" }),
            "schema cannot declare both type and oneOf"),
        (json!({ "oneOf": [{}] }),
            "schema.oneOf must be an array of at least two schemas"),
        (json!({ "oneOf": [{}, {}], "enum": [1] }),
            "schema.enum is not supported beside oneOf"),
        (json!({ "type": "object", "additionalProperties": "no" }),
            "schema.additionalProperties must be a boolean"),
        (json!({ "type": "object", "additionalProperties": false, "required": ["x"] }),
            "schema.required names \"x\" which is not in properties"),
        (json!({ "type": "object", "required": "x" }),
            "schema.required must be an array of strings"),
    ];
    for (schema, expected) in cases {
        let err = assert_supported_json_schema(&schema).unwrap_err();
        assert_eq!(err.violations, vec![expected.to_string()], "schema={schema}");
    }
}

#[test]
fn supported_schema_validates_scalar_literals() {
    let cases: Vec<(Value, &str)> = vec![
        (json!({ "type": "string", "enum": [] }), "schema.enum must be a non-empty array of string values"),
        (json!({ "type": "string", "enum": [1] }), "schema.enum must be a non-empty array of string values"),
        (json!({ "type": "boolean", "const": "yes" }), "schema.const must be a boolean value"),
        (json!({ "type": "integer", "const": 1.5 }), "schema.const must be a integer value"),
        (json!({ "type": "string", "enum": ["a", "b"], "const": "z" }),
            "schema.const must be one of schema.enum when both are declared"),
    ];
    for (schema, expected) in cases {
        let err = assert_supported_json_schema(&schema).unwrap_err();
        assert_eq!(err.violations, vec![expected.to_string()], "schema={schema}");
    }
}

#[test]
fn supported_schema_rejects_annotation_type_mismatches() {
    let err = assert_supported_json_schema(&json!({ "type": "string", "description": 3 })).unwrap_err();
    assert_eq!(err.violations, vec!["schema.description must be a string".to_string()]);

    let err = assert_supported_json_schema(&json!({ "type": "string", "title": true })).unwrap_err();
    assert_eq!(err.violations, vec!["schema.title must be a string".to_string()]);
}

#[test]
fn object_schema_requires_object_root() {
    let err = assert_object_json_schema(&json!({ "type": "string" })).unwrap_err();
    assert_eq!(
        err.violations,
        vec!["schema.type must be \"object\" (structured output is object-rooted)".to_string()]
    );

    // 本身非法时，只报子集违规（object-root 检查不叠加）
    let err = assert_object_json_schema(&json!({ "type": "wat" })).unwrap_err();
    assert_eq!(err.violations, vec!["schema.type must be one of object/array/string/number/integer/boolean/null".to_string()]);

    // 合法 object 根通过
    assert_object_json_schema(&json!({ "type": "object", "properties": {}, "additionalProperties": false })).unwrap();
}

#[test]
fn json_schema_error_has_stable_shape() {
    let err = JsonSchemaError::new(vec!["a".to_string(), "b".to_string()]);
    assert_eq!(err.message, "unsupported JSON schema: a; b");
    assert_eq!(err.to_string(), "UNSUPPORTED_SCHEMA: unsupported JSON schema: a; b");
}

// ---------------------------------------------------------------------------
// 值校验（validateJsonSchemaValue / validateArgs）
// ---------------------------------------------------------------------------

#[test]
fn value_validation_reports_path_qualified_violations() {
    let schema = parameter_schema_spec_to_json_schema(&json!({
        "name": { "type": "string", "required": true },
        "count": { "type": "integer" },
    }))
    .unwrap();

    assert_eq!(
        validate_json_schema_value(&schema, &json!({ "name": "x", "count": 2 }), ""),
        Vec::<String>::new()
    );

    // 根路径 '' → "arguments"
    let v = validate_args(&json!({
        "name": { "type": "string", "required": true },
    }), &json!({}));
    assert_eq!(v, vec!["missing required property \"name\"".to_string()]);

    // 根违规的“arguments”标签仅在根节点自身不合法时出现；属性路径无前缀。
    let v = validate_args(&json!({
        "name": { "type": "string" },
    }), &json!({ "name": 5 }));
    assert_eq!(v, vec!["\"name\" must be a string".to_string()]);

    let v = validate_args(&json!({
        "count": { "type": "integer" },
    }), &json!({ "count": 1.5 }));
    assert_eq!(v, vec!["\"count\" must be an integer".to_string()]);
}

#[test]
fn value_validation_enum_const_and_one_of() {
    let schema = parameter_schema_spec_to_json_schema(&json!({
        "color": { "type": "string", "enum": ["red", "blue"] },
        "flag": { "type": "boolean", "const": true },
    }))
    .unwrap();
    let v = validate_json_schema_value(&schema, &json!({ "color": "green", "flag": false }), "value");
    assert_eq!(
        v,
        vec![
            "\"value.color\" must be one of [\"red\",\"blue\"]".to_string(),
            "\"value.flag\" must be true".to_string(),
        ]
    );

    let one_of = json_schema::JsonSchemaNode {
        one_of: Some(vec![
            json_schema::JsonSchemaNode { r#type: Some(json_schema::JsonSchemaType::String), ..Default::default() },
            json_schema::JsonSchemaNode { r#type: Some(json_schema::JsonSchemaType::Integer), ..Default::default() },
        ]),
        ..Default::default()
    };
    assert!(validate_json_schema_value(&one_of, &json!("hi"), "value").is_empty());
    assert!(validate_json_schema_value(&one_of, &json!(2), "value").is_empty());
    let v = validate_json_schema_value(&one_of, &json!(2.5), "value");
    assert_eq!(
        v,
        vec!["\"value\" must match exactly one oneOf branch (matched 0)".to_string()]
    );
    let v = validate_json_schema_value(&one_of, &json!("hi"), "value");
    assert!(v.is_empty());
    // 同时匹配两个分支 → matched 2
    let both = json_schema::JsonSchemaNode {
        one_of: Some(vec![
            json_schema::JsonSchemaNode { ..Default::default() },
            json_schema::JsonSchemaNode { ..Default::default() },
        ]),
        ..Default::default()
    };
    let v = validate_json_schema_value(&both, &json!("any"), "value");
    assert_eq!(
        v,
        vec!["\"value\" must match exactly one oneOf branch (matched 2)".to_string()]
    );
}

#[test]
fn value_validation_object_openness_and_nested_arrays() {
    let closed = parameter_schema_spec_to_json_schema(&json!({
        "extra": { "type": "string" },
    }))
    .unwrap();
    // DSL 隐式开放根：未声明键仍合法
    assert!(validate_json_schema_value(&closed, &json!({ "extra": "x", "zzz": 1 }), "v").is_empty());

    let strict = json_schema::JsonSchemaNode {
        r#type: Some(json_schema::JsonSchemaType::Object),
        properties: Some(
            [("only".to_string(), json_schema::JsonSchemaNode { r#type: Some(json_schema::JsonSchemaType::String), ..Default::default() })]
                .into_iter()
                .collect(),
        ),
        additional_properties: Some(false),
        ..Default::default()
    };
    let v = validate_json_schema_value(&strict, &json!({ "only": "a", "other": 1 }), "args");
    assert_eq!(
        v,
        vec!["\"args.other\" is not a declared property (additionalProperties: false)".to_string()]
    );

    let array = parameter_schema_spec_to_json_schema(&json!({
        "items_list": { "type": "array", "items": { "type": "integer" } },
    }))
    .unwrap();
    let v = validate_json_schema_value(&array, &json!({ "items_list": [1, "x", 3] }), "value");
    assert_eq!(v, vec!["\"value.items_list[1]\" must be an integer".to_string()]);
}

#[test]
fn validate_args_is_total_for_malformed_inputs() {
    let spec = json!({ "tags": { "type": "array" } });
    // 任意怪形态不 panic；合法值无违规
    let _ = validate_args(&spec, &json!(null));
    let _ = validate_args(&spec, &json!("not-an-object"));
    let _ = validate_args(&spec, &json!([1, 2]));
    assert!(validate_args(&spec, &json!({ "tags": ["a", "b"] })).is_empty());
    let v = validate_args(&spec, &json!({ "tags": "oops" }));
    assert_eq!(v, vec!["\"tags\" must be an array".to_string()]);

    // 根节点自身不合法时用 "arguments" 标签
    let v = validate_args(&json!({ "a": { "type": "string" } }), &json!("root-not-object"));
    assert_eq!(v, vec!["\"arguments\" must be an object".to_string()]);
}

#[test]
fn tool_args_error_has_exact_shape() {
    let err = ToolArgsError::new(vec!["\"arguments.name\" must be a string".to_string()]);
    assert_eq!(err.message, "invalid arguments: \"arguments.name\" must be a string");
    assert_eq!(err.to_string(), "INVALID_ARGS: invalid arguments: \"arguments.name\" must be a string");
}

// ---------------------------------------------------------------------------
// define_tool
// ---------------------------------------------------------------------------

fn ctx(name: &str) -> ToolRunContext {
    ToolRunContext::new("call-1", "root-1", name, None)
}

fn echo_tool_options() -> DefineToolOptions {
    DefineToolOptions {
        name: "echo".to_string(),
        description: "echo a value".to_string(),
        parameters: json!({
            "text": { "type": "string", "required": true },
        }),
        output_schema: json!({ "type": "json" }),
        render: Arc::new(|_, value| vec![dsh_llm::ContentBlock::text(serde_json::to_string(value).unwrap())]),
        execute: Arc::new(|args, _| Ok(args["text"].clone())),
        ..Default::default()
    }
}

#[test]
fn define_tool_compiles_schema_and_wraps_execute() {
    let tool = define_tool(echo_tool_options()).unwrap();
    assert_eq!(tool.name, "echo");
    assert_eq!(tool.parameters["type"], json!("object"));
    assert_eq!(tool.parameters["required"], json!(["text"]));

    // 合法参数执行
    let result = (tool.execute)(&json!({ "text": "hello" }), &ctx("echo")).unwrap();
    assert_eq!(result, json!("hello"));

    // 非法参数 → ToolArgsError（code INVALID_ARGS）；属性路径无 arguments 前缀
    let failure = (tool.execute)(&json!({ "text": 42 }), &ctx("echo")).unwrap_err();
    assert_eq!(failure.code, "INVALID_ARGS");
    assert_eq!(failure.name, "ToolArgsError");
    assert_eq!(failure.message, "invalid arguments: \"text\" must be a string");
}

#[test]
fn define_tool_rejects_bad_timeout() {
    let err = define_tool(DefineToolOptions {
        name: "slow".to_string(),
        timeout_ms: Some(-1.0),
        ..echo_tool_options()
    })
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "defineTool(slow): timeoutMs must be a positive finite number"
    );

    let err = define_tool(DefineToolOptions {
        name: "slow".to_string(),
        timeout_ms: Some(f64::NAN),
        ..echo_tool_options()
    })
    .unwrap_err();
    assert!(err.to_string().contains("timeoutMs must be a positive finite number"));

    let ok = define_tool(DefineToolOptions {
        name: "slow".to_string(),
        timeout_ms: Some(100.0),
        ..echo_tool_options()
    })
    .unwrap();
    assert_eq!(ok.timeout_ms, Some(100.0));
}

#[test]
fn define_tool_soft_validation_on_presenters() {
    // 非法参数 → present_call/present_result 回退 None；is_concurrency_safe 回退 false
    let present_count = Arc::new(AtomicUsize::new(0));
    let pc = present_count.clone();
    let opts = DefineToolOptions {
        present_call: Some(Arc::new(move |_args| {
            pc.fetch_add(1, Ordering::SeqCst);
            Some(dsh_tools::ToolCallView::generic("echo"))
        })),
        is_concurrency_safe: Some(Arc::new(|_| true)),
        ..echo_tool_options()
    };
    let tool = define_tool(opts).unwrap();
    let view = (tool.present_call.as_ref().unwrap())(&json!({ "text": 42 }));
    assert!(view.is_none(), "soft validation suppresses present_call");
    assert_eq!(present_count.load(Ordering::SeqCst), 0, "user presenter not invoked on invalid args");
    assert!(!(tool.is_concurrency_safe.as_ref().unwrap())(&json!({ "text": 42 })));

    let view = (tool.present_call.as_ref().unwrap())(&json!({ "text": "ok" }));
    assert!(view.is_some(), "valid args reach user presenter");
    assert_eq!(present_count.load(Ordering::SeqCst), 1);
    assert!((tool.is_concurrency_safe.as_ref().unwrap())(&json!({ "text": "ok" })));
}

#[test]
fn define_tool_render_and_presentation_meta() {
    let tool = define_tool(echo_tool_options()).unwrap();
    let blocks = (tool.output.render)(&json!({ "text": "x" }), &json!("hello"));
    assert_eq!(blocks.len(), 1);
    // render 用 serde_json::to_string → 字符串值带引号
    assert_eq!(blocks[0].as_text().map(|t| t.text()), Some("\"hello\""));
}

// ---------------------------------------------------------------------------
// JsonSchemaNode 序列化往返
// ---------------------------------------------------------------------------

#[test]
fn node_round_trips_through_canonical_json() {
    // 参数 DSL（隐式根）：属性 schema 直接声明
    let schema = json!({
        "code": { "type": "string", "required": true },
        "count": { "type": "integer" },
    });
    let node = parameter_schema_spec_to_json_schema(&schema).unwrap();
    let v = node.to_json();
    assert_eq!(v["type"], json!("object"));
    assert_eq!(v["required"], json!(["code"]), "only required:true collected");
    // canonical 键序（BTreeMap 字典序）也通过再断言
    assert_supported_json_schema(&v).unwrap();
}

#[test]
fn value_validation_root_label_argument_for_validate_args_root() {
    // 根违规用 "arguments"（validateArgs 的 '' 根）而非 "value"（默认根）
    let schema = parameter_schema_spec_to_json_schema(&json!({
        "a": { "type": "number" },
    }))
    .unwrap();
    let via_validate_args = validate_args(&json!({ "a": { "type": "number" } }), &json!({ "a": "x" }));
    assert_eq!(via_validate_args, vec!["\"a\" must be a number".to_string()]);
    let via_low_level = validate_json_schema_value(&schema, &json!({ "a": "x" }), "");
    assert_eq!(via_low_level, vec!["\"a\" must be a number".to_string()]);
    let _ = Value::Null; // keep serde_json import referenced
}

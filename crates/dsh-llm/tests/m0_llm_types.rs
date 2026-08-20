//! dsh-llm types 契约测试。
//!
//! 权威参考：`deepseek-harness/packages/llm/llm/src/{types,message,call-config,brand}.ts`。
//! 断言：① 已知变体 JSON 往返形状与 TS 序列化一致（kebab/camel 字段名）；② 合并
//! 可扩展类型（ContentBlock/MessageSource/FinishReason/StreamChunk）未知类型进入
//! Unknown 扩展点后无损回写；③ callConfigEquals 语义；④ 品牌 id 透传。

use dsh_brand::{CallId, MessageId, ReasoningEffortId};
use dsh_llm::types::{
    ContentBlock, ContentBlockType, FinishReason, LlmFailure, Message, MessageSource,
    ReplayEnvelope, Role, StreamChunk, TokenUsage, ToolSchema,
};
use dsh_llm::{call_config, CallConfig};

/// 断言 JSON 语义等价（键序无关）。仓库差分规范为字典序（决策 D-014）——
/// 契约只锁 JSON 内容，不绑定 serde_json 的键序。
fn json_eq_serialized<T: serde::Serialize>(v: &T, expected_json: &str) {
    let actual = serde_json::to_value(v).unwrap();
    let expected: serde_json::Value = serde_json::from_str(expected_json).unwrap();
    assert_eq!(actual, expected, "serialized JSON must be semantically equal\n actual:  {actual}\n expected: {expected}");
}

#[test]
fn content_block_typed_variants_roundtrip_with_ts_field_names() {
    // text / reasoning 直写 text 字段
    let text: ContentBlock =
        serde_json::from_str(r#"{"type":"text","text":"hi"}"#).unwrap();
    json_eq_serialized(&text, r#"{"type":"text","text":"hi"}"#);
    let reasoning: ContentBlock =
        serde_json::from_str(r#"{"type":"reasoning","text":"think"}"#).unwrap();
    assert_eq!(reasoning.as_reasoning().unwrap().text(), "think");

    // tool-call：id/name/arguments 原样
    let call: ContentBlock = serde_json::from_str(
        r#"{"type":"tool-call","id":"c1","name":"fs_read","arguments":"{}"}"#,
    )
    .unwrap();
    assert_eq!(call.as_tool_call().unwrap().name(), "fs_read");

    // tool-result：toolCallId / isError 驼峰
    let result: ContentBlock = serde_json::from_str(
        r#"{"type":"tool-result","toolCallId":"c1","content":[{"type":"text","text":"ok"}],"isError":false}"#,
    )
    .unwrap();
    let tr = result.as_tool_result().unwrap();
    assert_eq!(tr.tool_call_id().raw(), "c1");
    assert_eq!(tr.is_error(), Some(false));

    // 无 isError 时缺省为 None（可选字段）
    let no_err: ContentBlock = serde_json::from_str(
        r#"{"type":"tool-result","toolCallId":"c1","content":[]}"#,
    )
    .unwrap();
    assert_eq!(no_err.as_tool_result().unwrap().is_error(), None);
}

#[test]
fn content_block_unknown_type_is_lossless_extension_point() {
    // M2+ 插件可能引入新 block 类型；Rust build 不认识时必须无损保留（merge-extensible）
    let raw = r#"{"type":"web-search","query":"rust","n":3}"#;
    let block: ContentBlock = serde_json::from_str(raw).unwrap();
    let ContentBlock::Unknown { type_, .. } = &block else {
        panic!("unknown block must enter the Unknown extension variant");
    };
    assert_eq!(type_, "web-search");
    // 回写与原 JSON 语义一致（含未知字段；键序按仓库规范序，不绑定）
    assert_eq!(
        serde_json::to_value(&block).unwrap(),
        serde_json::from_str::<serde_json::Value>(raw).unwrap()
    );
}

#[test]
fn content_block_type_vocabulary_matches_ts() {
    let known: Vec<&str> = ContentBlockType::ALL.to_vec();
    assert_eq!(known, ["text", "reasoning", "image", "tool-call", "tool-result"]);
}

#[test]
fn message_sources_roundtrip_with_ts_shape() {
    let user: MessageSource = serde_json::from_str(r#"{"kind":"user"}"#).unwrap();
    assert!(matches!(user, MessageSource::User));
    assert_eq!(serde_json::to_string(&user).unwrap(), r#"{"kind":"user"}"#);

    let tool: MessageSource =
        serde_json::from_str(r#"{"kind":"tool","callId":"c9"}"#).unwrap();
    assert_eq!(tool.as_tool().unwrap().call_id().raw(), "c9");

    let model: MessageSource = serde_json::from_str(
        r#"{"kind":"model","provider":"deepseek","model":"deepseek-chat"}"#,
    )
    .unwrap();
    assert_eq!(model.as_model().unwrap().provider, "deepseek");

    // plugin + snapshot form：form 与 sections 平铺在 source 对象顶层（…ContextFormed）
    let plugin: MessageSource = serde_json::from_str(
        r#"{"kind":"plugin","plugin":"dsh-skills","form":"snapshot","sections":[{"name":"a","text":"t"}]}"#,
    )
    .unwrap();
    let p = plugin.as_plugin().unwrap();
    assert_eq!(p.plugin(), "dsh-skills");
    let Some(crate_types::ContextForm::Snapshot { sections }) = p.form() else {
        panic!("plugin form must decode to snapshot");
    };
    assert_eq!(sections.len(), 1);

    // plugin 无 form（ContextFormed 缺省 = { form?: never }）
    let bare: MessageSource = serde_json::from_str(r#"{"kind":"plugin","plugin":"x"}"#).unwrap();
    assert!(bare.as_plugin().unwrap().form().is_none());
}

// 让测试直接用 dsh_llm 的命名空间
use dsh_llm::types as crate_types;

#[test]
fn message_source_unknown_kind_is_lossless() {
    let raw = r#"{"kind":"new-agent-kind","extra":42}"#;
    let src: MessageSource = serde_json::from_str(raw).unwrap();
    assert!(matches!(src, MessageSource::Unknown { .. }));
    assert_eq!(
        serde_json::to_value(&src).unwrap(),
        serde_json::from_str::<serde_json::Value>(raw).unwrap()
    );
}

#[test]
fn stream_chunk_variants_roundtrip() {
    let start: StreamChunk =
        serde_json::from_str(r#"{"type":"block-start","index":0,"blockType":"text"}"#).unwrap();
    assert!(matches!(start, StreamChunk::BlockStart { index: 0, .. }));
    let td: StreamChunk =
        serde_json::from_str(r#"{"type":"text-delta","index":0,"text":"he"}"#).unwrap();
    assert_eq!(td.as_delta_text().unwrap(), "he");

    let tc_delta: StreamChunk = serde_json::from_str(
        r#"{"type":"tool-call-delta","index":1,"id":"c1","name":"fs","argumentsDelta":"{\"pat"}"#,
    )
    .unwrap();
    assert_eq!(tc_delta.as_tool_call_delta_args().unwrap(), r#"{"pat"#);

    let finish: StreamChunk = serde_json::from_str(
        r#"{"type":"finish","reason":{"kind":"stop"}}"#,
    )
    .unwrap();
    assert!(matches!(
        finish,
        StreamChunk::Finish { reason: FinishReason::Stop, .. }
    ));

    let aborted: StreamChunk = serde_json::from_str(
        r#"{"type":"finish","reason":{"kind":"aborted","failure":{"message":"cancel","code":"ABORTED"}}}"#,
    )
    .unwrap();
    assert!(matches!(aborted, StreamChunk::Finish { reason: FinishReason::Aborted { .. }, .. }));
    json_eq_serialized(
        &aborted,
        r#"{"type":"finish","reason":{"kind":"aborted","failure":{"message":"cancel","code":"ABORTED"}}}"#,
    );
}

#[test]
fn stream_chunk_unknown_type_is_lossless() {
    let raw = r#"{"type":"custom-probe","payload":{"a":1}}"#;
    let chunk: StreamChunk = serde_json::from_str(raw).unwrap();
    assert!(matches!(chunk, StreamChunk::Unknown { .. }));
    assert_eq!(
        serde_json::to_value(&chunk).unwrap(),
        serde_json::from_str::<serde_json::Value>(raw).unwrap()
    );
}

#[test]
fn token_usage_and_failure_and_tool_schema_wire_shapes() {
    let usage: TokenUsage = serde_json::from_str(
        r#"{"inputTokens":10,"outputTokens":5,"cacheReadTokens":3,"reasoningTokens":2}"#,
    )
    .unwrap();
    assert_eq!(usage.input_tokens, 10);
    assert_eq!(usage.cache_read_tokens, Some(3));
    json_eq_serialized(
        &usage,
        r#"{"inputTokens":10,"outputTokens":5,"cacheReadTokens":3,"reasoningTokens":2}"#,
    );

    let failure: LlmFailure = serde_json::from_str(
        r#"{"message":"rate limited","code":"RATE_LIMITED","status":429,"providerRetryAfterMs":1000,"requestId":"r1"}"#,
    )
    .unwrap();
    assert_eq!(failure.status, Some(429));
    assert_eq!(failure.provider_retry_after_ms, Some(1000));

    let schema: ToolSchema = serde_json::from_str(
        r#"{"name":"fs_read","description":"read a file","parameters":{"type":"object","properties":{}}}"#,
    )
    .unwrap();
    assert_eq!(schema.name, "fs_read");
    assert_eq!(schema.parameters["type"], serde_json::json!("object"));
}

#[test]
fn replay_envelope_rides_finish_chunk() {
    let finish: StreamChunk = serde_json::from_str(
        r#"{"type":"finish","reason":{"kind":"stop"},"replayState":{"response":{"id":"r1"},"blocks":[{"x":1}]}}"#,
    )
    .unwrap();
    match finish {
        StreamChunk::Finish { replay_state, .. } => {
            let ReplayEnvelope { response, blocks } = replay_state.unwrap();
            assert_eq!(response, serde_json::json!({"id": "r1"}));
            assert_eq!(blocks, Some(vec![serde_json::json!({"x": 1})]));
        }
        _ => panic!("expected finish"),
    }
}

#[test]
fn messages_construct_and_narrow() {
    let assistant = Message::assistant(
        MessageId::from_raw("m1"),
        "deepseek",
        "deepseek-chat",
        vec![ContentBlock::text("hello")],
    );
    assert_eq!(assistant.role(), Role::Assistant);
    assert_eq!(assistant.source().as_model().unwrap().provider, "deepseek");
    assert!(assistant.is_assistant());

    let tool_result = Message::tool_result(
        MessageId::from_raw("m2"),
        CallId::from_raw("c1"),
        vec![ContentBlock::text("ok")],
    );
    assert_eq!(tool_result.role(), Role::User);
    assert_eq!(tool_result.source().as_tool().unwrap().call_id().raw(), "c1");
    assert!(tool_result.content[0].as_text().is_some());
}

#[test]
fn call_config_equality_semantics() {
    let a = CallConfig { provider: "deepseek".into(), model: "m".into(), ..Default::default() };
    let b = CallConfig { provider: "deepseek".into(), model: "m".into(), ..Default::default() };
    assert!(call_config::call_config_equals(&a, &b));

    // stop 列表逐元素比较
    let c = CallConfig { stop: Some(vec!["</s>".into()]), ..a.clone() };
    assert!(!call_config::call_config_equals(&a, &c));
    assert!(call_config::call_config_equals(
        &c,
        &CallConfig { stop: Some(vec!["</s>".into()]), ..a.clone() }
    ));

    // temperature/maxTokens/reasoningEffort 变化即不等
    let d = CallConfig { temperature: Some(0.7), ..a.clone() };
    assert!(!call_config::call_config_equals(&a, &d));
}

#[test]
fn generate_options_wire_uses_camel_case() {
    let json = serde_json::json!({
        "provider": "deepseek",
        "model": "deepseek-chat",
        "reasoningEffort": "high",
        "messages": [{"id":"m1","role":"user","content":[{"type":"text","text":"hi"}],"source":{"kind":"user"}}],
        "system": "sys",
        "tools": [],
        "temperature": 0.7,
        "maxTokens": 100,
        "stop": ["</s>"],
        "sessionId": "s1",
        "purpose": "compaction"
    });
    let opts: dsh_llm::types::GenerateOptions = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(opts.reasoning_effort.as_ref().map(ReasoningEffortId::raw), Some("high"));
    assert_eq!(opts.max_tokens, Some(100));
    assert_eq!(opts.session_id.as_ref().map(|s| s.raw()), Some("s1"));
    assert_eq!(opts.purpose.as_ref().map(|p| p.as_str()), Some("compaction"));
    // 往返保形状
    let back = serde_json::to_value(&opts).unwrap();
    assert_eq!(back["provider"], json["provider"]);
    assert_eq!(back["maxTokens"], json["maxTokens"]);
}

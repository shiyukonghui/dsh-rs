//! M1b 验收：`LlmRuntime` + `DeepSeekAdapter` 缝的端到端链路。
//!
//! 覆盖：注册 → resolve → stream（坐实 transport thunk）→ translate → assembler
//! 的完整语义链；以及 prepare_call 的单次派发 + 配置漂移 guard 在真实适配器上同样生效。

// LlmError 宽度是整个 dsh-llm seam 的设计（见 crates/dsh-llm/src/runtime.rs），
// transport thunk 的 Err 变体为此买单。
#![allow(clippy::result_large_err)]

use std::sync::Arc;

use dsh_llm::types::{FinishReason, GenerateOptions, Message, MessageId, TextBlock, ContentBlock};
use dsh_llm::{BlockAssembler, CallConfig, LlmRuntime};
use dsh_llm_deepseek::{
    DeepSeekAdapter, DeepSeekAdapterOptions, DeepSeekCatalogModel, DeepSeekConnection,
    PayloadsResolver, RequestDefaults,
};

fn connection() -> DeepSeekConnection {
    DeepSeekConnection {
        base_url: "https://api.deepseek.com".into(),
        defaults: RequestDefaults::default(),
        max_tokens: dsh_llm_deepseek::DEFAULT_MAX_TOKENS,
        default_context_window: dsh_llm_deepseek::DEFAULT_CONTEXT_WINDOW,
        models: vec![DeepSeekCatalogModel::new("deepseek-chat")],
        retry_policy: dsh_llm::retry::resolve_retry_policy(None, "deepseek").unwrap(),
    }
}

fn adapter_with(payloads: Vec<String>) -> Arc<dyn dsh_llm::LlmAdapter + Send + Sync> {
    let conn: Arc<dyn Fn() -> DeepSeekConnection + Send + Sync> = Arc::new(connection);
    let resolver: PayloadsResolver = Arc::new(move |_conn, _req, _ops| Ok(payloads.clone()));
    Arc::new(DeepSeekAdapter::new(DeepSeekAdapterOptions {
        resolve_connection: conn,
        resolve_payloads: resolver,
    }))
}

fn user(text: &str) -> Message {
    Message::user(MessageId::from_raw("u1"), vec![ContentBlock::Text(TextBlock { text: text.into() })])
}

fn options() -> GenerateOptions {
    GenerateOptions {
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        reasoning_effort: None,
        messages: vec![],
        system: None,
        tools: None,
        temperature: None,
        max_tokens: None,
        stop: None,
        session_id: None,
        purpose: None,
        signal: None,
    }
}

#[test]
fn runtime_stream_produces_assembled_generation() {
    let runtime = LlmRuntime::new();
    runtime.register_adapter(&["deepseek"], adapter_with(vec![
        r#"{"choices":[{"delta":{"reasoning_content":"think"}}]}"#.to_string(),
        r#"{"choices":[{"delta":{"content":"Hello,"}}]}"#.to_string(),
        r#"{"choices":[{"delta":{"content":" world"}}]}"#.to_string(),
        r#"{"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":12,"completion_tokens":4}}"#.to_string(),
        "[DONE]".to_string(),
    ])).unwrap();

    let mut options = options();
    options.messages = vec![user("hi")];

    let mut assembler = BlockAssembler::new();
    for chunk in runtime.stream(options) {
        assembler.push(chunk);
    }
    assert_eq!(assembler.blocks().len(), 2); // reasoning + text
    assert_eq!(assembler.blocks()[0].type_(), "reasoning");
    assert_eq!(assembler.blocks()[1].type_(), "text");
    assert_eq!(assembler.blocks()[1].as_text().map(|t| t.text.as_str()), Some("Hello, world"));
    let usage = assembler.usage().expect("usage reported");
    assert_eq!(usage.input_tokens, 12);
    assert_eq!(usage.output_tokens, 4);
    assert_eq!(assembler.finish(), FinishReason::Stop);
}

fn options_from_config(config: &dsh_llm::CallConfig) -> GenerateOptions {
    // 派发选项必须以已解析配置（物化了 defaultMaxTokens/defaultEffort）为准。
    GenerateOptions {
        provider: config.provider.clone(),
        model: config.model.clone(),
        reasoning_effort: config.reasoning_effort.clone(),
        messages: vec![],
        system: None,
        tools: None,
        temperature: config.temperature,
        max_tokens: config.max_tokens,
        stop: config.stop.clone(),
        session_id: None,
        purpose: None,
        signal: None,
    }
}

#[test]
fn prepare_call_single_dispatch_via_adapter() {
    let runtime = LlmRuntime::new();
    runtime.register_adapter(&["deepseek"], adapter_with(vec![
        r#"{"choices":[{"delta":{"content":"ok"}}]}"#.to_string(),
        r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#.to_string(),
        "[DONE]".to_string(),
    ])).unwrap();

    let config = CallConfig {
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        reasoning_effort: None,
        ..Default::default()
    };
    let mut prepared = runtime.prepare_call(&config).unwrap();
    assert_eq!(prepared.config.model, "deepseek-chat");
    // 必须用已解析配置（物化了 defaultMaxTokens/defaultEffort）派发。
    let resolved = prepared.config.clone();
    let mut stream = prepared.stream.take().expect("prepared stream");
    let opts = options_from_config(&resolved);
    let chunks: Vec<_> = match stream(opts) {
        Ok(iter) => iter.collect(),
        Err(err) => panic!("dispatch failed: {} ({})", err.message, err.code),
    };

    let mut assembler = BlockAssembler::new();
    for chunk in chunks {
        assembler.push(chunk);
    }
    assert_eq!(assembler.blocks()[0].as_text().map(|t| t.text.as_str()), Some("ok"));
}

#[test]
fn prepared_call_rejects_second_dispatch() {
    let runtime = LlmRuntime::new();
    runtime.register_adapter(&["deepseek"], adapter_with(vec!["[DONE]".to_string()])).unwrap();
    let config = CallConfig {
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        reasoning_effort: None,
        ..Default::default()
    };
    let mut prepared = runtime.prepare_call(&config).unwrap();
    let resolved = prepared.config.clone();
    let mut stream = prepared.stream.take().unwrap();
    let first_opts = options_from_config(&resolved);
    assert!(stream(first_opts).is_ok());
    let second = stream(options_from_config(&resolved));
    assert!(second.is_err());
    if let Err(err) = second {
        assert_eq!(err.code, "INVALID_PREPARED_CALL");
    }
}

#[test]
fn empty_response_maps_to_error_finish() {
    let runtime = LlmRuntime::new();
    runtime.register_adapter(&["deepseek"], adapter_with(vec![
        r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#.to_string(),
        "[DONE]".to_string(),
    ])).unwrap();
    let options = options();
    let mut assembler = BlockAssembler::new();
    for chunk in runtime.stream(options) {
        assembler.push(chunk);
    }
    assert!(assembler.blocks().is_empty());
    match assembler.finish() {
        FinishReason::Error { failure } => assert_eq!(failure.code, "EMPTY_RESPONSE"),
        other => panic!("expected error finish, got {other:?}"),
    }
}

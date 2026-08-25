//! M6 step5b（D-080）：真实 LLM 装配——把 dsh-core 流式 HTTP 传输 +
//! dsh-llm-deepseek 适配器（serialize/sse/translate 全在本 crate 单测覆盖）组装成
//! `LlmRuntime` 的 "deepseek" 适配器。
//!
//! 第一性原理：真实模型接入的最小契约 = 传输 + 适配。装配工厂只做两件事：
//! ① `resolve_connection` = 从装配参数解析连接事实（base_url/model/catalog/retry）；
//! ② `resolve_payloads`（transport thunk）= `dsh_core::llm_http::
//! chat_completions_stream`（POST serialize 的 WireRequest——已含 `"stream": true`）
//! → `dsh_llm_deepseek::sse::parse_sse` 解码 SSE `data:` payloads。
//!
//! key 来源仅 `DEEPSEEK_API_KEY` 环境变量（P4：key 永不入库/入配置、不入 git 历史）；
//! 无 key → 首回合 fail-loud（AUTH 明确消息），但模型发现/工具注册/API 面照常（P3）。
// LlmError（~144B）跨越 PayloadsResolver 闭包返回值是有意设计（完整 failure 供
// retry 透传），与 dsh-llm-deepseek 的 crate 级 allow 对齐。
#![allow(clippy::result_large_err)]

use std::sync::Arc;

use dsh_llm::{LlmError, LlmRuntime, StreamChunk};
use dsh_llm_deepseek::{
    http_error_code, parse_sse, DeepSeekAdapter, DeepSeekAdapterOptions, DeepSeekCatalogModel,
    DeepSeekConnection, PayloadsResolver, RequestDefaults, DEFAULT_CONTEXT_WINDOW, DEFAULT_MAX_TOKENS,
};

/// API key 环境变量名——M6 装配的**唯一** key 来源（进程环境，永不落盘）。
pub const DEEPSEEK_API_KEY_ENV: &str = "DEEPSEEK_API_KEY";

/// 从装配参数解析一次操作的连接事实。
fn deepseek_connection(base_url: &str, model: &str) -> DeepSeekConnection {
    DeepSeekConnection {
        base_url: base_url.to_string(),
        defaults: RequestDefaults::default(),
        max_tokens: DEFAULT_MAX_TOKENS,
        default_context_window: DEFAULT_CONTEXT_WINDOW,
        // catalog 至少包含装配模型（发现/能力面可用；请求不受 catalog 限制）。
        models: vec![DeepSeekCatalogModel::new(model)],
        retry_policy: dsh_llm::retry::resolve_retry_policy(None, "deepseek")
            .expect("None retry policy config is infallible (normal default)"),
    }
}

/// 装配 LlmRuntime + deepseek 适配器；key 显式传入（测试用，不进进程环境）。
/// 空/None key **不**报错——首个回合 fail-loud（诚实降级，P3）。
pub fn server_llm_runtime_with_key(base_url: &str, model: &str, key: Option<&str>) -> Arc<LlmRuntime> {
    let base_url = base_url.to_string();
    let model = model.to_string();
    let key = key.map(|s| s.to_string());
    let conn: Arc<dyn Fn() -> DeepSeekConnection + Send + Sync> = {
        let (b, m) = (base_url.clone(), model.clone());
        Arc::new(move || deepseek_connection(&b, &m))
    };
    let resolve: PayloadsResolver = Arc::new(move |_conn, wire, _opts| {
        let Some(k) = &key else {
            return Err(LlmError::new(
                format!(
                    "missing {DEEPSEEK_API_KEY_ENV}: set it to enable agent turns, then retry"
                ),
                "AUTH",
            ));
        };
        let body = serde_json::to_string(wire)
            .map_err(|e| LlmError::new(format!("request encode: {e}"), "INTERNAL"))?;
        let body_result = dsh_core::llm_http::chat_completions_stream(&base_url, Some(k.as_str()), &body)
            .map_err(|e| {
                let code = if e.status == 0 {
                    "NETWORK".to_string()
                } else {
                    http_error_code(e.status as u32, None)
                };
                LlmError::new(e.to_string(), code)
            })?;
        let payloads = parse_sse(&body_result.bytes)
            .map_err(|e| LlmError::new(format!("SSE parse: {e}"), "BAD_STREAM"))?;
        Ok(payloads)
    });
    let adapter = DeepSeekAdapter::new(DeepSeekAdapterOptions {
        resolve_connection: conn,
        resolve_payloads: resolve,
    });
    let rt = LlmRuntime::new();
    rt.register_adapter(&["deepseek"], Arc::new(adapter))
        .expect("deepseek adapter registers on a fresh runtime");
    Arc::new(rt)
}

/// 装配入口（生产）：key 仅从 `DEEPSEEK_API_KEY` 环境变量读取。
pub fn server_llm_runtime(base_url: &str, model: &str) -> Arc<LlmRuntime> {
    let key = std::env::var(DEEPSEEK_API_KEY_ENV).ok();
    server_llm_runtime_with_key(base_url, model, key.as_deref())
}

/// M6 step8（D-087）：provider caps 做实——从真实 `DeepSeekConnection.models` catalog
/// 构建 provider 目录视图（wire `llm.models` groups 消费 id/name；`caps` 增量附加
/// contextWindow/maxTokens/inputModalities + 默认容量 + 重试策略）。诚实：只列真实
/// catalog 条目（装配模型精确值缺省时以 defaults 为准，不伪造容量）。
pub fn server_catalog_view(base_url: &str, model: &str) -> serde_json::Value {
    use dsh_llm::retry::ResolvedRetryPolicy;
    let conn = deepseek_connection(base_url, model);
    let models: Vec<serde_json::Value> = conn
        .models
        .iter()
        .map(|m| {
            let mut o = serde_json::Map::new();
            o.insert("id".into(), serde_json::Value::String(m.id.clone()));
            if let Some(n) = &m.name {
                o.insert("name".into(), serde_json::Value::String(n.clone()));
            }
            if let Some(d) = &m.description {
                o.insert("description".into(), serde_json::Value::String(d.clone()));
            }
            if let Some(c) = m.context_window {
                o.insert("contextWindow".into(), serde_json::Value::from(c));
            }
            if let Some(t) = m.max_tokens {
                o.insert("maxTokens".into(), serde_json::Value::from(t));
            }
            if let Some(mods) = &m.input_modalities {
                o.insert(
                    "inputModalities".into(),
                    serde_json::Value::Array(
                        mods.iter()
                            .map(|x| serde_json::Value::String(format!("{x:?}").to_lowercase()))
                            .collect(),
                    ),
                );
            }
            serde_json::Value::Object(o)
        })
        .collect();
    let mut defaults = serde_json::Map::new();
    defaults.insert("contextWindow".into(), serde_json::Value::from(conn.default_context_window));
    defaults.insert("maxTokens".into(), serde_json::Value::from(conn.max_tokens));
    if let Some(t) = &conn.defaults.thinking {
        defaults.insert(
            "thinking".into(),
            serde_json::Value::String(format!("{t:?}").to_lowercase()),
        );
    }
    if let Some(e) = &conn.defaults.reasoning_effort {
        defaults.insert(
            "reasoningEffort".into(),
            serde_json::Value::String(format!("{e:?}").to_lowercase()),
        );
    }
    // 重试策略视图（真实 ResolvedRetryPolicy：模式 + 上限 + 退避）。
    let retry = match &conn.retry_policy {
        ResolvedRetryPolicy::Normal(n) => serde_json::json!({
            "mode": "normal",
            "maxRetries": n.max_retries,
            "retryableCodes": n.retryable_codes,
            "backoff": {"initialDelayMs": n.backoff.initial_delay_ms, "maxDelayMs": n.backoff.max_delay_ms, "jitterRatio": n.backoff.jitter_ratio},
        }),
        ResolvedRetryPolicy::Always(a) => serde_json::json!({
            "mode": "always",
            "backoff": {"initialDelayMs": a.backoff.initial_delay_ms, "maxDelayMs": a.backoff.max_delay_ms, "jitterRatio": a.backoff.jitter_ratio},
        }),
    };
    serde_json::json!({
        "provider": "deepseek",
        "models": models,
        "defaults": serde_json::Value::Object(defaults),
        "retry": retry,
    })
}

/// 驱动一轮 stream 的辅助（测试用）：provider/model 缺省 deepseek + 装配模型。
pub fn stream_once(runtime: &LlmRuntime, model: &str) -> Vec<StreamChunk> {
    let options = dsh_llm::types::GenerateOptions {
        provider: "deepseek".into(),
        model: model.to_string(),
        messages: vec![],
        system: None,
        tools: None,
        reasoning_effort: None,
        temperature: None,
        max_tokens: None,
        stop: None,
        session_id: None,
        purpose: None,
    };
    runtime.stream(options).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// 本地一次性 SSE 服务端：读取请求 → 断言 Bearer → 写 SSE 响应 → 关闭。
    fn serve_sse(response_body: &[u8]) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let body = response_body.to_vec();
        let handle = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                match sock.read(&mut tmp) {
                    Ok(0) => break,
                    Ok(n) => buf.extend_from_slice(&tmp[..n]),
                    Err(_) => break,
                }
            }
            sock.set_read_timeout(Some(std::time::Duration::from_millis(200))).ok();
            loop {
                match sock.read(&mut tmp) {
                    Ok(0) => break,
                    Ok(n) => buf.extend_from_slice(&tmp[..n]),
                    Err(_) => break,
                }
            }
            let mut out = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            out.push_str(&String::from_utf8_lossy(&body));
            sock.write_all(out.as_bytes()).unwrap();
            sock.flush().unwrap();
            let _ = sock;
            String::from_utf8_lossy(&buf).to_string()
        });
        (format!("http://127.0.0.1:{port}"), handle)
    }

    /// M6i 验收 #6：真实 deepseek 适配器经本地流式端点到 StreamChunk（text 块）。
    /// （SSE → translate 契约由 dsh-llm-deepseek 单测覆盖；此处验证 thunk 桥全链路。）
    #[test]
    fn server_llm_runtime_streams_text_from_local_endpoint() {
        let model = "deepseek-v4-flash-0731-ext";
        let sse = concat!(
            r#"data: {"choices":[{"delta":{"content":"hi from m6"}}]}"#, "\n\n",
            r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#, "\n\n",
            "data: [DONE]", "\n\n",
        );
        let (base, handle) = serve_sse(sse.as_bytes());
        let rt = server_llm_runtime_with_key(&base, model, Some("K"));
        assert_eq!(rt.list_models("deepseek").unwrap().len(), 1, "catalog listed");
        let chunks = stream_once(&rt, model);
        let mut assembler = dsh_llm::BlockAssembler::new();
        for chunk in &chunks {
            assembler.push(chunk.clone());
        }
        assert_eq!(assembler.blocks().len(), 1, "one text block");
        assert_eq!(assembler.blocks()[0].type_(), "text");
        assert_eq!(
            assembler.finish(),
            dsh_llm::types::FinishReason::Stop,
            "finish stop"
        );
        // thunk 真实 POST：请求带 key + 流式 body。
        let req = handle.join().unwrap();
        assert!(req.contains("Authorization: Bearer K"), "Bearer from env-derived key");
        assert!(req.contains("\"stream\":true"), "stream request body");
    }

    /// P3 诚实降级：无 key → 首回合 fail-loud（AUTH 明确消息），但 list_models 照常。
    #[test]
    fn server_llm_runtime_without_key_fails_loud_but_catalog_stays() {
        let model = "deepseek-v4-flash-0731-ext";
        let rt = server_llm_runtime_with_key("http://127.0.0.1:1", model, None);
        assert_eq!(rt.list_models("deepseek").unwrap().len(), 1, "models discovery unaffected");
        let chunks = stream_once(&rt, model);
        match chunks.first() {
            Some(dsh_llm::types::StreamChunk::Finish {
                reason: dsh_llm::types::FinishReason::Error { .. },
                ..
            }) => {}
            other => panic!("expected error finish (fail-loud), got {other:?}"),
        }
        // 消息含环境变量名（引导排查），code 不含敏感信息。
        let msg = match chunks.first() {
            Some(dsh_llm::types::StreamChunk::Finish {
                reason: dsh_llm::types::FinishReason::Error { failure, .. },
                ..
            }) => failure.message.clone(),
            _ => String::new(),
        };
        assert!(
            msg.contains(DEEPSEEK_API_KEY_ENV),
            "clear message mentions env var: {msg}"
        );
    }

    /// 认证失败（4xx）→ 结构化 LlmError（AUTH 码）经适配器暴露为 Error finish。
    #[test]
    fn server_llm_runtime_maps_http_auth_to_error_finish() {
        let model = "deepseek-v4-flash-0731-ext";
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut tmp = [0u8; 1024];
            let _ = sock.read(&mut tmp);
            let out = "HTTP/1.1 401 Unauthorized\r\nContent-Length: 14\r\nConnection: close\r\n\r\n{\"error\":\"no\"}";
            let _ = sock.write_all(out.as_bytes());
            let _ = sock.flush();
            let _ = sock;
        });
        let rt = server_llm_runtime_with_key(&format!("http://127.0.0.1:{port}"), model, Some("bad"));
        let chunks = stream_once(&rt, model);
        match chunks.first() {
            Some(dsh_llm::types::StreamChunk::Finish {
                reason: dsh_llm::types::FinishReason::Error { failure, .. },
                ..
            }) => {
                assert_eq!(failure.code, "AUTH", "401 → AUTH code");
                assert!(failure.message.contains("HTTP 401"), "msg: {}", failure.message);
            }
            other => panic!("expected auth error finish, got {other:?}"),
        }
    }

    /// M6 step8（D-087）：provider caps 做实——`server_catalog_view` 从真实
    /// `DeepSeekConnection.models` catalog 列录（含容量默认/重试/模式），不伪造。
    #[test]
    fn server_catalog_view_lists_real_deepseek_caps() {
        let model = "deepseek-v4-flash-0731-ext";
        let view = server_catalog_view("http://127.0.0.1:1", model);
        assert_eq!(view["provider"], "deepseek");
        let models = view["models"].as_array().expect("models array");
        assert!(!models.is_empty(), "catalog lists the assembled model");
        assert_eq!(models[0]["id"], model, "real model id from catalog");
        // 容量默认（catalog 精确值缺省时正面值；装配模型条目无精确值 → 用默认）。
        let defaults = &view["defaults"];
        assert!(
            defaults["contextWindow"].as_u64().unwrap_or(0) > 0,
            "default context window present"
        );
        assert!(
            defaults["maxTokens"].as_u64().unwrap_or(0) > 0,
            "default max tokens present"
        );
        // 重试策略（真实 ResolvedRetryPolicy 视图：模式 + 上限）。
        assert!(
            view["retry"]["mode"].as_str().is_some(),
            "retry mode present: {}",
            view["retry"]
        );
    }
}

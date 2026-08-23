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

use std::rc::Rc;

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
pub fn server_llm_runtime_with_key(base_url: &str, model: &str, key: Option<&str>) -> Rc<LlmRuntime> {
    let base_url = base_url.to_string();
    let model = model.to_string();
    let key = key.map(|s| s.to_string());
    let conn: Rc<dyn Fn() -> DeepSeekConnection> = {
        let (b, m) = (base_url.clone(), model.clone());
        Rc::new(move || deepseek_connection(&b, &m))
    };
    let resolve: PayloadsResolver = Rc::new(move |_conn, wire, _opts| {
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
    rt.register_adapter(&["deepseek"], Rc::new(adapter))
        .expect("deepseek adapter registers on a fresh runtime");
    Rc::new(rt)
}

/// 装配入口（生产）：key 仅从 `DEEPSEEK_API_KEY` 环境变量读取。
pub fn server_llm_runtime(base_url: &str, model: &str) -> Rc<LlmRuntime> {
    let key = std::env::var(DEEPSEEK_API_KEY_ENV).ok();
    server_llm_runtime_with_key(base_url, model, key.as_deref())
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
}

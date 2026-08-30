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
    DeepSeekConnection, Effort, PayloadsResolver, RequestDefaults, Thinking,
    DEFAULT_CONTEXT_WINDOW, DEFAULT_MAX_TOKENS,
};

/// API key 环境变量名——M6 装配的**唯一** key 来源（进程环境，永不落盘）。
pub const DEEPSEEK_API_KEY_ENV: &str = "DEEPSEEK_API_KEY";

/// D-221：卡面 provider 设置的共享权威（与 RemoteHost kv **同一 store**）。
/// llm-deepseek 卡 save → kv["llm-deepseek/settings"]；连接闭包每调用现读=热生效。
pub type LlmSharedKv =
    std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, serde_json::Value>>>;

/// 与 wasm-plugins/llm-deepseek 的 KV_SETTINGS_KEY 逐字一致（双端契约）。
pub const KV_LLM_DEEPSEEK: &str = "llm-deepseek/settings";

fn apply_effort(d: &mut RequestDefaults, e: &str) {
    d.reasoning_effort = match e {
        "off" => Some(Effort::Off),
        "low" => Some(Effort::Low),
        "high" => Some(Effort::High),
        "max" => Some(Effort::Max),
        _ => d.reasoning_effort,
    };
}

/// D-221：有效 provider 配置（每次调用 live 合并）——装配参基址 ← env 缺省
/// （`DSH_LLM_EFFORT`，D-219）← 卡面 kv 覆盖（baseURL/reasoningEffort/thinking）。
pub fn provider_cfg(base_url: &str, kv: Option<&LlmSharedKv>) -> (String, RequestDefaults) {
    let mut defaults = RequestDefaults::default();
    if let Ok(e) = std::env::var("DSH_LLM_EFFORT") {
        apply_effort(&mut defaults, &e);
    }
    let mut base = base_url.to_string();
    if let Some(kv) = kv {
        if let Ok(g) = kv.lock() {
            if let Some(v) = g.get(KV_LLM_DEEPSEEK) {
                if let Some(b) = v.get("baseURL").and_then(|s| s.as_str()) {
                    if !b.trim().is_empty() {
                        base = b.to_string();
                    }
                }
                if let Some(e) = v.get("reasoningEffort").and_then(|s| s.as_str()) {
                    apply_effort(&mut defaults, e);
                }
                match v.get("thinking").and_then(|s| s.as_str()) {
                    Some("disabled") => defaults.thinking = Some(Thinking::Disabled),
                    Some("enabled") => defaults.thinking = Some(Thinking::Enabled),
                    _ => {}
                }
            }
        }
    }
    (base, defaults)
}

/// 从装配参数解析一次操作的连接事实（env 缺省；无共享 kv 的旧形态）。
fn deepseek_connection(base_url: &str, model: &str) -> DeepSeekConnection {
    let (eff_base, defaults) = provider_cfg(base_url, None);
    connection_with(&eff_base, model, defaults)
}

fn connection_with(base_url: &str, model: &str, defaults: RequestDefaults) -> DeepSeekConnection {
    DeepSeekConnection {
        base_url: base_url.to_string(),
        defaults,
        max_tokens: DEFAULT_MAX_TOKENS,
        default_context_window: DEFAULT_CONTEXT_WINDOW,
        // catalog 至少包含装配模型（发现/能力面可用；请求不受 catalog 限制）。
        models: vec![DeepSeekCatalogModel::new(model)],
        retry_policy: dsh_llm::retry::resolve_retry_policy(None, "deepseek")
            .expect("None retry policy config is infallible (normal default)"),
    }
}

#[cfg(test)]
mod provider_cfg_tests {
    use super::*;

    #[test]
    fn kv_overrides_base_url_effort_and_thinking() {
        let kv: LlmSharedKv = std::sync::Arc::new(std::sync::Mutex::new(
            std::collections::HashMap::new(),
        ));
        kv.lock().unwrap().insert(
            KV_LLM_DEEPSEEK.into(),
            serde_json::json!({ "baseURL": "http://127.0.0.1:9/v1", "reasoningEffort": "low", "thinking": "disabled" }),
        );
        let (base, d) = provider_cfg("http://flag:1/v1", Some(&kv));
        assert_eq!(base, "http://127.0.0.1:9/v1", "卡面 baseURL 热覆盖装配基址");
        assert_eq!(d.reasoning_effort, Some(Effort::Low));
        assert_eq!(d.thinking, Some(Thinking::Disabled));
    }

    #[test]
    fn empty_or_unknown_kv_values_are_ignored() {
        let kv: LlmSharedKv = std::sync::Arc::new(std::sync::Mutex::new(
            std::collections::HashMap::new(),
        ));
        kv.lock().unwrap().insert(
            KV_LLM_DEEPSEEK.into(),
            serde_json::json!({ "baseURL": "  ", "reasoningEffort": "ultra" }),
        );
        let (base, d) = provider_cfg("http://flag:1/v1", Some(&kv));
        assert_eq!(base, "http://flag:1/v1", "空 baseURL 不覆盖");
        assert_eq!(d.reasoning_effort, None, "未知 effort 值不生效（诚实缺省）");
    }
}

/// 装配 LlmRuntime + deepseek 适配器；key 显式传入（测试用，不进进程环境）。
/// 空/None key **不**报错——首个回合 fail-loud（诚实降级，P3）。
/// 旧形态（无共享 kv）：保持既有调用方契约不变。
pub fn server_llm_runtime_with_key(base_url: &str, model: &str, key: Option<&str>) -> Arc<LlmRuntime> {
    server_llm_runtime_shared_kv(base_url, model, key, None)
}

/// D-221：共享 kv 形态——卡面 baseURL/reasoningEffort/thinking **live 覆盖**
/// （连接与传输每调用现读 `provider_cfg`，保存即热生效，无需重启）。
pub fn server_llm_runtime_shared_kv(
    base_url: &str,
    model: &str,
    key: Option<&str>,
    kv: Option<LlmSharedKv>,
) -> Arc<LlmRuntime> {
    let base_url = base_url.to_string();
    let model = model.to_string();
    let key = key.map(|s| s.to_string());
    let conn: Arc<dyn Fn() -> DeepSeekConnection + Send + Sync> = {
        let (b, m, kv) = (base_url.clone(), model.clone(), kv.clone());
        Arc::new(move || {
            let (eff, defaults) = provider_cfg(&b, kv.as_ref());
            connection_with(&eff, &m, defaults)
        })
    };
    let resolve: PayloadsResolver = Arc::new(move |_conn, wire, opts| {
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
        // D-115（Phase 4）：请求级取消 → 可中断读。cancel 谓词穿透 `options.signal`
        // （worker/accept 线程经共享令牌轮询）；置位 → 主动断开在途阻塞读（对齐 TS
        // `fetch(url, {signal})`；abort 不是传输错误，以 `Aborted` finish 归一）。
        // owned 闭包托住 `s` 的 Clone；取引用传调用方，owned 存活至本函数结束。
        let _cancel_owned: Option<Box<dyn Fn() -> bool + Send + Sync>> = opts
            .signal
            .as_ref()
            .map(|s| -> Box<dyn Fn() -> bool + Send + Sync> {
                let s = s.clone();
                Box::new(move || s.aborted())
            });
        let cancel: Option<&(dyn Fn() -> bool + Send + Sync)> = _cancel_owned.as_deref();
        // D-221：传输基址每调用现读（卡面 baseURL 热覆盖同权威）。
        let (eff_base, _) = provider_cfg(&base_url, kv.as_ref());
        let body_result = dsh_core::llm_http::chat_completions_stream_abortable(&eff_base, Some(k.as_str()), &body, cancel)
            .map_err(|e| {
                let code = if e.status == 0 {
                    "NETWORK".to_string()
                } else {
                    http_error_code(e.status as u32, None)
                };
                LlmError::new(e.to_string(), code)
            })?;
        if body_result.aborted {
            // 已取消：不留 partial 给解析器（SSE 可能未闭合）——返回空 payload，
            // 由适配器按 `FinishReason::Aborted` 归一（对齐 TS ABORTED finish）。
            return Ok(Vec::new());
        }
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
        signal: None,
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

    /// 慢速服务端：写响应头（Content-Length 声明远大于实际）+ 首段 SSE 后挂起
    /// 保持连接（模拟长生成流；客户端读端被 200ms 短超时轮询 cancel）。
    fn serve_slow_hang() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
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
            let sse = r#"data: {"choices":[{"delta":{"content":"first"}}]}"#;
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 100000000\r\nConnection: close\r\n\r\n{sse}\n\n"
            );
            sock.write_all(header.as_bytes()).unwrap();
            sock.flush().unwrap();
            // 挂起不关闭：客户端不能借 Content-Length/close 结束，只能靠 cancel 谓词。
            std::thread::sleep(std::time::Duration::from_millis(2000));
            let _ = sock;
        });
        format!("http://127.0.0.1:{port}")
    }

    /// D-115（Phase 4 传输中断）：长生成（慢速流挂起）中置 cancel 信号 →
    /// 阻塞读被打断 → 适配器以 `Aborted` finish 归一，且调用**及时返回**
    /// （不等待服务端 2s 挂起 / 原 30s 超时）。
    #[test]
    fn server_llm_runtime_aborts_slow_stream_on_signal() {
        let model = "deepseek-v4-flash-0731-ext";
        let base = serve_slow_hang();
        let rt = server_llm_runtime_with_key(&base, model, Some("K"));
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let setter = flag.clone();
        // 60ms 后置 cancel（首个读超时窗口内醒来轮询谓词）。
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(60));
            setter.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        let mut options = dsh_llm::types::GenerateOptions {
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
            signal: None,
        };
        let f = flag.clone();
        options.signal = Some(dsh_llm::AbortSignal::new(move || f.load(std::sync::atomic::Ordering::SeqCst)));
        let start = std::time::Instant::now();
        let chunks: Vec<dsh_llm::types::StreamChunk> = rt.stream(options).collect();
        let took = start.elapsed();
        assert!(
            took < std::time::Duration::from_secs(5),
            "signal must abort the blocking read well before the 2s hang / 30s timeout; took {took:?}"
        );
        match chunks.first() {
            Some(dsh_llm::types::StreamChunk::Finish {
                reason: dsh_llm::types::FinishReason::Aborted { failure },
                ..
            }) => {
                assert_eq!(failure.code, "ABORTED", "aborted finish surfaced to chunks");
            }
            other => panic!("expected Aborted finish, got {other:?}"),
        }
    }
}

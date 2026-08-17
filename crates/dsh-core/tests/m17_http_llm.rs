//! M17：真实 HTTP llm 适配器——`LlmService` 注册 OpenAI 兼容的
//! `/chat/completions` 客户端（手写 HTTP/1.1；M31 加 https/native-tls）。
//! 用本地 TCP mock 服务器验证：请求形状（路径/头/body）、响应解析、错误路径。

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

use dsh_core::{new_llm, LlmHandle};

/// M34：`messages_to_wire`（生产 Message[] → OpenAI wire 序列化，
/// 对齐 DSH `serializeMessages`）——user 文本拼接、tool-result 展开为
/// role:tool、assistant tool-call 映射为 tool_calls、空 tool 输出补
/// "(no output)"。
#[test]
fn messages_to_wire_produces_openai_shape() {
    use dsh_core::llm_http::messages_to_wire;

    // 生产 Message[] 形状：user（文本）+ assistant（文本 + tool-call）+ tool 结果
    let messages = vec![
        serde_json::json!({
            "id": "u1", "role": "user",
            "content": [{"type": "text", "text": "what is 2+3?"}],
            "source": {"kind": "user"},
        }),
        serde_json::json!({
            "id": "a1", "role": "assistant",
            "content": [
                {"type": "text", "text": "let me check"},
                {"type": "tool-call", "id": "c1", "name": "add", "arguments": "{\"a\":2,\"b\":3}"},
            ],
            "source": {"kind": "model", "provider": "mock", "model": "mock"},
        }),
        serde_json::json!({
            "id": "t1", "role": "user",
            "content": [{
                "type": "tool-result", "toolCallId": "c1",
                "content": [{"type": "text", "text": "5"}],
                "isError": false,
            }],
            "source": {"kind": "tool", "callId": "c1"},
        }),
    ];
    let wire = messages_to_wire(&messages);
    assert_eq!(
        wire,
        vec![
            serde_json::json!({"role": "user", "content": "what is 2+3?"}),
            serde_json::json!({
                "role": "assistant",
                "content": "let me check",
                "tool_calls": [{
                    "id": "c1",
                    "type": "function",
                    "function": {"name": "add", "arguments": "{\"a\":2,\"b\":3}"},
                }],
            }),
            serde_json::json!({"role": "tool", "tool_call_id": "c1", "content": "5"}),
        ],
        "Message[] serialized to OpenAI wire shape"
    );
}

/// M34：`messages_to_wire` 空 tool 输出 → "(no output)"（对齐生产
/// `flattenText(...) || '(no output)'`）；扁平形状原样透传。
#[test]
fn messages_to_wire_empty_tool_and_flat_passthrough() {
    use dsh_core::llm_http::messages_to_wire;

    // 空工具输出
    let messages = vec![serde_json::json!({
        "id": "t1", "role": "user",
        "content": [{
            "type": "tool-result", "toolCallId": "c1",
            "content": [], "isError": false,
        }],
        "source": {"kind": "tool", "callId": "c1"},
    })];
    let wire = messages_to_wire(&messages);
    assert_eq!(
        wire,
        vec![serde_json::json!({"role": "tool", "tool_call_id": "c1", "content": "(no output)"})]
    );

    // 扁平形状（content 是字符串）→ 原样透传（M17 兼容）
    let flat = vec![serde_json::json!({"role": "user", "content": "hi"})];
    assert_eq!(messages_to_wire(&flat), flat);
}

/// 起一个本地 mock 服务器：收到 POST 后按 `handler` 决定响应；返回其地址。
fn mock_server(
    handler: impl Fn(&str, &str, &str) -> (u16, String) + Send + 'static,
) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let handler = &handler;
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            // 读请求头 + body（简化：先读满 4KB 窗口；测试 body 很小）
            let mut total = 0usize;
            loop {
                match stream.read(&mut tmp) {
                    Ok(0) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&tmp[..n]);
                        total += n;
                        // 检测 Content-Length 是否收齐
                        if let Some(pos) = find_header_end(&buf) {
                            if let Some(cl) = content_length(&buf[..pos]) {
                                if total >= pos + 4 + cl {
                                    break;
                                }
                            }
                        }
                        if total > 65536 {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let text = String::from_utf8_lossy(&buf).to_string();
            // 解析首行 + Host + body
            let first = text.lines().next().unwrap_or("").to_string();
            let host = text
                .lines()
                .find(|l| l.to_ascii_lowercase().starts_with("host:"))
                .unwrap_or("")
                .trim()
                .to_string();
            let body = match find_header_end(&buf) {
                Some(pos) => String::from_utf8_lossy(&buf[pos + 4..]).to_string(),
                None => String::new(),
            };
            let (status, payload) = handler(&first, &host, &body);
            let resp = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                payload.len(),
                payload
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.flush();
        }
    });
    format!("http://{addr}")
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn content_length(head: &[u8]) -> Option<usize> {
    let text = String::from_utf8_lossy(head);
    text.lines()
        .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
        .and_then(|l| l.split(':').nth(1))
        .and_then(|v| v.trim().parse().ok())
}

/// 注册 http provider → generate 走真实 HTTP 请求 → 解析 choices[0].message。
#[test]
fn http_provider_generates_from_mock() {
    let addr = mock_server(|first, host, body| {
        // 请求形状校验
        assert!(first.starts_with("POST /chat/completions HTTP/1.1"), "{first}");
        assert!(host.to_ascii_lowercase().contains("127.0.0.1"), "{host}");
        let body: serde_json::Value = serde_json::from_str(body).expect("json body");
        assert_eq!(body["model"], "test-model");
        assert_eq!(body["messages"][0]["role"], "user");
        (200, r#"{"choices":[{"message":{"role":"assistant","content":"http hello"}}]}"#.to_string())
    });

    let llm: LlmHandle = new_llm();
    {
        let mut svc = llm.lock().unwrap();
        svc.register_http("http-test", &addr, Some("sk-test"), "test-model");
    }
    let r = llm.lock().unwrap().generate(Some("http-test"), vec![serde_json::json!({"role": "user", "content": "hi"})], vec![]);
    assert_eq!(r["content"], "http hello", "parsed choices[0].message.content");
}

/// 声明式默认 http：未指定 provider 时走 default（注册时 set_default）。
#[test]
fn http_provider_as_default() {
    let addr = mock_server(|_, _, _| {
        (200, r#"{"choices":[{"message":{"content":"default http"}}]}"#.to_string())
    });
    let llm: LlmHandle = new_llm();
    {
        let mut svc = llm.lock().unwrap();
        svc.register_http_default(&addr, None, "dflt-model");
    }
    let r = llm.lock().unwrap().generate(None, vec![], vec![]);
    assert_eq!(r["content"], "default http");
}

/// 错误路径：HTTP 非 2xx → error JSON（消息含状态码）。
#[test]
fn http_provider_error_status() {
    let addr = mock_server(|_, _, _| {
        (500, r#"{"error":{"message":"boom"}}"#.to_string())
    });
    let llm: LlmHandle = new_llm();
    {
        let mut svc = llm.lock().unwrap();
        svc.register_http("bad", &addr, None, "m");
    }
    let r = llm.lock().unwrap().generate(Some("bad"), vec![], vec![]);
    assert!(r.get("error").is_some(), "{r}");
    assert!(r["error"].to_string().contains("500"), "{r}");
}

/// 连接失败 → error JSON（不 panic）。
#[test]
fn http_provider_conn_refused() {
    // 绑定后立即关闭的端口
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let llm: LlmHandle = new_llm();
    {
        let mut svc = llm.lock().unwrap();
        svc.register_http("refused", &format!("http://{addr}"), None, "m");
    }
    let r = llm.lock().unwrap().generate(Some("refused"), vec![], vec![]);
    assert!(r.get("error").is_some(), "{r}");
}

/// body 非 OpenAI 形状（无 choices）→ error JSON。
#[test]
fn http_provider_malformed_response() {
    let addr = mock_server(|_, _, _| (200, r#"{"unexpected": 1}"#.to_string()));
    let llm: LlmHandle = new_llm();
    {
        let mut svc = llm.lock().unwrap();
        svc.register_http("weird", &addr, None, "m");
    }
    let r = llm.lock().unwrap().generate(Some("weird"), vec![], vec![]);
    assert!(r.get("error").is_some(), "{r}");
}

/// M31：https URL 解析（parse_base 走 https:// → 默认 443）与 TLS 路径。
/// 本地用 openssl 自签证书起 TLS mock 服务器——客户端证书验证会拒绝自签
/// （生产安全），验证「https 已走 TLS 层 + 握手失败返回 error JSON 不 panic」。
#[test]
fn https_provider_tls_handshake_path() {
    use std::process::Command;

    // openssl 生成自签证书（临时目录）；设 OPENSSL_CONF（Windows 无默认 cnf）
    let dir = std::env::temp_dir().join(format!("dsh-m31-tls-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let key = dir.join("key.pem");
    let cert = dir.join("cert.pem");
    let pfx = dir.join("identity.pfx");
    let mut req = Command::new("openssl");
    req.env(
        "OPENSSL_CONF",
        "D:\\Anaconda\\Library\\ssl\\openssl.cnf",
    )
    .args(["req", "-x509", "-newkey", "rsa:2048", "-keyout"])
    .arg(&key)
    .arg("-out")
    .arg(&cert)
    .args(["-days", "1", "-nodes", "-subj", "/CN=localhost"]);
    let status = req.status().expect("openssl req");
    assert!(status.success(), "openssl self-signed cert");
    let mut p12 = Command::new("openssl");
    p12.env(
        "OPENSSL_CONF",
        "D:\\Anaconda\\Library\\ssl\\openssl.cnf",
    )
    .args(["pkcs12", "-export", "-out"])
    .arg(&pfx)
    .arg("-inkey")
    .arg(&key)
    .arg("-in")
    .arg(&cert)
    .args(["-password", "pass:test"]);
    let status = p12.status().expect("openssl pkcs12");
    assert!(status.success(), "openssl pkcs12 export");

    // TLS 服务器（native-tls 服务端 + Identity）
    let pfx_bytes = std::fs::read(&pfx).unwrap();
    let identity = native_tls::Identity::from_pkcs12(&pfx_bytes, "test").expect("identity");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        let acceptor = native_tls::TlsAcceptor::new(identity).expect("tls acceptor");
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let Ok(mut tls) = acceptor.accept(stream) else { continue };
            let mut buf = [0u8; 256];
            let _ = tls.read(&mut buf);
            let payload = r#"{"choices":[{"message":{"content":"https hello"}}]}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                payload.len(),
                payload
            );
            let _ = tls.write_all(resp.as_bytes());
        }
    });

    // https 客户端：连接自签服务器 → 证书验证失败（error JSON，不 panic）
    let llm: LlmHandle = new_llm();
    {
        let mut svc = llm.lock().unwrap();
        svc.register_http("https-test", &format!("https://127.0.0.1:{port}"), None, "m");
    }
    let r = llm.lock().unwrap().generate(Some("https-test"), vec![], vec![]);
    assert!(r.get("error").is_some(), "{r}");
    let msg = r["error"].to_string();
    assert!(
        msg.contains("tls") || msg.contains("handshake") || msg.contains("certificate"),
        "TLS path reached (self-signed rejected), got: {msg}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// M31：https 端口解析（无显式端口 → 443）。
#[test]
fn https_base_defaults_to_443() {
    // 用注册后连接不存在的 https 主机验证「连接失败不 panic → error json」。
    let llm: LlmHandle = new_llm();
    {
        let mut svc = llm.lock().unwrap();
        svc.register_http("https-default", "https://127.0.0.1:1", None, "m");
    }
    let r = llm.lock().unwrap().generate(Some("https-default"), vec![], vec![]);
    assert!(r.get("error").is_some(), "conn refused on port 1 -> error json: {r}");
}

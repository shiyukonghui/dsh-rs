//! DSH 层服务插件：经 loader 配置挂载 session/tools/llm 服务。
//!
//! 第一性原理：缝的承载（SessionLog/ToolRegistry/LlmService）是宿主基础设施；
//! 本插件把它们**按配置**注册为 Cordis 服务，供 WASM loop 插件经 `LoopHost`
//! 桥接使用。
//!
//! 配置（cordis.yml 的 services entry `config`）：
//! - `services: ["sessions","tools","llm"]`——注册子集（默认全注册）；
//! - `tools: [{name, op}]`——**声明式工具**（op ∈ add/multiply/echo；启动器按
//!   配置注册，不再代码注册）；
//! - `llm: {provider, model, behavior}`——**声明式 llm 适配器**（behavior ∈
//!   tool-first/echo；provider 名 + 行为注册进 LlmService，供 loop 的 llm 缝
//!   按 provider 选择）。

use std::sync::Arc;

use dsh_core::*;

/// DSH 服务插件：apply 时提供 sessions/tools/llm 服务（可配置子集 + 声明式工具/llm）。
pub struct DshServicesPlugin {
    sessions: Option<SessionHandle>,
    tools: Option<ToolRegistryHandle>,
    llm: Option<LlmHandle>,
}

impl DshServicesPlugin {
    /// 构造服务插件（None 的服务不注册）。
    pub fn new(
        sessions: Option<SessionHandle>,
        tools: Option<ToolRegistryHandle>,
        llm: Option<LlmHandle>,
    ) -> Self {
        DshServicesPlugin {
            sessions,
            tools,
            llm,
        }
    }

    /// 默认构造：全部服务。
    pub fn all() -> Self {
        DshServicesPlugin::new(
            Some(new_session()),
            Some(new_tool_registry()),
            Some(new_llm()),
        )
    }

    /// 按声明式配置注册工具（`config.tools: [{name, op}]`）。
    fn register_declared_tools(&self, config: &Value) {
        let Some(tools) = &self.tools else { return };
        let Some(list) = config.get("tools").and_then(|v| v.as_array()) else {
            return;
        };
        let mut reg = tools.lock().unwrap();
        for item in list {
            let Some(name) = item.get("name").and_then(|v| v.as_str()) else {
                continue;
            };
            let op = item.get("op").and_then(|v| v.as_str()).unwrap_or("echo");
            let name_owned = name.to_string();
            match op {
                "add" => {
                    reg.register(name, move |args| {
                        let a = args.get("a").and_then(|v| v.as_i64()).unwrap_or(0);
                        let b = args.get("b").and_then(|v| v.as_i64()).unwrap_or(0);
                        serde_json::json!({"sum": a + b})
                    });
                }
                "multiply" => {
                    reg.register(name, move |args| {
                        let a = args.get("a").and_then(|v| v.as_i64()).unwrap_or(0);
                        let b = args.get("b").and_then(|v| v.as_i64()).unwrap_or(0);
                        serde_json::json!({"product": a * b})
                    });
                }
                _ => {
                    let name2 = name_owned.clone();
                    reg.register(name, move |args| {
                        serde_json::json!({"echo": args, "tool": name2})
                    });
                }
            }
        }
    }

    /// 按声明式配置注册 llm 适配器（`config.llm: {provider, behavior}`）。
    fn register_declared_llm(&self, config: &Value) {
        let Some(llm) = &self.llm else { return };
        let Some(llm_cfg) = config.get("llm").and_then(|v| v.as_object()) else {
            return;
        };
        let provider = llm_cfg
            .get("provider")
            .and_then(|v| v.as_str())
            .unwrap_or("default");
        // `llm.http: {base, api_key?, model}` —— 真实 HTTP 适配器（M17：
        // OpenAI 兼容 /chat/completions，手写 HTTP/1.1 客户端）。
        if let Some(http) = llm_cfg.get("http").and_then(|v| v.as_object()) {
            let Some(base) = http.get("base").and_then(|v| v.as_str()) else {
                return;
            };
            let api_key = http.get("api_key").and_then(|v| v.as_str());
            let model = http
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or("default");
            let mut svc = llm.lock().unwrap();
            if provider == "default" {
                svc.register_http_default(base, api_key, model);
            } else {
                svc.register_http(provider, base, api_key, model);
            }
            return;
        }
        let behavior = llm_cfg
            .get("behavior")
            .and_then(|v| v.as_str())
            .unwrap_or("echo");
        let provider_owned = provider.to_string();
        let mut svc = llm.lock().unwrap();
        match behavior {
            "tool-first" => {
                // 首轮返回 add 工具调用；含工具结果后返回最终回答
                // （回答含历史条数，验证多轮共享上下文：第二轮历史更长）。
                // M34：消息为生产 `Message[]` 形状——tool 结果消息的判别是
                // `content[0].type == "tool-result"`（或 `source.kind == "tool"`）。
                let adapter = move |messages: Vec<Value>, _tools: Vec<Value>| -> Value {
                    let has_tool_result = messages.iter().any(|m| {
                        m.get("content")
                            .and_then(|c| c.as_array())
                            .and_then(|a| a.first())
                            .and_then(|b| b.get("type"))
                            .and_then(|t| t.as_str())
                            == Some("tool-result")
                    });
                    if has_tool_result {
                        let n = messages.len();
                        serde_json::json!({"content": format!("sum is 5 (ctx={n})")})
                    } else {
                        serde_json::json!({"content": "", "tool_calls": [{
                            "call_id": "c1",
                            "name": "add",
                            "arguments": {"a": 2, "b": 3},
                        }]})
                    }
                };
                // 声明式适配器同时作为默认（loop 的 llm 缝不带 provider 参数）
                svc.set_default(adapter);
            }
            _ => {
                // echo：回显最后一条 user 文本消息（声明式适配器同时作为默认）。
                // M34：user 消息 content 为 text block 数组——取 text 拼接；
                // 排除 tool-result 消息（生产形状下 ToolResultMessage 的 role
                // 也是 "user"，其 content 无 text block）。
                let p = provider_owned.clone();
                let adapter = move |messages: Vec<Value>, _tools: Vec<Value>| -> Value {
                    let last = messages
                        .iter()
                        .rev()
                        .find(|m| {
                            m["role"] == "user"
                                && m.get("content")
                                    .and_then(|c| c.as_array())
                                    .and_then(|a| a.first())
                                    .and_then(|b| b.get("type"))
                                    .and_then(|t| t.as_str())
                                    != Some("tool-result")
                        })
                        .and_then(|m| {
                            m.get("content")
                                .and_then(|c| c.as_array())
                                .map(|blocks| {
                                    blocks
                                        .iter()
                                        .filter(|b| {
                                            b.get("type").and_then(|t| t.as_str()) == Some("text")
                                        })
                                        .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                                        .collect::<Vec<_>>()
                                        .join("")
                                })
                                .or_else(|| m.get("content").and_then(|c| c.as_str()).map(str::to_string))
                        })
                        .unwrap_or_default();
                    serde_json::json!({"content": format!("[{p}] {last}")})
                };
                svc.set_default(adapter);
            }
        }
    }
}

impl Plugin for DshServicesPlugin {
    fn name(&self) -> &'static str {
        "dsh:services"
    }

    fn apply(&self, ctx: &Cordis, config: Value) -> Result<EffectOutcome, CordisError> {
        // 按配置选择注册的服务（缺省全注册）
        let requested = config
            .get("services")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| vec!["sessions".into(), "tools".into(), "llm".into()]);
        let want = |name: &str| requested.iter().any(|s| s == name);

        if want("sessions") {
            if let Some(h) = &self.sessions {
                ctx.provide("sessions", Arc::new(h.clone()))?;
            }
        }
        if want("tools") {
            if let Some(h) = &self.tools {
                ctx.provide("tools", Arc::new(h.clone()))?;
                // 声明式工具
                self.register_declared_tools(&config);
            }
        }
        if want("llm") {
            if let Some(h) = &self.llm {
                ctx.provide("llm", Arc::new(h.clone()))?;
                // 声明式 llm 适配器
                self.register_declared_llm(&config);
            }
        }
        Ok(EffectOutcome::None)
    }
}

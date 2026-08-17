//! §M9：DSH 层缝的数据承载——SessionLog（append-only + 模型历史投影）、
//! ToolRegistry（注册/执行）、LlmService（模型适配）。宿主基础设施，
//! 独立于 WASM 层验证。
//!
//! M34：消息形状对齐 DSH 生产 `Message` 对象
//! （`{id, role, content: ContentBlock[], source}`）——投影规则与
//! `deriveEventMessage`（deepseek-harness `packages/core/session/src/surface.ts`）
//! 逐条一致：`user/message` → data 逐字透传；`assistant/message` →
//! `data.message`（content 空数组跳过）；`tool/result` → `data.message`。

use dsh_core::*;

/// 生产形状的 user/message 事件 data（即完整 Message 对象）。
fn user_message(id: &str, text: &str) -> serde_json::Value {
    json!({
        "id": id,
        "role": "user",
        "content": [{"type": "text", "text": text}],
        "source": {"kind": "user"},
    })
}

/// 生产形状的 assistant/message 事件 data（`{turn, step, message}` 包装）。
fn assistant_message(id: &str, text: &str) -> serde_json::Value {
    json!({
        "turn": 1,
        "step": 1,
        "message": {
            "id": id,
            "role": "assistant",
            "content": [{"type": "text", "text": text}],
            "source": {"kind": "model", "provider": "mock", "model": "mock"},
        },
    })
}

/// 生产形状的 tool/result 事件 data（`{turn, step, message}` 包装，
/// ToolResultMessage：role=user + content 含 tool-result block + source.tool）。
fn tool_result(id: &str, call_id: &str, text: &str, is_error: bool) -> serde_json::Value {
    json!({
        "turn": 1,
        "step": 1,
        "message": {
            "id": id,
            "role": "user",
            "content": [{
                "type": "tool-result",
                "toolCallId": call_id,
                "content": [{"type": "text", "text": text}],
                "isError": is_error,
            }],
            "source": {"kind": "tool", "callId": call_id},
        },
    })
}

/// SessionLog：append-only 事件 + 序号 + 模型历史投影（生产 Message 形状）。
#[test]
fn session_log_appends_and_projects() {
    let mut log = SessionLog::new();
    let s1 = log.append("turn/start", serde_json::to_vec(&json!({"turn": 1})).unwrap());
    let s2 = log.append(
        "user/message",
        serde_json::to_vec(&user_message("u1", "hi")).unwrap(),
    );
    let s3 = log.append(
        "assistant/message",
        serde_json::to_vec(&assistant_message("a1", "hello")).unwrap(),
    );
    assert_eq!(s1, 0);
    assert_eq!(s2, 1);
    assert_eq!(s3, 2);

    assert_eq!(log.event_kinds(), vec!["turn/start", "user/message", "assistant/message"]);

    // 模型历史投影：user/message data 逐字透传；assistant 取 data.message
    let messages = log.derive_messages();
    assert_eq!(
        messages,
        vec![
            user_message("u1", "hi"),
            json!({
                "id": "a1",
                "role": "assistant",
                "content": [{"type": "text", "text": "hello"}],
                "source": {"kind": "model", "provider": "mock", "model": "mock"},
            }),
        ]
    );
}

/// SessionLog：tool/result 投影为 ToolResultMessage（role=user + tool-result block）。
#[test]
fn session_log_projects_tool_result() {
    let mut log = SessionLog::new();
    log.append("tool/result", serde_json::to_vec(&tool_result("t1", "c1", "42", false)).unwrap());
    let messages = log.derive_messages();
    assert_eq!(
        messages,
        vec![json!({
            "id": "t1",
            "role": "user",
            "content": [{
                "type": "tool-result",
                "toolCallId": "c1",
                "content": [{"type": "text", "text": "42"}],
                "isError": false,
            }],
            "source": {"kind": "tool", "callId": "c1"},
        })]
    );
}

/// M32/M34：assistant/message 空 content 跳过（DSH `deriveEventMessage` 规则——
/// 仅承载 usage 的 max-tokens 助手消息不入模型历史）。
#[test]
fn session_log_skips_empty_assistant() {
    let mut log = SessionLog::new();
    log.append("user/message", serde_json::to_vec(&user_message("u1", "hi")).unwrap());
    log.append(
        "assistant/message",
        serde_json::to_vec(&json!({
            "turn": 1, "step": 1,
            "message": {
                "id": "a-empty",
                "role": "assistant",
                "content": [],
                "source": {"kind": "model", "provider": "mock", "model": "mock"},
            },
        }))
        .unwrap(),
    );
    log.append(
        "assistant/message",
        serde_json::to_vec(&assistant_message("a1", "real answer")).unwrap(),
    );
    let messages = log.derive_messages();
    assert_eq!(
        messages,
        vec![
            user_message("u1", "hi"),
            json!({
                "id": "a1",
                "role": "assistant",
                "content": [{"type": "text", "text": "real answer"}],
                "source": {"kind": "model", "provider": "mock", "model": "mock"},
            }),
        ],
        "empty assistant message skipped"
    );
}

/// M36：session surface 折叠——append 事件入 surface 节点序列；
/// 非 surface 事件（turn/start 等）不入列；`derive_messages` 只对
/// **当前 surface 节点**投影（对齐 DSH `foldSurface`/`SessionSurface.nodes`）。
#[test]
fn session_surface_append_tracks_nodes() {
    let mut log = SessionLog::new();
    log.append("turn/start", serde_json::to_vec(&json!({"turn": 1})).unwrap());
    log.append("user/message", serde_json::to_vec(&user_message("u1", "hi")).unwrap());
    log.append("step/start", serde_json::to_vec(&json!({"turn": 1, "step": 1})).unwrap());
    log.append("assistant/message", serde_json::to_vec(&assistant_message("a1", "hello")).unwrap());
    log.append("turn/end", serde_json::to_vec(&json!({"turn": 1, "reason": "completed"})).unwrap());

    // surface 节点 = surface-eligible 事件的 seq（user/assistant/tool）；
    // 边界/日志事件（turn/start、step/start、turn/end）不入列。
    assert_eq!(log.surface_nodes(), vec![1u64, 3u64], "surface nodes: eligible only");
    assert_eq!(log.replace_generation(), 0, "no replacement yet");

    // 投影只含当前 surface 节点（与遍历全部事件等价——此处无 replace）
    let messages = log.derive_messages();
    assert_eq!(
        messages,
        vec![
            user_message("u1", "hi"),
            json!({
                "id": "a1",
                "role": "assistant",
                "content": [{"type": "text", "text": "hello"}],
                "source": {"kind": "model", "provider": "mock", "model": "mock"},
            }),
        ]
    );
}

/// M36：surface replace——替换 [start, end] 范围内的 surface 节点为当前事件
/// （compaction 语义），旧节点被 shadow；`replaceGeneration` 递增。
/// M37：replace 必须带 `source_event_seqs` 覆盖被 shadow 节点（对齐生产
/// `assertProvenance`）。
#[test]
fn session_surface_replace_shadows_old_nodes() {
    let mut log = SessionLog::new();
    log.append("user/message", serde_json::to_vec(&user_message("u1", "q1")).unwrap()); // seq 0
    log.append("assistant/message", serde_json::to_vec(&assistant_message("a1", "old answer")).unwrap()); // seq 1
    log.append("tool/result", serde_json::to_vec(&tool_result("t1", "c1", "42", false)).unwrap()); // seq 2

    // compaction：把 [0, 1]（user + assistant）替换为一条新的 user 消息；
    // source_event_seqs 必须覆盖被 shadow 的节点 [0, 1]
    log.append_with_provenance(
        "user/message",
        serde_json::to_vec(&user_message("u2", "summarized")).unwrap(),
        SurfaceOp::Replace { start: 0, end: 1 },
        Some(vec![0, 1]),
    )
    .unwrap(); // seq 3

    // surface 节点 = [3, 2]（旧 u1/a1 被 shadow）；replaceGeneration 递增
    assert_eq!(log.surface_nodes(), vec![3u64, 2u64]);
    assert_eq!(log.replace_generation(), 1);

    // 投影 = 新 user（替换后）+ 原 tool-result（未被替换）
    let messages = log.derive_messages();
    assert_eq!(
        messages,
        vec![
            user_message("u2", "summarized"),
            json!({
                "id": "t1",
                "role": "user",
                "content": [{
                    "type": "tool-result",
                    "toolCallId": "c1",
                    "content": [{"type": "text", "text": "42"}],
                    "isError": false,
                }],
                "source": {"kind": "tool", "callId": "c1"},
            }),
        ],
        "replaced range shadowed; tool result retained"
    );
}

/// M37：replace 缺 source_event_seqs（None）→ 报错（被 shadow 节点无来源
/// 覆盖，对齐生产 `assertProvenance`：missing shadowed 报错）。
#[test]
fn session_surface_replace_requires_provenance() {
    let mut log = SessionLog::new();
    log.append("user/message", serde_json::to_vec(&user_message("u1", "q1")).unwrap()); // seq 0
    log.append("assistant/message", serde_json::to_vec(&assistant_message("a1", "a")).unwrap()); // seq 1

    // 无 source_event_seqs → 报错（sources 空，shadowed [0,1] 全部 missing）
    let err = log
        .append_with_provenance(
            "user/message",
            serde_json::to_vec(&user_message("u2", "x")).unwrap(),
            SurfaceOp::Replace { start: 0, end: 1 },
            None,
        )
        .unwrap_err();
    assert!(err.to_string().contains("source_event_seqs"), "{err}");

    // 部分覆盖 → 报错（missing 1）
    let err = log
        .append_with_provenance(
            "user/message",
            serde_json::to_vec(&user_message("u3", "y")).unwrap(),
            SurfaceOp::Replace { start: 0, end: 1 },
            Some(vec![0]),
        )
        .unwrap_err();
    assert!(err.to_string().contains("source_event_seqs"), "{err}");

    // 失败原子：surface 未变
    assert_eq!(log.surface_nodes(), vec![0u64, 1u64]);
    assert_eq!(log.replace_generation(), 0);
}

/// M37：source_event_seqs 引用校验（对齐生产 `assertProvenance`）——
/// 引用必须早于当前 seq、无重复、空数组仅 assistant/message 允许。
#[test]
fn session_surface_provenance_reference_validation() {
    let mut log = SessionLog::new();
    log.append("user/message", serde_json::to_vec(&user_message("u1", "q1")).unwrap()); // seq 0

    // 引用 >= 当前 seq（未来事件）→ 报错
    let err = log
        .append_with_provenance(
            "user/message",
            serde_json::to_vec(&user_message("u2", "x")).unwrap(),
            SurfaceOp::Append,
            Some(vec![5]),
        )
        .unwrap_err();
    assert!(err.to_string().contains("earlier"), "{err}");

    // 重复引用 → 报错
    let err = log
        .append_with_provenance(
            "user/message",
            serde_json::to_vec(&user_message("u3", "y")).unwrap(),
            SurfaceOp::Append,
            Some(vec![0, 0]),
        )
        .unwrap_err();
    assert!(err.to_string().contains("duplicate"), "{err}");

    // 空数组（非 assistant）→ 报错
    let err = log
        .append_with_provenance(
            "user/message",
            serde_json::to_vec(&user_message("u4", "z")).unwrap(),
            SurfaceOp::Append,
            Some(vec![]),
        )
        .unwrap_err();
    assert!(err.to_string().contains("empty"), "{err}");

    // 空数组（assistant/message）→ 允许（生产规则：known empty provider stream）
    let r = log.append_with_provenance(
        "assistant/message",
        serde_json::to_vec(&json!({
            "turn": 1, "step": 1,
            "message": {
                "id": "a-empty",
                "role": "assistant",
                "content": [],
                "source": {"kind": "model", "provider": "mock", "model": "mock"},
            },
        }))
        .unwrap(),
        SurfaceOp::Append,
        Some(vec![]),
    );
    assert!(r.is_ok(), "{r:?}");
}

/// M37：tool/result replace 约束（对齐生产 `assertToolResultRewrite`）——
/// 必须恰好重写 1 个当前 tool/result 节点，且只允许改 content。
#[test]
fn session_surface_tool_result_rewrite_rule() {
    let mut log = SessionLog::new();
    log.append("tool/result", serde_json::to_vec(&tool_result("t1", "c1", "42", false)).unwrap()); // seq 0

    // 只改 content → 允许（compaction 修正工具输出）
    let r = log.append_with_provenance(
        "tool/result",
        serde_json::to_vec(&tool_result("t1", "c1", "43", false)).unwrap(),
        SurfaceOp::Replace { start: 0, end: 0 },
        Some(vec![0]),
    );
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(log.replace_generation(), 1);

    // 改 callId（content 之外）→ 报错
    let err = log
        .append_with_provenance(
            "tool/result",
            serde_json::to_vec(&tool_result("t2", "c2", "43", false)).unwrap(),
            SurfaceOp::Replace { start: 1, end: 1 },
            Some(vec![1]),
        )
        .unwrap_err();
    assert!(err.to_string().contains("content"), "{err}");
}

/// M36：surface replace 非法范围（start/end 不在 surface 上）→ 报错（fail loud）。
#[test]
fn session_surface_replace_invalid_range_fails() {
    let mut log = SessionLog::new();
    log.append("user/message", serde_json::to_vec(&user_message("u1", "hi")).unwrap()); // seq 0

    // start 不在 surface（turn/start 非 surface 节点；seq 999 不存在）
    let err = log
        .append_with_op(
            "user/message",
            serde_json::to_vec(&user_message("u2", "x")).unwrap(),
            SurfaceOp::Replace { start: 999, end: 0 },
        )
        .unwrap_err();
    assert!(err.to_string().contains("surface"), "{err}");

    // end 在 surface 但 start 是日志事件（不在 surface）→ 报错
    log.append("turn/start", serde_json::to_vec(&json!({"turn": 1})).unwrap()); // seq 1 非 surface
    log.append("assistant/message", serde_json::to_vec(&assistant_message("a1", "x")).unwrap()); // seq 2
    let err = log
        .append_with_op(
            "user/message",
            serde_json::to_vec(&user_message("u3", "y")).unwrap(),
            SurfaceOp::Replace { start: 1, end: 2 },
        )
        .unwrap_err();
    assert!(err.to_string().contains("surface"), "{err}");

    // 失败后 surface 未被破坏（replace 是原子的）
    assert_eq!(log.surface_nodes(), vec![0u64, 2u64]);
    assert_eq!(log.replace_generation(), 0);
}

/// M36：非 surface 事件禁止带 replace op（surfaceOp 仅 surface-eligible 类型可带）。
#[test]
fn session_surface_replace_rejected_on_log_events() {
    let mut log = SessionLog::new();
    let err = log
        .append_with_op(
            "turn/start",
            serde_json::to_vec(&json!({"turn": 1})).unwrap(),
            SurfaceOp::Replace { start: 0, end: 0 },
        )
        .unwrap_err();
    assert!(err.to_string().contains("surface"), "{err}");
}

/// ToolRegistry：注册/执行/未注册错误。
#[test]
fn tool_registry_registers_and_executes() {
    let mut tools = ToolRegistry::new();
    tools.register("add", |args| {
        let a = args.get("a").and_then(|v| v.as_i64()).unwrap_or(0);
        let b = args.get("b").and_then(|v| v.as_i64()).unwrap_or(0);
        json!({"sum": a + b})
    });
    assert_eq!(tools.names(), vec!["add".to_string()]);

    let result = tools.execute("add", json!({"a": 2, "b": 3}));
    assert_eq!(result, json!({"sum": 5}));

    let missing = tools.execute("nope", json!({}));
    assert!(missing.get("error").is_some());
}

/// SessionLog/ToolRegistry 经 Arc<Mutex> 可作服务值（Send+Sync）。
#[test]
fn handles_are_send_sync_service_values() {
    let sessions = new_session();
    let tools = new_tool_registry();
    sessions.lock().unwrap().append("turn/start", vec![]);
    tools.lock().unwrap().register("noop", |_| json!({}));
    assert_eq!(sessions.lock().unwrap().events().len(), 1);
    assert_eq!(tools.lock().unwrap().names(), vec!["noop".to_string()]);
}

/// LlmService：默认适配器 / provider 适配器 / 未注册错误。
#[test]
fn llm_service_default_and_provider() {
    let mut llm = LlmService::new();
    llm.set_default(|messages, _tools| {
        json!({"content": format!("echo:{}", messages.len())})
    });
    llm.register_provider("mock", |_messages, _tools| json!({"content": "from-mock"}));

    // 默认适配器
    let r = llm.generate(None, vec![json!({"role": "user"})], vec![]);
    assert_eq!(r, json!({"content": "echo:1"}));

    // provider 适配器
    let r = llm.generate(Some("mock"), vec![], vec![]);
    assert_eq!(r, json!({"content": "from-mock"}));

    // 未知 provider → 回退默认
    let r = llm.generate(Some("nope"), vec![], vec![]);
    assert_eq!(r, json!({"content": "echo:0"}));
}

/// LlmService：无适配器 → 错误 JSON。
#[test]
fn llm_service_missing_adapter() {
    let llm = LlmService::new();
    let r = llm.generate(None, vec![], vec![]);
    assert!(r.get("error").is_some());
}

/// LlmService 句柄可作服务值（Send+Sync）。
#[test]
fn llm_handle_is_send_sync() {
    let llm = new_llm();
    llm.lock().unwrap().set_default(|_, _| json!({"content": "ok"}));
    assert_eq!(llm.lock().unwrap().generate(None, vec![], vec![]), json!({"content": "ok"}));
}

/// M47：session JSONL 持久化——`save_to` 写 header + 每事件一行，
/// `load_from` 重建 events + surface（append 语义重放），投影一致。
#[test]
fn session_save_load_roundtrip() {
    let dir = std::env::temp_dir().join(format!("dsh-m47-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("session.jsonl");

    let mut log = SessionLog::new();
    log.append("turn/start", serde_json::to_vec(&json!({"turn": 1})).unwrap());
    log.append("user/message", serde_json::to_vec(&user_message("u1", "hi")).unwrap());
    log.append("assistant/message", serde_json::to_vec(&assistant_message("a1", "hello")).unwrap());
    log.append("turn/end", serde_json::to_vec(&json!({"turn": 1, "reason": "completed"})).unwrap());
    log.save_to(&path).expect("save");

    // 文件首行是 header，其余是事件行
    let text = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 5, "header + 4 events: {lines:?}");
    assert!(lines[0].contains("\"type\":\"session\""), "header line: {}", lines[0]);

    // 重建：events + surface + 投影一致
    let loaded = SessionLog::load_from(&path).expect("load");
    assert_eq!(loaded.event_kinds(), log.event_kinds());
    assert_eq!(loaded.surface_nodes(), log.surface_nodes());
    assert_eq!(loaded.derive_messages(), log.derive_messages());

    std::fs::remove_dir_all(&dir).ok();
}

/// M47：load_from 对 torn tail（最后一行无换行/损坏）容忍——保留完整前缀。
#[test]
fn session_load_tolerates_torn_tail() {
    let dir = std::env::temp_dir().join(format!("dsh-m47-torn-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("session.jsonl");

    let mut log = SessionLog::new();
    log.append("user/message", serde_json::to_vec(&user_message("u1", "hi")).unwrap());
    log.append("assistant/message", serde_json::to_vec(&assistant_message("a1", "hello")).unwrap());
    log.save_to(&path).expect("save");

    // 追加一行损坏/torn 记录（无换行结尾 + 非法 JSON）→ load 忽略
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str("{\"kind\":\"broken\"");
    std::fs::write(&path, text).unwrap();

    let loaded = SessionLog::load_from(&path).expect("load tolerates torn tail");
    assert_eq!(loaded.event_kinds(), log.event_kinds(), "complete prefix preserved");
    assert_eq!(loaded.derive_messages(), log.derive_messages());

    std::fs::remove_dir_all(&dir).ok();
}

/// M47：load_from 对缺 header 的文件报错（fail loud）。
#[test]
fn session_load_missing_header_fails() {
    let dir = std::env::temp_dir().join(format!("dsh-m47-nohead-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("session.jsonl");
    std::fs::write(&path, "{\"kind\":\"user/message\",\"seq\":0}\n").unwrap();

    let err = SessionLog::load_from(&path).unwrap_err();
    assert!(err.to_string().contains("header"), "{err}");
    std::fs::remove_dir_all(&dir).ok();
}

/// M49：fork（分支会话，对齐 DSH `Session.fork`）——截取 [0, boundary] 事件
/// 前缀（boundary 省略 = 最后事件，但最后事件在 open turn 内会报错——
/// 此处用显式 turn/end 边界）；子会话 events/surface/投影 = 前缀重放。
#[test]
fn session_fork_slices_prefix() {
    let mut log = SessionLog::new();
    log.append("turn/start", serde_json::to_vec(&json!({"turn": 1})).unwrap()); // 0
    log.append("user/message", serde_json::to_vec(&user_message("u1", "q1")).unwrap()); // 1
    log.append("assistant/message", serde_json::to_vec(&assistant_message("a1", "a1")).unwrap()); // 2
    log.append("turn/end", serde_json::to_vec(&json!({"turn": 1, "reason": "completed"})).unwrap()); // 3
    log.append("turn/start", serde_json::to_vec(&json!({"turn": 2})).unwrap()); // 4
    log.append("user/message", serde_json::to_vec(&user_message("u2", "q2")).unwrap()); // 5

    // 显式 boundary = 3（turn 1 完成处——稳定前缀）
    let child2 = log.fork(Some(3)).expect("fork at turn boundary");
    assert_eq!(child2.event_kinds().len(), 4);
    assert_eq!(child2.derive_messages(), log.derive_messages()[..2].to_vec());
    // 子会话继续追加（分支探索）
    let mut child3 = log.fork(Some(3)).expect("fork branch");
    child3.append("user/message", serde_json::to_vec(&user_message("u-branch", "branch")).unwrap());
    assert_eq!(child3.event_kinds().len(), 5, "branch continues after fork");
    // 父会话不受影响
    assert_eq!(log.event_kinds().len(), 6);

    // 默认 boundary = 最后事件（5，open turn 内）→ 报错（对齐 OPEN_TURN）
    let err = log.fork(None).unwrap_err();
    assert!(err.to_string().contains("open turn"), "{err}");
}

/// M49：fork 空会话 → 空子会话。
#[test]
fn session_fork_empty_yields_empty() {
    let log = SessionLog::new();
    let child = log.fork(None).expect("empty fork");
    assert!(child.events().is_empty());
    assert!(child.derive_messages().is_empty());
}

/// M49：fork 边界校验——boundary 越界 / 落在 open turn 内 → 报错（fail loud，
/// 对齐 DSH `INVALID_BOUNDARY` / `OPEN_TURN`）。
#[test]
fn session_fork_invalid_boundary_fails() {
    let mut log = SessionLog::new();
    log.append("turn/start", serde_json::to_vec(&json!({"turn": 1})).unwrap()); // 0
    log.append("user/message", serde_json::to_vec(&user_message("u1", "q1")).unwrap()); // 1

    // boundary 越界
    let err = log.fork(Some(99)).unwrap_err();
    assert!(err.to_string().contains("boundary"), "{err}");

    // boundary 落在 open turn 内（最后 turn 边界是 turn/start → OPEN_TURN）
    let err = log.fork(Some(1)).unwrap_err();
    assert!(err.to_string().contains("open turn"), "{err}");

    // 父会话不受失败影响
    assert_eq!(log.event_kinds().len(), 2);
}

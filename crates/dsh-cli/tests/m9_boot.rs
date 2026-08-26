//! M9-DSH：启动器（app-boot 等价）——`dsh_cli::boot` 从 cordis.yml 启动
//! （注册插件仓库 → Include 挂载），`run_turn` 驱动 WASM loop。
//!
//! 覆盖：多轮会话、profile 叠加层（overlay 覆盖 loop）、manifest 形态
//! （.wasm 文件路径）——对应 deepseek-harness 的 `dsh` CLI 与 bundle 语义。

#![allow(clippy::arc_with_non_send_sync)]

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use dsh_core::*;

/// 确保 WASM 组件已构建（echo-loop / tool-loop）。
fn ensure_loop_built(dir: &str) {
    let manifest: PathBuf =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../../wasm-plugins/{dir}"));
    let wasm = manifest
        .join("target/wasm32-wasip1/debug")
        .join(format!("{}_plugin.wasm", dir.replace('-', "_")));
    if !wasm.exists() {
        let status = Command::new("cargo")
            .args(["component", "build", "--manifest-path"])
            .arg(manifest.join("Cargo.toml"))
            .status()
            .expect("run cargo component build");
        assert!(status.success(), "{dir} build failed");
    }
}

/// 读取 WASM 组件字节（构建后）。
fn loop_component(dir: &str) -> Vec<u8> {
    ensure_loop_built(dir);
    let manifest: PathBuf =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../../wasm-plugins/{dir}"));
    let wasm = manifest
        .join("target/wasm32-wasip1/debug")
        .join(format!("{}_plugin.wasm", dir.replace('-', "_")));
    fs::read(wasm).expect("read loop component")
}

fn write_cordis_yaml(dir: &std::path::Path, file: &str, loop_name: &str, wasm: &str) -> PathBuf {
    let path = dir.join(file);
    let yaml = format!(
        r#"
- id: services
  name: dsh:services
  config:
    services: [sessions, tools, llm]
- id: loop
  name: {loop_name}
  config:
    wasm: {wasm}
"#
    );
    fs::write(&path, yaml).unwrap();
    path
}

fn wasm_base() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../wasm-plugins")
}

fn unique_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dsh-m9-boot-{tag}-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// 启动器端到端：cordis.yml → boot → run_turn（echo-loop）。
#[test]
fn boot_loads_and_runs_turn() {
    ensure_loop_built("echo-loop");
    let dir = unique_dir("basic");
    let config = write_cordis_yaml(&dir, "cordis.yml", "echo-loop", "echo-loop");

    let boot = dsh_cli::boot(&config, &[], &wasm_base()).expect("boot");
    let result = dsh_cli::run_turn(&boot, &json!({"content": "boot hello"})).expect("run_turn");
    assert_eq!(result["echo"], "echo: boot hello");

    let log = boot.sessions.lock().unwrap();
    assert_eq!(log.event_kinds().len(), 6, "full turn via boot");
    fs::remove_dir_all(&dir).ok();
}

/// 多轮会话：同一 boot 连续 run_turn，session 事件累计。
#[test]
fn boot_runs_multiple_turns() {
    ensure_loop_built("echo-loop");
    let dir = unique_dir("multi");
    let config = write_cordis_yaml(&dir, "cordis.yml", "echo-loop", "echo-loop");

    let boot = dsh_cli::boot(&config, &[], &wasm_base()).expect("boot");
    let r1 = dsh_cli::run_turn(&boot, &json!({"content": "one"})).unwrap();
    assert_eq!(r1["echo"], "echo: one");
    let r2 = dsh_cli::run_turn(&boot, &json!({"content": "two"})).unwrap();
    assert_eq!(r2["echo"], "echo: two");

    let log = boot.sessions.lock().unwrap();
    assert_eq!(log.event_kinds().len(), 12, "two turns accumulated");
    fs::remove_dir_all(&dir).ok();
}

/// profile 叠加层：overlay 把 loop 从 echo-loop 换成 tool-loop（bundle 语义）。
#[test]
fn boot_profile_overlay_swaps_loop() {
    ensure_loop_built("echo-loop");
    ensure_loop_built("tool-loop");
    let dir = unique_dir("overlay");
    let base = write_cordis_yaml(&dir, "base.yml", "echo-loop", "echo-loop");
    let overlay = write_cordis_yaml(&dir, "overlay.yml", "tool-loop", "tool-loop");

    let boot = dsh_cli::boot(&base, &[overlay], &wasm_base()).expect("boot");
    // 注册 add 工具（宿主承载；tool-loop 经 tools 缝调用）
    let tools = boot
        .ctx
        .get_typed::<dsh_core::ToolRegistryHandle>("tools")
        .unwrap();
    tools.lock().unwrap().register("add", |args| {
        let a = args.get("a").and_then(|v| v.as_i64()).unwrap_or(0);
        let b = args.get("b").and_then(|v| v.as_i64()).unwrap_or(0);
        serde_json::json!({"sum": a + b})
    });
    // overlay 后的 loop 是 tool-loop：run_turn 返回 summary 而非 echo
    let result = dsh_cli::run_turn(&boot, &json!({"content": "compute"})).unwrap();
    assert_eq!(result["summary"], "2 + 3 = 5", "overlay swapped loop to tool-loop");
    fs::remove_dir_all(&dir).ok();
}

/// manifest 形态：config.wasm 指向 .wasm 文件路径（非构建目录）。
#[test]
fn boot_manifest_wasm_path() {
    ensure_loop_built("echo-loop");
    let dir = unique_dir("manifest");
    let wasm_file = wasm_base()
        .join("echo-loop/target/wasm32-wasip1/debug/echo_loop_plugin.wasm");
    assert!(wasm_file.exists(), "echo-loop wasm built");
    let config = write_cordis_yaml(&dir, "cordis.yml", "echo-loop", &wasm_file.to_string_lossy());

    let boot = dsh_cli::boot(&config, &[], &wasm_base()).expect("boot with .wasm manifest");
    let result = dsh_cli::run_turn(&boot, &json!({"content": "manifest"})).unwrap();
    assert_eq!(result["echo"], "echo: manifest");
    fs::remove_dir_all(&dir).ok();
}

/// 启动器：缺 loop entry 报错（配置错误 fail loud）。
#[test]
fn boot_requires_loop_entry() {
    let dir = unique_dir("err");
    let config = dir.join("cordis.yml");
    fs::write(&config, "- id: services\n  name: dsh:services\n  config: {}\n").unwrap();
    let err = match dsh_cli::boot(&config, &[], &wasm_base()) {
        Ok(_) => panic!("boot should fail without loop entry"),
        Err(e) => e,
    };
    assert!(
        err.to_string().contains("no loop entry"),
        "expected missing-loop error, got: {err}"
    );
    fs::remove_dir_all(&dir).ok();
}

/// 声明式配置：cordis.yml 的 services entry 声明工具（add）+ llm（tool-first），
/// 启动器按配置注册——tool-loop 经 tools 缝调声明式 add 工具。
#[test]
fn boot_declared_tools_and_llm() {
    ensure_loop_built("tool-loop");
    let dir = unique_dir("declared");
    let yaml = r#"
- id: services
  name: dsh:services
  config:
    services: [sessions, tools, llm]
    tools:
      - name: add
        op: add
    llm:
      provider: mock
      behavior: tool-first
- id: loop
  name: tool-loop
  config:
    wasm: tool-loop
"#;
    let config = dir.join("cordis.yml");
    fs::write(&config, yaml).unwrap();

    let boot = dsh_cli::boot(&config, &[], &wasm_base()).expect("boot");
    // 不再需要代码注册工具——add 由声明式配置注册
    let result = dsh_cli::run_turn(&boot, &json!({"content": "compute"})).unwrap();
    assert_eq!(result["summary"], "2 + 3 = 5", "declared add tool via config");
    fs::remove_dir_all(&dir).ok();
}

/// 多轮共享上下文：llm-loop 第二轮 llm 缝输入含前轮 session 历史
/// （tool-first 适配器回答含 ctx=N，N 随历史增长）。
#[test]
fn boot_multi_turn_shared_context() {
    ensure_loop_built("llm-loop");
    let dir = unique_dir("context");
    let yaml = r#"
- id: services
  name: dsh:services
  config:
    services: [sessions, tools, llm]
    tools:
      - name: add
        op: add
    llm:
      provider: mock
      behavior: tool-first
- id: loop
  name: llm-loop
  config:
    wasm: llm-loop
"#;
    let config = dir.join("cordis.yml");
    fs::write(&config, yaml).unwrap();

    let boot = dsh_cli::boot(&config, &[], &wasm_base()).expect("boot");

    // 第一轮：无前轮历史
    let r1 = dsh_cli::run_turn(&boot, &json!({"content": "q1"})).unwrap();
    assert_eq!(r1["turn"], 1, "first turn");
    // 第二轮：llm 缝输入含前轮历史（tool-first 回答 ctx=5：user+assistant+tool+user+assistant）
    let r2 = dsh_cli::run_turn(&boot, &json!({"content": "q2"})).unwrap();
    assert_eq!(r2["turn"], 2, "second turn");
    let answer = r2["answer"].as_str().unwrap_or("");
    assert!(
        answer.contains("ctx="),
        "llm received context length, got answer: {answer}"
    );

    // session 服务含两轮完整事件
    let log = boot.sessions.lock().unwrap();
    let kinds = log.event_kinds();
    assert_eq!(kinds.len(), 16, "two full turns (8 events each)");
    assert_eq!(kinds[0], "turn/start");
    assert_eq!(kinds[8], "turn/start", "second turn opened");
    fs::remove_dir_all(&dir).ok();
}

/// WASI 精细授予：`abi_only`（无 WASI 位）仍能跑 loop（组件不依赖 WASI 功能）。
#[test]
fn boot_works_without_wasi_caps() {
    ensure_loop_built("echo-loop");
    let dir = unique_dir("nowasi");
    let config = write_cordis_yaml(&dir, "cordis.yml", "echo-loop", "echo-loop");

    // boot 用 abi_only 能力构建 loop 插件（WASI 空上下文）
    let cordis = Cordis::new();
    let loader = dsh_loader::Loader::new(&cordis).unwrap();
    loader.register_plugin("dsh:services", Arc::new(dsh_wasmrt::DshServicesPlugin::all()));
    let bytes = loop_component("echo-loop");
    let plugin = Arc::new(
        dsh_wasmrt::WasmLoopPlugin::new_owned("echo-loop", &bytes, dsh_wasmrt::Capabilities::abi_only())
            .unwrap(),
    );
    let dyn_plugin: Arc<dyn Plugin> = plugin.clone();
    loader.register_plugin("echo-loop", dyn_plugin);
    let include = dsh_loader::Include::new(&loader, &config, vec![]);
    include.load().unwrap();

    let r = plugin.run_turn(&cordis, &json!({"content": "no-wasi"})).unwrap();
    assert_eq!(r["echo"], "echo: no-wasi", "loop runs with abi-only caps (no WASI)");
    fs::remove_dir_all(&dir).ok();
}

/// llm provider 选择：声明式注册两个 provider（tool-first + echo），
/// loop 按 provider 名从 LlmService 选适配器。
#[test]
fn llm_provider_selection() {
    let llm = dsh_core::new_llm();
    {
        let mut svc = llm.lock().unwrap();
        svc.register_provider("tool-first", |messages, _| {
            // M34：生产 Message[] 形状——tool 结果判别 content[0].type
            if messages.iter().any(|m| {
                m["content"]
                    .get(0)
                    .and_then(|b| b.get("type"))
                    .and_then(|t| t.as_str())
                    == Some("tool-result")
            }) {
                serde_json::json!({"content": "tool-first answer"})
            } else {
                serde_json::json!({"content": "", "tool_calls": [{"call_id": "c1", "name": "add", "arguments": {"a": 1, "b": 1}}]})
            }
        });
        svc.register_provider("echo", |messages, _| {
            // M34：user 消息 content 为 text block 数组
            let last = messages.iter().rev().find(|m| m["role"] == "user")
                .and_then(|m| {
                    m.get("content")
                        .and_then(|c| c.as_array())
                        .map(|blocks| {
                            blocks
                                .iter()
                                .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
                                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                                .collect::<Vec<_>>()
                                .join("")
                        })
                        .or_else(|| m.get("content").and_then(|c| c.as_str()).map(str::to_string))
                })
                .unwrap_or_default();
            serde_json::json!({"content": format!("echo:{last}")})
        });
    }
    // 按 provider 名选择
    let r1 = llm.lock().unwrap().generate(
        Some("echo"),
        vec![serde_json::json!({
            "id": "u1", "role": "user",
            "content": [{"type": "text", "text": "hi"}],
            "source": {"kind": "user"},
        })],
        vec![],
    );
    assert_eq!(r1, serde_json::json!({"content": "echo:hi"}));
    let r2 = llm.lock().unwrap().generate(
        Some("tool-first"),
        vec![serde_json::json!({
            "id": "u1", "role": "user",
            "content": [{"type": "text", "text": "hi"}],
            "source": {"kind": "user"},
        })],
        vec![],
    );
    assert!(r2.get("tool_calls").is_some(), "tool-first returns tool call");
    // 未知 provider → 回退 default（无 → error）
    let r3 = llm.lock().unwrap().generate(Some("nope"), vec![], vec![]);
    assert!(r3.get("error").is_some(), "unknown provider falls back to error");
}

/// WASI 能力按 entry 配置：loop entry 声明 `caps`（受限/全量），启动器按配置授予。
#[test]
fn boot_caps_from_entry_config() {
    ensure_loop_built("echo-loop");
    let dir = unique_dir("caps");

    // 受限 caps：仅 provide/emit/get（无 WASI）
    let yaml = r#"
- id: services
  name: dsh:services
  config: {}
- id: loop
  name: echo-loop
  config:
    wasm: echo-loop
    caps: [provide, emit, get]
"#;
    let config = dir.join("cordis.yml");
    fs::write(&config, yaml).unwrap();
    let boot = dsh_cli::boot(&config, &[], &wasm_base()).expect("boot with restricted caps");
    let r = dsh_cli::run_turn(&boot, &json!({"content": "restricted"})).unwrap();
    assert_eq!(r["echo"], "echo: restricted", "loop runs with configured caps");
    fs::remove_dir_all(&dir).ok();
}

/// HMR 端到端（M15）：修改配置（llm behavior echo → tool-first）→ `boot.refresh()`
/// → 新 turn 走新 llm 适配器（对应 Cordis hmr 的 refresh 语义）。
#[test]
fn boot_refresh_hot_reloads_llm_behavior() {
    ensure_loop_built("llm-loop");
    let dir = unique_dir("hmr");
    let config = dir.join("cordis.yml");
    let base_yaml = |behavior: &str| {
        format!(
            r#"
- id: services
  name: dsh:services
  config:
    services: [sessions, tools, llm]
    llm:
      provider: mock
      behavior: {behavior}
- id: loop
  name: llm-loop
  config:
    wasm: llm-loop
"#
        )
    };
    fs::write(&config, base_yaml("echo")).unwrap();

    let boot = dsh_cli::boot(&config, &[], &wasm_base()).expect("boot");

    // echo 行为：回答回显最后一条 user 消息
    let r1 = dsh_cli::run_turn(&boot, &json!({"content": "hello-hmr"})).unwrap();
    let a1 = r1["answer"].as_str().unwrap_or("");
    assert!(a1.contains("hello-hmr"), "echo behavior, got: {a1}");

    // 修改配置：behavior → tool-first
    fs::write(&config, base_yaml("tool-first")).unwrap();
    (boot.refresh)().expect("boot refresh (HMR)");

    // tool-first：首轮无工具结果 → 返回 add 工具调用 → loop 执行后第二轮含 ctx
    let r2 = dsh_cli::run_turn(&boot, &json!({"content": "q2"})).unwrap();
    let a2 = r2["answer"].as_str().unwrap_or("");
    assert!(
        a2.contains("ctx="),
        "tool-first behavior after refresh, got: {a2}"
    );
    fs::remove_dir_all(&dir).ok();
}

/// HMR refresh async 事务（M23）：`boot.refresh` 走 `load_async`（sync_async
/// allSettled）——配置含无效 entry 时 refresh 报错（失败数量），且不破坏
/// 已运行配置（整事务回滚/失败不 panic）。
#[test]
fn boot_refresh_async_transaction_reports_failure() {
    ensure_loop_built("echo-loop");
    let dir = unique_dir("hmr-tx");
    let config = dir.join("cordis.yml");
    let base_yaml = |loop_name: &str| {
        format!(
            r#"
- id: services
  name: dsh:services
  config:
    services: [sessions, tools, llm]
- id: loop
  name: {loop_name}
  config:
    wasm: {loop_name}
"#
        )
    };
    fs::write(&config, base_yaml("echo-loop")).unwrap();
    let boot = dsh_cli::boot(&config, &[], &wasm_base()).expect("boot");
    let r1 = dsh_cli::run_turn(&boot, &json!({"content": "before"})).unwrap();
    assert_eq!(r1["echo"], "echo: before");

    // 改配置：loop name 指向未注册插件 → sync_async 失败（unknown plugin）
    fs::write(
        &config,
        r#"
- id: services
  name: dsh:services
  config:
    services: [sessions, tools, llm]
- id: loop
  name: no-such-loop
  config:
    wasm: echo-loop
"#,
    )
    .unwrap();
    let err = (boot.refresh)().unwrap_err();
    assert!(
        err.to_string().contains("errors"),
        "refresh failed with error count, got: {err}"
    );

    // 失败不 panic；再次恢复配置后 refresh 成功
    fs::write(&config, base_yaml("echo-loop")).unwrap();
    (boot.refresh)().expect("refresh after restore");
    let r2 = dsh_cli::run_turn(&boot, &json!({"content": "after"})).unwrap();
    assert_eq!(r2["echo"], "echo: after");
    fs::remove_dir_all(&dir).ok();
}

/// M45：headless 单发模式（对齐 DSH `dsh --profile headless "job"`）——
/// echo-loop：提交任务 → 从 session 事件推导最终答案（assistant 文本）+
/// turn 结束原因；completed → reason 正确。
#[test]
fn headless_echo_loop_returns_answer_and_reason() {
    ensure_loop_built("echo-loop");
    let dir = unique_dir("headless-echo");
    let config = write_cordis_yaml(&dir, "cordis.yml", "echo-loop", "echo-loop");

    let boot = dsh_cli::boot(&config, &[], &wasm_base()).expect("boot");
    let result = dsh_cli::run_headless(&boot, "hello headless").expect("headless run");
    assert_eq!(result.answer, "echo: hello headless");
    assert_eq!(result.reason, "completed");
    fs::remove_dir_all(&dir).ok();
}

/// M45：headless 经 llm-loop 完整 turn——答案来自 llm 缝输出（多步：模型 →
/// 工具 → 最终回答），reason 从 turn/end 推导。
#[test]
fn headless_llm_loop_full_turn() {
    ensure_loop_built("llm-loop");
    let dir = unique_dir("headless-llm");
    let yaml = r#"
- id: services
  name: dsh:services
  config:
    services: [sessions, tools, llm]
    tools:
      - name: add
        op: add
    llm:
      provider: mock
      behavior: tool-first
- id: loop
  name: llm-loop
  config:
    wasm: llm-loop
"#;
    let config = dir.join("cordis.yml");
    fs::write(&config, yaml).unwrap();

    let boot = dsh_cli::boot(&config, &[], &wasm_base()).expect("boot");
    let result = dsh_cli::run_headless(&boot, "what is 2+3?").expect("headless run");
    // tool-first 适配器回答含历史条数（ctx=N）
    assert!(result.answer.contains("sum is 5"), "final answer: {}", result.answer);
    assert_eq!(result.reason, "completed");
    fs::remove_dir_all(&dir).ok();
}

/// M48：恢复会话（`restore_session` + `--session-in` 语义）——先跑一轮并
/// 保存（--session-out），新 boot 恢复后再跑一轮：llm 缝输入含前轮历史
/// （tool-first 回答的 ctx=N 增大——多轮共享上下文，对齐 DSH resume）。
#[test]
fn restore_session_resumes_context() {
    ensure_loop_built("llm-loop");
    let dir = unique_dir("resume");
    let yaml = r#"
- id: services
  name: dsh:services
  config:
    services: [sessions, tools, llm]
    tools:
      - name: add
        op: add
    llm:
      provider: mock
      behavior: tool-first
- id: loop
  name: llm-loop
  config:
    wasm: llm-loop
"#;
    let config = dir.join("cordis.yml");
    fs::write(&config, yaml).unwrap();
    let session_file = dir.join("session.jsonl");

    // 第一轮：保存会话
    let boot1 = dsh_cli::boot(&config, &[], &wasm_base()).expect("boot1");
    let r1 = dsh_cli::run_headless(&boot1, "first").expect("first headless");
    boot1.sessions.lock().unwrap().save_to(&session_file).expect("save");
    let ctx1: u64 = r1
        .answer
        .split("ctx=")
        .nth(1)
        .and_then(|s| s.split(')').next())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);

    // 第二轮：恢复会话后跑新任务 → ctx 更大（历史累积）
    let boot2 = dsh_cli::boot(&config, &[], &wasm_base()).expect("boot2");
    dsh_cli::restore_session(&boot2, &session_file).expect("restore");
    let r2 = dsh_cli::run_headless(&boot2, "second").expect("second headless");
    let ctx2: u64 = r2
        .answer
        .split("ctx=")
        .nth(1)
        .and_then(|s| s.split(')').next())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    assert!(ctx2 > ctx1, "restored session carries prior context (ctx1={ctx1}, ctx2={ctx2})");
    fs::remove_dir_all(&dir).ok();
}

/// M48：restore_session 对不存在文件报错（fail loud）。
#[test]
fn restore_session_missing_file_fails() {
    ensure_loop_built("echo-loop");
    let dir = unique_dir("resume-missing");
    let config = write_cordis_yaml(&dir, "cordis.yml", "echo-loop", "echo-loop");
    let boot = dsh_cli::boot(&config, &[], &wasm_base()).expect("boot");

    let err = dsh_cli::restore_session(&boot, &dir.join("nope.jsonl")).unwrap_err();
    assert!(err.to_string().contains("session load"), "{err}");
    fs::remove_dir_all(&dir).ok();
}

/// M56：`dump_config`（对齐生产 `dsh --dump-config`）——合并主配置 + overlays
/// 序列化为 YAML（不 boot loop）；overlay 的 config 替换 + insert 可见。
#[test]
fn dump_config_merges_overlays() {
    let dir = unique_dir("dump");
    let base = write_cordis_yaml(&dir, "base.yml", "echo-loop", "echo-loop");
    let overlay = write_cordis_yaml(&dir, "overlay.yml", "tool-loop", "tool-loop");

    let yaml = dsh_cli::dump_config(&base, &[overlay]).expect("dump-config");
    // overlay 替换 loop 的 name（echo-loop → tool-loop）
    assert!(yaml.contains("tool-loop"), "overlay applied: {yaml}");
    // 未命中的 services entry 保留
    assert!(yaml.contains("dsh:services"), "base entries preserved: {yaml}");
    // 输出是合法 YAML entries 列表
    let parsed: serde_json::Value = serde_yaml::from_str(&yaml).expect("valid yaml");
    let arr = parsed.as_array().expect("top-level array");
    assert!(arr.iter().any(|e| e.get("name").and_then(|n| n.as_str()) == Some("tool-loop")));
    fs::remove_dir_all(&dir).ok();
}

/// M58：HMR refresh 换 loop 组件——修改 config.wasm 指向不同组件（echo-loop
/// → tool-loop），refresh 后 `run_turn` 走新组件（返回 summary 而非 echo）。
#[test]
fn boot_refresh_swaps_loop_component() {
    ensure_loop_built("echo-loop");
    ensure_loop_built("tool-loop");
    let dir = unique_dir("hmr-swap");
    let config = dir.join("cordis.yml");
    let base_yaml = |wasm: &str| {
        format!(
            r#"
- id: services
  name: dsh:services
  config:
    services: [sessions, tools, llm]
- id: loop
  name: loop
  config:
    wasm: {wasm}
"#
        )
    };
    fs::write(&config, base_yaml("echo-loop")).unwrap();

    let boot = dsh_cli::boot(&config, &[], &wasm_base()).expect("boot");
    // 注册 add 工具（tool-loop 需要）
    let tools = boot
        .ctx
        .get_typed::<dsh_core::ToolRegistryHandle>("tools")
        .unwrap();
    tools.lock().unwrap().register("add", |args| {
        let a = args.get("a").and_then(|v| v.as_i64()).unwrap_or(0);
        let b = args.get("b").and_then(|v| v.as_i64()).unwrap_or(0);
        serde_json::json!({"sum": a + b})
    });

    let r1 = dsh_cli::run_turn(&boot, &json!({"content": "first"})).unwrap();
    assert_eq!(r1["echo"], "echo: first", "initial loop is echo-loop");

    // 改 config.wasm → tool-loop → refresh
    fs::write(&config, base_yaml("tool-loop")).unwrap();
    (boot.refresh)().expect("refresh swaps loop component");

    let r2 = dsh_cli::run_turn(&boot, &json!({"content": "second"})).unwrap();
    assert_eq!(r2["summary"], "2 + 3 = 5", "after refresh, loop is tool-loop");
    fs::remove_dir_all(&dir).ok();
}

// ---- 服务装配单元 Phase 1（E1 entry 化）：新增服务插件 entry 可声明装配 ----

/// 受控自定义服务插件：apply 时 provide 一个可观察标记服务（模拟未来 llm-pi-ai/自定义服务）。
struct TestSvcPlugin;
impl dsh_core::Plugin for TestSvcPlugin {
    fn name(&self) -> &'static str {
        "dsh:test-svc"
    }

    fn apply(
        &self,
        ctx: &dsh_core::Cordis,
        _config: dsh_core::Value,
    ) -> Result<dsh_core::EffectOutcome, dsh_core::CordisError> {
        ctx.provide("test-svc-marker", Arc::new(42i64))?;
        Ok(dsh_core::EffectOutcome::None)
    }
}

/// 带第二个服务插件的 cordis.yml（loop 在最后，服务 entry 在 loop 前——暴露
/// 「非 services 即 loop」假设：修改前 boot 因 `needs config.wasm` 失败）。
fn write_cordis_with_extra_service(dir: &std::path::Path) -> PathBuf {
    let path = dir.join("cordis-extra-svc.yml");
    let yaml = r#"
- id: services
  name: dsh:services
  config:
    services: [sessions]
- id: svc
  name: dsh:test-svc
- id: loop
  name: echo-loop
  config:
    wasm: echo-loop
"#;
    fs::write(&path, yaml).unwrap();
    path
}

/// E1（T1）：cordis.yml 声明新增服务插件 entry（非 dsh:services）→
/// `boot_with_host_plugins` 经「cordis.yml entry → loader 按名解析 → apply」装配，
/// 服务可见——从「非 services 必 config.wasm」假设中解放。
#[test]
fn boot_assembles_declared_service_plugin_entry_by_name() {
    ensure_loop_built("echo-loop");
    let dir = unique_dir("svc-entry");
    let config = write_cordis_with_extra_service(&dir);

    let boot = dsh_cli::boot_with_host_plugins(
        &config,
        &[],
        &wasm_base(),
        &[("dsh:test-svc", Arc::new(TestSvcPlugin) as Arc<dyn dsh_core::Plugin>)],
    )
    .expect("boot with a declared (non-services) service entry");

    let marker = boot
        .ctx
        .get_typed::<i64>("test-svc-marker")
        .expect("declared service entry applied and provided its service");
    assert_eq!(*marker, 42);
    fs::remove_dir_all(&dir).ok();
}

/// E1（T2）：HMR refresh 的 loop 定位按 config.wasm 判定（不看 name != dsh:services）；
/// 服务 entry 在 loop 前也不误判 loop。
#[test]
fn refresh_locates_loop_by_config_wasm_not_name() {
    ensure_loop_built("echo-loop");
    let dir = unique_dir("refresh-svc");
    let config = write_cordis_with_extra_service(&dir);

    let boot = dsh_cli::boot_with_host_plugins(
        &config,
        &[],
        &wasm_base(),
        &[("dsh:test-svc", Arc::new(TestSvcPlugin) as Arc<dyn dsh_core::Plugin>)],
    )
    .expect("boot with extra service entry");

    // refresh 重读主配置 + 重挂载：不因服务 entry 出现在 loop 前而把「loop 定位」误指到服务行
    (boot.refresh)().expect("refresh with a service entry before the loop entry");
    let r = dsh_cli::run_turn(&boot, &json!({"content": "after refresh"})).unwrap();
    assert_eq!(r["echo"], "echo: after refresh", "loop still resolved by config.wasm");
    fs::remove_dir_all(&dir).ok();
}

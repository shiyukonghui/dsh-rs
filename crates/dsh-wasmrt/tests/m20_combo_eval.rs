//! K4/F-05 spike：组合求值 WASM 面 vs native 面的**结果一致性**。
//!
//! 两面同源（都是 dsh-eval），因此这些测试锚定的是 **WASM 执行路径本身忠实**：
//! C ABI 编组、JSON 往返、数值语义、错误传播没有任何偏差。语料含真实 shipped
//! preset 的全部 `disabled_expr` + 覆盖 dsh-eval 语法面的合成表达式；`row_disabled`
//! 语义（fail-closed：求值失败 = 禁用）逐行复刻断言。
#![allow(clippy::arc_with_non_send_sync)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use dsh_wasmrt::{ComboEvaluator, NativeComboEvaluator, WasmComboEvaluator};

/// 构建（如缺失）并读取 combo-eval wasm 插件字节（C ABI，wasm32-unknown-unknown）。
fn combo_eval_wasm() -> Vec<u8> {
    let manifest: PathBuf =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../wasm-plugins/combo-eval");
    let wasm_path = manifest.join("target/wasm32-unknown-unknown/release/combo_eval.wasm");
    if !wasm_path.exists() {
        let status = Command::new("cargo")
            .args(["build", "--target", "wasm32-unknown-unknown", "--release"])
            .current_dir(&manifest)
            .status()
            .expect("run cargo build for combo-eval wasm plugin");
        assert!(status.success(), "combo-eval wasm plugin build failed");
    }
    fs::read(wasm_path).expect("read combo-eval wasm")
}

fn wasm_eval() -> WasmComboEvaluator {
    WasmComboEvaluator::new(&combo_eval_wasm()).expect("load combo-eval wasm plugin")
}

/// win32 门面（与 standing 测试同构；`process.platform === 'win32'` 为真）。
fn win32_scope() -> serde_json::Value {
    serde_json::json!({
        "process": { "platform": "win32", "env": {"PSModulePath": "/m"}},
        "config": { "text": "hi" },
    })
}

fn linux_scope() -> serde_json::Value {
    serde_json::json!({
        "process": { "platform": "linux", "env": {}},
        "config": { "text": "hi" },
    })
}

/// 断言两面同语料给出逐字节相同结果/错误。
fn assert_consistent(e: &dyn ComboEvaluator, n: &dyn ComboEvaluator, scope: &serde_json::Value, expr: &str) {
    let a = e.eval(scope, expr);
    let b = n.eval(scope, expr);
    match (a, b) {
        (Ok(va), Ok(vb)) => assert_eq!(va, vb, "value mismatch for `{expr}`"),
        (Err(ea), Err(eb)) => assert_eq!(ea, eb, "error mismatch for `{expr}`"),
        (oa, ob) => panic!(
            "face divergence for `{expr}`: wasm={:?} native={:?}",
            oa.map(|v| v.to_string()),
            ob.map(|v| v.to_string())
        ),
    }
}

/// 复刻 `row_disabled`（fail-closed：求值失败 = 禁用），对两面门控输出一致。
fn assert_row_disabled_consistent(
    e: &dyn ComboEvaluator,
    n: &dyn ComboEvaluator,
    scope: &serde_json::Value,
    expr: &str,
) {
    let gate = |f: &dyn ComboEvaluator| -> bool {
        match f.eval(scope, expr) {
            Ok(v) => dsh_eval::truthy(&v),
            Err(_) => true,
        }
    };
    assert_eq!(gate(e), gate(n), "row_disabled divergence for `{expr}`");
}

#[test]
fn both_faces_evaluate_real_preset_expressions_identically_on_win32_and_linux() {
    let w = wasm_eval();
    let n = NativeComboEvaluator;
    // 真实 shipped preset 的全部 disabled_expr（standard/cordis/code/minimal 实收）。
    let real_exprs = [
        "process.platform === 'win32'",
        "process.platform !== 'win32'",
    ];
    for expr in real_exprs {
        assert_consistent(&w, &n, &win32_scope(), expr);
        assert_consistent(&w, &n, &linux_scope(), expr);
        assert_row_disabled_consistent(&w, &n, &win32_scope(), expr);
        assert_row_disabled_consistent(&w, &n, &linux_scope(), expr);
    }
    // win32 语义本身：=== 真、!== 假（门控翻转）。
    assert!(dsh_eval::truthy(&w.eval(&win32_scope(), "process.platform === 'win32'").unwrap()));
    assert!(!dsh_eval::truthy(&w.eval(&win32_scope(), "process.platform !== 'win32'").unwrap()));
    assert!(!dsh_eval::truthy(&w.eval(&linux_scope(), "process.platform === 'win32'").unwrap()));
}

#[test]
fn both_faces_agree_across_the_evaluator_grammar() {
    let w = wasm_eval();
    let n = NativeComboEvaluator;
    let scopes = [win32_scope(), linux_scope()];
    let exprs = [
        // 字面量
        "null", "true", "false", "42", "-7", "3.5", "'str'", "[1, 2, 3]", "{\"a\": 1}",
        // 成员/索引读取 + 一元 + 算术/比较
        "process.platform",
        "process.env.PSModulePath",
        "config.text",
        "!process",
        "1 + 2 * 3",
        "10 % 3",
        "2 < 3 && 3 < 4",
        "5 > 8 || true",
        "process.platform === 'win32' || process.platform === 'darwin'",
        "process.platform !== 'linux' && config.text === 'hi'",
        // 三元
        "process.platform === 'win32' ? 'W' : 'U'",
        "config.text.length === 2 ? 1 : 0",
        // 白名单函数
        "String(process.platform)",
        "Number('7') + 1",
        "Boolean(process)",
        "Array.isArray([1])",
        "Object.keys(config).length === 1",
        // 引用缺失键 → undefined → truthy false；期语义与 native 一致（同源）
        "missing.deep",
        "missing === undefined",
        // 错误（不支持语法）→ 两面同为 Err && 原始错误串相同
        "1 ++ 2",
        "for (;;) {}",
        "foo(1)",
        "x = 3",
        "`tpl ${x}`",
    ];
    for scope in &scopes {
        for expr in exprs {
            assert_consistent(&w, &n, scope, expr);
            assert_row_disabled_consistent(&w, &n, scope, expr);
        }
    }
}

#[test]
fn wasm_face_matches_native_for_every_real_row_of_shipped_presets() {
    let w = wasm_eval();
    let n = NativeComboEvaluator;
    // 直接读真实 preset 组合的行（preset_rows 等价物）：对本文件依赖最小，用 parse。
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources/agent-presets");
    for id in ["standard", "code", "minimal", "cordis"] {
        let text = fs::read_to_string(root.join(id).join("agent.cordis.yml")).unwrap();
        let rows = dsh_agent_presets::parse::parse_composition(&text).unwrap();
        let proc = dsh_eval::process_facade();
        fn walk_rows(
            rows: &[dsh_agent_presets::parse::CompositionRow],
            wera: &WasmComboEvaluator,
            na: &NativeComboEvaluator,
            proc: &serde_json::Value,
        ) -> Vec<(String, bool, bool)> {
            let mut out = Vec::new();
            for row in rows {
                if row.group {
                    out.extend(walk_rows(&row.children, wera, na, proc));
                    continue;
                }
                if let Some(expr) = &row.disabled_expr {
                    let scope = serde_json::json!({
                        "process": proc.clone(),
                        "config": row.config.clone().unwrap_or(serde_json::Value::Null),
                    });
                    let gate = |f: &dyn ComboEvaluator| match f.eval(&scope, expr) {
                        Ok(v) => dsh_eval::truthy(&v),
                        Err(_) => true,
                    };
                    out.push((row.name.clone(), gate(wera), gate(na)));
                }
            }
            out
        }
        let gates = walk_rows(&rows, &w, &n, &proc);
        for (name, wg, ng) in &gates {
            assert_eq!(
                wg, ng,
                "{id}::{name}: wasm/native same disabled gate (real facade)"
            );
        }
        if !gates.is_empty() {
            eprintln!("{id}: {gates:?}");
        }
    }
}

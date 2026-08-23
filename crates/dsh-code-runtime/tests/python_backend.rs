//! dsh-code-runtime：python 子进程后端集成测试（M5-DESIGN §7.5 计划，D-066 真实落地）。
//!
//! 本箱 python 可用（D:\Anaconda）→ 全部真实执行；`python_available()` 探测失败则整组
//! 跳过并打印原因（非实现缺陷，环境门控）。覆盖：返回值/lossless 大整数/日志捕获/
//! 异常/binding 派发与拒绝/超时/中止/输出超限/非有限完成值/命名空间契约。

use dsh_code_runtime::{
    python_available, CancellationToken, CodeBindingFunction, CodeBindingNamespace,
    CodeRunFailureKind, CodeRunRequest, CodeRuntime, PythonCodeRuntime, PythonConfig,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

fn runtime() -> PythonCodeRuntime {
    PythonCodeRuntime::new(PythonConfig {
        timeout_ms: 8_000,
        max_output_bytes: 64 * 1024,
        ..PythonConfig::default()
    })
}

fn global_alive() -> bool {
    if !python_available() {
        eprintln!("    ~ 跳过：本环境无可用 python（非实现缺陷）");
        false
    } else {
        true
    }
}

fn request<'a>(program: &'a str, bindings: Vec<CodeBindingNamespace>) -> CodeRunRequest<'a> {
    CodeRunRequest {
        program,
        bindings,
        signal: None,
    }
}

fn echo_binding() -> CodeBindingFunction {
    Arc::new(|args: Value| Ok(args))
}

fn bindings_with(names: &[(&str, CodeBindingFunction)]) -> Vec<CodeBindingNamespace> {
    let mut fns = HashMap::new();
    for (name, f) in names {
        fns.insert((*name).to_string(), Arc::clone(f));
    }
    vec![CodeBindingNamespace {
        global: "tools".into(),
        functions: fns,
        error_class: None,
    }]
}

#[test]
fn python_return_value() {
    if !global_alive() {
        return;
    }
    let result = runtime().run(&request("return 1 + 41", vec![]));
    assert!(result.error.is_none(), "err: {:?}", result.error);
    assert_eq!(result.value, Some(json!(42)));
}

#[test]
fn python_big_integer_lossless() {
    if !global_alive() {
        return;
    }
    let result = runtime().run(&request("return 1152921504606846976", vec![])); // 2^60
    assert_eq!(
        result.value,
        Some(json!(1152921504606846976u64)),
        "整数精确跨界"
    );
}

#[test]
fn python_logs_captured_via_log_frames() {
    if !global_alive() {
        return;
    }
    let result = runtime().run(&request(
        "print(\"hello-log\")\nprint(\"line-2\")\nreturn None",
        vec![],
    ));
    assert!(result.error.is_none());
    assert!(result.value.is_none(), "python None → 无完成值");
    assert!(
        result.logs.iter().any(|l| l.contains("hello-log")),
        "logs: {:?}",
        result.logs
    );
    assert!(
        result.logs.iter().any(|l| l.contains("line-2")),
        "logs: {:?}",
        result.logs
    );
}

#[test]
fn python_exception_is_failure_field() {
    if !global_alive() {
        return;
    }
    let result = runtime().run(&request("raise ValueError(\"boom\")", vec![]));
    let err = result.error.expect("exception failure");
    assert_eq!(err.kind, CodeRunFailureKind::Exception);
    assert!(err.message.contains("boom"), "msg: {}", err.message);
}

#[test]
fn python_binding_dispatch_echo() {
    if !global_alive() {
        return;
    }
    let bindings = bindings_with(&[("echo", echo_binding())]);
    let result = runtime().run(&request("return tools.echo({'n': 41})['n'] + 1", bindings));
    assert!(result.error.is_none(), "err: {:?}", result.error);
    assert_eq!(result.value, Some(json!(42)), "binding round-trip + 1");
}

#[test]
fn python_binding_reject_raises_in_program() {
    if !global_alive() {
        return;
    }
    let reject: CodeBindingFunction =
        Arc::new(|_args: Value| Err::<Value, String>("nope".to_string()));
    let bindings = vec![CodeBindingNamespace {
        global: "tools".into(),
        functions: {
            let mut m = HashMap::new();
            m.insert("deny".into(), reject);
            m
        },
        error_class: None,
    }];
    let result = runtime().run(&request("return tools.deny(1)", bindings));
    let err = result.error.expect("reject → program raises");
    assert_eq!(err.kind, CodeRunFailureKind::Exception);
    assert!(
        err.message.contains("nope") || err.message.contains("deny"),
        "{}",
        err.message
    );
}

#[test]
fn python_timeout_terminates_run() {
    if !global_alive() {
        return;
    }
    let started = std::time::Instant::now();
    let rt = PythonCodeRuntime::new(PythonConfig {
        timeout_ms: 500,
        max_output_bytes: 64 * 1024,
        ..PythonConfig::default()
    });
    let result = rt.run(&request("while True:\n    pass", vec![]));
    let err = result.error.expect("timeout failure");
    assert_eq!(err.kind, CodeRunFailureKind::Timeout);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "硬预算内返回"
    );
}

#[test]
fn python_abort_cancels_run() {
    if !global_alive() {
        return;
    }
    let token = CancellationToken::new();
    token.cancel();
    let result = runtime().run(&CodeRunRequest {
        program: "while True:\n    pass",
        bindings: vec![],
        signal: Some(&token),
    });
    let err = result.error.expect("abort failure");
    assert_eq!(err.kind, CodeRunFailureKind::Abort);
}

#[test]
fn python_output_limit_classified() {
    if !global_alive() {
        return;
    }
    let rt = PythonCodeRuntime::new(PythonConfig {
        timeout_ms: 5_000,
        max_output_bytes: 16,
        ..PythonConfig::default()
    });
    let result = rt.run(&request("return \"x\" * 100000", vec![]));
    let err = result.error.expect("output-limit");
    assert_eq!(err.kind, CodeRunFailureKind::OutputLimit);
}

#[test]
fn python_non_finite_completion_is_invalid_output() {
    if !global_alive() {
        return;
    }
    let result = runtime().run(&request("return float(\"nan\")", vec![]));
    let err = result
        .error
        .expect("invalid-output（worker 自检失败序列化）");
    assert_eq!(err.kind, CodeRunFailureKind::InvalidOutput);
}

#[test]
fn invalid_namespace_is_honest_contract_failure() {
    if !global_alive() {
        return;
    }
    let bindings = vec![CodeBindingNamespace {
        global: "console".into(),
        functions: HashMap::new(),
        error_class: None,
    }];
    let result = runtime().run(&request("return 1", bindings));
    let err = result.error.expect("契约误用失败");
    assert_eq!(err.kind, CodeRunFailureKind::WorkerExit);
    assert!(
        err.message.contains("invalid binding namespace"),
        "{}",
        err.message
    );
}

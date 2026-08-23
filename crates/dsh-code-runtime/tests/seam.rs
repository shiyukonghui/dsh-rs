//! dsh-code-runtime 缝：可移植标识符排除集 + 校验 + TS 诚实桩（M5-DESIGN §7.1/7.2/7.4）。

use dsh_code_runtime::is_dunder_member;
use dsh_code_runtime::CancellationToken;
use dsh_code_runtime::{
    validate_binding_namespace, CodeBindingErrorClass, CodeBindingNamespace, CodeLanguage,
    CodeRunFailureKind, CodeRunRequest, CodeRuntime, Isolation, ThreadWorkerStub,
    PORTABLE_RESERVED_WORDS, RESERVED_BINDING_GLOBALS, RESERVED_ERROR_MEMBERS,
};

#[test]
fn reserved_binding_globals_covers_each_backend_slot() {
    for name in [
        "console",
        "__dsh_main__",
        "__builtins__",
        "__name__",
        "__debug__",
    ] {
        assert!(
            RESERVED_BINDING_GLOBALS.contains(&name),
            "{name} must be reserved"
        );
    }
    assert!(!RESERVED_BINDING_GLOBALS.contains(&"tools"));
}

#[test]
fn reserved_error_members_covers_js_and_python_protocol() {
    for name in [
        "name",
        "message",
        "stack",
        "args",
        "with_traceback",
        "add_note",
    ] {
        assert!(
            RESERVED_ERROR_MEMBERS.contains(&name),
            "{name} must be reserved"
        );
    }
    assert!(!RESERVED_ERROR_MEMBERS.contains(&"code"));
}

#[test]
fn dunder_member_matches_dunder_forms_only() {
    assert!(is_dunder_member("__dict__"));
    assert!(is_dunder_member("__init__"));
    assert!(is_dunder_member("__x__"));
    assert!(!is_dunder_member("_private"));
    assert!(!is_dunder_member("name"));
    assert!(!is_dunder_member("__mid"));
    assert!(!is_dunder_member("__")); // 空中间 → 非真 dunder
    assert!(!is_dunder_member("____"));
}

#[test]
fn portable_reserved_words_is_ecma_union_python() {
    assert!(PORTABLE_RESERVED_WORDS.contains(&"function"));
    assert!(PORTABLE_RESERVED_WORDS.contains(&"lambda"));
    assert!(PORTABLE_RESERVED_WORDS.contains(&"nonlocal"));
    assert!(PORTABLE_RESERVED_WORDS.contains(&"class"));
    assert!(!PORTABLE_RESERVED_WORDS.contains(&"tools"));
}

#[test]
fn validate_namespace_portable_identifier_rule() {
    // 非法标识符拒绝
    assert!(validate_binding_namespace(&ns("$tools")).is_err());
    assert!(validate_binding_namespace(&ns("1tools")).is_err());
    // ✅ 合法
    assert!(validate_binding_namespace(&ns("tools")).is_ok());
    assert!(validate_binding_namespace(&ns("_x1")).is_ok());
}

#[test]
fn validate_namespace_reserved_word_and_global() {
    assert!(validate_binding_namespace(&ns("lambda")).is_err(), "保留字");
    assert!(
        validate_binding_namespace(&ns("console")).is_err(),
        "后端占位槽"
    );
    assert!(validate_binding_namespace(&ns("__name__")).is_err());
}

#[test]
fn validate_namespace_error_class_member_name_rules() {
    let bad_dunder = ns_with_error("ToolsError", "__dict__");
    assert!(
        validate_binding_namespace(&bad_dunder).is_err(),
        "dunder 成员拒绝"
    );
    let bad_reserved = ns_with_error("ToolsError", "name");
    assert!(
        validate_binding_namespace(&bad_reserved).is_err(),
        "保留错误成员拒绝"
    );
    let bad_class = ns_with_error("lambda", "code");
    assert!(
        validate_binding_namespace(&bad_class).is_err(),
        "错误类名保留字拒绝"
    );
    let ok = ns_with_error("ToolsError", "memberCode");
    assert!(validate_binding_namespace(&ok).is_ok());
}

#[test]
fn language_and_isolation_roundtrip() {
    assert_eq!(CodeLanguage::TypeScript.as_str(), "typescript");
    assert_eq!(CodeLanguage::Python.as_str(), "python");
    assert_eq!(Isolation::WorkerThread.as_str(), "worker-thread");
    assert_eq!(Isolation::Process.as_str(), "process");
}

#[test]
fn thread_worker_stub_is_honest() {
    let stub = ThreadWorkerStub;
    assert_eq!(stub.language(), CodeLanguage::TypeScript);
    assert_eq!(stub.isolation(), Isolation::WorkerThread);
    let result = stub.run(&CodeRunRequest {
        program: "return 1",
        bindings: vec![],
        signal: None,
    });
    assert!(result.value.is_none());
    assert!(result.logs.is_empty());
    let err = result.error.expect("恒失败");
    assert_eq!(err.kind, CodeRunFailureKind::WorkerExit);
    assert_eq!(err.message, "requires a code runtime");
}

#[test]
fn cancellation_token_fires() {
    let token = CancellationToken::new();
    assert!(!token.is_cancelled());
    token.cancel();
    assert!(token.is_cancelled());
}

fn ns(global: &str) -> CodeBindingNamespace {
    CodeBindingNamespace {
        global: global.to_string(),
        functions: std::collections::HashMap::new(),
        error_class: None,
    }
}

fn ns_with_error(global: &str, member: &str) -> CodeBindingNamespace {
    CodeBindingNamespace {
        global: global.to_string(),
        functions: std::collections::HashMap::new(),
        error_class: Some(CodeBindingErrorClass {
            name: global.to_string(),
            member_name_property: member.to_string(),
        }),
    }
}

//! 缝契约（M5-DESIGN §7.1/7.2，逐字 `code-runtime/src/index.ts`）：可移植标识符排除集、
//! 命名空间校验、`CodeRuntime` trait。一套共享排除集保证「list 在一个后端有效 → 所有
//! 后端有效」。

use crate::types::{CodeBindingNamespace, CodeLanguage, CodeRunRequest, CodeRunResult, Isolation};

/// 每个后端都拒绝的绑定全局：`console`（worker 日志捕获）、`__dsh_main__`/`__builtins__`
/// /`__name__`（python 引导与预置模块全局）、`__debug__`（CPython 编译期常量名，注入
/// 亦不可达）。一处共享，保持可移植承诺真实。
pub static RESERVED_BINDING_GLOBALS: &[&str] = &[
    "console",
    "__dsh_main__",
    "__builtins__",
    "__name__",
    "__debug__",
];

/// `member_name_property` 每个后端都拒绝的名字（JS `Error` 排除项 + python
/// 异常协议成员）；dunder 形式（`__x__`，非空中间）整体拒绝。
pub static RESERVED_ERROR_MEMBERS: &[&str] = &[
    "name",
    "message",
    "stack",
    "args",
    "with_traceback",
    "add_note",
];

/// dunder 形式（`__x__`，非空中间）：python 对象协议槽，每个后端都拒绝为错误成员。
pub fn is_dunder_member(s: &str) -> bool {
    s.len() >= 5 && s.starts_with("__") && s.ends_with("__")
}

/// 每个可移植目标语言（ECMAScript ∪ Python）的保留字并集，任何后端都拒绝为
/// 全局/错误类名。扩充语言 = 加宽并集（破坏性审查，设计如此）。
pub static PORTABLE_RESERVED_WORDS: &[&str] = &[
    // ECMAScript 保留字与严格模式保留名
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "new",
    "null",
    "return",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
    "let",
    "static",
    "implements",
    "interface",
    "package",
    "private",
    "protected",
    "public",
    "arguments",
    "eval",
    // Python 3.x 关键字 + 软关键字（type/_ 安全起见纳入）
    "False",
    "None",
    "True",
    "and",
    "as",
    "assert",
    "async",
    "def",
    "del",
    "elif",
    "except",
    "from",
    "global",
    "is",
    "lambda",
    "nonlocal",
    "not",
    "or",
    "pass",
    "raise",
    "match",
    "type",
    "_",
];

/// LANGUAGE-PORTABLE 标识符子集 `[A-Za-z_][A-Za-z0-9_]*`。
pub fn is_portable_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// 校验一个绑定命名空间（可移植承诺：此面唯一）。
pub fn validate_binding_namespace(ns: &CodeBindingNamespace) -> Result<(), String> {
    let global = &ns.global;
    if !is_portable_identifier(global) {
        return Err(format!(
            "binding global {global:?} is not a portable identifier ([A-Za-z_][A-Za-z0-9_]*)"
        ));
    }
    if PORTABLE_RESERVED_WORDS.contains(&global.as_str()) {
        return Err(format!("binding global {global:?} is a reserved word"));
    }
    if RESERVED_BINDING_GLOBALS.contains(&global.as_str()) {
        return Err(format!(
            "binding global {global:?} is a reserved binding global"
        ));
    }
    if let Some(ec) = &ns.error_class {
        if !is_portable_identifier(&ec.name) {
            return Err(format!(
                "error class name {:?} is not a portable identifier",
                ec.name
            ));
        }
        if PORTABLE_RESERVED_WORDS.contains(&ec.name.as_str()) {
            return Err(format!("error class name {:?} is a reserved word", ec.name));
        }
        let member = &ec.member_name_property;
        if member.is_empty() {
            return Err("error memberNameProperty must be non-empty".to_string());
        }
        if RESERVED_ERROR_MEMBERS.contains(&member.as_str()) {
            return Err(format!("error memberNameProperty {member:?} is reserved"));
        }
        if is_dunder_member(member) {
            return Err(format!(
                "error memberNameProperty {member:?} is a dunder member"
            ));
        }
    }
    Ok(())
}

/// code 执行缝：跑一段模型写的程序，对宿主异步绑定。
/// 错误是结果字段；`run()` 只在契约误用时失败。
pub trait CodeRuntime: Send + Sync {
    /// `program` 的源语言（信息性，非门控）。
    fn language(&self) -> CodeLanguage;
    /// 执行基质（信息性）。
    fn isolation(&self) -> Isolation;
    /// 执行一次。实现把程序当敌对方、隔离各次运行。
    fn run(&self, request: &CodeRunRequest) -> CodeRunResult;
}

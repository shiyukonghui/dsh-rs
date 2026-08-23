//! 缝词汇类型（M5-DESIGN §7.1，逐字 `code-runtime/src/types.ts`）：调用方交给
//! `CodeRuntime` 什么、拿回什么。纯类型，无运行时代码。

use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// 宿主侧暴露给程序的一个可调用成员。参数与返回值 MUST 是 lossless JSON；
/// runtime 拒绝有损/不可克隆值而非破坏运行。拒绝会在程序内体现为该调用的 rejection。
pub type CodeBindingFunction = Arc<dyn Fn(Value) -> Result<Value, String> + Send + Sync>;

/// 程序可见、按命名空间注入的 typed rejection 契约。
#[derive(Debug, Clone)]
pub struct CodeBindingErrorClass {
    /// 构造全局标识（程序员可见的 `Error.name`）。
    pub name: String,
    /// 非空自有属性，存放成员名（不得为 `RESERVED_ERROR_MEMBERS` 或 dunder 形式）。
    pub member_name_property: String,
}

/// 一个命名通用对象（例如 `tools`）暴露给程序的函数组。函数名任意字符串
/// （`__proto__`/`constructor` 视为普通自有属性，永不视作原型碰撞）。
pub struct CodeBindingNamespace {
    /// 程序可见的全局标识（LANGUAGE-PORTABLE 子集，非保留字，非 `RESERVED_BINDING_GLOBALS`）。
    pub global: String,
    pub functions: HashMap<String, CodeBindingFunction>,
    pub error_class: Option<CodeBindingErrorClass>,
}

/// 中止令牌：run 的硬中断（甚至循环中）。触发后 runtime 停止程序并 `Abort`。
#[derive(Debug, Clone, Default)]
pub struct CancellationToken(pub Arc<AtomicBool>);

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// 一次运行：程序源码 + runtime 据以行动的一切（显式优于隐式，请求不带隐藏旋钮）。
pub struct CodeRunRequest<'a> {
    /// 程序体（runtime 期望的语言，作为 async 函数体：顶层 await/return 可用）。
    pub program: &'a str,
    /// 暴露给程序的主机函数（每命名空间一个全局对象）。
    pub bindings: Vec<CodeBindingNamespace>,
    /// 中止信号。
    pub signal: Option<&'a CancellationToken>,
}

/// 失败类别。正交结果独立报告：预算到期不是异常、中止不是超时、基质死亡两者都不是。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeRunFailureKind {
    /// 程序抛出或 Parse/Transform 失败。
    Exception,
    /// 实现自有预算到期；message 说明是哪种。
    Timeout,
    /// `CancellationToken` 触发。
    Abort,
    /// 执行基质死亡而未 settle（如 OOM）。
    WorkerExit,
    /// 完成值不是 lossless JSON。
    InvalidOutput,
    /// 序列化的外层 log/值/诊断超出配置上限。
    OutputLimit,
}

impl CodeRunFailureKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            CodeRunFailureKind::Exception => "exception",
            CodeRunFailureKind::Timeout => "timeout",
            CodeRunFailureKind::Abort => "abort",
            CodeRunFailureKind::WorkerExit => "worker-exit",
            CodeRunFailureKind::InvalidOutput => "invalid-output",
            CodeRunFailureKind::OutputLimit => "output-limit",
        }
    }
}

/// 一次运行失败；`message` 适合反馈给模型自我修正。
#[derive(Debug, Clone)]
pub struct CodeRunFailure {
    pub kind: CodeRunFailureKind,
    pub message: String,
    pub detail: Option<String>,
}

/// 一次运行的结果。错误是结果上的字段，永不 reject `run()`——
/// 报告失败的程序是调用方的职责，不是异常路径。
#[derive(Debug)]
pub struct CodeRunResult {
    /// 程序完成值（成功穿越 lossless-JSON 边界时）。
    pub value: Option<Value>,
    /// 程序按序发出的文本（仅受外层结果约束）。
    pub logs: Vec<String>,
    /// 失败时 Present。
    pub error: Option<CodeRunFailure>,
}

/// 程序源语言（`code-runtime/types` 知情值：'typescript' | 'python'）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeLanguage {
    TypeScript,
    Python,
}

impl CodeLanguage {
    pub fn as_str(&self) -> &'static str {
        match self {
            CodeLanguage::TypeScript => "typescript",
            CodeLanguage::Python => "python",
        }
    }
}

/// 执行基质（信息性，非安全声明）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Isolation {
    WorkerThread,
    Process,
}

impl Isolation {
    pub fn as_str(&self) -> &'static str {
        match self {
            Isolation::WorkerThread => "worker-thread",
            Isolation::Process => "process",
        }
    }
}

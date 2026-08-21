//! system-prompt 伴生 invariant：校验水岭**权威输出**（对齐 TS invariant.ts）。
//!
//! 宿主不在本 crate：暴露纯校验器 + 一个「以 prepend 全局监听器包住水岭」的安装
//! 辅助，供上层 invariant 宿主（M2g）注册 `@deepseek-ai/dsh-system-prompt` 清单。
//!
//! 窄化说明（D-028）：TS 侧需校验「text 非 string」「变量值非法型」等**运行时型违
//! 规**——Rust 的类型系统已在 `PromptAssembly` 上排除它们（waterfall listener 只
//! 能产出 `String`/`Option<String>`）。Rust 面保留可触达的违规类别：空名/重名/
//! 变量名非法。消息逐字对齐 B.6 契约，类型不可能的条目只在清单 doc 里记录。

use crate::{is_variable_name, PromptAssembly, SystemPrompt};

/// 包清单名（invariant 宿主注册用）。
pub const PACKAGE_NAME: &str = "@deepseek-ai/dsh-system-prompt";

/// 校验水岭最终返回的权威组装；每处违规调用 `fail(消息)`（可多次）。
pub fn validate_assembly(assembly: &PromptAssembly, fail: &mut dyn FnMut(String)) {
    let mut section_names: Vec<String> = Vec::new();
    for section in &assembly.sections {
        if section.name.is_empty() {
            fail("assembled section names must be non-empty".to_string());
        }
        if section_names.contains(&section.name) {
            fail(format!(
                "assembled section name {} is duplicated",
                crate::quoted(&section.name)
            ));
        }
        section_names.push(section.name.clone());
    }

    let mut context_names: Vec<String> = Vec::new();
    for context in &assembly.contexts {
        if context.name.is_empty() {
            fail("assembled context names must be non-empty".to_string());
        }
        if context_names.contains(&context.name) {
            fail(format!(
                "assembled context name {} is duplicated",
                crate::quoted(&context.name)
            ));
        }
        context_names.push(context.name.clone());
    }

    for tool in &assembly.tools {
        if tool.name.is_empty() {
            fail("assembled tool names must be non-empty".to_string());
        }
    }

    for (name, _value) in &assembly.variables {
        if !is_variable_name(name) {
            fail(format!(
                "assembled variable name {} is invalid",
                crate::quoted(name)
            ));
        }
    }
}

/// 安装：把 `validate_assembly` 以 `{global:true, prepend:true}` 包在其它监听器外，
/// 校验**瀑布最终返回**的权威物。所有违规以 `; ` 连接为一个 Err。
pub fn install(sp: &SystemPrompt) {
    let listener: crate::AssembleListener = Rc::new(move |assembly, _, next| {
        let assembled = next(assembly)?;
        let mut failures: Vec<String> = Vec::new();
        {
            let mut fail = |msg: String| failures.push(msg);
            validate_assembly(&assembled, &mut fail);
        }
        if !failures.is_empty() {
            return Err(failures.join("; "));
        }
        Ok(assembled)
    });
    sp.register_assemble_listener(None, true, listener);
}

use std::rc::Rc;

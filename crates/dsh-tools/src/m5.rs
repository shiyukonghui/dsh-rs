//! M5h: M5 执行工具的「定义构造器」（纯定义面，slot-bind 通用机制）。
//!
//! 镜像 `m4.rs` 的 [M4Tool] 模式但参数化：参数/输出/渲染由调用方（dsh-cli web.rs 接线）
//! 传入——M5 各 crate（dsh-fs/dsh-shell/dsh-terminal/dsh-code-runtime）已各自拥有纯面
//! schema/render，本模块为避免依赖环（dsh-tools 是被依赖基座）不引用任何 M5 crate。
//!
//! 一个 [M5Tool] 定义注册后，execute 从共享槽读入：已 [`M5Tool::bind`] → 委托真实宿主
//! 执行器；未绑定 → 结构化 `NOT_BOUND` 错误（复用 [`crate::m4::not_bound_failure`]），
//! 绝不伪装成功（D-052 同款承诺；M5-DESIGN §8）。后续宿主句柄装配经 web.rs 注入。

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::Value;

use crate::m4::not_bound_failure;
use crate::schema::{define_tool, DefineToolOptions, ToolDefinitionError};
use crate::types::{ToolDefinition, ToolExecute, ToolRender};

/// 一个可绑定宿主执行器的 M5 工具定义。注册的是 [`M5Tool::definition`]。
/// `bind` 在注册后随时可调（同 `Rc` 生效）。
pub struct M5Tool {
    def: Rc<ToolDefinition>,
    slot: Rc<RefCell<Option<ToolExecute>>>,
}

impl M5Tool {
    /// 待注册/已注册的定义（`Rc`，`bind` 后同一定义即刻委托宿主）。
    pub fn definition(&self) -> Rc<ToolDefinition> {
        self.def.clone()
    }

    /// 绑定真实宿主执行器（web.rs 接线时注入）。
    pub fn bind(&self, executor: ToolExecute) {
        *self.slot.borrow_mut() = Some(executor);
    }

    /// 当前是否已绑定宿主执行器。
    pub fn is_bound(&self) -> bool {
        self.slot.borrow().is_some()
    }
}

/// 构建一个 slot 承载的 M5 工具定义（参数/输出/渲染来自 M5 crate 纯面；未绑定 →
/// `NOT_BOUND` 结构化错误）。
pub fn define_m5_tool(
    name: &str,
    description: String,
    parameters: Value,
    output_schema: Value,
    render: ToolRender,
) -> Result<M5Tool, ToolDefinitionError> {
    let slot: Rc<RefCell<Option<ToolExecute>>> = Rc::new(RefCell::new(None));
    let slot_for_execute = slot.clone();
    let unbound = not_bound_failure(name, "M5");
    let execute: ToolExecute = Rc::new(move |args, ctx| {
        let executor = slot_for_execute.borrow().clone();
        match executor {
            Some(f) => f(args, ctx),
            None => Err(unbound.clone()),
        }
    });
    let def = define_tool(DefineToolOptions {
        name: name.to_string(),
        description,
        parameters,
        output_schema,
        render,
        execute,
        ..Default::default()
    })?;
    Ok(M5Tool {
        def: Rc::new(def),
        slot,
    })
}

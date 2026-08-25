//! `model-selection`：`installModelSelection(agentCtx, selection)` 的 Rust 版。
//!
//! 安装两个 scoped 水岭监听器 + 无 fallback、无 pending 合并：
//! 1. `system-prompt/assemble`（prompt 组装侧）：入口先捕获 `selection.current` →
//!    跑完链（`next`）→ `selection.assembled = 捕获值` → 有值则覆盖 variables 的
//!    provider/model。
//! 2. `agent/request`（请求路由侧）：`resolved = next(payload)` →
//!    `selected = selection.assembled`；无值原样透传；有值则**无条件剥离**继承的
//!    reasoningEffort、写 provider/model、有 reasoningEffort 才恢复。
//!
//! 差异声明：TS 的安装发生在 agent scope 内由 scope teardown 拆除；Rust 侧
//! M2d-2 只安装（拆除由 M2e loop 的生命周期负责——即「作用域拆除替代 disposer」，
//! D-030 记）。无 fallback/并发合并（loop 层职责，报告 A.3 model-selection 明示）。
//!
//! D-115：`sel: Rc<RefCell>` → `Arc<Mutex>`、bus 监听器 `Rc<dyn Fn>` → `Arc<dyn Fn +
//! Send + Sync>`（worker 化前置）。dsh-system-prompt 一侧仍 `Rc<dyn Fn>`（不在 D-115
//! 库存，Phase 3 预算外）——`register_assemble_listener` 保持 Rc，闭包内捕获 Arc 即可。

use std::rc::Rc;
use std::sync::{Arc, Mutex};

use dsh_scope::ScopeKey;
use dsh_system_prompt::SystemPrompt;

use crate::agent_bus::{AgentBus, NextFn};
use crate::types::ModelSelectionRef;

/// 组装侧变量：覆盖或追加 `provider`/`model`（对齐
/// `{ ...assembled.variables, provider, model }`）。
fn upsert_variable(variables: &mut Vec<(String, Option<String>)>, name: &str, value: String) {
    match variables.iter_mut().find(|(n, _)| *n == name) {
        Some(slot) => slot.1 = Some(value),
        None => variables.push((name.to_string(), Some(value))),
    }
}

/// 安装两个水岭监听器；`sel` 的 `current`（选择侧）与 `assembled`（组装捕获侧）
/// 由驱动方（loop/lifecycle）维护读。
#[allow(clippy::type_complexity)]
pub fn install_model_selection(
    sp: &Rc<SystemPrompt>,
    bus: &AgentBus,
    scope: &ScopeKey,
    sel: Arc<Mutex<ModelSelectionRef>>,
) {
    // 1) prompt 组装侧（system-prompt assemble 水岭，scoped 到 agent）
    //    注：dsh-system-prompt 的 listener 仍是 Rc<dyn Fn>（D-115 库存外）。
    let sel1 = sel.clone();
    let scope1 = scope.clone();
    sp.register_assemble_listener(Some(scope1), false, Rc::new(
        move |assembly, _ctx, next| {
            // 进入时捕获 current（不理会链中变化）
            let selected = sel1.lock().unwrap().current.clone();
            let mut result = next(assembly)?;
            sel1.lock().unwrap().assembled = selected.clone();
            if let Some(s) = selected {
                upsert_variable(&mut result.variables, "provider", s.provider);
                upsert_variable(&mut result.variables, "model", s.model);
            }
            Ok(result)
        },
    ));

    // 2) 请求路由侧（agent/request 水岭，scoped 到 agent）
    let scope2 = scope.clone();
    bus.on_chain(
        "agent/request",
        false,
        Some(scope2),
        Arc::new(move |payload: serde_json::Value, next: NextFn| {
            let resolved = next(payload);
            let selected = sel.lock().unwrap().assembled.clone();
            let Some(s) = selected else {
                return resolved;
            };
            let mut out = resolved;
            if let Some(map) = out.as_object_mut() {
                // 无条件剥离继承的 reasoningEffort（恢复所选 provider/default 行为）
                map.remove("reasoningEffort");
                map.insert("provider".into(), serde_json::Value::String(s.provider.clone()));
                map.insert("model".into(), serde_json::Value::String(s.model.clone()));
                if let Some(re) = s.reasoning_effort {
                    map.insert(
                        "reasoningEffort".into(),
                        serde_json::Value::String(re.raw().to_string()),
                    );
                }
            }
            out
        }),
    );
}

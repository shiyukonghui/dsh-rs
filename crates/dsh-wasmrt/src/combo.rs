//! K4/F-05 spike：组合求值的双面求值器——**WASM 面**（`dsh-eval` 编译进 wasm，
//! 经 C ABI `host_provide` 回传结果）与 **native 面**（`dsh_eval` 直连）都由
//! `dsh-eval` 同一源码驱动，native 是 WASM 面不可用/出错时的幂等**兜底**。
//!
//! 用途：组合 `disabled_expr` / 门控表达式走这两个面之一求值；结果一致性测试
//! （`tests/m20_combo_eval.rs`）锚定的是 **WASM 执行路径本身是忠实的**（ABI /
//! JSON 编组 / 数值 / 错误传播），因为两面的求值语义天然同源，一致性测试即验证
//! wasm 宿主桥没有引入偏差。
//!
//! `ComboEvaluator::eval(scope, expr)`：`scope` 为 flat 标识符 → 值 的 JSON 对象
//! （与 `row_disabled` 喂给 `dsh_eval::Scope` 的 `{process, config}` 同构）；
//! 求值失败 → `Err(原始错误串)`（fail-closed 门控由调用方按 `dsh_eval::truthy` 处理）。

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use dsh_core::{Cordis, CordisError, Plugin, Value};
use serde_json::json;

use crate::{load_wasm_plugin, Capabilities};

/// 复刻 `row_disabled` 把 JSON scope 摊平成 `dsh_eval::Scope`（flat 标识符→值）。
fn scope_map(scope: &Value) -> HashMap<String, Value> {
    let mut map = HashMap::new();
    if let Some(obj) = scope.as_object() {
        for (k, v) in obj {
            map.insert(k.clone(), v.clone());
        }
    }
    map
}

/// 组合表达式求值器（双面）。`expr` 求值失败用 `Err` 携带原始错误串。
pub trait ComboEvaluator {
    fn eval(&self, scope: &Value, expr: &str) -> Result<Value, String>;
}

/// native 兜底面：`dsh_eval` 直连（当前 `row_disabled` 的路径）。
#[derive(Debug, Default, Clone, Copy)]
pub struct NativeComboEvaluator;

impl ComboEvaluator for NativeComboEvaluator {
    fn eval(&self, scope: &Value, expr: &str) -> Result<Value, String> {
        dsh_eval::evaluate(&scope_map(scope), expr).map_err(|e| e.0.to_string())
    }
}

/// F-05 语义的组合求值器：**WASM 面为主、native 兜底**——WASM 面求值失败/不可用
/// 时回落 native（同源，结果一致；m20 一致性测试锚定 wasm 执行路径忠实）。
pub struct FallbackEval {
    pub primary: Rc<dyn ComboEvaluator>,
    pub fallback: Rc<dyn ComboEvaluator>,
}

impl FallbackEval {
    pub fn new(primary: Rc<dyn ComboEvaluator>, fallback: Rc<dyn ComboEvaluator>) -> Self {
        FallbackEval { primary, fallback }
    }
}

impl ComboEvaluator for FallbackEval {
    fn eval(&self, scope: &Value, expr: &str) -> Result<Value, String> {
        self.primary
            .eval(scope, expr)
            .or_else(|_| self.fallback.eval(scope, expr))
    }
}

/// WASM 面：加载 `combo-eval` wasm 插件（dsh-eval 同源编译体），每次求值在
/// 一次性 `Cordis` 里跑 `plugin_apply({scope, expr})`，经 `host_provide` 落
/// `eval.result` 服务，宿主读取返回。WASM 面失败 → `Err`（调用方走 native 兜底）。
pub struct WasmComboEvaluator {
    plugin: Arc<dyn Plugin>,
}

impl WasmComboEvaluator {
    pub fn new(blob: &[u8]) -> Result<Self, CordisError> {
        let plugin = load_wasm_plugin("combo-eval", blob, Capabilities::new(crate::CAPS_PROVIDE))?;
        Ok(WasmComboEvaluator { plugin })
    }

    /// 缺省 blob 从 `wasm-plugins/combo-eval` 构建产物读取（测试/宿主装配用）。
    pub fn from_default_build() -> Result<Self, CordisError> {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../wasm-plugins/combo-eval");
        let wasm_path = manifest.join("target/wasm32-unknown-unknown/release/combo_eval.wasm");
        if !wasm_path.exists() {
            return Err(CordisError::Internal(format!(
                "combo_eval.wasm not built at {} — run `cargo build --target wasm32-unknown-unknown --release` in wasm-plugins/combo-eval",
                wasm_path.display()
            )));
        }
        let bytes = std::fs::read(&wasm_path).map_err(|e| CordisError::Internal(e.to_string()))?;
        Self::new(&bytes)
    }
}

impl ComboEvaluator for WasmComboEvaluator {
    fn eval(&self, scope: &Value, expr: &str) -> Result<Value, String> {
        let cordis = Cordis::new();
        let cfg = json!({ "scope": scope, "expr": expr });
        let _fid = cordis
            .plugin_arc(self.plugin.clone(), cfg)
            .map_err(|e| e.to_string())?;
        // apply 成功 → 经 host_provide 落 eval.result。
        match cordis.get_value("eval.result") {
            Some(v) => {
                if v.get("ok").and_then(Value::as_bool).unwrap_or(false) {
                    Ok(v.get("value").cloned().unwrap_or(Value::Null))
                } else {
                    let msg = v
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("combo-eval wasm error")
                        .to_string();
                    Err(msg)
                }
            }
            None => Err("combo-eval wasm produced no result (apply failed)".to_string()),
        }
    }
}

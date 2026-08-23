//! run_code 工具纯面（M5-DESIGN §7.4）：`code`(req) + `description`(req)；
//! 程序经嵌套 execution 分发 registry 真实工具（`<parent>:code:<n>` 确定性 id）；
//! run_code 从不暴露给程序自身（无递归）。执行/分发接线留 step7 web.rs。

use serde_json::{json, Value};

/// run_code 参数 schema（code/description 必填）。
pub fn run_code_schema() -> Value {
    json!({
        "code": { "type": "string", "required": true, "description": "Program body to run (single expression / async body with return)." },
        "description": { "type": "string", "required": true, "description": "One-line description of what the code does." }
    })
}

/// 解析 run_code 参数（逐字必填语义）。
pub fn parse_run_code_args(args: &Value) -> Result<(String, String), String> {
    let code = match args.get("code").and_then(|v| v.as_str()) {
        Some(c) if !c.trim().is_empty() => c.to_string(),
        _ => return Err("run_code: invalid code: expected a non-empty string".to_string()),
    };
    let description = match args.get("description").and_then(|v| v.as_str()) {
        Some(d) => d.to_string(),
        _ => return Err("run_code: invalid description: expected a string".to_string()),
    };
    Ok((code, description))
}

/// 确定性嵌套派发 id：`<parent>:code:<n>`（n 为该父下的第 n 个 code 派发，0 基）。
pub fn code_dispatch_id(parent: &str, n: usize) -> String {
    format!("{parent}:code:{n}")
}

/// run_code 从不暴露给程序自身：从注入的命名中剔除 `run_code`（无递归）。
pub fn exclude_run_code<I, S>(names: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    names
        .into_iter()
        .filter(|n| n.as_ref() != "run_code")
        .map(|s| s.as_ref().to_string())
        .collect()
}

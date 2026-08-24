//! `dsh-eval` —— Cordis `!!js` 加载器表达式的受限求值器（对应 PLAN §4.6）。
//!
//! JS 侧是 `with(ctx){ eval(expr) }`（任意 JS）。本 crate 实现**受限子集**：
//! - 标识符读取（作用域内的 `ctx` / `config` / `env` 及其它键）、成员访问、索引
//! - 字面量（null/true/false/数字/字符串/数组/对象）
//! - 一元（`!` `-`）、二元算术/比较/逻辑（含 `===` / `==`）、三元
//! - 白名单函数调用：`String` / `Number` / `Boolean` / `Array.isArray` / `Object.keys`
//!
//! 不支持：赋值、语句、闭包、模板字符串、任意函数调用（fail loud）。

use std::collections::HashMap;

use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq)]
pub struct EvalError(pub String);

impl std::fmt::Display for EvalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "eval error: {}", self.0)
    }
}

impl std::error::Error for EvalError {}

/// 求值作用域：标识符 → 值。
pub type Scope = HashMap<String, Value>;

/// 求值一个表达式字符串。
pub fn evaluate(scope: &Scope, expr: &str) -> Result<Value, EvalError> {
    let tokens = tokenize(expr)?;
    let mut parser = Parser {
        tokens,
        pos: 0,
        scope,
    };
    let ast = parser.parse_ternary()?;
    parser.expect_end()?;
    ast.eval(scope)
}

// ---- tokenizer ----

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Str(String),
    /// M54：模板字符串原始文本（含 `${...}`；parser 拆段）。
    Template(String),
    Ident(String),
    Punct(String),
}

/// 归一化数字：整数值用整数表示（`7.0` → `7`），与 JS 数字语义一致。
fn num(n: f64) -> Value {
    if n.fract() == 0.0 && n.abs() < 9.0e15 {
        Value::from(n as i64)
    } else {
        Value::from(n)
    }
}

fn tokenize(input: &str) -> Result<Vec<Tok>, EvalError> {
    let chars: Vec<char> = input.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            ' ' | '\t' | '\r' | '\n' => i += 1,
            '"' | '\'' => {
                let quote = c;
                let mut s = String::new();
                i += 1;
                let mut closed = false;
                while i < chars.len() {
                    let ch = chars[i];
                    if ch == quote {
                        closed = true;
                        i += 1;
                        break;
                    }
                    if ch == '\\' && i + 1 < chars.len() {
                        let next = chars[i + 1];
                        s.push(match next {
                            'n' => '\n',
                            't' => '\t',
                            'r' => '\r',
                            '\\' => '\\',
                            '\'' => '\'',
                            '"' => '"',
                            other => other,
                        });
                        i += 2;
                        continue;
                    }
                    s.push(ch);
                    i += 1;
                }
                if !closed {
                    return Err(EvalError("unterminated string literal".into()));
                }
                tokens.push(Tok::Str(s));
            }
            '`' => {
                // M54：模板字符串——反引号包裹，保留原始文本（含 `${...}`）
                let mut s = String::new();
                i += 1;
                let mut closed = false;
                while i < chars.len() {
                    let ch = chars[i];
                    if ch == '`' {
                        closed = true;
                        i += 1;
                        break;
                    }
                    s.push(ch);
                    i += 1;
                }
                if !closed {
                    return Err(EvalError("unterminated template literal".into()));
                }
                tokens.push(Tok::Template(s));
            }
            '0'..='9' => {
                let start = i;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
                // 小数部分（仅当 `数字.数字`）
                if i + 1 < chars.len() && chars[i] == '.' && chars[i + 1].is_ascii_digit() {
                    i += 1;
                    while i < chars.len() && chars[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                let text: String = chars[start..i].iter().collect();
                let n: f64 = text
                    .parse()
                    .map_err(|_| EvalError(format!("invalid number {text:?}")))?;
                tokens.push(Tok::Num(n));
            }
            c if c.is_ascii_alphabetic() || c == '_' || c == '$' => {
                let start = i;
                while i < chars.len()
                    && (chars[i].is_ascii_alphanumeric() || chars[i] == '_' || chars[i] == '$')
                {
                    i += 1;
                }
                tokens.push(Tok::Ident(chars[start..i].iter().collect()));
            }
            _ => {
                // 三字符运算符
                let three: String = chars
                    .get(i..i + 3)
                    .map(|s| s.iter().collect())
                    .unwrap_or_default();
                if matches!(three.as_str(), "===" | "!==") {
                    tokens.push(Tok::Punct(three));
                    i += 3;
                } else {
                    // 多字符运算符
                    let two: String = chars
                        .get(i..i + 2)
                        .map(|s| s.iter().collect())
                        .unwrap_or_default();
                    if matches!(two.as_str(), "==" | "!=" | "<=" | ">=" | "&&" | "||" | "?." | "??") {
                        tokens.push(Tok::Punct(two));
                        i += 2;
                    } else if matches!(c, '(' | ')' | '[' | ']' | ',' | '?' | ':' | '+' | '-' | '*' | '/' | '%' | '!' | '<' | '>' | '.') {
                        tokens.push(Tok::Punct(c.to_string()));
                        i += 1;
                    } else {
                        return Err(EvalError(format!("unexpected character {c:?}")));
                    }
                }
            }
        }
    }
    Ok(tokens)
}

// ---- AST ----

#[derive(Debug, Clone)]
enum Expr {
    Value(Value),
    Ident(String),
    Member(Box<Expr>, Box<Expr>),
    /// M50：可选链成员访问（`a?.b` / `a?.[i]`）——基对象为 null/undefined
    /// 时短路返回 Null（JS 语义），否则等价 [`Expr::Member`]。
    OptionalMember(Box<Expr>, Box<Expr>),
    /// M53：`typeof` 一元运算符（返回 JS 类型字符串）。
    Typeof(Box<Expr>),
    /// M54：模板字符串——段序列（字符串字面量 / 表达式交替），eval 拼接。
    Template(Vec<Expr>),
    Unary(&'static str, Box<Expr>),
    Binary(&'static str, Box<Expr>, Box<Expr>),
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    Call(Box<Expr>, Vec<Expr>),
    /// M59：可选调用（`fn?.()`）——callee 为 null/未定义时短路返回 Null。
    OptionalCall(Box<Expr>, Vec<Expr>),
}

impl Expr {
    fn eval(&self, scope: &Scope) -> Result<Value, EvalError> {
        match self {
            Expr::Value(v) => Ok(v.clone()),
            Expr::Ident(name) => scope
                .get(name)
                .cloned()
                .ok_or_else(|| EvalError(format!("identifier `{name}` is not in scope"))),
            Expr::Member(base, key) => member_access(base.eval(scope)?, key.eval(scope)?),
            Expr::OptionalMember(base, key) => {
                // M50：基对象 null 或**未定义标识符**（不在 scope）→ 短路 Null；
                // 成员未命中（缺失键/越界）也传播为 Null（JS：`a?.b.c` 中
                // `a?.b` 缺失 → undefined 继续短路）；否则等价 Member。
                let base_val = match base.eval(scope) {
                    Ok(v) => v,
                    Err(EvalError(m)) if m.contains("not in scope") => Value::Null,
                    Err(e) => return Err(e),
                };
                if base_val.is_null() {
                    return Ok(Value::Null);
                }
                match member_access(base_val, key.eval(scope)?) {
                    Ok(v) => Ok(v),
                    Err(EvalError(m))
                        if m.starts_with("no member") || m.contains("index out of bounds") =>
                    {
                        Ok(Value::Null)
                    }
                    Err(e) => Err(e),
                }
            }
            Expr::Typeof(inner) => {
                // M53：JS typeof——未定义标识符 → "undefined"（Rust 无
                // undefined）；null → "object"（JS 遗留）；其余按 JSON 类型。
                let ty = match inner.eval(scope) {
                    Ok(Value::Null) => "object", // JS 遗留（typeof null === 'object'）
                    Ok(Value::Bool(_)) => "boolean",
                    Ok(Value::Number(_)) => "number",
                    Ok(Value::String(_)) => "string",
                    Ok(_) => "object",
                    Err(EvalError(m)) if m.contains("not in scope") => "undefined",
                    Err(e) => return Err(e),
                };
                Ok(Value::String(ty.to_string()))
            }
            Expr::Template(parts) => {
                // M54：段拼接——字符串字面量段直接追加；表达式段转字符串
                // （JS 模板字符串的隐式 String()：null → "null"）。
                let mut out = String::new();
                for part in parts {
                    match part {
                        Expr::Value(Value::String(s)) => out.push_str(s),
                        other => out.push_str(&value_str(&other.eval(scope)?)),
                    }
                }
                Ok(Value::String(out))
            }
            Expr::Unary(op, inner) => {
                let v = inner.eval(scope)?;
                match *op {
                    "!" => Ok(Value::Bool(!truthy(&v))),
                    "-" => Ok(num(-v.as_f64().unwrap_or(0.0))),
                    _ => Err(EvalError(format!("unknown unary op {op}"))),
                }
            }
            Expr::Binary(op, l, r) => {
                let a = l.eval(scope)?;
                let b = r.eval(scope)?;
                binary_op(op, a, b)
            }
            Expr::Ternary(c, t, f) => {
                if truthy(&c.eval(scope)?) {
                    t.eval(scope)
                } else {
                    f.eval(scope)
                }
            }
            Expr::OptionalCall(callee, args) => {
                // M59：callee 为 null 或未定义标识符 → 短路 Null（不调用）；
                // 白名单函数名（String/Number/Boolean）直接调用（非 scope 标识
                // 符但合法调用目标）；否则等价普通调用。
                match callee.as_ref() {
                    Expr::Ident(n) if matches!(n.as_str(), "String" | "Number" | "Boolean") => {
                        eval_call(callee, args, scope)
                    }
                    Expr::Ident(_) => {
                        // 未定义标识符 → 短路
                        if callee.eval(scope).is_err() {
                            Ok(Value::Null)
                        } else {
                            eval_call(callee, args, scope)
                        }
                    }
                    _ => {
                        let callee_val = callee.eval(scope)?;
                        if callee_val.is_null() {
                            Ok(Value::Null)
                        } else {
                            eval_call(callee, args, scope)
                        }
                    }
                }
            }
            Expr::Call(callee, args) => eval_call(callee, args, scope),
        }
    }
}

/// 调用求值（Call 与 OptionalCall 共用）：白名单全局函数或 `base.key` 白名单。
fn eval_call(callee: &Expr, args: &[Expr], scope: &Scope) -> Result<Value, EvalError> {
    let name = match callee {
        Expr::Ident(n) => n.as_str(),
        Expr::Member(base, key) => {
            let base_name = match base.as_ref() {
                Expr::Ident(n) => n.clone(),
                _ => return Err(EvalError("unsupported call target".into())),
            };
            let key_name = match key.as_ref() {
                Expr::Ident(n) => n.clone(),
                Expr::Value(Value::String(n)) => n.clone(),
                _ => return Err(EvalError("unsupported call target".into())),
            };
            let full = format!("{base_name}.{key_name}");
            match full.as_str() {
                "Array.isArray" | "Object.keys" | "process.cwd" => {
                    return call_whitelist(&full, args, scope);
                }
                _ => return Err(EvalError(format!("call `{full}` is not allowed"))),
            }
        }
        _ => return Err(EvalError("unsupported call target".into())),
    };
    match name {
        "String" | "Number" | "Boolean" => call_whitelist(name, args, scope),
        _ => Err(EvalError(format!("call `{name}` is not allowed"))),
    }
}

/// 成员访问（Member 与 OptionalMember 共用）：字符串键（含数组 length）或
/// 数字索引。
///
/// JS 语义（P2-a 修正）：**对象**上缺键 → `undefined`（本实现以 `Null` 表示，
/// 使 `process.env.X ?? fallback` 正确回落）；**非对象**基值（string/number/bool/
/// null）上取键 → 报错（JS `TypeError`，保持 fail-loud）；数组越界 → 报错。
fn member_access(base_val: Value, key: Value) -> Result<Value, EvalError> {
    match key {
        Value::String(k) => {
            // 数组的 length 属性（JS Array.length）
            if k == "length" {
                if let Some(arr) = base_val.as_array() {
                    return Ok(Value::from(arr.len()));
                }
            }
            match &base_val {
                Value::Object(map) => Ok(map.get(&k).cloned().unwrap_or(Value::Null)),
                _ => Err(EvalError(format!("no member `{k}` on value"))),
            }
        }
        Value::Number(n) => base_val
            .get(n.as_u64().map(|v| v as usize).unwrap_or(0))
            .cloned()
            .ok_or_else(|| EvalError("array index out of bounds".into())),
        other => Err(EvalError(format!("invalid member key {other}"))),
    }
}

/// M54：解析模板字符串——按 `${...}` 分割为段序列（文本段 → `Expr::Value`，
/// 表达式段 → 递归 tokenize + parse）。
fn parse_template(raw: String, scope: &Scope) -> Result<Expr, EvalError> {
    let mut parts = Vec::new();
    let mut rest: &str = &raw;
    while let Some(start) = rest.find("${") {
        let prefix = &rest[..start];
        if !prefix.is_empty() {
            parts.push(Expr::Value(Value::String(prefix.to_string())));
        }
        let after = &rest[start + 2..];
        let end = after
            .find('}')
            .ok_or_else(|| EvalError("unterminated template interpolation".into()))?;
        let expr_text = &after[..end];
        let inner = evaluate(scope, expr_text)?;
        parts.push(Expr::Value(inner));
        rest = &after[end + 1..];
    }
    if !rest.is_empty() {
        parts.push(Expr::Value(Value::String(rest.to_string())));
    }
    Ok(Expr::Template(parts))
}

fn call_whitelist(name: &str, args: &[Expr], scope: &Scope) -> Result<Value, EvalError> {
    let arg = |i: usize| -> Result<Value, EvalError> {
        args.get(i)
            .ok_or_else(|| EvalError(format!("`{name}` expects an argument")))?
            .eval(scope)
    };
    match name {
        // P2-a（spike-6 结论）：`process.cwd()` 无参调用 → 作用域 `process.cwd` 成员
        // （值为字符串；`process` 门面由 host 注入 `process_facade`）。
        "process.cwd" => {
            let proc = scope
                .get("process")
                .ok_or_else(|| EvalError("call `process.cwd` needs a `process` facade in scope".into()))?;
            proc.get("cwd")
                .cloned()
                .ok_or_else(|| EvalError("`process.cwd` is not provided by the facade".into()))
        }
        "String" => Ok(Value::String(value_str(&arg(0)?))),
        "Number" => {
            let v = arg(0)?;
            // JS Number()：数字原样；字符串按数字解析
            let n = v
                .as_f64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
                .unwrap_or(0.0);
            Ok(num(n))
        }
        "Boolean" => Ok(Value::Bool(truthy(&arg(0)?))),
        "Array.isArray" => Ok(Value::Bool(arg(0)?.is_array())),
        "Object.keys" => {
            let v = arg(0)?;
            let keys: Vec<Value> = v
                .as_object()
                .map(|m| m.keys().map(|k| Value::String(k.clone())).collect())
                .unwrap_or_default();
            Ok(Value::Array(keys))
        }
        _ => Err(EvalError(format!("call `{name}` is not allowed"))),
    }
}

fn binary_op(op: &str, a: Value, b: Value) -> Result<Value, EvalError> {
    match op {
        "+" => {
            if a.is_number() && b.is_number() {
                Ok(num(a.as_f64().unwrap_or(0.0) + b.as_f64().unwrap_or(0.0)))
            } else {
                Ok(Value::String(format!("{}{}", value_str(&a), value_str(&b))))
            }
        }
        "-" => Ok(num(a.as_f64().unwrap_or(0.0) - b.as_f64().unwrap_or(0.0))),
        "*" => Ok(num(a.as_f64().unwrap_or(0.0) * b.as_f64().unwrap_or(0.0))),
        "/" => Ok(num(a.as_f64().unwrap_or(0.0) / b.as_f64().unwrap_or(0.0))),
        "%" => Ok(num(a.as_f64().unwrap_or(0.0) % b.as_f64().unwrap_or(0.0))),
        "===" | "==" => Ok(Value::Bool(loose_eq(&a, &b))),
        "!==" | "!=" => Ok(Value::Bool(!loose_eq(&a, &b))),
        "<" => Ok(Value::Bool(cmp(&a, &b) == std::cmp::Ordering::Less)),
        "<=" => Ok(Value::Bool(cmp(&a, &b) != std::cmp::Ordering::Greater)),
        ">" => Ok(Value::Bool(cmp(&a, &b) == std::cmp::Ordering::Greater)),
        ">=" => Ok(Value::Bool(cmp(&a, &b) != std::cmp::Ordering::Less)),
        "in" => {
            // M55：`'key' in obj`——左侧为字符串键（或数字索引），右侧为
            // 对象/数组；否则报错（fail loud）。
            let key = match &a {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                other => return Err(EvalError(format!("`in` left operand must be a key, got {other}"))),
            };
            match &b {
                Value::Object(map) => Ok(Value::Bool(map.contains_key(&key))),
                Value::Array(arr) => Ok(Value::Bool(
                    key.parse::<usize>().map(|i| i < arr.len()).unwrap_or(false),
                )),
                other => Err(EvalError(format!("`in` right operand must be object/array, got {other}"))),
            }
        }
        "&&" => {
            if truthy(&a) {
                Ok(b)
            } else {
                Ok(a)
            }
        }
        "||" => {
            if truthy(&a) {
                Ok(a)
            } else {
                Ok(b)
            }
        }
        "??" => {
            // M51：nullish coalescing——仅当左侧为 null 时取右侧
            // （0/''/false 保留左侧，与 `||` 的 truthiness 短路不同）。
            if a.is_null() {
                Ok(b)
            } else {
                Ok(a)
            }
        }
        _ => Err(EvalError(format!("unknown binary op {op}"))),
    }
}

fn loose_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        _ => a == b,
    }
}

fn cmp(a: &Value, b: &Value) -> std::cmp::Ordering {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal),
        _ => value_str(a).cmp(&value_str(b)),
    }
}

/// JS truthiness。
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

pub fn value_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        _ => serde_json::to_string(v).unwrap_or_else(|_| "<unprintable>".to_string()),
    }
}

/// 递归替换 `{"__jsExpr": "..."}` 节点为求值结果（Cordis `interpolate`）。
pub fn interpolate(scope: &Scope, value: &Value) -> Result<Value, EvalError> {
    match value {
        Value::Object(map) => {
            if map.len() == 1 {
                if let Some(Value::String(expr)) = map.get("__jsExpr") {
                    return evaluate(scope, expr);
                }
            }
            let mut out = Map::new();
            for (k, v) in map {
                out.insert(k.clone(), interpolate(scope, v)?);
            }
            Ok(Value::Object(out))
        }
        Value::Array(items) => items
            .iter()
            .map(|i| interpolate(scope, i))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        other => Ok(other.clone()),
    }
}

// ---- parser（递归下降） ----

struct Parser<'a> {
    tokens: Vec<Tok>,
    pos: usize,
    scope: &'a Scope,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.tokens.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn expect_punct(&mut self, p: &str) -> Result<(), EvalError> {
        match self.peek() {
            Some(Tok::Punct(x)) if x == p => {
                self.pos += 1;
                Ok(())
            }
            other => Err(EvalError(format!("expected `{p}`, got {other:?}"))),
        }
    }

    fn expect_end(&mut self) -> Result<(), EvalError> {
        if self.pos == self.tokens.len() {
            Ok(())
        } else {
            Err(EvalError(format!(
                "unexpected trailing tokens: {:?}",
                &self.tokens[self.pos..]
            )))
        }
    }

    fn parse_ternary(&mut self) -> Result<Expr, EvalError> {
        let cond = self.parse_or()?;
        if matches!(self.peek(), Some(Tok::Punct(p)) if p == "?") {
            self.next();
            let t = self.parse_ternary()?;
            self.expect_punct(":")?;
            let f = self.parse_ternary()?;
            return Ok(Expr::Ternary(Box::new(cond), Box::new(t), Box::new(f)));
        }
        Ok(cond)
    }

    fn parse_or(&mut self) -> Result<Expr, EvalError> {
        let mut left = self.parse_and()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Punct(p)) if p == "||" => "||",
                Some(Tok::Punct(p)) if p == "??" => "??", // M51：nullish coalescing
                _ => break,
            };
            self.next();
            let right = self.parse_and()?;
            left = Expr::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr, EvalError> {
        let mut left = self.parse_equality()?;
        while matches!(self.peek(), Some(Tok::Punct(p)) if p == "&&") {
            self.next();
            let right = self.parse_equality()?;
            left = Expr::Binary("&&", Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_equality(&mut self) -> Result<Expr, EvalError> {
        let mut left = self.parse_comparison()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Punct(p)) if matches!(p.as_str(), "===" | "==" | "!==" | "!=") => {
                    p.clone()
                }
                _ => break,
            };
            self.next();
            let right = self.parse_comparison()?;
            left = Expr::Binary(Box::leak(op.into_boxed_str()), Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_comparison(&mut self) -> Result<Expr, EvalError> {
        let mut left = self.parse_additive()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Punct(p)) if matches!(p.as_str(), "<" | "<=" | ">" | ">=") => p.clone(),
                // M55：`in` 关系运算符（关键字 token；`'k' in obj`）
                Some(Tok::Ident(name)) if name == "in" => "in".to_string(),
                _ => break,
            };
            self.next();
            let right = self.parse_additive()?;
            left = Expr::Binary(Box::leak(op.into_boxed_str()), Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_additive(&mut self) -> Result<Expr, EvalError> {
        let mut left = self.parse_multiplicative()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Punct(p)) if matches!(p.as_str(), "+" | "-") => p.clone(),
                _ => break,
            };
            self.next();
            let right = self.parse_multiplicative()?;
            left = Expr::Binary(Box::leak(op.into_boxed_str()), Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_multiplicative(&mut self) -> Result<Expr, EvalError> {
        let mut left = self.parse_unary()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Punct(p)) if matches!(p.as_str(), "*" | "/" | "%") => p.clone(),
                _ => break,
            };
            self.next();
            let right = self.parse_unary()?;
            left = Expr::Binary(Box::leak(op.into_boxed_str()), Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr, EvalError> {
        match self.peek() {
            Some(Tok::Ident(name)) if name == "typeof" => {
                // M53：`typeof` 一元运算符（关键字；优先级高于二元）
                self.next();
                let inner = self.parse_unary()?;
                Ok(Expr::Typeof(Box::new(inner)))
            }
            Some(Tok::Punct(p)) if p == "!" || p == "-" => {
                let op = p.clone();
                self.next();
                let inner = self.parse_unary()?;
                Ok(Expr::Unary(Box::leak(op.into_boxed_str()), Box::new(inner)))
            }
            _ => self.parse_postfix(),
        }
    }

    fn parse_postfix(&mut self) -> Result<Expr, EvalError> {
        let mut expr = self.parse_primary()?;
        loop {
            match self.peek() {
                Some(Tok::Punct(p)) if p == "?." => {
                    // M50：可选链——`?.name` / `?.[expr]`；M59：`?.(` 可选调用
                    self.next();
                    if matches!(self.peek(), Some(Tok::Punct(q)) if q == "(") {
                        self.next();
                        let mut args = Vec::new();
                        if !matches!(self.peek(), Some(Tok::Punct(q)) if q == ")") {
                            loop {
                                args.push(self.parse_ternary()?);
                                if matches!(self.peek(), Some(Tok::Punct(q)) if q == ",") {
                                    self.next();
                                } else {
                                    break;
                                }
                            }
                        }
                        self.expect_punct(")")?;
                        expr = Expr::OptionalCall(Box::new(expr), args);
                    } else if matches!(self.peek(), Some(Tok::Punct(q)) if q == "[") {
                        self.next();
                        let key = self.parse_ternary()?;
                        self.expect_punct("]")?;
                        expr = Expr::OptionalMember(Box::new(expr), Box::new(key));
                    } else {
                        let key = match self.next() {
                            Some(Tok::Ident(name)) => Expr::Value(Value::String(name)),
                            other => {
                                return Err(EvalError(format!(
                                    "expected member name after `?.`, got {other:?}"
                                )))
                            }
                        };
                        expr = Expr::OptionalMember(Box::new(expr), Box::new(key));
                    }
                }
                Some(Tok::Punct(p)) if p == "." => {
                    self.next();
                    let key = match self.next() {
                        Some(Tok::Ident(name)) => Expr::Value(Value::String(name)),
                        other => return Err(EvalError(format!("expected member name, got {other:?}"))),
                    };
                    expr = Expr::Member(Box::new(expr), Box::new(key));
                }
                Some(Tok::Punct(p)) if p == "[" => {
                    self.next();
                    let key = self.parse_ternary()?;
                    self.expect_punct("]")?;
                    expr = Expr::Member(Box::new(expr), Box::new(key));
                }
                Some(Tok::Punct(p)) if p == "(" => {
                    self.next();
                    let mut args = Vec::new();
                    if !matches!(self.peek(), Some(Tok::Punct(p)) if p == ")") {
                        loop {
                            args.push(self.parse_ternary()?);
                            if matches!(self.peek(), Some(Tok::Punct(p)) if p == ",") {
                                self.next();
                            } else {
                                break;
                            }
                        }
                    }
                    self.expect_punct(")")?;
                    expr = Expr::Call(Box::new(expr), args);
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    fn parse_primary(&mut self) -> Result<Expr, EvalError> {
        match self.next() {
            Some(Tok::Num(n)) => Ok(Expr::Value(num(n))),
            Some(Tok::Str(s)) => Ok(Expr::Value(Value::String(s))),
            Some(Tok::Template(raw)) => parse_template(raw, self.scope),
            Some(Tok::Ident(name)) => match name.as_str() {
                "true" => Ok(Expr::Value(Value::Bool(true))),
                "false" => Ok(Expr::Value(Value::Bool(false))),
                "null" => Ok(Expr::Value(Value::Null)),
                _ => Ok(Expr::Ident(name)),
            },
            Some(Tok::Punct(p)) if p == "(" => {
                let inner = self.parse_ternary()?;
                self.expect_punct(")")?;
                Ok(inner)
            }
            Some(Tok::Punct(p)) if p == "[" => {
                let mut items = Vec::new();
                if !matches!(self.peek(), Some(Tok::Punct(q)) if q == "]") {
                    loop {
                        items.push(self.parse_ternary()?);
                        if matches!(self.peek(), Some(Tok::Punct(q)) if q == ",") {
                            self.next();
                        } else {
                            break;
                        }
                    }
                }
                self.expect_punct("]")?;
                Ok(Expr::Value(Value::Array(
                    items
                        .iter()
                        .map(|e| e.eval(self.scope))
                        .collect::<Result<Vec<_>, _>>()?,
                )))
            }
            Some(Tok::Punct(p)) if p == "{" => {
                let mut map = Map::new();
                if !matches!(self.peek(), Some(Tok::Punct(q)) if q == "}") {
                    loop {
                        let key = match self.next() {
                            Some(Tok::Ident(k)) | Some(Tok::Str(k)) => k,
                            other => return Err(EvalError(format!("expected object key, got {other:?}"))),
                        };
                        self.expect_punct(":")?;
                        let val = self.parse_ternary()?;
                        map.insert(key, val.eval(self.scope)?);
                        if matches!(self.peek(), Some(Tok::Punct(q)) if q == ",") {
                            self.next();
                        } else {
                            break;
                        }
                    }
                }
                self.expect_punct("}")?;
                Ok(Expr::Value(Value::Object(map)))
            }
            other => Err(EvalError(format!("unexpected token {other:?}"))),
        }
    }
}

/// Node `process` 门面值（`platform`/`env`/`cwd`），供 `!!js` 表达式求值。
/// platform 映射 Rust → Node：`windows`→`win32` / `macos`→`darwin` / `linux`→`linux`。
/// env = 当前进程全部环境变量；cwd = 当前工作目录。host 注入进 eval 作用域。
pub fn process_facade() -> Value {
    let platform = match std::env::consts::OS {
        "windows" => "win32",
        "macos" => "darwin",
        "linux" => "linux",
        other => other,
    };
    let env: serde_json::Map<String, Value> =
        std::env::vars().map(|(k, v)| (k, Value::from(v))).collect();
    let cwd = std::env::current_dir()
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default();
    serde_json::json!({ "platform": platform, "env": env, "cwd": cwd })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// spike-6 结论回归：win32 平台门控表达式按声明的平台求值（**不再** fail-closed
    /// 全禁用）。`process.facade` 也可由 host 注入固定值（`eval_scope_with_process`）。
    #[test]
    fn process_gate_platform_expression() {
        let mut scope = Scope::new();
        scope.insert(
            "process".to_string(),
            serde_json::json!({
                "platform": "win32",
                "env": { "DSH_CWD": "C:\\x" },
                "cwd": "C:\\repo",
            }),
        );
        assert!(truthy(&evaluate(&scope, "process.platform === 'win32'").unwrap()));
        assert!(!truthy(&evaluate(&scope, "process.platform !== 'win32'").unwrap()));
        // minimal cwd 行：`process.env.DSH_CWD ?? process.cwd()`
        let cwd = evaluate(&scope, "process.env.DSH_CWD ?? process.cwd()").unwrap();
        assert_eq!(cwd, serde_json::json!("C:\\x"));
        // `process.cwd()` 无参调用 → facade.cwd
        assert_eq!(
            evaluate(&scope, "process.cwd()").unwrap(),
            serde_json::json!("C:\\repo")
        );
        // 非 facade 目标调用仍被拒（白名单收窄）。
        let err = evaluate(&scope, "process.kill()").unwrap_err();
        assert!(err.to_string().contains("not allowed"), "err: {err}");
    }

    /// 成员链 `process.env.X` / `process.platform`（Member 路径，非 Call）。
    #[test]
    fn process_member_chain() {
        let mut scope = Scope::new();
        scope.insert(
            "process".to_string(),
            serde_json::json!({ "platform": "linux", "env": { "HOME": "/root" }, "cwd": "/w" }),
        );
        assert_eq!(evaluate(&scope, "process.env.HOME").unwrap(), serde_json::json!("/root"));
        assert_eq!(evaluate(&scope, "process.platform").unwrap(), serde_json::json!("linux"));
        // JS 语义：对象上缺键 = undefined（Null），`??` 可回落（P2-a 修正）。
        assert_eq!(evaluate(&scope, "process.env.NOPE").unwrap(), Value::Null);
        assert_eq!(
            evaluate(&scope, "process.env.NOPE ?? 'x'").unwrap(),
            serde_json::json!("x")
        );
        // 非对象基值取键 → fail（JS TypeError 等价，保持 fail-loud）。
        assert!(evaluate(&scope, "process.platform.x").is_err());
        // 空 scope 引用 process（无门面注入）→ fail。
        let empty = Scope::new();
        assert!(evaluate(&empty, "process.cwd()").is_err());
    }

    /// 平台映射：windows→win32 / macos→darwin / linux→linux。
    #[test]
    fn process_facade_platform_mapping() {
        let f = process_facade();
        let platform = f["platform"].as_str().unwrap();
        match std::env::consts::OS {
            "windows" => assert_eq!(platform, "win32"),
            "macos" => assert_eq!(platform, "darwin"),
            "linux" => assert_eq!(platform, "linux"),
            _ => assert!(!platform.is_empty(), "unknown os maps to itself, must stay non-empty"),
        }
        assert!(f["env"].is_object());
        assert!(f["cwd"].as_str().unwrap().contains(std::path::MAIN_SEPARATOR));
    }
}

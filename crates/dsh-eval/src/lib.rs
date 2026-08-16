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
                    if matches!(two.as_str(), "==" | "!=" | "<=" | ">=" | "&&" | "||") {
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
    Unary(&'static str, Box<Expr>),
    Binary(&'static str, Box<Expr>, Box<Expr>),
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    Call(Box<Expr>, Vec<Expr>),
}

impl Expr {
    fn eval(&self, scope: &Scope) -> Result<Value, EvalError> {
        match self {
            Expr::Value(v) => Ok(v.clone()),
            Expr::Ident(name) => scope
                .get(name)
                .cloned()
                .ok_or_else(|| EvalError(format!("identifier `{name}` is not in scope"))),
            Expr::Member(base, key) => {
                let base_val = base.eval(scope)?;
                match key.eval(scope)? {
                    Value::String(k) => {
                        // 数组的 length 属性（JS Array.length）
                        if k == "length" {
                            if let Some(arr) = base_val.as_array() {
                                return Ok(Value::from(arr.len()));
                            }
                        }
                        base_val.get(&k).cloned().ok_or_else(|| {
                            EvalError(format!("no member `{k}` on value"))
                        })
                    }
                    Value::Number(n) => base_val
                        .get(n.as_u64().map(|v| v as usize).unwrap_or(0))
                        .cloned()
                        .ok_or_else(|| EvalError("array index out of bounds".into())),
                    other => Err(EvalError(format!("invalid member key {other}"))),
                }
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
            Expr::Call(callee, args) => {
                let name = match callee.as_ref() {
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
                            "Array.isArray" | "Object.keys" => {
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
        }
    }
}

fn call_whitelist(name: &str, args: &[Expr], scope: &Scope) -> Result<Value, EvalError> {
    let arg = |i: usize| -> Result<Value, EvalError> {
        args.get(i)
            .ok_or_else(|| EvalError(format!("`{name}` expects an argument")))?
            .eval(scope)
    };
    match name {
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
        while matches!(self.peek(), Some(Tok::Punct(p)) if p == "||") {
            self.next();
            let right = self.parse_and()?;
            left = Expr::Binary("||", Box::new(left), Box::new(right));
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

//! `dsh-schema` —— Schemastery 配置 schema 的 Rust 移植（对应 PLAN §1.10 / M4）。
//!
//! 语义对齐 vendored `vendor/schemastery/src/index.ts`：
//! - `resolve(data, schema, opts)`：nullable + required/default 处理 → 类型 resolver；
//!   `loose` 时 resolver 抛错返回默认值。
//! - object/array/tuple/dict 的 `property` 逐项校验，`autofix` 时删除无效项并回退默认。
//! - union 逐个尝试、全部失败聚合错误消息（`expected {type} but got {json}`）；
//!   intersect 逐个 strict 校验并合并对象。
//! - transform：先 strict 校验 inner，再回调转换。
//! - lazy：递归 schema（builder 惰性展开）。
//! - `ValidationError` 消息带路径前缀（`$.a.b[0]`）。
//!
//! 已知 M4 差异：`function`/`is(Date)` 等非 JSON 类型在 Value-land 不可表达
//! （`is` 按 JSON 类型名映射）；bitset 的 adapted 键数组不写回；`clone(default)`
//! 用 serde_json 深拷贝（等价 structuredClone）。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use serde_json::{Map, Value};

/// 元数据（Cordis `Schemastery.Meta` 的 M4 子集）。
#[derive(Debug, Clone, Default)]
pub struct Meta {
    pub required: bool,
    pub default: Option<Value>,
    pub loose: bool,
    pub max: Option<f64>,
    pub min: Option<f64>,
    pub step: Option<f64>,
    pub pattern: Option<(String, String)>, // (source, flags)
    pub role: Option<String>,
    pub hidden: bool,
    pub collapse: bool,
    pub disabled: bool,
    pub badges: Vec<(String, String)>, // (text, type)
    pub description: Option<String>,
    pub comment: Option<String>,
    pub link: Option<String>,
    pub extra: Value,
}

impl Meta {
    fn new() -> Self {
        Meta {
            extra: Value::Null,
            ..Meta::default()
        }
    }
}

/// transform 回调：`fn(&Value) -> Result<Value, String>`。
pub type TransformFn = Rc<dyn Fn(&Value) -> Result<Value, String>>;

/// M57：自定义类型 resolver（对齐 Schemastery `Resolve`）——
/// `fn(data, schema, options) -> Result<Value, ValidationError>`。
/// `Arc`：全局注册表（`Mutex<HashMap>`）要求 Send + Sync。
pub type CustomResolver =
    Arc<dyn Fn(&Value, &SchemaRef, &ResolveOptions) -> Result<Value, ValidationError> + Send + Sync>;

/// M57：自定义类型注册表（全局；对齐 `Schema.extend` 的 `resolvers` dict）。
static CUSTOM_RESOLVERS: std::sync::OnceLock<std::sync::Mutex<HashMap<String, CustomResolver>>> =
    std::sync::OnceLock::new();

fn custom_registry() -> &'static std::sync::Mutex<HashMap<String, CustomResolver>> {
    CUSTOM_RESOLVERS.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// lazy 惰性展开状态。
pub struct LazyState {
    pub builder: Rc<dyn Fn() -> SchemaRef>,
    pub resolved: RefCell<Option<SchemaRef>>,
}

/// Schema 节点种类。
#[derive(Clone)]
pub enum SchemaKind {
    Any,
    Never,
    Const(Value),
    String,
    Number,
    Boolean,
    Function,
    Is(String),
    Bitset(HashMap<String, u64>),
    Array(Box<SchemaRef>),
    Dict {
        inner: Box<SchemaRef>,
        s_key: Box<SchemaRef>,
    },
    Tuple(Vec<SchemaRef>),
    Object(HashMap<String, SchemaRef>),
    Union(Vec<SchemaRef>),
    Intersect(Vec<SchemaRef>),
    Transform {
        inner: Box<SchemaRef>,
        preserve: bool,
        callback: TransformFn,
    },
    Lazy(Rc<LazyState>),
    /// M57：自定义类型（`Schema::extend` 注册的 resolver；type 名）。
    Custom(String),
}

/// Schema 节点。
#[derive(Clone)]
pub struct Schema {
    pub kind: SchemaKind,
    pub meta: Meta,
}

pub type SchemaRef = Rc<Schema>;

impl Schema {
    fn new(kind: SchemaKind) -> SchemaRef {
        Rc::new(Schema {
            kind,
            meta: Meta::new(),
        })
    }

    /// 克隆并修改 meta 的链式构建。
    fn with_meta(s: &SchemaRef, f: impl FnOnce(&mut Meta)) -> SchemaRef {
        let mut meta = s.meta.clone();
        f(&mut meta);
        Rc::new(Schema {
            kind: s.kind.clone(),
            meta,
        })
    }

    // ---- 组合子（Cordis `Schema.extend` + `defineMethod`） ----

    pub fn any() -> SchemaRef {
        Schema::new(SchemaKind::Any)
    }
    pub fn never() -> SchemaRef {
        Schema::new(SchemaKind::Never)
    }
    pub fn const_value(value: Value) -> SchemaRef {
        Schema::new(SchemaKind::Const(value))
    }
    pub fn string() -> SchemaRef {
        Schema::new(SchemaKind::String)
    }
    pub fn number() -> SchemaRef {
        Schema::new(SchemaKind::Number)
    }
    /// natural = number().step(1).min(0)
    pub fn natural() -> SchemaRef {
        let s = Schema::number();
        Schema::with_meta(&s, |m| {
            m.step = Some(1.0);
            m.min = Some(0.0);
        })
    }
    /// percent = number().step(0.01).min(0).max(1).role('slider')
    pub fn percent() -> SchemaRef {
        let s = Schema::number();
        Schema::with_meta(&s, |m| {
            m.step = Some(0.01);
            m.min = Some(0.0);
            m.max = Some(1.0);
            m.role = Some("slider".to_string());
        })
    }
    pub fn boolean() -> SchemaRef {
        Schema::new(SchemaKind::Boolean)
    }
    pub fn function() -> SchemaRef {
        Schema::new(SchemaKind::Function)
    }
    /// `is(name)`：按 JSON 类型名映射（M4 差异：不校验原型链）。
    pub fn is(name: &str) -> SchemaRef {
        Schema::new(SchemaKind::Is(name.to_string()))
    }
    pub fn bitset(bits: HashMap<String, u64>) -> SchemaRef {
        let s = Schema::new(SchemaKind::Bitset(bits));
        Schema::with_meta(&s, |m| m.default = Some(Value::from(0)))
    }
    pub fn array(inner: SchemaRef) -> SchemaRef {
        let s = Schema::new(SchemaKind::Array(Box::new(inner)));
        Schema::with_meta(&s, |m| m.default = Some(Value::Array(Vec::new())))
    }
    pub fn dict(inner: SchemaRef, s_key: SchemaRef) -> SchemaRef {
        let s = Schema::new(SchemaKind::Dict {
            inner: Box::new(inner),
            s_key: Box::new(s_key),
        });
        Schema::with_meta(&s, |m| m.default = Some(Value::Object(Map::new())))
    }
    pub fn tuple(list: Vec<SchemaRef>) -> SchemaRef {
        let s = Schema::new(SchemaKind::Tuple(list));
        Schema::with_meta(&s, |m| m.default = Some(Value::Array(Vec::new())))
    }
    pub fn object(dict: HashMap<String, SchemaRef>) -> SchemaRef {
        let s = Schema::new(SchemaKind::Object(dict));
        Schema::with_meta(&s, |m| m.default = Some(Value::Object(Map::new())))
    }
    pub fn union(list: Vec<SchemaRef>) -> SchemaRef {
        Schema::new(SchemaKind::Union(list))
    }

    /// date（M26）：union[is(Date), transform(string 校验 RFC3339)]。
    /// Value-land 中 is(Date) 恒失败；字符串经 datetime 校验后原样返回
    /// （JSON 兼容；等价 TS `Schema.date` 的 string 分支）。
    pub fn date() -> SchemaRef {
        let inner = Schema::string();
        let callback: TransformFn = Rc::new(|v| {
            let s = v.as_str().unwrap_or_default();
            match parse_datetime(s) {
                Some(_) => Ok(v.clone()),
                None => Err(format!("invalid date \"{s}\"")),
            }
        });
        let string_branch = Schema::transform(inner, true, callback);
        Schema::union(vec![Schema::is("Date"), string_branch])
    }

    /// regExp（M26）：union[is(RegExp), transform(string 校验可编译)]。
    /// Value-land 中 is(RegExp) 恒失败；字符串校验为正则可编译后原样返回。
    pub fn reg_exp(flag: &str) -> SchemaRef {
        let flag = flag.to_string();
        let inner = Schema::string();
        let callback: TransformFn = Rc::new(move |v| {
            let s = v.as_str().unwrap_or_default();
            let expr = build_regex(s, &flag);
            match regex::Regex::new(&expr) {
                Ok(_) => Ok(v.clone()),
                Err(e) => Err(format!("{e}")),
            }
        });
        let string_branch = Schema::transform(inner, true, callback);
        Schema::union(vec![Schema::is("RegExp"), string_branch])
    }
    pub fn intersect(list: Vec<SchemaRef>) -> SchemaRef {
        Schema::new(SchemaKind::Intersect(list))
    }
    pub fn transform(inner: SchemaRef, preserve: bool, callback: TransformFn) -> SchemaRef {
        Schema::new(SchemaKind::Transform {
            inner: Box::new(inner),            preserve,
            callback,
        })
    }
    pub fn lazy(builder: Rc<dyn Fn() -> SchemaRef>) -> SchemaRef {
        Schema::new(SchemaKind::Lazy(Rc::new(LazyState {
            builder,
            resolved: RefCell::new(None),
        })))
    }

    /// M57：注册自定义类型 resolver（对齐 Schemastery `Schema.extend(type,
    /// resolve)`）——全局注册表；`resolve` 对 `SchemaKind::Custom(type)` 查表。
    pub fn extend(type_name: &str, resolver: impl Fn(&Value, &SchemaRef, &ResolveOptions) -> Result<Value, ValidationError> + Send + Sync + 'static) {
        custom_registry()
            .lock()
            .unwrap()
            .insert(type_name.to_string(), Arc::new(resolver));
    }

    /// M57：构造自定义类型节点（`Schema::extend` 已注册的 type）。
    pub fn custom(type_name: &str) -> SchemaRef {
        Schema::new(SchemaKind::Custom(type_name.to_string()))
    }

    // ---- meta 链 ----

    pub fn required(s: &SchemaRef) -> SchemaRef {
        Schema::with_meta(s, |m| m.required = true)
    }
    pub fn loose(s: &SchemaRef) -> SchemaRef {
        Schema::with_meta(s, |m| m.loose = true)
    }
    pub fn hidden(s: &SchemaRef) -> SchemaRef {
        Schema::with_meta(s, |m| m.hidden = true)
    }
    pub fn collapse(s: &SchemaRef) -> SchemaRef {
        Schema::with_meta(s, |m| m.collapse = true)
    }
    pub fn disabled(s: &SchemaRef) -> SchemaRef {
        Schema::with_meta(s, |m| m.disabled = true)
    }
    pub fn with_default(s: &SchemaRef, value: Value) -> SchemaRef {
        Schema::with_meta(s, |m| m.default = Some(value))
    }
    pub fn pattern(s: &SchemaRef, source: &str, flags: &str) -> SchemaRef {
        Schema::with_meta(s, |m| m.pattern = Some((source.to_string(), flags.to_string())))
    }
    pub fn min(s: &SchemaRef, v: f64) -> SchemaRef {
        Schema::with_meta(s, |m| m.min = Some(v))
    }
    pub fn max(s: &SchemaRef, v: f64) -> SchemaRef {
        Schema::with_meta(s, |m| m.max = Some(v))
    }
    pub fn step(s: &SchemaRef, v: f64) -> SchemaRef {
        Schema::with_meta(s, |m| m.step = Some(v))
    }
    pub fn role(s: &SchemaRef, text: &str) -> SchemaRef {
        Schema::with_meta(s, |m| m.role = Some(text.to_string()))
    }
    pub fn description(s: &SchemaRef, text: &str) -> SchemaRef {
        Schema::with_meta(s, |m| m.description = Some(text.to_string()))
    }
    pub fn comment(s: &SchemaRef, text: &str) -> SchemaRef {
        Schema::with_meta(s, |m| m.comment = Some(text.to_string()))
    }
    pub fn link(s: &SchemaRef, url: &str) -> SchemaRef {
        Schema::with_meta(s, |m| m.link = Some(url.to_string()))
    }
    pub fn badge(s: &SchemaRef, text: &str, kind: &str) -> SchemaRef {
        let t = text.to_string();
        let k = kind.to_string();
        Schema::with_meta(s, |m| m.badges.push((t, k)))
    }

    /// `role('secret')` 快捷：写保密槽位（settings redact 语义；角色名逐字对齐）。
    pub fn secret(s: &SchemaRef) -> SchemaRef {
        Schema::role(s, "secret")
    }

    /// meta 任意键（对齐 TS `Schema.prototype.extra(key, value)`——键直接落在
    /// meta 顶层；重复键后者覆盖，`Null` 值等价删除）。
    pub fn extra(s: &SchemaRef, key: &str, value: Value) -> SchemaRef {
        Schema::with_meta(s, |m| {
            let map = match m.extra.as_object_mut() {
                Some(map) => map,
                None => {
                    m.extra = Value::Object(Default::default());
                    match m.extra.as_object_mut() {
                        Some(map) => map,
                        // 上面刚赋值 Object，必 Some；兜底返回空对象。
                        None => {
                            m.extra = Value::Object(Default::default());
                            m.extra.as_object_mut().unwrap()
                        }
                    }
                }
            };
            if value.is_null() {
                map.remove(key);
            } else {
                map.insert(key.to_string(), value);
            }
        })
    }

    /// S了一遍成为 wire JSON（对齐 Schemastery `Schema.prototype.toJSON`）。
    ///
    /// 输出 `{uid, refs}`：
    /// - 每节点序列化为 `{type, meta, ...结构字段}`；`inner/list/dict/sKey`
    ///   等嵌套 schema 引用以 **uid 数字** 占位（refs 键 = uid 字符串）；
    /// - `callback`/`builder` 等函数在 wire 上不存在（JSON.stringify 跳过函数——
    ///   对齐 TS：`preserve` 布尔仍输出，lazy 的 inner 只在已展开时输出）；
    /// - uid 从 0 分配（前端 `new Schema({uid, refs})` 按 refs[uid] 重建，无所谓顺序）。
    pub fn to_json(&self) -> Value {
        let mut refs: serde_json::Map<String, Value> = serde_json::Map::new();
        let mut next_uid = 0usize;
        let root_uid = schema_to_json_node(self, &mut refs, &mut next_uid);
        serde_json::json!({ "uid": root_uid, "refs": refs })
    }}

// ---- 校验 ----

#[derive(Debug, Clone, PartialEq)]
pub enum PathSeg {
    Key(String),
    Index(usize),
}

/// 校验选项（Cordis `Schemastery.Options` 的 M4/M25 子集）。
#[derive(Debug, Clone, Default)]
pub struct ResolveOptions {
    pub path: Vec<PathSeg>,
    pub autofix: bool,
    /// strict 模式（M25）：多余键/项不合并（object/tuple/intersect），
    /// dict 的 sKey 校验失败跳过（非 strict 抛错）。对应 Cordis
    /// `Schema.resolve(data, schema, options, strict)` 的第 4 参。
    pub strict: bool,
}

/// 校验错误：消息 + 路径（前缀 `$.a.b[0]`）。
#[derive(Debug, Clone)]
pub struct ValidationError {
    pub message: String,
    pub path: Vec<PathSeg>,
}

impl ValidationError {
    pub fn new(message: impl Into<String>, path: &[PathSeg]) -> Self {
        ValidationError {
            message: message.into(),
            path: path.to_vec(),
        }
    }
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut prefix = "$".to_string();
        for seg in &self.path {
            match seg {
                PathSeg::Key(k) => prefix.push_str(&format!(".{k}")),
                PathSeg::Index(i) => prefix.push_str(&format!("[{i}]")),
            }
        }
        let head = if prefix == "$" { String::new() } else { format!("{prefix} ") };
        write!(f, "{head}{}", self.message)
    }
}

/// 解析/校验入口（Cordis `Schema.resolve`）。
pub fn resolve(data: &Value, schema: &SchemaRef, options: &ResolveOptions) -> Result<Value, ValidationError> {
    if data.is_null() {
        // nullable 输入：required 报错；否则用 default（intersect 链取首个非空 default）
        if schema.meta.required {
            return Err(ValidationError::new("missing required value", &options.path));
        }
        let mut current = schema.clone();
        let mut fallback = current.meta.default.clone();
        while let (SchemaKind::Intersect(list), None) = (&current.kind, &fallback) {
            current = list[0].clone();
            fallback = current.meta.default.clone();
        }
        return match fallback {
            Some(d) => Ok(d.clone()),
            None => Ok(Value::Null),
        };
    }

    let resolved = resolve_kind(data, schema, options);
    match resolved {
        Ok(v) => Ok(v),
        Err(e) => {
            if schema.meta.loose {
                Ok(schema.meta.default.clone().unwrap_or(Value::Null))
            } else {
                Err(e)
            }
        }
    }
}

/// B4：配置简化（Cordis `Config.simplify` = schemastery `Schema.prototype.simplify`，
/// @deepseek-ai/schemastery/src/index.ts:407-442）。
///
/// 语义（逐分支对齐）：
/// - 值与 `meta.default` 深等 → `Null`（无默认 → false）。
/// - `null` 透传（schemastery `isNullable`）。
/// - `object`：逐键用子 schema 简化，`Null` 项**删键**；结果与 default 深等 → `Null`。
/// - `dict`：逐键用 inner 简化（**保留** `Null` 项）；结果与 default 深等 → `Null`。
/// - `array`/`tuple`：逐项映射（tuple 按索引对齐），无收尾 default 检查。
/// - `intersect`：逐成员简化后 `Object.assign` 合并。
/// - `union`：逐个 `try resolve(value, s, {})`，第一个可解析成员返回其简化。
/// - 其余类型：原值。
///
/// DIV-9-1：`deepEqual` 用 serde_json 深等（JSON 值域无 `undefined`，dict 的 default
/// 特判降级为常规深等）。DIV-9-4：`Lazy` 直接原值透传（与 schemastery 未分派 lazy 一致）。
pub fn simplify(schema: &SchemaRef, value: &Value) -> Value {
    if equals_default(value, &schema.meta.default) {
        return Value::Null;
    }
    if value.is_null() {
        return value.clone();
    }
    match &schema.kind {
        SchemaKind::Object(fields) => {
            let mut result = Map::new();
            if let Value::Object(map) = value {
                for (k, v) in map {
                    let item = match fields.get(k) {
                        Some(s) => simplify(s, v),
                        // object：key 未声明 → `schema?.simplify` 为 undefined → 删键
                        None => Value::Null,
                    };
                    // object：null/undefined 项删键（`!isNullable(item)`）
                    if !item.is_null() {
                        result.insert(k.clone(), item);
                    }
                }
            }
            let res = Value::Object(result.clone());
            if equals_default(&res, &schema.meta.default) {
                return Value::Null;
            }
            Value::Object(result)
        }
        SchemaKind::Dict { inner, .. } => {
            let mut result = Map::new();
            if let Value::Object(map) = value {
                for (k, v) in map {
                    result.insert(k.clone(), simplify(inner, v));
                }
            }
            let res = Value::Object(result.clone());
            if equals_default(&res, &schema.meta.default) {
                return Value::Null;
            }
            Value::Object(result)
        }
        SchemaKind::Array(inner) => {
            let arr = match value {
                Value::Array(items) => items.iter().map(|v| simplify(inner, v)).collect(),
                _ => Vec::new(),
            };
            Value::Array(arr)
        }
        SchemaKind::Tuple(items) => {
            let arr = match value {
                Value::Array(vs) => vs
                    .iter()
                    .enumerate()
                    .map(|(i, v)| match items.get(i) {
                        Some(s) => simplify(s, v),
                        None => v.clone(),
                    })
                    .collect(),
                _ => Vec::new(),
            };
            Value::Array(arr)
        }
        SchemaKind::Intersect(list) => {
            let mut result = Map::new();
            for s in list {
                if let Value::Object(m) = simplify(s, value) {
                    for (k, v) in m {
                        result.insert(k, v);
                    }
                }
            }
            Value::Object(result)
        }
        SchemaKind::Union(list) => {
            let opts = ResolveOptions {
                path: Vec::new(),
                autofix: false,
                strict: false,
            };
            for s in list {
                if resolve(value, s, &opts).is_ok() {
                    return simplify(s, value);
                }
            }
            value.clone()
        }
        _ => value.clone(),
    }
}

/// `value` 与 schema 默认值深等（无默认 → false）。
fn equals_default(value: &Value, default: &Option<Value>) -> bool {
    match default {
        Some(d) => value == d,
        None => false,
    }
}

fn resolve_kind(data: &Value, schema: &SchemaRef, options: &ResolveOptions) -> Result<Value, ValidationError> {
    match &schema.kind {
        SchemaKind::Any => Ok(data.clone()),
        SchemaKind::Never => Err(ValidationError::new(
            format!("expected nullable but got {}", js_string(data)),
            &options.path,
        )),
        SchemaKind::Const(value) => {
            if data == value {
                Ok(value.clone())
            } else {
                Err(ValidationError::new(
                    format!("expected {} but got {}", const_str(value), js_string(data)),
                    &options.path,
                ))
            }
        }
        SchemaKind::String => {
            let Value::String(s) = data else {
                return Err(ValidationError::new(
                    format!("expected string but got {}", js_string(data)),
                    &options.path,
                ));
            };
            if let Some((source, flags)) = &schema.meta.pattern {
                let expr = build_regex(source, flags);
                if let Ok(re) = regex::Regex::new(&expr) {
                    if !re.is_match(s) {
                        return Err(ValidationError::new(
                            format!("expect string to match regexp /{source}/{flags}"),
                            &options.path,
                        ));
                    }
                }
            }
            check_range(s.len() as f64, &schema.meta, "string length", &options.path)?;
            Ok(data.clone())
        }
        SchemaKind::Number => {
            let Some(n) = data.as_f64() else {
                return Err(ValidationError::new(
                    format!("expected number but got {}", js_string(data)),
                    &options.path,
                ));
            };
            check_range(n, &schema.meta, "number", &options.path)?;
            if let Some(step) = schema.meta.step {
                if !is_multiple_of(n, schema.meta.min.unwrap_or(0.0), step) {
                    return Err(ValidationError::new(
                        format!("expected number multiple of {step} but got {n}"),
                        &options.path,
                    ));
                }
            }
            Ok(data.clone())
        }
        SchemaKind::Boolean => {
            if data.is_boolean() {
                Ok(data.clone())
            } else {
                Err(ValidationError::new(
                    format!("expected boolean but got {}", js_string(data)),
                    &options.path,
                ))
            }
        }
        SchemaKind::Function => Err(ValidationError::new(
            format!("expected function but got {}", js_string(data)),
            &options.path,
        )),
        SchemaKind::Is(name) => {
            let matches = match name.as_str() {
                "String" => data.is_string(),
                "Number" => data.is_number(),
                "Boolean" => data.is_boolean(),
                "Array" => data.is_array(),
                "Object" => data.is_object(),
                _ => false,
            };
            if matches {
                Ok(data.clone())
            } else {
                Err(ValidationError::new(
                    format!("expected {name} but got {}", js_string(data)),
                    &options.path,
                ))
            }
        }
        SchemaKind::Bitset(bits) => {
            let value = if let Some(n) = data.as_u64() {
                n
            } else if let Some(keys) = data.as_array() {
                let mut v = 0u64;
                for k in keys {
                    let Some(k) = k.as_str() else {
                        return Err(ValidationError::new(
                            format!("expected string but got {}", js_string(k)),
                            &options.path,
                        ));
                    };
                    v |= bits.get(k).copied().unwrap_or(0);
                }
                v
            } else {
                return Err(ValidationError::new(
                    format!("expected number or array but got {}", js_string(data)),
                    &options.path,
                ));
            };
            Ok(Value::from(value))
        }
        SchemaKind::Array(inner) => {
            let Value::Array(items) = data else {
                return Err(ValidationError::new(
                    format!("expected array but got {}", js_string(data)),
                    &options.path,
                ));
            };
            check_range(
                items.len() as f64,
                &schema.meta,
                "array length",
                &options.path,
            )?;
            let mut out = Vec::with_capacity(items.len());
            for (index, item) in items.iter().enumerate() {
                out.push(property(item, &PathSeg::Index(index), inner, options)?);
            }
            Ok(Value::Array(out))
        }
        SchemaKind::Dict { inner, s_key } => {
            let Value::Object(map) = data else {
                return Err(ValidationError::new(
                    format!("expected object but got {}", js_string(data)),
                    &options.path,
                ));
            };
            let mut out = Map::new();
            for (key, value) in map {
                // key 经 sKey 校验：strict 下失败跳过该键；非 strict 抛错
                // （对应 Cordis dict 的 catch → strict ? continue : throw）。
                let r_key = match resolve(&Value::String(key.clone()), s_key, options) {
                    Ok(v) => v.as_str().map(|s| s.to_string()).unwrap_or_else(|| key.clone()),
                    Err(e) => {
                        if options.strict {
                            continue;
                        }
                        return Err(e);
                    }
                };
                let v = property(value, &PathSeg::Key(key.clone()), inner, options)?;
                out.insert(r_key, v);
            }
            Ok(Value::Object(out))
        }
        SchemaKind::Tuple(list) => {
            let Value::Array(items) = data else {
                return Err(ValidationError::new(
                    format!("expected array but got {}", js_string(data)),
                    &options.path,
                ));
            };
            let mut out = Vec::with_capacity(list.len());
            for (index, inner) in list.iter().enumerate() {
                let item = items.get(index).unwrap_or(&Value::Null);
                out.push(property(item, &PathSeg::Index(index), inner, options)?);
            }
            // 非 strict：追加多余项（strict 丢弃）
            if !options.strict {
                out.extend(items.iter().skip(list.len()).cloned());
            }
            Ok(Value::Array(out))
        }
        SchemaKind::Object(dict) => {
            let Value::Object(map) = data else {
                return Err(ValidationError::new(
                    format!("expected object but got {}", js_string(data)),
                    &options.path,
                ));
            };
            let mut out = Map::new();
            for (key, inner) in dict {
                let child = map.get(key).cloned().unwrap_or(Value::Null);
                match property_opt(&child, &PathSeg::Key(key.clone()), inner, options) {
                    Ok(value) => {
                        if !value.is_null() || map.contains_key(key) {
                            out.insert(key.clone(), value);
                        }
                    }
                    Err(_) if options.autofix => {
                        // autofix：删除无效项并回退默认（Cordis `property`）
                        let value = inner.meta.default.clone().unwrap_or(Value::Null);
                        if !value.is_null() {
                            out.insert(key.clone(), value);
                        }
                    }
                    Err(e) => return Err(e),
                }
            }
            // 非 strict：合并多余键（strict 丢弃）
            if !options.strict {
                for (key, value) in map {
                    if !dict.contains_key(key) {
                        out.insert(key.clone(), value.clone());
                    }
                }
            }
            Ok(Value::Object(out))
        }
        SchemaKind::Union(list) => {
            let mut messages = Vec::new();
            for inner in list {
                match resolve(data, inner, options) {
                    Ok(v) => return Ok(v),
                    Err(e) => messages.push(e.message),
                }
            }
            Err(ValidationError::new(
                format!(
                    "expected {} but got {}",
                    schema_to_string(schema),
                    serde_json::to_string(data).unwrap_or_default()
                ),
                &options.path,
            ))
        }
        SchemaKind::Intersect(list) => {
            if list.is_empty() {
                return Ok(data.clone());
            }
            let mut result: Option<Value> = None;
            for inner in list {
                let value = resolve(data, inner, options)?;
                if value.is_null() {
                    continue;
                }
                match &result {
                    None => result = Some(value),
                    Some(prev) => {
                        if prev.is_object() && value.is_object() {
                            let mut merged = prev.as_object().unwrap().clone();
                            for (k, v) in value.as_object().unwrap() {
                                merged.insert(k.clone(), v.clone());
                            }
                            result = Some(Value::Object(merged));
                        } else if prev != &value {
                            return Err(ValidationError::new(
                                format!(
                                    "expected {} but got {}",
                                    schema_to_string(schema),
                                    serde_json::to_string(data).unwrap_or_default()
                                ),
                                &options.path,
                            ));
                        }
                    }
                }
            }
            // 非 strict：合并剩余对象键（strict 丢弃）
            let mut result = result.unwrap_or_else(|| data.clone());
            if !options.strict {
                if let (Value::Object(r), Value::Object(m)) = (&mut result, data) {
                    for (k, v) in m {
                        if !r.contains_key(k) {
                            r.insert(k.clone(), v.clone());
                        }
                    }
                }
            }
            Ok(result)
        }
        SchemaKind::Transform {
            inner,
            preserve: _,
            callback,
        } => {
            let result = resolve(data, inner, options)?;
            callback(&result).map_err(|msg| ValidationError::new(msg, &options.path))
        }
        SchemaKind::Lazy(state) => {
            let inner = {
                let mut resolved = state.resolved.borrow_mut();
                if resolved.is_none() {
                    *resolved = Some((state.builder)());
                }
                resolved.clone().unwrap()
            };
            resolve(data, &inner, options)
        }
        SchemaKind::Custom(type_name) => {
            // M57：查全局注册表；未注册 → unsupported（fail loud）。
            let registry = custom_registry().lock().unwrap();
            match registry.get(type_name) {
                Some(resolver) => resolver(data, schema, options),
                None => Err(ValidationError::new(
                    format!("unsupported type \"{type_name}\""),
                    &options.path,
                )),
            }
        }
    }
}

/// 逐项校验（Cordis `property`）：错误时 autofix 回退默认，否则抛。
fn property(data: &Value, seg: &PathSeg, schema: &SchemaRef, options: &ResolveOptions) -> Result<Value, ValidationError> {
    match property_opt(data, seg, schema, options) {
        Ok(v) => Ok(v),
        Err(_) if options.autofix => Ok(schema.meta.default.clone().unwrap_or(Value::Null)),
        Err(e) => Err(e),
    }
}

fn property_opt(
    data: &Value,
    seg: &PathSeg,
    schema: &SchemaRef,
    options: &ResolveOptions,
) -> Result<Value, ValidationError> {
    let mut opts = options.clone();
    opts.path.push(seg.clone());
    resolve(data, schema, &opts)
}

fn check_range(data: f64, meta: &Meta, description: &str, path: &[PathSeg]) -> Result<(), ValidationError> {
    if let Some(max) = meta.max {
        if data > max {
            return Err(ValidationError::new(
                format!("expected {description} <= {max} but got {data}"),
                path,
            ));
        }
    }
    if let Some(min) = meta.min {
        if data < min {
            return Err(ValidationError::new(
                format!("expected {description} >= {min} but got {data}"),
                path,
            ));
        }
    }
    Ok(())
}

fn build_regex(source: &str, flags: &str) -> String {
    let mut prefix = String::new();
    if flags.contains('i') {
        prefix.push_str("(?i)");
    }
    if flags.contains('m') {
        prefix.push_str("(?m)");
    }
    if flags.contains('s') {
        prefix.push_str("(?s)");
    }
    format!("{prefix}{source}")
}

/// RFC3339 日期时间校验（M26 date 组合子用）。
/// 接受 `YYYY-MM-DDTHH:MM:SS[.frac]?(Z|±HH:MM)`；返回是否合法。
/// 轻量实现（无 chrono 依赖）：校验字段格式与取值范围。
pub fn parse_datetime(s: &str) -> Option<()> {
    let bytes = s.as_bytes();
    // 基本长度：YYYY-MM-DDTHH:MM:SS + 后缀（Z 或 ±HH:MM）≥ 20
    if bytes.len() < 20 {
        return None;
    }
    // 固定分隔符位置
    if bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T'
        || bytes[13] != b':' || bytes[16] != b':'
    {
        return None;
    }
    // 数字段
    let digits = |start: usize, end: usize| -> Option<u32> {
        if end > bytes.len() {
            return None;
        }
        let mut v = 0u32;
        for &b in &bytes[start..end] {
            if !b.is_ascii_digit() {
                return None;
            }
            v = v * 10 + (b - b'0') as u32;
        }
        Some(v)
    };
    let _year = digits(0, 4)?;
    let month = digits(5, 7)?;
    let day = digits(8, 10)?;
    let hour = digits(11, 13)?;
    let minute = digits(14, 16)?;
    let second = digits(17, 19)?;
    // 范围（u32 无负数；year 恒 ≥ 0）
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    // 小数秒（可选）
    let mut i = 19;
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i == start {
            return None;
        }
    }
    // 时区：Z 或 ±HH:MM
    if i >= bytes.len() {
        return None;
    }
    match bytes[i] {
        b'Z' => i += 1,
        b'+' | b'-' => {
            i += 1;
            let tz_h = digits(i, i + 2)?;
            if bytes.get(i + 2) != Some(&b':') {
                return None;
            }
            let tz_m = digits(i + 3, i + 5)?;
            if tz_h > 23 || tz_m > 59 {
                return None;
            }
            i += 5;
        }
        _ => return None,
    }
    if i != bytes.len() {
        return None;
    }
    Some(())
}

/// 十进制安全取模检查（Cordis `isMultipleOf`/`decimalShift`）。
fn decimal_shift(data: f64, digits: u32) -> f64 {
    let s = format!("{}", data);
    if s.contains('e') {
        return data * 10f64.powi(digits as i32);
    }
    match s.find('.') {
        None => data * 10f64.powi(digits as i32),
        Some(idx) => {
            let frac = &s[idx + 1..];
            let integer = &s[..idx];
            if frac.len() <= digits as usize {
                let padded = format!("{frac:0<width$}", width = digits as usize);
                format!("{integer}{padded}").parse().unwrap_or(data)
            } else {
                let (a, b) = frac.split_at(digits as usize);
                format!("{integer}{a}.{b}").parse().unwrap_or(data)
            }
        }
    }
}

fn is_multiple_of(data: f64, min: f64, step: f64) -> bool {
    let step = step.abs();
    let step_str = format!("{}", step);
    if !step_str.contains('.') {
        return (data - min) % step == 0.0;
    }
    let idx = step_str.find('.').unwrap();
    let digits = (step_str.len() - idx - 1) as u32;
    (decimal_shift(data, digits) - decimal_shift(min, digits)) % decimal_shift(step, digits) == 0.0
}

/// JS `String(value)` 的 Value-land 近似。
fn js_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        other => serde_json::to_string(other).unwrap_or_else(|_| "<unprintable>".to_string()),
    }
}

/// const 值展示（字符串带引号）。
fn const_str(v: &Value) -> String {
    if v.is_string() {
        serde_json::to_string(v).unwrap_or_default()
    } else {
        js_string(v)
    }
}

/// 紧凑 TS 类型字符串（Cordis `Schema.toString`）。
pub fn schema_to_string(schema: &SchemaRef) -> String {
    fn fmt(schema: &SchemaRef, inline: bool) -> String {
        match &schema.kind {
            SchemaKind::Any => "any".to_string(),
            SchemaKind::Never => "never".to_string(),
            SchemaKind::Const(v) => const_str(v),
            SchemaKind::String => "string".to_string(),
            SchemaKind::Number => "number".to_string(),
            SchemaKind::Boolean => "boolean".to_string(),
            SchemaKind::Function => "function".to_string(),
            SchemaKind::Is(name) => name.clone(),
            SchemaKind::Bitset(_) => "bitset".to_string(),
            SchemaKind::Array(inner) => format!("{}[]", fmt(inner, true)),
            SchemaKind::Dict { inner, s_key } => {
                format!("{{ [key: {}]: {} }}", fmt(s_key, true), fmt(inner, true))
            }
            SchemaKind::Tuple(list) => {
                let items: Vec<String> = list.iter().map(|s| fmt(s, true)).collect();
                format!("[{}]", items.join(", "))
            }
            SchemaKind::Object(dict) => {
                if dict.is_empty() {
                    return "{}".to_string();
                }
                let mut keys: Vec<&String> = dict.keys().collect();
                keys.sort();
                let items: Vec<String> = keys
                    .iter()
                    .map(|k| {
                        let inner = &dict[*k];
                        let opt = if inner.meta.required { "" } else { "?" };
                        format!("{k}{opt}: {}", fmt(inner, true))
                    })
                    .collect();
                format!("{{ {} }}", items.join(", "))
            }
            SchemaKind::Union(list) => {
                let inner: Vec<String> = list.iter().map(|s| fmt(s, false)).collect();
                let joined = inner.join(" | ");
                if inline {
                    format!("({joined})")
                } else {
                    joined
                }
            }
            SchemaKind::Intersect(list) => {
                let inner: Vec<String> = list.iter().map(|s| fmt(s, true)).collect();
                inner.join(" & ")
            }
            SchemaKind::Transform { inner, .. } => fmt(inner, true),
            SchemaKind::Lazy(_) => "any".to_string(),
            SchemaKind::Custom(name) => name.clone(),
        }
    }
    fmt(schema, false)
}

/// 序列化一个 schema 节点为 wire JSON，并把整棵树写入 `refs`。返回分配的 uid。
///
/// 对齐 Schemastery `toJSON` 的 refs 表语义：节点自身不含 uid 字段，嵌套 schema
/// 只以 uid 数字出现在结构字段（`dict`/`inner`/`list`/`sKey`）；`callback`/`builder`
/// 等函数不出现；`lazy` 在被展开（LazyState.resolved 为 Some）时输出 inner。
fn schema_to_json_node(
    schema: &Schema,
    refs: &mut serde_json::Map<String, Value>,
    next_uid: &mut usize,
) -> u64 {
    let uid = *next_uid as u64;
    *next_uid += 1;
    let mut node = serde_json::Map::new();
    // type 判别。
    let type_name = match &schema.kind {
        SchemaKind::Any => "any",
        SchemaKind::Never => "never",
        SchemaKind::Const(_) => "const",
        SchemaKind::String => "string",
        SchemaKind::Number => "number",
        SchemaKind::Boolean => "boolean",
        SchemaKind::Function => "function",
        SchemaKind::Is(_) => "is",
        SchemaKind::Bitset(_) => "bitset",
        SchemaKind::Array(_) => "array",
        SchemaKind::Dict { .. } => "dict",
        SchemaKind::Tuple(_) => "tuple",
        SchemaKind::Object(_) => "object",
        SchemaKind::Union(_) => "union",
        SchemaKind::Intersect(_) => "intersect",
        SchemaKind::Transform { .. } => "transform",
        SchemaKind::Lazy(_) => "lazy",
        SchemaKind::Custom(name) => name.as_str(),
    };
    node.insert("type".to_string(), Value::String(type_name.to_string()));
    // 结构字段（嵌套引用 → uid 数字）。`&Rc<Schema>` 经 deref 协变到 `&Schema`。
    let mut child = |inner: &SchemaRef| schema_to_json_node(inner, refs, next_uid);
    match &schema.kind {
        SchemaKind::Const(v) => {
            node.insert("value".to_string(), v.clone());
        }
        SchemaKind::Is(name) => {
            node.insert("constructor".to_string(), Value::String(name.clone()));
        }
        SchemaKind::Bitset(bits) => {
            let mut b = serde_json::Map::new();
            for (k, v) in bits {
                b.insert(k.clone(), Value::from(*v));
            }
            node.insert("bits".to_string(), Value::Object(b));
        }
        SchemaKind::Array(inner) => {
            node.insert("inner".to_string(), Value::from(child(inner)));
        }
        SchemaKind::Dict { inner, s_key } => {
            node.insert("inner".to_string(), Value::from(child(inner)));
            node.insert("sKey".to_string(), Value::from(child(s_key)));
        }
        SchemaKind::Tuple(list) | SchemaKind::Union(list) | SchemaKind::Intersect(list) => {
            let items: Vec<Value> = list.iter().map(child).map(Value::from).collect();
            node.insert("list".to_string(), Value::Array(items));
        }
        SchemaKind::Object(dict) => {
            let mut d = serde_json::Map::new();
            let mut keys: Vec<&String> = dict.keys().collect();
            keys.sort();
            for key in keys {
                d.insert(key.clone(), Value::from(child(&dict[key])));
            }
            node.insert("dict".to_string(), Value::Object(d));
        }
        SchemaKind::Transform { inner, preserve, .. } => {
            node.insert("inner".to_string(), Value::from(child(inner)));
            if *preserve {
                node.insert("preserve".to_string(), Value::Bool(true));
            }
            // callback（函数）不序列化——对齐 JSON.stringify 跳过函数。
        }
        SchemaKind::Lazy(lazy) => {
            if let Some(inner) = lazy.resolved.borrow().as_ref() {
                node.insert("inner".to_string(), Value::from(child(inner)));
            }
        }
        _ => {}
    }
    node.insert("meta".to_string(), meta_to_json(&schema.meta));
    refs.insert(uid.to_string(), Value::Object(node));
    uid
}

/// Meta → wire JSON（对齐 TS schema.meta 顶层键）。`extra` 的 object 键展开到
/// meta 顶层（TS `extra(key, value)` 语义）；`Null` 键不输出；缺省值不输出。
fn meta_to_json(meta: &Meta) -> Value {
    let mut m = serde_json::Map::new();
    if meta.required {
        m.insert("required".to_string(), Value::Bool(true));
    }
    if meta.loose {
        m.insert("loose".to_string(), Value::Bool(true));
    }
    if meta.hidden {
        m.insert("hidden".to_string(), Value::Bool(true));
    }
    if meta.collapse {
        m.insert("collapse".to_string(), Value::Bool(true));
    }
    if meta.disabled {
        m.insert("disabled".to_string(), Value::Bool(true));
    }
    if let Some(v) = &meta.default {
        m.insert("default".to_string(), v.clone());
    }
    if let Some((source, flags)) = &meta.pattern {
        m.insert(
            "pattern".to_string(),
            serde_json::json!({ "source": source, "flags": flags }),
        );
    }
    if let Some(v) = meta.min {
        m.insert("min".to_string(), serde_json::json!(v));
    }
    if let Some(v) = meta.max {
        m.insert("max".to_string(), serde_json::json!(v));
    }
    if let Some(v) = meta.step {
        m.insert("step".to_string(), serde_json::json!(v));
    }
    if let Some(v) = &meta.role {
        m.insert("role".to_string(), Value::String(v.clone()));
    }
    if let Some(v) = &meta.description {
        m.insert("description".to_string(), Value::String(v.clone()));
    }
    if let Some(v) = &meta.comment {
        m.insert("comment".to_string(), Value::String(v.clone()));
    }
    if let Some(v) = &meta.link {
        m.insert("link".to_string(), Value::String(v.clone()));
    }
    if !meta.badges.is_empty() {
        let badges: Vec<Value> = meta
            .badges
            .iter()
            .map(|(text, kind)| serde_json::json!({ "text": text, "type": kind }))
            .collect();
        m.insert("badges".to_string(), Value::Array(badges));
    }
    // extra object 键展开到 meta 顶层（TS `meta[key]=value` 语义）。
    if let Some(extra) = meta.extra.as_object() {
        for (k, v) in extra {
            if !v.is_null() {
                m.insert(k.clone(), v.clone());
            }
        }
    }
    Value::Object(m)
}

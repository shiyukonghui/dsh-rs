//! Logger 服务（对应 PLAN §1.7）。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::types::{FiberId, Value};

/// 日志严重级别（Cordis `LoggerType`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LoggerType {
    Error,
    Info,
    Warn,
    Debug,
}

impl LoggerType {
    /// 数值级别：ERROR=0 < INFO=1 < WARN=2 < DEBUG=3。
    pub fn level(&self) -> u8 {
        match self {
            LoggerType::Error => 0,
            LoggerType::Info => 1,
            LoggerType::Warn => 2,
            LoggerType::Debug => 3,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            LoggerType::Error => "error",
            LoggerType::Info => "info",
            LoggerType::Warn => "warn",
            LoggerType::Debug => "debug",
        }
    }
}

/// 结构化日志记录（Cordis `Message`）。
#[derive(Debug, Clone)]
pub struct Message {
    pub sn: u64,
    pub ts: u64,
    pub name: String,
    pub r#type: LoggerType,
    pub level: u8,
    pub args: Vec<Value>,
    /// 产生日志的 fiber（诊断用途）。
    pub fiber: Option<FiberId>,
}

/// 日志导出器（Cordis `Exporter`）。
pub type Exporter = Box<dyn Fn(&Message)>;

/// 导出器的级别过滤配置。
#[derive(Debug, Clone, Default)]
pub struct ExporterConfig {
    /// 按 logger 名的阈值。
    pub levels: HashMap<String, u8>,
    /// 默认阈值（无 name 命中时）。
    pub default_level: Option<u8>,
}

/// Logger 门面：`ctx.logger(name)` 返回，提供 error/info/warn/debug。
#[derive(Clone)]
pub struct Logger {
    pub(crate) ctx: crate::context::Cordis,
    pub name: String,
    /// 本 logger 的默认阈值（无 exporter 配置时生效）。
    pub level: Option<u8>,
}

impl Logger {
    pub fn error(&self, args: Vec<Value>) {
        self.log(LoggerType::Error, args);
    }
    pub fn info(&self, args: Vec<Value>) {
        self.log(LoggerType::Info, args);
    }
    pub fn warn(&self, args: Vec<Value>) {
        self.log(LoggerType::Warn, args);
    }
    pub fn debug(&self, args: Vec<Value>) {
        self.log(LoggerType::Debug, args);
    }

    /// 记录一个错误（Cordis 单 Error 参数的展开路径）。
    pub fn log_err(&self, err: &crate::error::CordisError) {
        self.error(vec![Value::String(err.to_string())]);
    }

    /// 记录聚合错误：逐个展开（Cordis AggregateError 展开路径）。
    pub fn log_aggregate(&self, agg: &crate::error::AggregateError) {
        for e in &agg.errors {
            self.log_err(e);
        }
    }

    fn log(&self, r#type: LoggerType, args: Vec<Value>) {
        let mut rt = self.ctx.rt.borrow_mut();
        rt.logger.sn += 1;
        let msg = Message {
            sn: rt.logger.sn,
            ts: now_ms(),
            name: self.name.clone(),
            r#type,
            level: r#type.level(),
            args,
            fiber: rt.current_fiber(),
        };
        rt.logger.emit(msg);
    }
}

/// Logger 状态（运行时持有）。
pub struct LoggerState {
    pub exporters: Vec<(u64, Exporter, ExporterConfig)>,
    pub sn: u64,
    pub next_exporter: u64,
    pub buffer: Rc<RefCell<Vec<Message>>>,
    pub buffer_size: usize,
}

impl LoggerState {
    pub fn new() -> Self {
        let buffer = Rc::new(RefCell::new(Vec::new()));
        let buffer_size = 1000usize;
        let mut state = LoggerState {
            exporters: Vec::new(),
            sn: 0,
            next_exporter: 0,
            buffer: buffer.clone(),
            buffer_size,
        };
        // 内置 buffer 导出器（Cordis LoggerService 构造器注册）
        state.register(
            Box::new(move |msg| {
                let mut buf = buffer.borrow_mut();
                buf.push(msg.clone());
                let overflow = buf.len().saturating_sub(buffer_size);
                if overflow > 0 {
                    buf.drain(0..overflow);
                }
            }),
            ExporterConfig::default(),
        );
        state
    }

    /// 注册导出器，返回 id（供移除）。
    pub fn register(&mut self, exporter: Exporter, config: ExporterConfig) -> u64 {
        self.next_exporter += 1;
        let id = self.next_exporter;
        self.exporters.push((id, exporter, config));
        id
    }

    pub fn remove(&mut self, id: u64) -> bool {
        let before = self.exporters.len();
        self.exporters.retain(|(i, _, _)| *i != id);
        before != self.exporters.len()
    }

    /// 派发一条消息：按阈值过滤后交给每个导出器。
    /// 阈值语义忠实 Cordis：`targetLevel < level` 跳过——阈值是「最高显示级别」，
    /// 默认 INFO(1) 下 warn(2)/debug(3) 会被过滤。
    pub fn emit(&mut self, msg: Message) {
        for (_, exporter, config) in &self.exporters {
            let target = config
                .levels
                .get(&msg.name)
                .copied()
                .or(config.default_level)
                .unwrap_or(1);
            if target < msg.level {
                continue;
            }
            exporter(&msg);
        }
    }

    pub fn buffer_snapshot(&self) -> Vec<Message> {
        self.buffer.borrow().clone()
    }
}

impl Default for LoggerState {
    fn default() -> Self {
        Self::new()
    }
}

/// printf 风格格式化（Cordis `Logger.format`）：%s %d %i %f %o %O %c %C，%% 转义。
pub fn format_message(msg: &Message) -> String {
    let mut args: Vec<Value> = msg.args.clone();
    let fmt = match args.first() {
        Some(Value::String(_)) => {
            let f = args.remove(0);
            f.as_str().unwrap().to_string()
        }
        _ => "%o".to_string(),
    };

    let chars: Vec<char> = fmt.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '%' && i + 1 < chars.len() {
            let spec = chars[i + 1];
            match spec {
                '%' => {
                    out.push('%');
                    i += 2;
                    continue;
                }
                's' => {
                    if !args.is_empty() {
                        out.push_str(&value_str(&args.remove(0)));
                    }
                    i += 2;
                    continue;
                }
                'd' | 'i' => {
                    let v = args.first().cloned().unwrap_or(Value::Null);
                    if !args.is_empty() {
                        args.remove(0);
                    }
                    let n = v.as_i64().unwrap_or(0);
                    out.push_str(&n.to_string());
                    i += 2;
                    continue;
                }
                'f' => {
                    let v = args.first().cloned().unwrap_or(Value::Null);
                    if !args.is_empty() {
                        args.remove(0);
                    }
                    let n = v.as_f64().unwrap_or(0.0);
                    out.push_str(&n.to_string());
                    i += 2;
                    continue;
                }
                'o' | 'O' => {
                    let v = args.first().cloned().unwrap_or(Value::Null);
                    if !args.is_empty() {
                        args.remove(0);
                    }
                    out.push_str(&serde_json::to_string(&v).unwrap_or_else(|_| "null".to_string()));
                    i += 2;
                    continue;
                }
                'c' => {
                    if !args.is_empty() {
                        args.remove(0);
                    }
                    i += 2;
                    continue;
                }
                'C' => {
                    if !args.is_empty() {
                        out.push_str(&value_str(&args.remove(0)));
                    }
                    i += 2;
                    continue;
                }
                _ => {
                    out.push(c);
                    i += 1;
                    continue;
                }
            }
        }
        out.push(c);
        i += 1;
    }

    // 剩余参数追加（对象走 JSON）
    for arg in args {
        out.push(' ');
        if arg.is_object() || arg.is_array() {
            out.push_str(&serde_json::to_string(&arg).unwrap_or_else(|_| "null".to_string()));
        } else {
            out.push_str(&value_str(&arg));
        }
    }

    // 每行 maxLength=10240 截断
    const MAX_LINE: usize = 10240;
    out.split('\n')
        .map(|line| {
            if line.len() > MAX_LINE {
                format!("{}...", &line[..MAX_LINE])
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Value → 展示字符串。
pub fn value_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        _ => serde_json::to_string(v).unwrap_or_else(|_| "<unprintable>".to_string()),
    }
}

/// camelCase / snake_case → kebab-case（Cordis `hyphenate`）。
pub fn hyphenate(name: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = name.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
        } else if *c == '_' {
            out.push('-');
        } else {
            out.push(*c);
        }
    }
    out
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

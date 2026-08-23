//! python 子进程后端（M5-DESIGN §7.3 真实落地）。
//!
//! 传输：stdin/stdout JSON-lines（本平台 `std::process` 无法给子进程建额外 fd →
//! 协议不走 PROTOCOL_FD=3，改走 0/1，用户输出经 `log` 帧回流；见 D-066）。宿主视入站
//! 帧为敌：`validate_child_frame` 字段校验 + REBUILD。超时/中止 → dsh-subprocess 树级
//! `terminate()`。完成值 lossless：`classify_admission` 区分 invalid-output/output-limit。

use crate::json_lossless::{parse_lossless_json, AdmissionError};
use crate::seam::{validate_binding_namespace, CodeRuntime};
use crate::types::{
    CancellationToken, CodeLanguage, CodeRunFailure, CodeRunFailureKind, CodeRunRequest,
    CodeRunResult, Isolation,
};
use dsh_subprocess::{ChildStdio, StdinMode, StdoutMode, SubprocessCollect};
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 编译期嵌入 python worker 脚本（随 crate 打包，运行时落临时目录）。
pub const WORKER_SOURCE: &str = include_str!("../python_worker/worker.py");

/// python 后端配置（显式优于隐式）。
#[derive(Debug, Clone)]
pub struct PythonConfig {
    /// python 可执行（缺省 → `locate_python`）。
    pub python: Option<PathBuf>,
    /// 单次 run 的硬预算（毫秒）。
    pub timeout_ms: u64,
    /// 外层结果预算：日志 + 完成值序列化字节上限。
    pub max_output_bytes: usize,
    /// 终止宽限（毫秒）透传 dsh-subprocess。
    pub grace_ms: u64,
}

impl Default for PythonConfig {
    fn default() -> Self {
        PythonConfig {
            python: None,
            timeout_ms: 30_000,
            max_output_bytes: 1024 * 1024,
            grace_ms: 3_000,
        }
    }
}

/// 定位 python：`DSH_PYTHON`/`PYTHON` 环境 > Windows 常见安装位。
pub fn locate_python() -> Option<PathBuf> {
    for var in ["DSH_PYTHON", "PYTHON"] {
        if let Ok(p) = std::env::var(var) {
            let p = PathBuf::from(p);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    #[cfg(windows)]
    for cand in [
        r"D:\Anaconda\python.exe",
        r"C:\Python312\python.exe",
        r"C:\Python311\python.exe",
        r"C:\Python\python.exe",
    ] {
        let p = PathBuf::from(cand);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// 探测：当前环境能否跑 python（一次缓存；失败打印原因，不假装）。
pub fn python_available() -> bool {
    use std::sync::OnceLock;
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        let python = match locate_python() {
            Some(p) => p,
            None => {
                eprintln!("dsh-code-runtime: 未定位到 python（需求定位到 D:\\Anaconda\\python.exe / PYTHON）");
                return false;
            }
        };
        match std::process::Command::new(&python).arg("-c").arg("import sys; sys.exit(0)").output() {
            Ok(out) if out.status.success() => true,
            Ok(out) => {
                eprintln!("dsh-code-runtime: python 探测失败（{}）", String::from_utf8_lossy(&out.stderr).trim());
                false
            }
            Err(e) => {
                eprintln!("dsh-code-runtime: python 启动失败（{e}）");
                false
            }
        }
    })
}

/// 宿主侧终止原因（run 的 halted 分类）。
enum RunHalt {
    Timeout,
    Abort,
    WorkerExit(String),
}

/// 类型化子进程帧（校验 + REBUILD 后）。
enum ChildFrame {
    BootAck,
    Log(String),
    Call {
        id: i64,
        global: String,
        name: String,
        args: serde_json::Value,
    },
    Done {
        value: Option<serde_json::Value>,
        error: Option<(CodeRunFailureKind, String)>,
    },
}

/// 上游原始帧（保留畸形以便归类 WorkerExit）。
enum RawFrame {
    Json(serde_json::Value),
    Malformed(String),
    Eof,
}

/// `validate_child_frame`：字段校验 + REBUILD。未知/畸形 → Err（宿主归类 WorkerExit）。
fn validate_child_frame(v: serde_json::Value) -> Result<ChildFrame, String> {
    let obj = v.as_object().ok_or("frame is not an object")?;
    let typ = obj
        .get("type")
        .and_then(|t| t.as_str())
        .ok_or("frame missing type")?;
    match typ {
        "boot_ack" => Ok(ChildFrame::BootAck),
        "log" => {
            let text = obj
                .get("text")
                .and_then(|t| t.as_str())
                .ok_or("log frame missing text")?;
            Ok(ChildFrame::Log(text.to_string()))
        }
        "call" => {
            let id = obj
                .get("id")
                .and_then(|i| i.as_i64())
                .ok_or("call missing id")?;
            let global = obj
                .get("global")
                .and_then(|g| g.as_str())
                .ok_or("call missing global")?;
            let name = obj
                .get("name")
                .and_then(|n| n.as_str())
                .ok_or("call missing name")?;
            let args = obj.get("args").cloned().unwrap_or(serde_json::Value::Null);
            Ok(ChildFrame::Call {
                id,
                global: global.to_string(),
                name: name.to_string(),
                args,
            })
        }
        "done" => {
            let value = obj.get("value").cloned();
            let error = match obj.get("error") {
                Some(e) => {
                    let eo = e.as_object().ok_or("done error is not an object")?;
                    let kind = match eo.get("kind").and_then(|k| k.as_str()) {
                        Some("exception") => CodeRunFailureKind::Exception,
                        Some("invalid-output") => CodeRunFailureKind::InvalidOutput,
                        Some("output-limit") => CodeRunFailureKind::OutputLimit,
                        Some(other) => return Err(format!("done error kind {other:?}")),
                        None => return Err("done error missing kind".to_string()),
                    };
                    let message = eo
                        .get("message")
                        .and_then(|m| m.as_str())
                        .ok_or("done error missing message")?;
                    Some((kind, message.to_string()))
                }
                None => None,
            };
            Ok(ChildFrame::Done { value, error })
        }
        other => Err(format!("unknown child frame type {other:?}")),
    }
}

/// python 真实后端。
pub struct PythonCodeRuntime {
    config: PythonConfig,
}

impl PythonCodeRuntime {
    pub fn new(config: PythonConfig) -> Self {
        PythonCodeRuntime { config }
    }

    fn fail(&self, kind: CodeRunFailureKind, message: String) -> CodeRunResult {
        CodeRunResult {
            value: None,
            logs: Vec::new(),
            error: Some(CodeRunFailure {
                kind,
                message,
                detail: None,
            }),
        }
    }

    /// 把 halted 分类转结果，并附子进程诊断（stderr 尾）。
    fn halt_result(
        &self,
        handle: &mut dsh_subprocess::SubprocessHandle,
        halt: RunHalt,
    ) -> CodeRunResult {
        handle.terminate();
        let mut result = match halt {
            RunHalt::Timeout => self.fail(
                CodeRunFailureKind::Timeout,
                format!("run timed out after {}ms", self.config.timeout_ms),
            ),
            RunHalt::Abort => self.fail(CodeRunFailureKind::Abort, "aborted by caller".to_string()),
            RunHalt::WorkerExit(reason) => self.fail(CodeRunFailureKind::WorkerExit, reason),
        };
        let _ = handle.wait();
        let tail = handle.read_stderr(0).trim().to_string();
        if !tail.is_empty() {
            if let Some(e) = result.error.as_mut() {
                e.detail = Some(tail);
            }
        }
        result
    }

    /// 有限等待一个已校验帧。
    fn wait_frame(
        &self,
        queue: &Arc<Mutex<VecDeque<RawFrame>>>,
        deadline: Instant,
        abort: Option<&CancellationToken>,
    ) -> Result<Option<ChildFrame>, RunHalt> {
        loop {
            if let Some(tok) = abort {
                if tok.is_cancelled() {
                    return Err(RunHalt::Abort);
                }
            }
            if Instant::now() >= deadline {
                return Err(RunHalt::Timeout);
            }
            let mut guard = queue
                .lock()
                .map_err(|_| RunHalt::WorkerExit("queue poisoned".to_string()))?;
            match guard.pop_front() {
                Some(RawFrame::Json(v)) => {
                    let frame = validate_child_frame(v)
                        .map_err(|e| RunHalt::WorkerExit(format!("malformed child frame: {e}")))?;
                    return Ok(Some(frame));
                }
                Some(RawFrame::Malformed(e)) => {
                    return Err(RunHalt::WorkerExit(format!("malformed child frame: {e}")))
                }
                Some(RawFrame::Eof) => {
                    // 读者只会在 read()=0（干净 EOF）后推 Eof，且已在它之前推完所有帧；
                    // 到此处 = 子进程退出而未 done → worker-exit。
                    return Err(RunHalt::WorkerExit(
                        "worker exited without settling (protocol EOF)".to_string(),
                    ));
                }
                None => {}
            }
            drop(guard);
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn run_inner(&self, request: &CodeRunRequest) -> CodeRunResult {
        // 契约校验先行：非法命名空间 → 诚实失败（worker 反正拒绝；host 预检不装）。
        for ns in &request.bindings {
            if let Err(e) = validate_binding_namespace(ns) {
                return self.fail(
                    CodeRunFailureKind::WorkerExit,
                    format!("invalid binding namespace (contract misuse): {e}"),
                );
            }
        }
        let python = match locate_python() {
            Some(p) => p,
            None => {
                return self.fail(
                    CodeRunFailureKind::WorkerExit,
                    "no python runtime available (set DSH_PYTHON or install python)".to_string(),
                )
            }
        };

        // 临时目录落 worker.py（-u 关行缓冲；worker 用 `log` 帧回流输出）。
        let tmp = match tempfile::tempdir() {
            Ok(d) => d,
            Err(e) => return self.fail(CodeRunFailureKind::WorkerExit, format!("tempdir: {e}")),
        };
        let worker_path = tmp.path().join("worker.py");
        if let Err(e) = std::fs::write(&worker_path, WORKER_SOURCE) {
            return self.fail(CodeRunFailureKind::WorkerExit, format!("write worker: {e}"));
        }

        let spec = dsh_subprocess::SubprocessSpawnSpec {
            argv: vec![
                python.display().to_string(),
                "-u".into(),
                worker_path.display().to_string(),
            ],
            cwd: tmp.path().to_path_buf(),
            stdio: ChildStdio {
                stdin: StdinMode::Pipe,
                stdout: StdoutMode::Pipe,
                stderr: StdoutMode::Collect(SubprocessCollect {
                    max_bytes: 64 * 1024,
                    spill: None,
                }),
            },
            grace_ms: self.config.grace_ms,
            signal: None,
            env: None,
        };
        let mut handle = match dsh_subprocess::spawn(&spec) {
            Ok(h) => h,
            Err(e) => return self.fail(CodeRunFailureKind::WorkerExit, format!("spawn: {e}")),
        };

        // 协议读线程：stdout → 行 → RawFrame 队列（含 Eof）。
        let queue: Arc<Mutex<VecDeque<RawFrame>>> = Arc::new(Mutex::new(VecDeque::new()));
        let mut reader = match handle.take_stdout_reader() {
            Some(r) => r,
            None => {
                return self.fail(
                    CodeRunFailureKind::WorkerExit,
                    "no stdout reader (protocol)".to_string(),
                )
            }
        };
        let q2 = Arc::clone(&queue);
        let reader_thread = std::thread::spawn(move || {
            let mut buf: Vec<u8> = Vec::new();
            loop {
                let mut b = [0u8; 2048];
                match reader.read(&mut b) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&b[..n]);
                        while let Some(pos) = buf.iter().position(|x| *x == b'\n') {
                            let line: Vec<u8> = buf.drain(..=pos).collect();
                            let line = std::str::from_utf8(&line[..line.len() - 1]).unwrap_or("");
                            match parse_lossless_json(line) {
                                Ok(v) => q2.lock().unwrap().push_back(RawFrame::Json(v)),
                                Err(e) => q2.lock().unwrap().push_back(RawFrame::Malformed(e)),
                            }
                        }
                    }
                }
            }
            q2.lock().unwrap().push_back(RawFrame::Eof);
        });

        // boot（先写：worker 引导需 namespaces/max_output_bytes）
        {
            let boot_specs: Vec<serde_json::Value> = request
                .bindings
                .iter()
                .map(|ns| {
                    let mut spec = serde_json::Map::new();
                    spec.insert("global".into(), serde_json::Value::String(ns.global.clone()));
                    spec.insert(
                        "names".into(),
                        serde_json::Value::Array(
                            ns.functions.keys().map(|k| serde_json::Value::String(k.clone())).collect(),
                        ),
                    );
                    if let Some(ec) = &ns.error_class {
                        spec.insert(
                            "error_class".into(),
                            serde_json::json!({ "name": ec.name, "member_name_property": ec.member_name_property }),
                        );
                    }
                    serde_json::Value::Object(spec)
                })
                .collect();
            let boot = serde_json::json!({ "type": "boot", "namespaces": boot_specs, "max_output_bytes": self.config.max_output_bytes });
            if let Err(e) = self.write_frame(&mut handle, &boot) {
                handle.terminate();
                return self.fail(
                    CodeRunFailureKind::WorkerExit,
                    format!("boot write failed: {e}"),
                );
            }
        }

        let deadline = Instant::now() + Duration::from_millis(self.config.timeout_ms);
        // 等 boot_ack
        match self.wait_frame(&queue, deadline, request.signal) {
            Ok(Some(ChildFrame::BootAck)) => {}
            Ok(Some(_)) => {
                return self.fail(
                    CodeRunFailureKind::WorkerExit,
                    "protocol: early non-boot_ack frame".to_string(),
                )
            }
            Ok(None) => {
                return self.fail(
                    CodeRunFailureKind::WorkerExit,
                    "no boot_ack (worker silent)".to_string(),
                )
            }
            Err(halt) => return self.halt_result(&mut handle, halt),
        }

        // run
        let run = serde_json::json!({ "type": "run", "code": request.program });
        if let Err(e) = self.write_frame(&mut handle, &run) {
            handle.terminate();
            return self.fail(
                CodeRunFailureKind::WorkerExit,
                format!("run write failed: {e}"),
            );
        }

        // 绑定派发表（函数本身即 Arc<dyn Fn>，克隆 Arc 即可共享）。
        let mut bindings: HashMap<String, HashMap<String, crate::types::CodeBindingFunction>> =
            HashMap::new();
        for ns in &request.bindings {
            let entry = bindings.entry(ns.global.clone()).or_default();
            for (name, f) in &ns.functions {
                entry.insert(name.clone(), Arc::clone(f));
            }
        }

        let mut logs: Vec<String> = Vec::new();
        // 循环不休携带 logs；Finished 只描述 completion，循环后一次性装配错误/值。
        enum Finished {
            Value(serde_json::Value),
            NoValue,
            Error {
                kind: CodeRunFailureKind,
                message: String,
            },
            Halt(RunHalt),
            CallWrite(String),
        }
        let finished = loop {
            match self.wait_frame(&queue, deadline, request.signal) {
                Ok(Some(ChildFrame::Log(text))) => logs.push(text),
                Ok(Some(ChildFrame::Call {
                    id,
                    global,
                    name,
                    args,
                })) => {
                    let reply = match bindings.get(&global).and_then(|m| m.get(&name)) {
                        Some(f) => match f(args) {
                            Ok(value) => {
                                serde_json::json!({ "type": "reply", "id": id, "ok": true, "value": value })
                            }
                            Err(message) => {
                                serde_json::json!({ "type": "reply", "id": id, "ok": false, "message": message })
                            }
                        },
                        None => {
                            serde_json::json!({ "type": "reply", "id": id, "ok": false, "message": format!("unknown binding {global}.{name}") })
                        }
                    };
                    if let Err(e) = self.write_frame(&mut handle, &reply) {
                        break Finished::CallWrite(format!("reply write failed: {e}"));
                    }
                }
                Ok(Some(ChildFrame::Done { value, error })) => {
                    break match error {
                        Some((kind, message)) => Finished::Error { kind, message },
                        None => match value {
                            // 顶层 null = python None（显式或落到函数尾）→ 无完成值。
                            Some(serde_json::Value::Null) => Finished::NoValue,
                            Some(v) => Finished::Value(v),
                            None => Finished::NoValue,
                        },
                    };
                }
                Ok(Some(_)) => {} // BootAck 不应再出现，忽略
                Ok(None) => {}
                Err(halt) => break Finished::Halt(halt),
            }
        };

        let mut result = match finished {
            Finished::Value(v) => {
                match crate::json_lossless::classify_admission(&v, self.config.max_output_bytes) {
                    Ok(()) => CodeRunResult {
                        value: Some(v),
                        logs: Vec::new(),
                        error: None,
                    },
                    Err(AdmissionError::InvalidOutput) => CodeRunResult {
                        value: None,
                        logs: Vec::new(),
                        error: Some(CodeRunFailure {
                            kind: CodeRunFailureKind::InvalidOutput,
                            message: "completion value is not lossless JSON".into(),
                            detail: None,
                        }),
                    },
                    Err(AdmissionError::OutputLimit) => CodeRunResult {
                        value: None,
                        logs: Vec::new(),
                        error: Some(CodeRunFailure {
                            kind: CodeRunFailureKind::OutputLimit,
                            message: "completion value exceeded the output budget".into(),
                            detail: None,
                        }),
                    },
                }
            }
            Finished::NoValue => CodeRunResult {
                value: None,
                logs: Vec::new(),
                error: None,
            },
            Finished::Error { kind, message } => CodeRunResult {
                value: None,
                logs: Vec::new(),
                error: Some(CodeRunFailure {
                    kind,
                    message,
                    detail: None,
                }),
            },
            Finished::Halt(halt) => self.halt_result(&mut handle, halt),
            Finished::CallWrite(message) => {
                handle.terminate();
                self.fail(CodeRunFailureKind::WorkerExit, message)
            }
        };
        result.logs = logs;

        let_terminate_and_join(&mut handle, reader_thread);
        let _ = tmp;
        result
    }

    fn write_frame(
        &self,
        handle: &mut dsh_subprocess::SubprocessHandle,
        frame: &serde_json::Value,
    ) -> std::io::Result<()> {
        let mut line = serde_json::to_string(frame).map_err(std::io::Error::other)?;
        line.push('\n');
        let writer = handle
            .stdin_writer()
            .ok_or_else(|| std::io::Error::other("no stdin writer"))?;
        writer.write_all(line.as_bytes())?;
        writer.flush()
    }
}

fn let_terminate_and_join(
    handle: &mut dsh_subprocess::SubprocessHandle,
    thread: std::thread::JoinHandle<()>,
) {
    handle.terminate();
    let _ = thread.join();
}

impl CodeRuntime for PythonCodeRuntime {
    fn language(&self) -> CodeLanguage {
        CodeLanguage::Python
    }
    fn isolation(&self) -> Isolation {
        Isolation::Process
    }
    fn run(&self, request: &CodeRunRequest) -> CodeRunResult {
        self.run_inner(request)
    }
}

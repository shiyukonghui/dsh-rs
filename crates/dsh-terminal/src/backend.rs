//! dsh-terminal 真实 PTY 后端（M5-DESIGN §6.2，`portable-pty` 0.8 Windows=ConPTY）。
//!
//! 与 shell 层相同的单线程纪律：一个后台读线程把 master 输出追加进
//! `Arc<Mutex<Scrollback>>`（有界：`scrollback_max_bytes` 保尾 + `scrollback_lines` 裁行）；
//! `send()` 写 master 后在等待窗口内轮询「最后一次追加时间」判定
//! `InferredIdle / Timeout / SessionExit`。shell 程序参数化（设计默认 bash；测试/受控
//! 环境注入 cmd），本沙箱 msys 运行时被环境拒绝 → 集成测试用 `cmd` + 可达性门控。

use crate::registry::TerminalBackend;
use crate::types::{
    TerminalBackendKind, TerminalConfig, TerminalError, TerminalErrorCode, TerminalSendRequest,
    TerminalSendResult, TerminalSessionStatus, TerminalSignal, TerminalWaitReason,
};
use portable_pty::{native_pty_system, Child, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 滚动缓冲（保尾）：`max_lines` 裁行 + `max_bytes` 上限。
#[derive(Debug, Default)]
struct Scrollback {
    text: String,
    last_append: Option<Instant>,
}

impl Scrollback {
    fn append(&mut self, chunk: &str, max_bytes: usize, max_lines: usize) {
        self.text.push_str(chunk);
        self.last_append = Some(Instant::now());
        if self.text.len() > max_bytes {
            // 保尾：从字节上限再往回收容（UTF-8 边界裁剪行后可能略小于上限）。
            self.text = tail_utf8(&self.text, max_bytes);
        }
        // 按行裁（保留最后 max_lines 行；保留残缺尾行）
        let lines: Vec<&str> = self.text.split('\n').collect();
        if lines.len() > max_lines {
            self.text = lines[lines.len() - max_lines..].join("\n");
        }
    }
}

/// 从 `s` 取最后 `max_bytes` 字节并保证 UTF-8 边界。
fn tail_utf8(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = s.len() - max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[end..].to_string()
}

/// 真实 PTY 会话后端（`program` 注入以便测试/受限环境；`kind` 随方言）。
pub struct PtyBackend {
    label: String,
    program: String,
    kind: TerminalBackendKind,
    cfg: TerminalConfig,
    pair: Option<portable_pty::PtyPair>,
    child: Option<Box<dyn Child + Send + Sync>>,
    scrollback: Arc<Mutex<Scrollback>>,
    exited: Arc<AtomicBool>,
    reader_thread: Option<std::thread::JoinHandle<()>>,
    closed: bool,
    status: TerminalSessionStatus,
    writer: Option<boxed::Writer>,
}

// 极小 boxed 别名，规避 type_complexity。
mod boxed {
    use std::io::Write;
    pub type Writer = Box<dyn Write + Send>;
}

impl PtyBackend {
    pub fn new(label: &str, program: &str, kind: TerminalBackendKind) -> PtyBackend {
        PtyBackend {
            label: label.to_string(),
            program: program.to_string(),
            kind,
            cfg: TerminalConfig::default(),
            pair: None,
            child: None,
            scrollback: Arc::new(Mutex::new(Scrollback::default())),
            exited: Arc::new(AtomicBool::new(false)),
            reader_thread: None,
            closed: false,
            status: TerminalSessionStatus::Running,
            writer: None,
        }
    }

    /// 等待窗口内判定交付（wrote_at 之后：进程退出 → SessionExit；静默 ≥
    /// idle_silence → InferredIdle；超时 → Timeout）。
    fn wait_for_delivery(&mut self, wrote_at: Instant) -> TerminalWaitReason {
        let cfg = &self.cfg;
        let deadline = wrote_at + Duration::from_millis(cfg.timeout_ms);
        loop {
            // 退出优先：reader EOF 或 child try_wait 已回收（ConPTY 在关闭伪控制台
            // 前不会 EOF 输出管道，故不能只依赖 exited）。
            if self.exited.load(Ordering::SeqCst)
                || self
                    .child
                    .as_mut()
                    .map(|c| c.try_wait().ok().flatten().is_some())
                    .unwrap_or(false)
            {
                return TerminalWaitReason::SessionExit;
            }
            let now = Instant::now();
            if now >= deadline {
                return TerminalWaitReason::Timeout;
            }
            if let Ok(guard) = self.scrollback.lock() {
                if let Some(last) = guard.last_append {
                    if now.duration_since(last) >= Duration::from_millis(cfg.idle_silence_ms) {
                        return TerminalWaitReason::InferredIdle;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(cfg.poll_interval_ms));
        }
    }

    fn snapshot(&self, wait_reason: TerminalWaitReason) -> TerminalSendResult {
        let (viewport, truncated) = self.snapshot_read();
        TerminalSendResult {
            viewport,
            wait_reason,
            session_status: self.status,
            truncated,
        }
    }

    fn snapshot_read(&self) -> (String, bool) {
        let max = self.cfg.max_read_bytes;
        let guard = self.scrollback.lock().expect("scrollback lock");
        let text = &guard.text;
        if text.len() <= max {
            (text.clone(), false)
        } else {
            (tail_utf8(text, max), true)
        }
    }
}

impl TerminalBackend for PtyBackend {
    fn open(&mut self, _owner: &str, cfg: &TerminalConfig) -> Result<(), TerminalError> {
        self.cfg = cfg.clone();
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: cfg.rows,
                cols: cfg.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| {
                TerminalError::new(TerminalErrorCode::NoBackend, format!("openpty: {e}"))
            })?;
        let cmd = CommandBuilder::new(&self.program);
        let child = pair.slave.spawn_command(cmd).map_err(|e| {
            TerminalError::new(
                TerminalErrorCode::NoBackend,
                format!("spawn {program}: {e}", program = self.program),
            )
        })?;
        let mut reader = pair.master.try_clone_reader().map_err(|e| {
            TerminalError::new(TerminalErrorCode::NoBackend, format!("master reader: {e}"))
        })?;
        let writer = pair.master.take_writer().map_err(|e| {
            TerminalError::new(TerminalErrorCode::NoBackend, format!("master writer: {e}"))
        })?;
        let scrollback = Arc::clone(&self.scrollback);
        let exited = Arc::clone(&self.exited);
        let max_bytes = cfg.scrollback_max_bytes;
        let max_lines = cfg.scrollback_lines;
        let thread = std::thread::spawn(move || {
            let mut buf = [0u8; 2048];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => {
                        exited.store(true, Ordering::SeqCst);
                        break;
                    }
                    Ok(n) => {
                        let chunk = String::from_utf8_lossy(&buf[..n]);
                        if let Ok(mut sb) = scrollback.lock() {
                            sb.append(&chunk, max_bytes, max_lines);
                        }
                    }
                }
            }
        });
        self.pair = Some(pair);
        self.child = Some(child);
        self.writer = Some(writer);
        self.reader_thread = Some(thread);
        self.status = TerminalSessionStatus::Running;
        Ok(())
    }

    fn send(&mut self, req: &TerminalSendRequest) -> Result<TerminalSendResult, TerminalError> {
        let writer = self.writer.as_mut().ok_or_else(|| {
            TerminalError::new(
                TerminalErrorCode::NoBackend,
                "terminal not open".to_string(),
            )
        })?;
        let wrote_at = Instant::now();
        writer
            .write_all(req.text.as_bytes())
            .map_err(|e| TerminalError::new(TerminalErrorCode::NoBackend, e.to_string()))?;
        if req.submit {
            writer
                .write_all(b"\r")
                .map_err(|e| TerminalError::new(TerminalErrorCode::NoBackend, e.to_string()))?;
        }
        writer
            .flush()
            .map_err(|e| TerminalError::new(TerminalErrorCode::NoBackend, e.to_string()))?;
        let reason = self.wait_for_delivery(wrote_at);
        if matches!(reason, TerminalWaitReason::SessionExit) {
            self.status = TerminalSessionStatus::Exited;
        }
        Ok(self.snapshot(reason))
    }

    fn read(&mut self, max_read_bytes: usize) -> Result<String, TerminalError> {
        let guard = self.scrollback.lock().expect("scrollback lock");
        if guard.text.len() <= max_read_bytes {
            Ok(guard.text.clone())
        } else {
            Ok(tail_utf8(&guard.text, max_read_bytes))
        }
    }

    fn signal(&mut self, _sig: TerminalSignal) -> Result<(), TerminalError> {
        // ConPTY 无机敏信号发送（portable-pty 0.8）；最佳努力 kill 直系进程树。
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            self.status = TerminalSessionStatus::Aborted;
        }
        Ok(())
    }

    fn close(&mut self) -> Result<(), TerminalError> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        if self.status == TerminalSessionStatus::Running {
            self.status = TerminalSessionStatus::Exited;
        }
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
        }
        // 关键顺序：先丢弃 pair（关 ConPTY → 伪控制台拔出 → 输出管道 EOF），
        // 再 join reader 线程——否则 ConPTY master read 永不返回、join 卡死。
        self.pair = None;
        self.writer = None;
        self.child = None;
        if let Some(thread) = self.reader_thread.take() {
            let _ = thread.join();
        }
        Ok(())
    }

    fn label(&self) -> &str {
        &self.label
    }

    fn kind(&self) -> TerminalBackendKind {
        self.kind
    }
}

impl Drop for PtyBackend {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

//! dsh-subprocess 本地后端（`spawn` 原语，M5-DESIGN §2.1/§2.2/§2.3）。
//!
//! 由 `tests/spawn.rs` 红测驱动。实现要点：
//! - `std::process::Command` 装配：argv[0]=program、cwd 显式、env 缺省用 scrubbed 父环境
//!   （`scrubbed_parent_env`，仅当 spec.env 为 None 时）。
//! - 收集模式经**后台线程 drain** 管道（max_bytes 上限 + 可选 spill），使 IO 不占用
//!   调用线程，贴合「核心单线程 + 服务线程」纪律（readFrom(0) 返回批结果）。
//! - `wait()` settle 一次从不 reject（首胜缓存 outcome）；`terminate()` 后续增量落地。

use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::types::{
    CollectedOutput, ProcessError, StdinMode, StdoutMode, SubprocessCollect, SubprocessOutcome,
    SubprocessSpawnSpec,
};

/// 收集器结果：已收集字节 + lossy/spill 标记。
#[derive(Debug)]
struct CollectResult {
    data: Vec<u8>,
    spill_path: Option<PathBuf>,
}

fn drain_pipe<R: Read + Send + 'static>(
    mut reader: R,
    max_bytes: usize,
    spill: Option<(u64, PathBuf)>,
) -> CollectResult {
    // 始终 drain 到 EOF（避免管道满 → 子进程写阻塞/写失败）。内存仅保留尾部
    // ≤ max_bytes；发生溢出且配置了 spill → 完整流写盘，返回 spill 路径。
    let mut tail: Vec<u8> = Vec::new();
    let mut spill_path: Option<PathBuf> = None;
    let mut spill_file: Option<std::io::BufWriter<std::fs::File>> = None;
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                // 到达溢出阈值后，后续字节仅写 spill（若有），否则直接丢弃。
                if spill_path.is_some() {
                    if let Some(file) = &mut spill_file {
                        use std::io::Write;
                        let _ = file.write_all(&buf[..n]);
                    }
                    continue;
                }
                // 未溢出：追加到 tail，超限即进入溢出路径。
                tail.extend_from_slice(&buf[..n]);
                if tail.len() > max_bytes {
                    match &spill {
                        Some((_, dir)) if !tail.is_empty() => {
                            let _ = std::fs::create_dir_all(dir);
                            let path = dir.join(format!("spill-{}.log", uuid_fallback()));
                            if let Ok(file) = std::fs::File::create(&path) {
                                spill_path = Some(path.clone());
                                spill_file = Some(std::io::BufWriter::new(file));
                                use std::io::Write;
                                let _ = spill_file
                                    .as_mut()
                                    .and_then(|w| w.write_all(&tail).ok());
                                // 已溢出：完整流落盘，内存清空（readFrom(0) 恢复自 spill）
                                tail.clear();
                            }
                        }
                        _ => {
                            // 无 spill：只保留最后 max_bytes 作为诊断 tail。
                            let drop = tail.len() - max_bytes;
                            tail.drain(..drop);
                        }
                    }
                }
            }
            Err(_) => break,
        }
    }
    if let Some(file) = spill_file.as_mut() {
        use std::io::Write;
        let _ = file.flush();
    }
    CollectResult { data: tail, spill_path }
}

/// 单测可并发的短 ID（生产侧将由宿主注入唯一前缀）。
fn uuid_fallback() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("spill-{nanos}")
}

/// 有界收集：后台线程 drain 管道 → 内存 capped + 可选 spill。
fn start_collector(
    reader: impl Read + Send + 'static,
    cfg: &SubprocessCollect,
) -> Arc<Mutex<CollectedOutput>> {
    let max_bytes = cfg.max_bytes;
    let spill = cfg
        .spill
        .as_ref()
        .map(|s| (s.max_bytes, s.dir.clone()));
    let out = Arc::new(Mutex::new(CollectedOutput::from_bytes(Vec::new(), false, None)));
    let out2 = out.clone();
    thread::spawn(move || {
        let res = drain_pipe(reader, max_bytes, spill);
        // lossy：原字节非合法 UTF-8 → 标记（供诊断；前端 lossy-render 据此提示）
        let lossy = std::str::from_utf8(&res.data).is_err();
        let mut slot = out2.lock().expect("collector lock");
        *slot = CollectedOutput::from_bytes(res.data, lossy, res.spill_path);
    });
    out
}

/// 参考 `spawn()`（`subprocess-local/src/spawn.ts`）：装配零默认 spec → 真实子进程。
#[allow(clippy::result_large_err)]
pub fn spawn(spec: &SubprocessSpawnSpec) -> Result<SubprocessHandle, ProcessError> {
    if spec.argv.is_empty() {
        return Err(ProcessError::Spawn("empty argv".to_string()));
    }
    let program = &spec.argv[0];
    let mut cmd = Command::new(program);
    cmd.args(&spec.argv[1..]);
    cmd.current_dir(&spec.cwd);
    // 显式 env：spec.env 覆盖（键值透传）；缺省 scrubbed 父环境。
    match &spec.env {
        Some(pairs) => {
            cmd.env_clear();
            for (k, v) in pairs {
                cmd.env(k, v);
            }
        }
        None => {
            let parent: Vec<_> = std::env::vars_os().collect();
            let scrubbed = crate::scrubbed_parent_env(&parent);
            cmd.env_clear();
            for (k, v) in scrubbed {
                cmd.env(k, v);
            }
        }
    }

    // 装配 stdio
    match &spec.stdio.stdin {
        StdinMode::Ignore => {
            cmd.stdin(Stdio::null());
        }
        StdinMode::Pipe => {
            cmd.stdin(Stdio::piped());
        }
        StdinMode::WriteBytes(_) => {
            cmd.stdin(Stdio::piped());
        }
    }
    match &spec.stdio.stdout {
        StdoutMode::Collect(_) => {
            cmd.stdout(Stdio::piped());
        }
        StdoutMode::Inherit => {
            cmd.stdout(Stdio::inherit());
        }
        StdoutMode::Pipe => {
            cmd.stdout(Stdio::piped());
        }
    }
    match &spec.stdio.stderr {
        StdoutMode::Collect(_) => {
            cmd.stderr(Stdio::piped());
        }
        StdoutMode::Inherit => {
            cmd.stderr(Stdio::inherit());
        }
        StdoutMode::Pipe => {
            cmd.stderr(Stdio::piped());
        }
    }

    let mut child = cmd.spawn().map_err(|e| ProcessError::Spawn(e.to_string()))?;
    let pid = child.id();

    // stdin WriteBytes：写入后关闭（一次性数据）
    if let StdinMode::WriteBytes(data) = &spec.stdio.stdin {
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            let _ = stdin.write_all(data);
            let _ = stdin.flush();
        }
    }

    // 收集线程句柄
    let stdout_slot = match &spec.stdio.stdout {
        StdoutMode::Collect(cfg) => {
            let rd = child.stdout.take().expect("collect stdout piped");
            Some(start_collector(rd, cfg))
        }
        _ => None,
    };
    let stderr_slot = match &spec.stdio.stderr {
        StdoutMode::Collect(cfg) => {
            let rd = child.stderr.take().expect("collect stderr piped");
            Some(start_collector(rd, cfg))
        }
        _ => None,
    };

    Ok(SubprocessHandle {
        child: Some(child),
        pid,
        stdout_slot,
        stderr_slot,
        outcome: None,
        grace_ms: spec.grace_ms,
    })
}

/// 参考 `SubprocessHandle`（简化为 spawn 原语层面所需的最小面）。
pub struct SubprocessHandle {
    child: Option<Child>,
    pub pid: u32,
    stdout_slot: Option<Arc<Mutex<CollectedOutput>>>,
    stderr_slot: Option<Arc<Mutex<CollectedOutput>>>,
    outcome: Option<SubprocessOutcome>,
    grace_ms: u64,
}

impl SubprocessHandle {
    /// 等待运行结束；settle 一次（首次后缓存），从不 reject。
    pub fn wait(&mut self) -> SubprocessOutcome {
        if let Some(o) = &self.outcome {
            return o.clone();
        }
        let outcome = if let Some(child) = self.child.as_mut() {
            let status = child.wait();
            match status {
                Ok(st) => {
                    if let Some(code) = st.code() {
                        SubprocessOutcome { exit_code: Some(code), signal: None }
                    } else if st.success() {
                        SubprocessOutcome { exit_code: Some(0), signal: None }
                    } else {
                        // 信号终止（code() == None）：按平台惯例推断
                        SubprocessOutcome { exit_code: None, signal: None }
                    }
                }
                Err(_) => {
                    // wait 错误不应出现；归为运行异常而非 spawn 级
                    SubprocessOutcome { exit_code: None, signal: None }
                }
            }
        } else {
            SubprocessOutcome { exit_code: None, signal: None }
        };
        self.outcome = Some(outcome.clone());
        outcome
    }

    /// 收集的 stdout 全量（readFrom(0) 语义，非消费）。
    pub fn collected_stdout(&self) -> String {
        self.stdout_slot
            .as_ref()
            .map(|s| s.lock().expect("stdout lock").read_from(0))
            .unwrap_or_default()
    }

    /// 收集的 stderr 全量。
    pub fn collected_stderr(&self) -> String {
        self.stderr_slot
            .as_ref()
            .map(|s| s.lock().expect("stderr lock").read_from(0))
            .unwrap_or_default()
    }

    /// 终止（树级；本次增量先落 Windows taskkill + unix killpg 骨架，后续细化）。
    pub fn terminate(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let pid = child.id();
            let _ = kill_tree(pid, self.grace_ms);
            let _ = child.wait();
            self.child = None;
        }
    }
}

/// 平台树级终止：Windows taskkill /T /F；unix killpg（nix）。
fn kill_tree(pid: u32, _grace_ms: u64) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .status();
        Ok(())
    }
    #[cfg(not(windows))]
    {
        // 进程组信号（SIGTERM→grace→SIGKILL）骨架：先用 SIGTERM
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid as i32),
            nix::sys::signal::Signal::SIGTERM,
        );
        Ok(())
    }
}

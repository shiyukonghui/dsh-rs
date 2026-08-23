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
                                let _ = spill_file.as_mut().and_then(|w| w.write_all(&tail).ok());
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
    CollectResult {
        data: tail,
        spill_path,
    }
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
/// 返回 (结果槽, join 句柄)——wait 时 join 以便「流排干后再 settle」（镜像参考
/// done 在 stdio 关闭后 resolve 的语义）。
fn start_collector(
    reader: impl Read + Send + 'static,
    cfg: &SubprocessCollect,
) -> (Arc<Mutex<CollectedOutput>>, thread::JoinHandle<()>) {
    let max_bytes = cfg.max_bytes;
    let spill = cfg.spill.as_ref().map(|s| (s.max_bytes, s.dir.clone()));
    let out = Arc::new(Mutex::new(CollectedOutput::from_bytes(
        Vec::new(),
        false,
        None,
    )));
    let out2 = out.clone();
    let join = thread::spawn(move || {
        let res = drain_pipe(reader, max_bytes, spill);
        // lossy：原字节非合法 UTF-8 → 标记（供诊断；前端 lossy-render 据此提示）
        let lossy = std::str::from_utf8(&res.data).is_err();
        let mut slot = out2.lock().expect("collector lock");
        *slot = CollectedOutput::from_bytes(res.data, lossy, res.spill_path);
    });
    (out, join)
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

    let mut child = cmd
        .spawn()
        .map_err(|e| ProcessError::Spawn(e.to_string()))?;
    let pid = child.id();

    // 收集线程 join 句柄（settle 时 join：流排干后再缓存 outcome）
    let mut pending_joins: Vec<thread::JoinHandle<()>> = Vec::new();

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
            let (slot, join) = start_collector(rd, cfg);
            pending_joins.push(join);
            Some(slot)
        }
        _ => None,
    };
    let stderr_slot = match &spec.stdio.stderr {
        StdoutMode::Collect(cfg) => {
            let rd = child.stderr.take().expect("collect stderr piped");
            let (slot, join) = start_collector(rd, cfg);
            pending_joins.push(join);
            Some(slot)
        }
        _ => None,
    };

    Ok(SubprocessHandle {
        child: Some(child),
        pid,
        stdout_slot,
        stderr_slot,
        joins: pending_joins,
        outcome: None,
        grace_ms: spec.grace_ms,
        #[cfg(windows)]
        job: {
            // spawn 后立即赋入 job（其后代自动继承成员资格）；失败静默降级。
            let job = crate::win_job::Job::new();
            if let Some(j) = &job {
                let _ = j.add_pid(pid);
            }
            job
        },
    })
}

/// 参考 `SubprocessHandle`（简化为 spawn 原语层面所需的最小面）。
pub struct SubprocessHandle {
    child: Option<Child>,
    pub pid: u32,
    stdout_slot: Option<Arc<Mutex<CollectedOutput>>>,
    stderr_slot: Option<Arc<Mutex<CollectedOutput>>>,
    joins: Vec<thread::JoinHandle<()>>,
    outcome: Option<SubprocessOutcome>,
    grace_ms: u64,
    #[cfg(windows)]
    job: Option<crate::win_job::Job>,
}

impl SubprocessHandle {
    /// 等待运行结束；settle 一次（首次后缓存），从不 reject。
    /// 返回前 join 收集线程：进程退出后管道关闭 → 收集器到 EOF → 结果落槽，
    /// 保证「流排干后再读」不丢尾（镜像参考 done 在 stdio 关闭后 resolve）。
    pub fn wait(&mut self) -> SubprocessOutcome {
        if let Some(o) = &self.outcome {
            return o.clone();
        }
        let outcome = self.reap_child();
        self.finish_settle(outcome)
    }

    /// 带超时的等待：在 `timeout` 内进程退出则返回 outcome，否则返回 None
    /// （不 kill；调用方自行 terminate）。单线程核心的同步超时手段（try_wait 轮询）。
    pub fn wait_timeout(&mut self, timeout: std::time::Duration) -> Option<SubprocessOutcome> {
        if let Some(o) = &self.outcome {
            return Some(o.clone());
        }
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match self.try_reap_child() {
                Some(outcome) => return Some(self.finish_settle(outcome)),
                None => {
                    if std::time::Instant::now() >= deadline {
                        return None;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        }
    }

    /// 子进程 wait + 收集线程 join，缓存 outcome。
    fn finish_settle(&mut self, outcome: SubprocessOutcome) -> SubprocessOutcome {
        for join in self.joins.drain(..) {
            let _ = join.join();
        }
        self.outcome = Some(outcome.clone());
        outcome
    }

    /// 阻塞 wait 子进程并映射 outcome。
    fn reap_child(&mut self) -> SubprocessOutcome {
        if let Some(child) = self.child.as_mut() {
            match child.wait() {
                Ok(st) => map_status(st),
                Err(_) => SubprocessOutcome {
                    exit_code: None,
                    signal: None,
                },
            }
        } else {
            SubprocessOutcome {
                exit_code: None,
                signal: None,
            }
        }
    }

    /// 非阻塞 try_wait；未退出 → None。
    fn try_reap_child(&mut self) -> Option<SubprocessOutcome> {
        self.child
            .as_mut()
            .and_then(|c| c.try_wait().ok().flatten())
            .map(map_status)
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

    /// 增量读取：从 `offset` 起的 stdout 文本（非消费快照）。
    pub fn read_stdout(&self, offset: usize) -> String {
        self.stdout_slot
            .as_ref()
            .map(|s| s.lock().expect("stdout lock").read_from(offset))
            .unwrap_or_default()
    }

    /// 增量读取：从 `offset` 起的 stderr 文本。
    pub fn read_stderr(&self, offset: usize) -> String {
        self.stderr_slot
            .as_ref()
            .map(|s| s.lock().expect("stderr lock").read_from(offset))
            .unwrap_or_default()
    }

    /// 当前 stdout 缓冲的字节长度（增量游标下界）。
    pub fn stdout_len(&self) -> usize {
        self.stdout_slot
            .as_ref()
            .map(|s| s.lock().expect("stdout lock").data_len())
            .unwrap_or(0)
    }

    /// 当前 stderr 缓冲的字节长度。
    pub fn stderr_len(&self) -> usize {
        self.stderr_slot
            .as_ref()
            .map(|s| s.lock().expect("stderr lock").data_len())
            .unwrap_or(0)
    }

    /// stdout 是否损失型（原字节非合法 UTF-8）。
    pub fn stdout_lossy(&self) -> bool {
        self.stdout_slot
            .as_ref()
            .map(|s| s.lock().expect("stdout lock").lossy())
            .unwrap_or(false)
    }

    /// stderr 是否损失型。
    pub fn stderr_lossy(&self) -> bool {
        self.stderr_slot
            .as_ref()
            .map(|s| s.lock().expect("stderr lock").lossy())
            .unwrap_or(false)
    }

    /// stdout 完整流 spill 文件（若溢出落盘）。
    pub fn stdout_spill_path(&self) -> Option<std::path::PathBuf> {
        self.stdout_slot.as_ref().and_then(|s| {
            s.lock()
                .expect("stdout lock")
                .spill_path()
                .map(|p| p.to_path_buf())
        })
    }

    /// stderr 完整流 spill 文件（若溢出落盘）。
    pub fn stderr_spill_path(&self) -> Option<std::path::PathBuf> {
        self.stderr_slot.as_ref().and_then(|s| {
            s.lock()
                .expect("stderr lock")
                .spill_path()
                .map(|p| p.to_path_buf())
        })
    }

    /// 终止（树级；本次增量先落 Windows taskkill + unix killpg 骨架，后续细化）。
    /// 终止后 settle 终态 outcome（wait 缓存），供调用方读取被 kill 后的状态。
    pub fn terminate(&mut self) {
        if let Some(mut child) = self.child.take() {
            let pid = child.id();
            // 树级终止：优先平台原语（Windows Job Object / unix killpg），taskkill 兜底；
            // 受限环境两者被拒时仍用 std child.kill() 保证确定性终止。
            #[cfg(windows)]
            if let Some(job) = &self.job {
                job.kill();
            }
            let _ = kill_tree(pid, self.grace_ms);
            let _ = child.kill();
            let outcome = match child.wait() {
                Ok(st) => map_status(st),
                Err(_) => SubprocessOutcome {
                    exit_code: None,
                    signal: None,
                },
            };
            self.finish_settle(outcome);
        }
    }
}

/// 参考 `map_status`（`spawn.ts`）：把 `std::process::ExitStatus` 映成 outcome。
fn map_status(st: std::process::ExitStatus) -> SubprocessOutcome {
    if let Some(code) = st.code() {
        SubprocessOutcome {
            exit_code: Some(code),
            signal: None,
        }
    } else if st.success() {
        SubprocessOutcome {
            exit_code: Some(0),
            signal: None,
        }
    } else {
        // 信号终止（code() == None）：按平台惯例推断
        SubprocessOutcome {
            exit_code: None,
            signal: None,
        }
    }
}

/// 平台树级终止：Windows taskkill /T /F；unix killpg（nix）。
fn kill_tree(pid: u32, _grace_ms: u64) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
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

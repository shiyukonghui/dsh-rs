//! `dsh-jobs` 注册表 —— 对齐 `packages/jobs/jobs/src/index.ts` 的纯内存单线程语义。
//!
//! 状态机：`running` →（可选 `stopping`）→ 恰一终态（completed|killed|failed）。
//! - id：`<kind>-N`（每 kind 独立计数器）。
//! - 结算 first-wins：终态先到者保持不变，后到被忽略。
//! - 授权围栏：owner.sessionId 指定时他人 get/kill/read 拒；无主 job 任何 caller 可见。
//! - 活跃上限 `max_concurrent_per_owner`。
//! - `reported`：kill/read/teardown 承诺报告后置 true，抑制重复完成通知。
//!
//! producer 是同步 `run() -> Hooks`（单线程）；结算由宿主在 settle 时机调用 `settle`。

use std::collections::HashMap;

/// 任务生命周期状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum JobStatus {
    #[default]
    Running,
    Stopping,
    Completed,
    Killed,
    Failed,
}

impl JobStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            JobStatus::Running => "running",
            JobStatus::Stopping => "stopping",
            JobStatus::Completed => "completed",
            JobStatus::Killed => "killed",
            JobStatus::Failed => "failed",
        }
    }
    fn is_terminal(&self) -> bool {
        matches!(self, JobStatus::Completed | JobStatus::Killed | JobStatus::Failed)
    }
}

/// producer 提供的完成结果。
#[derive(Debug, Clone, Default)]
pub struct JobSettlement {
    pub status: JobStatus,
    pub detail: Option<String>,
    pub output: Option<String>,
}

/// producer 同步 start 的 hooks（单线程模型）。
pub struct ProducerHooks {
    /// 请求终止（同步、幂等）；reason 逐字转发。
    pub on_cancel: Box<dyn Fn(&str)>,
    /// 消费自上次读取以来的输出增量（stream）。
    pub read_output: Option<Box<dyn FnMut() -> String>>,
}

/// 注册表配置。
pub struct JobRegistryConfig {
    pub max_concurrent_per_owner: usize,
    pub now: Box<dyn Fn() -> i64>,
}

impl std::fmt::Debug for JobRegistryConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobRegistryConfig")
            .field("max_concurrent_per_owner", &self.max_concurrent_per_owner)
            .finish()
    }
}

impl Default for JobRegistryConfig {
    fn default() -> Self {
        JobRegistryConfig { max_concurrent_per_owner: 10, now: Box::new(now_millis) }
    }
}

/// start 规格。
pub struct StartSpec<'a> {
    pub kind: &'a str,
    pub label: &'a str,
    /// owner session id（无主 job 则 None = 任何 caller 可见）。
    pub owner: Option<String>,
    /// 启动工作并同步返回 hooks。
    pub producer: Box<dyn FnMut() -> ProducerHooks + 'a>,
}

/// start 错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobStartError {
    EmptyKind,
    EmptyLabel,
    OwnerQuota,
    /// producer `run()` 抛错——什么都不登记、id 不消费（对齐 TS「a throwing starter
    /// leaves nothing registered」）。携带 panic 文本用于诊断。
    ProducerPanic(String),
}

/// 单条 job 记录。
struct JobRecord {
    kind: String,
    label: String,
    owner: Option<String>,
    status: JobStatus,
    detail: Option<String>,
    started_at: i64,
    finished_at: Option<i64>,
    reported: bool,
    output_buf: Vec<String>,
    on_cancel: Box<dyn Fn(&str)>,
}

/// 只读投影。
#[derive(Debug, Clone)]
pub struct JobSnapshot {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub owner: Option<String>,
    pub status: JobStatus,
    pub detail: Option<String>,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub reported: bool,
}

/// 操作错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobOpsError {
    UnknownJob,
    AlreadyFinished,
    Foreign,
}

/// kill 结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillOutcome {
    Requested,
    AlreadyFinished,
}

/// 任务注册表（单线程，Rc 可跨宿主共享成员读取）。
pub struct JobRegistry {
    jobs: HashMap<String, JobRecord>,
    counters: HashMap<String, u64>,
    config: JobRegistryConfig,
}

impl JobRegistry {
    pub fn new(config: JobRegistryConfig) -> Self {
        JobRegistry { jobs: HashMap::new(), counters: HashMap::new(), config }
    }

    fn next_id(&mut self, kind: &str) -> String {
        let n = self.counters.entry(kind.to_string()).or_insert(0);
        *n += 1;
        format!("{kind}-{n}")
    }

    /// start：preflight（kind/label 非空、owner 活跃上限）→ producer() → 登记。
    /// producer `run()` 抛错（Rust panic）→ 回滚：不登记、id 计数器退回（对齐 TS
    /// 「a throwing starter leaves nothing registered」）。
    pub fn start(&mut self, spec: StartSpec<'_>) -> Result<String, JobStartError> {
        if spec.kind.is_empty() {
            return Err(JobStartError::EmptyKind);
        }
        if spec.label.is_empty() {
            return Err(JobStartError::EmptyLabel);
        }
        if let Some(owner) = &spec.owner {
            let active = self
                .jobs
                .values()
                .filter(|j| j.owner.as_ref() == Some(owner) && !j.status.is_terminal())
                .count();
            if active >= self.config.max_concurrent_per_owner {
                return Err(JobStartError::OwnerQuota);
            }
        }
        // producer 先跑再分配 id：抛错时 id 从未发出（不消费计数器 → 下次仍 kind-N
        // 同号），也无记录残留。
        let producer = spec.producer;
        let hooks = std::panic::catch_unwind(std::panic::AssertUnwindSafe(producer))
            .map_err(|payload| JobStartError::ProducerPanic(panic_message(&payload)))?;
        let id = self.next_id(spec.kind);
        let now = (self.config.now)();
        self.jobs.insert(
            id.clone(),
            JobRecord {
                kind: spec.kind.to_string(),
                label: spec.label.to_string(),
                owner: spec.owner,
                status: JobStatus::Running,
                detail: None,
                started_at: now,
                finished_at: None,
                reported: false,
                output_buf: Vec::new(),
                on_cancel: hooks.on_cancel,
            },
        );
        // 若 producer 提供 read_output，则把流合并进增量；本单线程模型由宿主在 settle
        // 时把终态 output 交给 settle；read_output 作为 stream 增量由宿主喝入。
        if let Some(mut read) = hooks.read_output {
            if let Some(rec) = self.jobs.get_mut(&id) {
                let delta = read();
                if !delta.is_empty() {
                    rec.output_buf.push(delta);
                }
            }
        }
        Ok(id)
    }

    /// 结算（first-wins）：终态已定则忽略后到。
    pub fn settle(&mut self, id: &str, settlement: JobSettlement) {
        if let Some(rec) = self.jobs.get_mut(id) {
            if rec.status.is_terminal() {
                return; // first-wins
            }
            rec.status = settlement.status;
            rec.detail = settlement.detail;
            if let Some(output) = settlement.output {
                if !output.is_empty() {
                    rec.output_buf.push(output);
                }
            }
            rec.finished_at = Some((self.config.now)());
        }
    }

    fn authorize(&self, id: &str, caller: Option<&str>) -> Result<(), JobOpsError> {
        let rec = self.jobs.get(id).ok_or(JobOpsError::UnknownJob)?;
        if let Some(owner) = &rec.owner {
            if caller != Some(owner.as_str()) {
                return Err(JobOpsError::Foreign);
            }
        }
        Ok(())
    }

    /// get：只读投影。
    pub fn get(&self, id: &str, caller: Option<&str>) -> Result<JobSnapshot, JobOpsError> {
        self.authorize(id, caller)?;
        Ok(self.snapshot_of(id))
    }

    fn snapshot_of(&self, id: &str) -> JobSnapshot {
        let rec = self.jobs.get(id).expect("authorized id");
        JobSnapshot {
            id: id.to_string(),
            kind: rec.kind.clone(),
            label: rec.label.clone(),
            owner: rec.owner.clone(),
            status: rec.status,
            detail: rec.detail.clone(),
            started_at: rec.started_at,
            finished_at: rec.finished_at,
            reported: rec.reported,
        }
    }

    /// kill：running → 请求 producer cancel + 置 stopping；已终态 → already-finished。
    pub fn kill(&mut self, id: &str, caller: Option<&str>, reason: Option<&str>) -> Result<KillOutcome, JobOpsError> {
        self.authorize(id, caller)?;
        let status = self.jobs.get(id).map(|j| j.status).ok_or(JobOpsError::UnknownJob)?;
        if status.is_terminal() {
            return Ok(KillOutcome::AlreadyFinished);
        }
        if status == JobStatus::Running {
            let rec = self.jobs.get_mut(id).expect("exists");
            rec.status = JobStatus::Stopping;
            (rec.on_cancel)(reason.unwrap_or(""));
        }
        Ok(KillOutcome::Requested)
    }

    /// read：final-output job 未结算 text 空、结算后终态 output 幂等（从不消费）；
    /// stream delta 由宿主滚动压入。承诺 reported。
    pub fn read(&mut self, id: &str, caller: Option<&str>) -> Result<JobRead, JobOpsError> {
        self.authorize(id, caller)?;
        let rec = self.jobs.get_mut(id).expect("authorized");
        // 幂等：返回全部缓冲（不消费），final-output 语义与 stream 消费由宿主区分。
        let text = rec.output_buf.join("\n");
        rec.reported = true;
        let snapshot = self.snapshot_of(id);
        Ok(JobRead { text, snapshot })
    }

    /// wait：等 job 到达终态。单线程显式 settle 模型下实现为即时检查（无后台线程、
    /// 无阻塞——诚实降级，见 D-004）：
    /// - 已终态（completed|killed|failed）→ 返回该 snapshot 且置 `reported`（对齐 TS
    ///   `wait`：报告终态即抑制重复完成通知）；
    /// - 仍 running/stopping → 返回当前 snapshot，`reported` 不动（TS `wait` 在等待
    ///   timeout 时同样返回 snapshot，而非抛错——这里即「瞬时 timeout」语义）。
    /// - 未知/越权 → JobOpsError（与 get/read 同围栏）。
    pub fn wait(&mut self, id: &str, caller: Option<&str>) -> Result<JobSnapshot, JobOpsError> {
        self.authorize(id, caller)?;
        let rec = self.jobs.get_mut(id).expect("authorized");
        if rec.status.is_terminal() {
            rec.reported = true;
        }
        Ok(self.snapshot_of(id))
    }

    /// list：owner 只见自己的 + 无主；无 owner 参数 → 全部无主。
    pub fn list(&self, caller: Option<&str>) -> Vec<JobSnapshot> {
        let mut out: Vec<JobSnapshot> = self
            .jobs
            .iter()
            .filter(|(_, j)| match (&j.owner, caller) {
                (None, _) => true,
                (Some(o), Some(c)) => o == c,
                (Some(_), None) => false,
            })
            .map(|(id, _)| self.snapshot_of(id))
            .collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    /// JobView wire 形状（taskViewSchema）：{id,kind,label,status,detail?,startedAt,finishedAt?}。
    pub fn view(&self, id: &str, caller: Option<&str>) -> Result<serde_json::Value, JobOpsError> {
        self.authorize(id, caller)?;
        Ok(snapshot_to_view(&self.snapshot_of(id)))
    }
}

/// read 结果。
#[derive(Debug, Clone)]
pub struct JobRead {
    pub text: String,
    pub snapshot: JobSnapshot,
}

fn now_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// 提取 panic 载荷的人类可读文本（&str / String / 兜底 marker）。
fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "producer panicked during start".to_string()
    }
}

/// 投影单个 snapshot 到 wire JobView（taskViewSchema）：`{id,kind,label,status,
/// detail?,startedAt,finishedAt?}`。刻意丢弃 `owner/reported/outputLimitBytes`
/// （内部字段，对齐 `api/jobs.ts` JobView 三字段缺席说明）。
pub fn snapshot_to_view(snap: &JobSnapshot) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    obj.insert("id".into(), serde_json::Value::String(snap.id.clone()));
    obj.insert("kind".into(), serde_json::Value::String(snap.kind.clone()));
    obj.insert("label".into(), serde_json::Value::String(snap.label.clone()));
    obj.insert("status".into(), serde_json::Value::String(snap.status.as_str().to_string()));
    if let Some(d) = &snap.detail {
        obj.insert("detail".into(), serde_json::Value::String(d.clone()));
    }
    obj.insert("startedAt".into(), serde_json::Value::from(snap.started_at));
    if let Some(f) = snap.finished_at {
        obj.insert("finishedAt".into(), serde_json::Value::from(f));
    }
    serde_json::Value::Object(obj)
}

/// `session/jobs` 帧的 `jobs` 数组渲染（taskViewSchema[]）。纯构造函数：
/// 给定某 owner 可见的 snapshots 列表 → wire 数组；空集返回 `[]`（前端缺失键 ≡ 空集，
/// 无结束哨兵）。内部字段（owner/reported/outputLimitBytes）绝不上线。
pub fn jobs_frame(snapshots: &[JobSnapshot]) -> serde_json::Value {
    serde_json::Value::Array(snapshots.iter().map(snapshot_to_view).collect())
}

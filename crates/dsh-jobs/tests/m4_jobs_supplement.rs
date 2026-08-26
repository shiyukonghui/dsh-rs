//! M4h dsh-jobs 补齐测试（TDD 红-绿）。
//!
//! 覆盖验收 #6「list/read/kill/wait + 子代理 producer 真实跑 + session/jobs 帧」的
//! 本 crate 部分：
//! - `wait`：终态 → 返回该 snapshot + reported（对齐 TS wait 报告抑制）；running/stopping
//!   → 返回当前 snapshot（TS wait 在 timeout 也返回 snapshot，非抛错），诚实不阻塞。
//! - `jobs_frame`：把某 owner 可见的 snapshots 渲染成 wire JobView[]（taskViewSchema），
//!   绝无 owner/reported/outputLimitBytes 泄漏。
//! - producer `run()` 抛错 → start Err + 注册表无残留（id 不消费、无僵尸记录）。

use dsh_jobs::registry::{
    JobRegistry, JobRegistryConfig, JobSettlement, JobStartError, JobStatus, ProducerHooks,
    StartSpec,
};

fn registry() -> JobRegistry {
    JobRegistry::new(JobRegistryConfig { max_concurrent_per_owner: 10, now: Box::new(|| 1000) })
}

fn start_plain(r: &mut JobRegistry, kind: &'static str, label: &'static str) -> String {
    r.start(StartSpec { kind, label, owner: None, producer: fake_start() }).unwrap()
}

fn fake_start() -> Box<dyn FnMut() -> ProducerHooks + Send> {
    Box::new(move || ProducerHooks {
        on_cancel: Box::new(|_| {}),
        read_output: None,
    })
}

// ---------------------------------------------------------------------------
// wait
// ---------------------------------------------------------------------------

/// 终态 job wait → Ok + reported=true（TS：wait 报告终态，抑制重复完成通知）。
#[test]
fn wait_terminal_returns_snapshot_and_reports() {
    let mut r = registry();
    let id = start_plain(&mut r, "bash", "cmd a");
    r.settle(
        &id,
        JobSettlement { status: JobStatus::Completed, detail: Some("exit 0".into()), output: Some("done".into()) },
    );
    let snap = r.wait(&id, None).expect("terminal wait ok");
    assert_eq!(snap.status, JobStatus::Completed);
    assert_eq!(snap.detail.as_deref(), Some("exit 0"));
    assert!(snap.reported, "terminal wait marks reported");
    // 与 get 一致：reported 已持久（幂等，重复 wait 仍 reported）。
    assert!(r.get(&id, None).unwrap().reported);
    assert!(r.wait(&id, None).unwrap().reported);
}

/// running job wait → 返回当前 snapshot（TS wait 在未结算时也返回 snapshot），
/// 不阻塞、不伪装终态、不误标 reported。
#[test]
fn wait_running_returns_live_snapshot_not_blocked() {
    let mut r = registry();
    let id = start_plain(&mut r, "bash", "cmd a");
    let snap = r.wait(&id, None).expect("live wait ok (non-blocking)");
    assert_eq!(snap.status, JobStatus::Running);
    assert!(snap.finished_at.is_none());
    assert!(!snap.reported, "live wait must not claim a report");
}

/// stopping job wait → 返回 stopping snapshot。
#[test]
fn wait_stopping_returns_live_snapshot() {
    let mut r = registry();
    let id = start_plain(&mut r, "bash", "cmd a");
    r.kill(&id, None, Some("user cancelled")).unwrap();
    let snap = r.wait(&id, None).expect("stopping wait ok");
    assert_eq!(snap.status, JobStatus::Stopping);
    assert!(!snap.reported);
}

/// unknown / foreign job wait → 与 get 同 error 语义（Unauthorized 复用 authorize）。
#[test]
fn wait_auth_fence() {
    let mut r = registry();
    assert!(r.wait("nope", None).is_err());
    let o = "sess-owner";
    let id = r
        .start(StartSpec { kind: "bash", label: "x", owner: Some(o.to_string()), producer: fake_start() })
        .unwrap();
    assert!(r.wait(&id, Some("sess-other")).is_err());
    assert!(r.wait(&id, Some(o)).is_ok());
}

// ---------------------------------------------------------------------------
// jobs_frame（session/jobs 帧的 jobs 数组投影；taskViewSchema）
// ---------------------------------------------------------------------------

#[test]
fn jobs_frame_empty_is_array() {
    let v = dsh_jobs::jobs_frame(&[]);
    assert!(v.is_array());
    assert_eq!(v.as_array().unwrap().len(), 0);
    // 空快照没有结束哨兵——空数组即表示空集（README：absence ≡ []）。
    assert_eq!(v, serde_json::json!([]));
}

/// 两 job（一 completed 一 running）：wire 字段齐全、可选字段缺省省略、无内部泄漏。
#[test]
fn jobs_frame_wire_fields_no_leak() {
    let mut r = registry();
    let done = start_plain(&mut r, "subagent", "delegate A");
    r.settle(
        &done,
        JobSettlement { status: JobStatus::Completed, detail: Some("ok".into()), output: Some("out".into()) },
    );
    let running = start_plain(&mut r, "bash", "cmd b");

    let snap_done = r.get(&done, None).unwrap();
    let snap_running = r.get(&running, None).unwrap();
    let v = dsh_jobs::jobs_frame(&[snap_done, snap_running]);
    let arr = v.as_array().expect("array frame");
    assert_eq!(arr.len(), 2);

    // completed：全部字段，含 detail/finishedAt。
    let d = &arr[0];
    assert_eq!(d["id"], "subagent-1");
    assert_eq!(d["kind"], "subagent");
    assert_eq!(d["label"], "delegate A");
    assert_eq!(d["status"], "completed");
    assert_eq!(d["detail"], "ok");
    assert!(d.get("startedAt").is_some());
    assert!(d.get("finishedAt").is_some());
    // running：finishedAt/detail 缺省省略。
    let rn = &arr[1];
    assert_eq!(rn["id"], "bash-1");
    assert_eq!(rn["status"], "running");
    assert!(rn.get("finishedAt").is_none());
    assert!(rn.get("detail").is_none());
    // 内部字段绝不上线。
    for job in arr {
        assert!(job.get("owner").is_none(), "owner leaks");
        assert!(job.get("reported").is_none(), "reported leaks");
        assert!(job.get("ownerSession").is_none(), "ownerSession leaks");
        assert!(job.get("outputLimitBytes").is_none(), "outputLimitBytes leaks");
    }
}

// ---------------------------------------------------------------------------
// producer run() 抛错 → 回滚
// ---------------------------------------------------------------------------

/// producer run() panic → start Err + 注册表无残留 + id 不消费（下次仍 kind-1）。
#[test]
fn producer_panic_rolls_back_registration() {
    let mut r = registry();
    let err = r
        .start(StartSpec {
            kind: "bash",
            label: "doomed",
            owner: None,
            producer: Box::new(|| -> ProducerHooks { panic!("producer exploded") }),
        })
        .unwrap_err();
    assert!(matches!(err, JobStartError::ProducerPanic(_)));
    // 无僵尸记录。
    assert!(r.list(None).is_empty());
    // id 未被消费：下一次 start 从 kind-1 重新计数（对齐「leave nothing registered」）。
    let id = start_plain(&mut r, "bash", "replacement");
    assert_eq!(id, "bash-1");
}

/// producer panic 对 owner 配额也回滚（不占活跃名额）。
#[test]
fn producer_panic_does_not_consume_owner_quota() {
    let mut r = JobRegistry::new(JobRegistryConfig { max_concurrent_per_owner: 1, now: Box::new(|| 1000) });
    let o = Some("sess-owner".to_string());
    let e = r
        .start(StartSpec { kind: "bash", label: "doomed", owner: o.clone(), producer: Box::new(|| -> ProducerHooks { panic!("boom") }) })
        .unwrap_err();
    assert!(matches!(e, JobStartError::ProducerPanic(_)));
    // 配额尚未占用 → 同 owner 仍可 start。
    let id = r
        .start(StartSpec { kind: "bash", label: "ok", owner: o, producer: fake_start() })
        .unwrap();
    assert_eq!(id, "bash-1");
}

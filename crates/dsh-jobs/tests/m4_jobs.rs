//! M4e dsh-jobs 注册表测试（TDD 红-绿）。
//!
//! 对齐 `packages/jobs/jobs/src/index.ts` + `jobs-local`：id= `<kind>-N`、状态机
//! running→stopping→恰一终态、first-wins 结算、授权围栏（owner session）、活跃上限、
//! read 增量、kill requested/already-finished、reported 抑制、JobView wire。

use dsh_jobs::registry::{
    JobRegistry, JobRegistryConfig, StartSpec, JobStartError, JobOpsError, JobStatus, JobSettlement,
};
use std::cell::RefCell;
use std::rc::Rc;

fn registry() -> JobRegistry {
    JobRegistry::new(JobRegistryConfig { max_concurrent_per_owner: 10, now: Box::new(|| 1000) })
}

// 用递增时钟（首次 1000，之后每次 +1000）注册 registry —— 便于 startedAt≠finishedAt 断言。
fn registry_tick() -> JobRegistry {
    let tick = std::rc::Rc::new(std::cell::Cell::new(1000_i64));
    let tick2 = tick.clone();
    JobRegistry::new(JobRegistryConfig {
        max_concurrent_per_owner: 10,
        now: Box::new(move || {
            let t = tick2.get();
            tick2.set(t + 1000);
            t
        }),
    })
}

/// id 分配：`<kind>-N`（bash-1, bash-2, subagent-1）。
#[test]
fn id_allocation_kind_counter() {
    let mut r = registry();
    let b1 = r.start(StartSpec { kind: "bash", label: "cmd a", owner: None, producer: fake_start() }).unwrap();
    let b2 = r.start(StartSpec { kind: "bash", label: "cmd b", owner: None, producer: fake_start() }).unwrap();
    let s1 = r.start(StartSpec { kind: "subagent", label: "delegate", owner: None, producer: fake_start() }).unwrap();
    assert_eq!(b1, "bash-1");
    assert_eq!(b2, "bash-2");
    assert_eq!(s1, "subagent-1");
}

/// 空 kind / 空 label → start 拒。
#[test]
fn start_rejects_empty() {
    let mut r = registry();
    let e1 = r.start(StartSpec { kind: "", label: "x", owner: None, producer: fake_start() }).unwrap_err();
    assert!(matches!(e1, JobStartError::EmptyKind));
    let e2 = r.start(StartSpec { kind: "bash", label: "", owner: None, producer: fake_start() }).unwrap_err();
    assert!(matches!(e2, JobStartError::EmptyLabel));
}

/// 活跃上限：超过 max_concurrent_per_owner → 拒。
#[test]
fn per_owner_active_limit() {
    let mut r = JobRegistry::new(JobRegistryConfig { max_concurrent_per_owner: 2, now: Box::new(|| 1000) });
    let o = "sess-owner";
    assert!(r.start(start_owned("bash", "a", o)).is_ok());
    assert!(r.start(start_owned("bash", "b", o)).is_ok());
    let e = r.start(start_owned("bash", "c", o)).unwrap_err();
    assert!(matches!(e, JobStartError::OwnerQuota));
}

/// 生命周期：running → kill → stopping → settle(killed) → 终态。
#[test]
fn lifecycle_kill_to_terminal() {
    let mut r = registry();
    let id = r.start(StartSpec { kind: "bash", label: "x", owner: None, producer: fake_start() }).unwrap();
    assert_eq!(r.get(&id, None).unwrap().status, JobStatus::Running);
    let outcome = r.kill(&id, None, Some("user cancelled")).unwrap();
    assert_eq!(outcome, dsh_jobs::registry::KillOutcome::Requested);
    assert_eq!(r.get(&id, None).unwrap().status, JobStatus::Stopping);
    // 结算 first-wins：killed 后 producer 报 completed 被忽略（终态 first 为准）。
    r.settle(&id, JobSettlement { status: JobStatus::Killed, detail: Some("user cancelled".into()), output: None });
    assert_eq!(r.get(&id, None).unwrap().status, JobStatus::Killed);
    r.settle(&id, JobSettlement { status: JobStatus::Completed, detail: None, output: None });
    assert_eq!(r.get(&id, None).unwrap().status, JobStatus::Killed, "first-wins");
}

/// completed 结算：直接 running→completed（不经 stopping → 恰一终态）。
#[test]
fn settle_completed_directly() {
    let mut r = registry_tick();
    let id = r.start(StartSpec { kind: "bash", label: "x", owner: None, producer: fake_start() }).unwrap();
    r.settle(&id, JobSettlement { status: JobStatus::Completed, detail: None, output: Some("done".into()) });
    let snap = r.get(&id, None).unwrap();
    assert_eq!(snap.status, JobStatus::Completed);
    assert_eq!(snap.finished_at, Some(2000));
    assert_eq!(snap.started_at, 1000);
}

/// 授权围栏：owner 指定时，他人 caller get/kill → 拒；无主 job 任何 caller 可见。
#[test]
fn authorization_fence() {
    let mut r = registry();
    let o = "sess-owner";
    let id = r.start(start_owned("bash", "x", o)).unwrap();
    // 他人 get
    assert!(r.get(&id, Some("sess-other")).is_err());
    // owner get
    assert!(r.get(&id, Some(o)).is_ok());
    // 无主 get 对任何 caller 可用
    let uid = r.start(StartSpec { kind: "bash", label: "u", owner: None, producer: fake_start() }).unwrap();
    assert!(r.get(&uid, Some("sess-any")).is_ok());
}

/// read：final-output job 结算后返回终态 output（幂等）。
#[test]
fn read_final_output() {
    let mut r = registry();
    let id = r.start(StartSpec { kind: "bash", label: "x", owner: None, producer: fake_start() }).unwrap();
    // 未结算时 text 空
    let pre = r.read(&id, None).unwrap();
    assert_eq!(pre.text, "");
    r.settle(&id, JobSettlement { status: JobStatus::Completed, detail: None, output: Some("hello".into()) });
    let post = r.read(&id, None).unwrap();
    assert_eq!(post.text, "hello");
    // 幂等
    assert_eq!(r.read(&id, None).unwrap().text, "hello");
}

/// unknown id / foreign → JobOpsError。
#[test]
fn ops_error_codes() {
    let mut r = registry();
    let uid = "does-not-exist";
    assert!(matches!(r.get(uid, None).unwrap_err(), JobOpsError::UnknownJob));
    // kill 已结算的 job → already-finished
    let id = r.start(StartSpec { kind: "bash", label: "x", owner: None, producer: fake_start() }).unwrap();
    r.settle(&id, JobSettlement { status: JobStatus::Completed, detail: None, output: None });
    assert_eq!(r.kill(&id, None, None).unwrap(), dsh_jobs::registry::KillOutcome::AlreadyFinished);
}

/// reported 抑制：settle 后默认未上报；kill/read 承诺后 reported=true。
#[test]
fn reported_flag_suppression() {
    let mut r = registry();
    let id = r.start(StartSpec { kind: "bash", label: "x", owner: None, producer: fake_start() }).unwrap();
    r.settle(&id, JobSettlement { status: JobStatus::Completed, detail: None, output: Some("o".into()) });
    let snap = r.get(&id, None).unwrap();
    assert!(!snap.reported);
    // read 承诺报告
    r.read(&id, None).unwrap();
    assert!(r.get(&id, None).unwrap().reported);
}

/// JobView wire：只含 id/kind/label/status/detail?/startedAt/finishedAt?（无 owner/reported）。
#[test]
fn job_view_wire_shape() {
    let mut r = registry_tick();
    let id = r.start(StartSpec { kind: "subagent", label: "delegate", owner: None, producer: fake_start() }).unwrap();
    r.settle(&id, JobSettlement { status: JobStatus::Completed, detail: Some("ok".into()), output: None });
    let view = r.view(&id, None).unwrap();
    assert_eq!(view["id"], "subagent-1");
    assert_eq!(view["kind"], "subagent");
    assert_eq!(view["label"], "delegate");
    assert_eq!(view["status"], "completed");
    assert_eq!(view["detail"], "ok");
    assert_eq!(view["startedAt"], 1000);
    assert_eq!(view["finishedAt"], 2000);
    assert!(view.get("owner").is_none());
    assert!(view.get("reported").is_none());
}

/// list：owner 只见自己的 + 无主；他人不见 owner 的。
#[test]
fn list_authorization() {
    let mut r = registry();
    let o1 = "sess-1";
    let o2 = "sess-2";
    r.start(start_owned("bash", "x", o1)).unwrap();
    r.start(start_owned("bash", "y", o2)).unwrap();
    r.start(StartSpec { kind: "bash", label: "u", owner: None, producer: fake_start() }).unwrap();
    let l1 = r.list(Some(o1));
    let ids1: Vec<&str> = l1.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids1, vec!["bash-1", "bash-3"], "owner1 只见自己 + 无主");
    let l2 = r.list(Some(o2));
    let ids2: Vec<&str> = l2.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids2, vec!["bash-2", "bash-3"], "owner2 只见自己 + 无主");
}

fn start_owned(kind: &'static str, label: &'static str, owner: &'static str) -> StartSpec<'static> {
    StartSpec { kind, label, owner: Some(owner.to_string()), producer: fake_start() }
}

/// 构造内存 producer（cancel 记录 + no-op done）。
fn fake_start() -> Box<dyn FnMut() -> dsh_jobs::registry::ProducerHooks> {
    let reads = Rc::new(RefCell::new(Vec::<String>::new()));
    Box::new(move || {
        let reads = reads.clone();
        dsh_jobs::registry::ProducerHooks {
            on_cancel: Box::new(|_| {}),
            read_output: Some(Box::new(move || {
                let mut v = reads.borrow_mut();
                if v.is_empty() { String::new() } else { v.remove(0) }
            })),
        }
    })
}

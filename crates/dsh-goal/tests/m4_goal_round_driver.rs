//! M4b dsh-goal round-driver 驱动测试（TDD 红-绿）。
//!
//! 对齐 `packages/goal/goal-round-driver/src/index.ts`：当
//! `phase==active ∧ armed ∧ roundsStarted < maxGoalRounds ∧ agent idle ∧ 无竞争 inbox`
//! 时排队下一轮；超限自动 `block {code:"round-limit"}`；session-start/fork 的 disarmed
//! 由 service.disarm() 承接。通过 `GoalDriverPort` 抽象宿主，driver 不持有 agent-loop。

use dsh_goal::round_driver::{drive_once, round_driver_outcome, RoundOutcome, StatusPort};
use dsh_goal::service::{GoalService, ServiceOptions};
use dsh_goal::types::{GoalActivation, GoalId, GoalPhase};

/// 内存假宿主：status/idle/has_pending/followup 可编程。
struct FakePort {
    status: &'static str,
    pending: bool,
    followed: Vec<String>,
}

impl FakePort {
    fn new() -> Self {
        FakePort { status: "idle", pending: false, followed: Vec::new() }
    }
}

impl StatusPort for FakePort {
    fn status_idle(&self) -> bool {
        self.status == "idle"
    }
    fn has_pending_inbox(&self) -> bool {
        self.pending
    }
    fn followup(&mut self, _id: &GoalId, message: &str) -> Result<(), String> {
        self.followed.push(message.to_string());
        Ok(())
    }
}

fn svc() -> GoalService {
    GoalService::new(ServiceOptions::default())
}

/// active + armed + roundsStarted(cap-1) + idle + 空 inbox → 应续跑。
#[test]
fn drives_next_round_when_eligible() {
    let mut s = svc();
    let r = s.create("task", Some(2)).expect("create");
    // roundsStarted = 1（已准入 1 轮，cap 2 → 还能跑 1 轮）
    s.admit_round(&r.id, 1).expect("admit 1");
    let mut port = FakePort::new();
    let decision = round_driver_outcome(&s, &r.id, &port);
    assert_eq!(decision, Some(RoundOutcome::Continue));
    let out = drive_once(&mut s, &mut port, &r.id).expect("drive ok");
    assert!(matches!(out, RoundOutcome::Continue));
    assert_eq!(port.followed.len(), 1, "应发起一次 followup 续跑");
    assert!(port.followed[0].contains("Round"));
}

/// disarmed → 不续跑（session-start/fork 后）。
#[test]
fn disarmed_does_not_drive() {
    let mut s = svc();
    let r = s.create("task", Some(5)).expect("create");
    s.disarm();
    let mut port = FakePort::new();
    assert_eq!(round_driver_outcome(&s, &r.id, &port), None);
    assert_eq!(drive_once(&mut s, &mut port, &r.id), Ok(RoundOutcome::Noop));
    assert!(port.followed.is_empty());
}

/// idle 但 inbox 有竞争消息 → 不续跑（等下一轮）。
#[test]
fn pending_inbox_blocks_drive() {
    let mut s = svc();
    let r = s.create("task", Some(5)).expect("create");
    let mut port = FakePort::new();
    port.pending = true;
    assert_eq!(round_driver_outcome(&s, &r.id, &port), None);
}

/// running（非 idle）→ 不续跑。
#[test]
fn running_agent_blocks_drive() {
    let mut s = svc();
    let r = s.create("task", Some(5)).expect("create");
    let mut port = FakePort::new();
    port.status = "running";
    assert_eq!(round_driver_outcome(&s, &r.id, &port), None);
}

/// paused / complete / blocked → 不续跑。
#[test]
fn non_active_phase_blocks_drive() {
    let mut s = svc();
    let r1 = s.create("t", Some(5)).expect("create");
    let r2 = s.pause(&r1).expect("pause");
    let port = FakePort::new();
    assert_eq!(round_driver_outcome(&s, &r1.id, &port), None);
    // resume 后 armed + active → 续跑
    s.resume(&r2).expect("resume");
    assert_eq!(round_driver_outcome(&s, &r1.id, &port), Some(RoundOutcome::Continue));
}

/// roundsStarted >= maxGoalRounds → 不再续跑（已到 cap）。
#[test]
fn cap_reached_blocks_drive() {
    let mut s = svc();
    let r = s.create("t", Some(2)).expect("create");
    s.admit_round(&r.id, 2).expect("admit to cap");
    let port = FakePort::new();
    assert_eq!(round_driver_outcome(&s, &r.id, &port), None);
}

/// continue 时构造的轮次提示含目标与轮次进度（Round: N/M）。
#[test]
fn round_prompt_renders() {
    let mut s = svc();
    let r = s.create("ship", Some(3)).expect("create");
    s.admit_round(&r.id, 2).expect("admit 2");
    let mut port = FakePort::new();
    drive_once(&mut s, &mut port, &r.id).expect("drive");
    let text = &port.followed[0];
    assert!(text.contains("ship"));
    assert!(text.contains("Round: 3/3"), "下一轮是第 3 轮/共 3 轮: {text}");
}

/// resumed 之后 activation 保持 armed（不经 disarmed 窗口）。
#[test]
fn resume_keeps_armed() {
    let mut s = svc();
    let r1 = s.create("t", Some(5)).expect("create");
    s.disarm();
    let r2 = s.resume(&r1).expect("resume");
    assert_eq!(s.get(&r1.id).expect("get").activation, GoalActivation::Armed);
    // resume 后无其他阻碍 → 续跑
    let port = FakePort::new();
    assert_eq!(round_driver_outcome(&s, &r1.id, &port), Some(RoundOutcome::Continue));
    let _ = r2;
}

/// 简单 sanity：phase 枚举 wire 名不回归。
#[test]
fn phase_wire_names() {
    assert_eq!(GoalPhase::Active.as_str(), "active");
    assert_eq!(GoalPhase::Paused.as_str(), "paused");
    assert_eq!(GoalPhase::Blocked.as_str(), "blocked");
    assert_eq!(GoalPhase::Complete.as_str(), "complete");
}

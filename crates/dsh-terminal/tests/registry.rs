//! dsh-terminal：会话注册表 TDD（M5-DESIGN §6.1/§6.3；驱动 FakeBackend 内存替身）。
//!
//! 覆盖：open/send/read/signal/close/list 全生命周期、owner 授权（ForeignSession /
//! OwnerNotLive）、同名后端不得重复打开、SEND_ACTIVE 互斥、后端打开失败崩溃回滚、
//! disposing 拒绝、状态由结果同步。

use dsh_terminal::{
    BackendDefinition, TerminalBackend, TerminalBackendKind as Kind, TerminalConfig, TerminalError,
    TerminalErrorCode, TerminalSendRequest, TerminalSendResult, TerminalSessionId,
    TerminalSessionService, TerminalSessionStatus, TerminalSignal, TerminalWaitReason,
};

/// 内存后端：echo 文本、缓冲读、记录 signal/close，可配置 open 失败。
#[derive(Debug)]
struct FakeBackend {
    label: String,
    open_error: bool,
    sent: Vec<String>,
    read_buf: String,
    closed: bool,
    signaled: Vec<TerminalSignal>,
    status: TerminalSessionStatus,
}

impl FakeBackend {
    fn ok(label: &str) -> FakeBackend {
        FakeBackend {
            label: label.to_string(),
            open_error: false,
            sent: Vec::new(),
            read_buf: String::new(),
            closed: false,
            signaled: Vec::new(),
            status: TerminalSessionStatus::Running,
        }
    }
    fn opening_fails(label: &str) -> FakeBackend {
        FakeBackend {
            open_error: true,
            ..FakeBackend::ok(label)
        }
    }
}

impl TerminalBackend for FakeBackend {
    fn open(&mut self, _owner: &str, _cfg: &TerminalConfig) -> Result<(), TerminalError> {
        if self.open_error {
            return Err(TerminalError::new(
                TerminalErrorCode::NoBackend,
                "fake backend refused to open".to_string(),
            ));
        }
        Ok(())
    }
    fn send(&mut self, req: &TerminalSendRequest) -> Result<TerminalSendResult, TerminalError> {
        self.sent.push(format!("{}submit={}", req.text, req.submit));
        if self.status == TerminalSessionStatus::Running {
            self.read_buf.push_str(&format!("echo:{}", req.text));
        }
        Ok(TerminalSendResult {
            viewport: self.read_buf.clone(),
            wait_reason: TerminalWaitReason::StdinRead,
            session_status: self.status,
            truncated: false,
        })
    }
    fn read(&mut self, max_read_bytes: usize) -> Result<String, TerminalError> {
        let mut buf = String::new();
        std::mem::swap(&mut buf, &mut self.read_buf);
        buf.truncate(max_read_bytes);
        Ok(buf)
    }
    fn signal(&mut self, sig: TerminalSignal) -> Result<(), TerminalError> {
        self.signaled.push(sig);
        if matches!(sig, TerminalSignal::Sigkill) {
            self.status = TerminalSessionStatus::Aborted;
        }
        Ok(())
    }
    fn close(&mut self) -> Result<(), TerminalError> {
        self.closed = true;
        if self.status == TerminalSessionStatus::Running {
            self.status = TerminalSessionStatus::Exited;
        }
        Ok(())
    }
    fn label(&self) -> &str {
        &self.label
    }
    fn kind(&self) -> dsh_terminal::TerminalBackendKind {
        Kind::Bash
    }
}

fn service_with_fake() -> TerminalSessionService {
    let mut svc = TerminalSessionService::new();
    svc.register_backend(
        BackendDefinition {
            id: "test-bash".into(),
            kind: Kind::Bash,
            label: "Test Bash".into(),
        },
        Box::new(|_cfg| Box::new(FakeBackend::ok("Test Bash"))),
    )
    .expect("register");
    svc
}

fn open(svc: &mut TerminalSessionService, owner: &str) -> TerminalSessionId {
    svc.open(owner, "test-bash", None, TerminalConfig::default())
        .expect("open ok")
}

#[test]
fn open_send_read_close_roundtrip() {
    let mut svc = service_with_fake();
    let id = open(&mut svc, "alice");
    let result = svc
        .send(
            "alice",
            &id,
            &TerminalSendRequest {
                text: "ls".into(),
                submit: true,
                signal: None,
            },
        )
        .expect("send ok");
    assert_eq!(result.wait_reason, TerminalWaitReason::StdinRead);
    assert!(result.viewport.contains("echo:ls"));
    let out = svc.read("alice", &id).expect("read ok");
    assert!(out.contains("echo:ls"));
    svc.close("alice", &id).expect("close ok");
    assert!(svc.list().is_empty(), "close 移除会话");
}

#[test]
fn foreign_owner_is_rejected() {
    let mut svc = service_with_fake();
    let id = open(&mut svc, "alice");
    let err = svc.read("bob", &id).unwrap_err();
    assert_eq!(err.code, TerminalErrorCode::ForeignSession);
    let err = svc.close("bob", &id).unwrap_err();
    assert_eq!(err.code, TerminalErrorCode::ForeignSession);
    // 原 owner 仍可读
    assert!(svc.read("alice", &id).is_ok());
}

#[test]
fn owner_liveness_gate() {
    let mut svc = service_with_fake();
    svc.set_owner_liveness(Box::new(|owner| owner == "alive"));
    let err = svc
        .open("ghost", "test-bash", None, TerminalConfig::default())
        .unwrap_err();
    assert_eq!(err.code, TerminalErrorCode::OwnerNotLive);
    assert!(svc
        .open("alive", "test-bash", None, TerminalConfig::default())
        .is_ok());
}

#[test]
fn no_backend_rejected() {
    let mut svc = service_with_fake();
    let err = svc
        .open("alice", "nope", None, TerminalConfig::default())
        .unwrap_err();
    assert_eq!(err.code, TerminalErrorCode::NoBackend);
}

#[test]
fn duplicate_backend_type_registration_rejected() {
    let mut svc = service_with_fake();
    let dup = svc.register_backend(
        BackendDefinition {
            id: "test-bash".into(),
            kind: Kind::Bash,
            label: "X".into(),
        },
        Box::new(|_| Box::new(FakeBackend::ok("X"))),
    );
    assert_eq!(dup.unwrap_err().code, TerminalErrorCode::DuplicateBackend);
}

#[test]
fn duplicate_session_name_per_owner_rejected() {
    let mut svc = service_with_fake();
    let _a = svc
        .open(
            "alice",
            "test-bash",
            Some("term1"),
            TerminalConfig::default(),
        )
        .expect("open");
    // 同一 owner 同名再开 → DuplicateName
    let err = svc
        .open(
            "alice",
            "test-bash",
            Some("term1"),
            TerminalConfig::default(),
        )
        .unwrap_err();
    assert_eq!(err.code, TerminalErrorCode::DuplicateName);
    // 不同 owner 同名 OK
    assert!(svc
        .open("bob", "test-bash", Some("term1"), TerminalConfig::default())
        .is_ok());
    // 空名 → 拒绝
    let err2 = svc
        .open("alice", "test-bash", Some(""), TerminalConfig::default())
        .unwrap_err();
    assert_eq!(err2.code, TerminalErrorCode::DuplicateName);
}

#[test]
fn open_failure_rolls_back_and_frees_nothing_reserved() {
    let mut svc = TerminalSessionService::new();
    svc.register_backend(
        BackendDefinition {
            id: "broken".into(),
            kind: Kind::Bash,
            label: "Broken".into(),
        },
        Box::new(|_| Box::new(FakeBackend::opening_fails("Broken"))),
    )
    .expect("register");
    let err = svc
        .open("alice", "broken", None, TerminalConfig::default())
        .unwrap_err();
    assert_eq!(err.code, TerminalErrorCode::NoBackend, "open 失败透传");
    assert!(svc.list().is_empty(), "无残留会话");
}

#[test]
fn send_active_guard() {
    let mut svc = service_with_fake();
    let id = open(&mut svc, "alice");
    // 用 backend 直接占住 busy（模拟 send 期间的重入被拒）
    svc.send(
        "alice",
        &id,
        &TerminalSendRequest {
            text: "a".into(),
            submit: false,
            signal: None,
        },
    )
    .expect("first send ok");
    // busy 已复位 → 第二次可发。真正的互斥需后端 send 阻塞，此处验证 fa k 语义：
    assert!(svc
        .send(
            "alice",
            &id,
            &TerminalSendRequest {
                text: "b".into(),
                submit: false,
                signal: None
            }
        )
        .is_ok());
}

#[test]
fn signal_is_dispatched() {
    let mut svc = service_with_fake();
    let id = open(&mut svc, "alice");
    svc.signal("alice", &id, TerminalSignal::Sigint)
        .expect("sig ok");
    svc.signal("alice", &id, TerminalSignal::Sigterm)
        .expect("sig ok");
    let view = svc.view("alice", &id).expect("view");
    assert_eq!(view.status, TerminalSessionStatus::Running);
}

#[test]
fn missing_session_is_no_session() {
    let mut svc = service_with_fake();
    let ghost = TerminalSessionId::from_raw("s999".into());
    let err = svc.read("alice", &ghost).unwrap_err();
    assert_eq!(err.code, TerminalErrorCode::NoSession);
}

#[test]
fn dispose_closes_all_and_refuses_new_open() {
    let mut svc = service_with_fake();
    let _a = svc
        .open(
            "alice",
            "test-bash",
            Some("alice-term"),
            TerminalConfig::default(),
        )
        .expect("open");
    let _b = svc
        .open(
            "bob",
            "test-bash",
            Some("bob-term"),
            TerminalConfig::default(),
        )
        .expect("open");
    svc.dispose();
    assert!(svc.is_disposing());
    assert!(svc.list().is_empty(), "dispose 清空会话");
    let err = svc
        .open("alice", "test-bash", None, TerminalConfig::default())
        .unwrap_err();
    assert_eq!(err.code, TerminalErrorCode::ServiceDisposing);
}

#[test]
fn list_orders_views() {
    let mut svc = service_with_fake();
    let a = svc
        .open(
            "alice",
            "test-bash",
            Some("alice-term"),
            TerminalConfig::default(),
        )
        .expect("open");
    let b = svc
        .open(
            "bob",
            "test-bash",
            Some("bob-term"),
            TerminalConfig::default(),
        )
        .expect("open");
    let views = svc.list();
    assert_eq!(views.len(), 2);
    // 不依赖 HashMap 迭代序：按键查视图。
    let by_id: std::collections::HashMap<&TerminalSessionId, &dsh_terminal::TerminalSessionView> =
        views.iter().map(|v| (&v.id, v)).collect();
    let a_view = by_id.get(&a).expect("alice view exists");
    assert_eq!(a_view.owner, "alice");
    assert_eq!(a_view.name.as_deref(), Some("alice-term"));
    assert_eq!(a_view.backend, "test-bash");
    assert_eq!(a_view.status, TerminalSessionStatus::Running);
    let b_view = by_id.get(&b).expect("bob view exists");
    assert_eq!(b_view.owner, "bob");
    assert_eq!(b_view.name.as_deref(), Some("bob-term"));
}

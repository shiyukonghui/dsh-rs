//! dsh-fs：observation policy（M5-DESIGN §4.3）。
//!
//! 参考 `fs-observation-policy/src/index.ts`：按属主记录 observed 状态，派生
//! `fs/write-intent` / `fs/edit-intent` 决策：
//!
//! * writeIntent：seen-present → replaceIfVersion{saw}；否则 createIfAbsent。
//! * editIntent：unseen → FS_NOT_OBSERVED；seen-absent → FS_NOT_OBSERVED（不可编缺失）；
//!   seen-present → {version: saw}（CAS 基础）。
//! * recordObservation：present{version} | absent 按 owner+targetKey 落账。
//!
//! Rust 侧无 WeakMap/owner 对象 → 以 OwnerId(u64) 模拟弱引用语义（随 owner 释放清理）。

use dsh_fs::{
    policy::{Observation, ObservationGate},
    FsErrorCode, FsTarget, FsTargetKey, FsVersion, FsWriteIntent,
};

fn target(key: &str) -> FsTarget {
    FsTarget { target_key: FsTargetKey(key.to_string()), display_path: key.to_string() }
}

fn gate() -> ObservationGate {
    ObservationGate::new()
}

#[test]
fn write_intent_unseen_is_create_if_absent() {
    let g = gate();
    let intent = g.write_intent(1, &target("a.txt"));
    assert_eq!(intent, FsWriteIntent::CreateIfAbsent);
}

#[test]
fn write_intent_seen_present_is_replace_if_version() {
    let mut g = gate();
    let t = target("a.txt");
    g.record(1, &t, Observation::Present { version: FsVersion("v9".into()) });
    let intent = g.write_intent(1, &t);
    assert_eq!(
        intent,
        FsWriteIntent::ReplaceIfVersion { version: FsVersion("v9".into()) }
    );
}

#[test]
fn edit_intent_unseen_rejects_not_observed() {
    let g = gate();
    let err = g.edit_intent(1, &target("b.txt")).unwrap_err();
    assert_eq!(err.code(), FsErrorCode::FsNotObserved);
}

#[test]
fn edit_intent_seen_absent_rejects_not_observed() {
    let mut g = gate();
    let t = target("c.txt");
    g.record(1, &t, Observation::Absent);
    let err = g.edit_intent(1, &t).unwrap_err();
    assert_eq!(err.code(), FsErrorCode::FsNotObserved);
}

#[test]
fn edit_intent_seen_present_is_version_cas() {
    let mut g = gate();
    let t = target("d.txt");
    g.record(1, &t, Observation::Present { version: FsVersion("v3".into()) });
    let v = g.edit_intent(1, &t).expect("seen present");
    assert_eq!(v.version, FsVersion("v3".into()));
}

#[test]
fn observations_are_per_owner() {
    let mut g = gate();
    let t = target("e.txt");
    g.record(1, &t, Observation::Present { version: FsVersion("v1".into()) });
    // owner 2 未观察 → 仍 createIfAbsent
    let intent = g.write_intent(2, &t);
    assert_eq!(intent, FsWriteIntent::CreateIfAbsent);
    // owner 1 已观察 → replaceIfVersion
    let intent = g.write_intent(1, &t);
    assert_eq!(intent, FsWriteIntent::ReplaceIfVersion { version: FsVersion("v1".into()) });
}

#[test]
fn record_absent_after_present_allows_create() {
    let mut g = gate();
    let t = target("f.txt");
    g.record(1, &t, Observation::Present { version: FsVersion("v1".into()) });
    g.record(1, &t, Observation::Absent); // 外部删除 → 观察到 absent
    let intent = g.write_intent(1, &t);
    assert_eq!(intent, FsWriteIntent::CreateIfAbsent);
}

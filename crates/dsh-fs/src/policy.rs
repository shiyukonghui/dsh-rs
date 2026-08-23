//! dsh-fs observation policy（M5-DESIGN §4.3）。
//!
//! 参考 `fs-observation-policy/src/index.ts`（逐字语义）：per-owner observed 状态 →
//! 派生 write/edit intent 决策。Rust 侧无 WeakMap，以 `ObservedGate`（HashMap<owner,
//! HashMap<targetKey, Observation>>）+ OwnerId(u64) 模拟；owner 释放由宿主负责清理
//! （与 WeakMap 自动回收的语义差异记入注释，宿主在会话结束时调用 `drop_owner`）。

use crate::types::{FsError, FsErrorCode, FsTarget, FsVersion, FsWriteIntent};
use std::collections::HashMap;

pub type OwnerId = u64;

/// 参考 `FsObservation`：present{version} | absent。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation {
    Present { version: FsVersion },
    Absent,
}

/// 参考 `ObservedStateGate`：按 owner+targetKey 记忆观察；派生写/编决策。
#[derive(Debug, Default)]
pub struct ObservationGate {
    by_owner: HashMap<OwnerId, HashMap<String, Observation>>,
}

impl ObservationGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// 参考 `recordObservation`：记录该 owner 对目标的一次权威观察。
    pub fn record(&mut self, owner: OwnerId, target: &FsTarget, obs: Observation) {
        let by_target = self.by_owner.entry(owner).or_default();
        by_target.insert(target.target_key.0.clone(), obs);
    }

    /// 参考 `writeIntent`：优先语「已观察 present → replaceIfVersion{saw}；未观察或
    /// absent → createIfAbsent」。
    pub fn write_intent(&self, owner: OwnerId, target: &FsTarget) -> FsWriteIntent {
        match self.get(owner, target) {
            Some(Observation::Present { version }) => FsWriteIntent::ReplaceIfVersion {
                version: version.clone(),
            },
            _ => FsWriteIntent::CreateIfAbsent,
        }
    }

    /// 参考 `editIntent`：未观察或 absent → FS_NOT_OBSERVED（不可编不存在）；
    /// seen-present → {version: saw}（CAS 基础）。
    pub fn edit_intent(
        &self,
        owner: OwnerId,
        target: &FsTarget,
    ) -> Result<FsEditIntentVersion, FsError> {
        match self.get(owner, target) {
            Some(Observation::Present { version }) => {
                Ok(FsEditIntentVersion { version: version.clone() })
            }
            _ => Err(FsError::new(
                format!("edit requires reading \"{}\" first", target.display_path),
                FsErrorCode::FsNotObserved,
            )),
        }
    }

    /// owner 释放时清理（宿主调用；对应 WeakMap 的自动回收）。
    pub fn drop_owner(&mut self, owner: OwnerId) {
        self.by_owner.remove(&owner);
    }

    pub fn clear(&mut self) {
        self.by_owner.clear();
    }

    fn get(&self, owner: OwnerId, target: &FsTarget) -> Option<&Observation> {
        self.by_owner.get(&owner)?.get(&target.target_key.0)
    }
}

/// 参考 `editIntent` 返回的版本守卫。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsEditIntentVersion {
    pub version: FsVersion,
}

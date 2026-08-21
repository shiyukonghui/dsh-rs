//! 读模型投影：watermark readFrom + 状态折叠（M1d）。
//!
//! 权威参考：`deepseek-harness/packages/session/session-projection/`（registry/units +
//! watermark observedSeq + snapshot/checkpoint + restoreFloor）。
//! 本模块为纯内存折叠：单线程、无 IO、无数据库——只对 `dsh-session`/`dsh-persistence`
//! 的类型做纯函数折叠。持久化的水印恢复经 `SessionPersistence::read_from` 获取事件后缀。

use std::collections::HashMap;

use dsh_brand::SessionId;
use dsh_persistence::{PersistenceError, SessionPersistence};
use dsh_session::types::SessionEvent;
use serde_json::Value;

/// 单元初始态工厂（无参、返回状态）。
type ProjectionInit = Box<dyn Fn() -> Value>;
/// 单元折叠器（一条事件 → 可变状态）。
type ProjectionApply = Box<dyn Fn(&mut Value, &SessionEvent)>;
/// 单元投影器（状态 → 消费者值）。
type ProjectionView = Box<dyn Fn(&Value) -> Value>;

/// 一个投影单元：把一条事件序列折叠进一个状态，并把状态投影为消费者值。
///
/// `state_version` 是非负整数（Rust `u64` 结构性保证——类型本身排除负数与非整数）。
/// 细节语义由调用方闭包决定：
/// - `init` 产生每个会话的初始状态（从空开始折叠的种子）；
/// - `apply` 把一条事件折进可变状态（忽略不关心的事件即可）；
/// - `view` 把（内部）折叠状态投影为（对外）消费者值。
pub struct ProjectionUnit {
    key: String,
    state_version: u64,
    init: ProjectionInit,
    apply: ProjectionApply,
    view: ProjectionView,
}

impl ProjectionUnit {
    /// 构造一个投影单元（闭包需 `'static`——它们捕获纯数据，无外部借用）。
    pub fn new<Init, Apply, View>(
        key: impl Into<String>,
        state_version: u64,
        init: Init,
        apply: Apply,
        view: View,
    ) -> Self
    where
        Init: Fn() -> Value + 'static,
        Apply: Fn(&mut Value, &SessionEvent) + 'static,
        View: Fn(&Value) -> Value + 'static,
    {
        Self {
            key: key.into(),
            state_version,
            init: Box::new(init),
            apply: Box::new(apply),
            view: Box::new(view),
        }
    }

    /// 单元键。
    pub fn key(&self) -> &str {
        &self.key
    }

    /// 状态版本（非负整数）。
    pub fn state_version(&self) -> u64 {
        self.state_version
    }

    /// 拷贝初始状态（每会话折叠的种子）。
    fn initial_state(&self) -> Value {
        (self.init)()
    }

    /// 把一条事件折进状态（拷贝语义——折叠产生新状态快照）。
    fn fold(&self, state: &mut Value, event: &SessionEvent) {
        (self.apply)(state, event);
    }

    /// 把状态投影为消费者值。
    fn project(&self, state: &Value) -> Value {
        (self.view)(state)
    }
}

/// 投影单元注册表：按键存储单元；重复键冲突校验。
#[derive(Default)]
pub struct ProjectionRegistry {
    units: HashMap<String, ProjectionUnit>,
}

impl ProjectionRegistry {
    /// 空注册表。
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一个单元。
    ///
    /// - 重复键且 `state_version` 不同 → 报错（拒绝共享）；
    /// - 重复键且版本相同 → no-op 成功（幂等重新注册）；
    /// - 新键 → 成功后可用。
    ///
    /// `state_version` 的「非负整数」校验由 `u64` 类型结构性保证，无需运行时检查。
    pub fn register(&mut self, unit: ProjectionUnit) -> Result<(), String> {
        let key = unit.key().to_string();
        if let Some(existing) = self.units.get(&key) {
            if existing.state_version() != unit.state_version() {
                return Err(format!(
                    "session projection key {} is already registered at stateVersion {}; \
                     refusing to share it with stateVersion {}",
                    key,
                    existing.state_version(),
                    unit.state_version()
                ));
            }
            return Ok(());
        }
        self.units.insert(key, unit);
        Ok(())
    }

    /// 可忽略地迭代全部单元（键序不定；单元间折叠相互独立，顺序不影响语义）。
    pub fn units(&self) -> impl Iterator<Item = &ProjectionUnit> {
        self.units.values()
    }

    /// 按键取单元（调度/按需投影用）。
    pub fn get(&self, key: &str) -> Option<&ProjectionUnit> {
        self.units.get(key)
    }
}

/// 每个会话的读模型折叠状态：对注册表所有单元按事件折叠 + watermark（observedSeq）。
pub struct ProjectionSession<'a> {
    registry: &'a ProjectionRegistry,
    state: HashMap<String, Value>,
    observed_seq: u64,
    observed: bool,
}

impl<'a> ProjectionSession<'a> {
    /// 从注册表出发、以每个单元的 `init` 状态开始的一个空会话。
    pub fn new(registry: &'a ProjectionRegistry) -> Self {
        let mut state = HashMap::new();
        for unit in registry.units() {
            state.insert(unit.key().to_string(), unit.initial_state());
        }
        Self {
            registry,
            state,
            observed_seq: 0,
            observed: false,
        }
    }

    /// 折叠一条事件：对每个单元 `apply` 其状态；`observedSeq = event.seq`。
    pub fn observe(&mut self, event: &SessionEvent) {
        for unit in self.registry.units() {
            let state = self
                .state
                .get_mut(unit.key())
                .expect("every registered unit has a seeded state");
            unit.fold(state, event);
        }
        self.observed_seq = event.seq;
        self.observed = true;
    }

    /// 当前投影快照：`asOfSeq` = 最后观察到的 seq（从未观察 = 0）；值 = 投影后的消费者值。
    pub fn snapshot(&self) -> ProjectionSnapshot {
        let mut values = HashMap::new();
        for unit in self.registry.units() {
            let state = self
                .state
                .get(unit.key())
                .expect("every registered unit has a seeded state");
            values.insert(unit.key().to_string(), unit.project(state));
        }
        ProjectionSnapshot { as_of_seq: self.observed_seq, values }
    }

    /// 当前检查点行：键 → `{ver, seq, val}`；`seq` = observedSeq（从未观察 = -1，与 TS 一致）。
    pub fn checkpoint(&self) -> HashMap<String, ProjectionCheckpointRow> {
        let seq = if self.observed { self.observed_seq as i64 } else { -1 };
        let mut rows = HashMap::new();
        for unit in self.registry.units() {
            let state = self
                .state
                .get(unit.key())
                .expect("every registered unit has a seeded state");
            rows.insert(
                unit.key().to_string(),
                ProjectionCheckpointRow { ver: unit.state_version(), seq, val: state.clone() },
            );
        }
        rows
    }
}

/// 消费侧投影快照：watermark + 每个单元一个投影值。
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectionSnapshot {
    /// 快照覆盖到的最后观察 seq（从未观察 = 0）。
    pub as_of_seq: u64,
    /// 键 → 投影值（`view(state)`）。
    pub values: HashMap<String, Value>,
}

/// 单个单元的可持久化检查点行。
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectionCheckpointRow {
    /// 产生该行的 `state_version`。
    pub ver: u64,
    /// 该行对应的 observedSeq（从未观察 = -1，与 TS `seq: -1` 一致）。
    pub seq: i64,
    /// 折叠态（原始 state；`view` 在 `snapshot` 时应用，检查点保真可继续折叠的状态）。
    pub val: Value,
}

/// 惰性冷折叠：从持久化读 `[fromSeq, ∞)` 事件后缀并折叠进空状态。
///
/// - 严格只折叠 `read_from` 返回的后缀事件（不重复折叠——调用方管理 watermark）；
/// - 会话缺失时传播 `PersistenceError`（`SessionSuffix` 为空本身不是错误）。
pub fn projection_from_persistence(
    persistence: &dyn SessionPersistence,
    id: &SessionId,
    units: &ProjectionRegistry,
    from_seq: u64,
) -> Result<ProjectionSnapshot, PersistenceError> {
    let suffix = persistence.read_from(id, from_seq)?;
    let mut session = ProjectionSession::new(units);
    for event in &suffix.events {
        session.observe(event);
    }
    Ok(session.snapshot())
}

/// 冷恢复的 one-below-anchor 阶梯辅助（M1d 保留的只读小助手）：
///
/// - `floor: None` → `read_from(0)`（从头折叠）；
/// - `floor: Some(f)` → `read_from(f - 1)`（折 anchor 之下一格，补追 checkpoint 后落到日志
///   的新后缀；`f = 0` 时饱和到 `read_from(0)`）。
///
/// 注意：完整 TS `restoreFloor` 语义（含 shrink 检测：log 缩短到 floor-1 之下的收缩拒绝）
/// 在 M1d 超出范围——此处仅做一致的「一格之下」只读恢复。
pub fn projection_restore_from_floor(
    persistence: &dyn SessionPersistence,
    id: &SessionId,
    units: &ProjectionRegistry,
    floor: Option<u64>,
) -> Result<ProjectionSnapshot, PersistenceError> {
    let from = floor.map(|f| f.saturating_sub(1)).unwrap_or(0);
    projection_from_persistence(persistence, id, units, from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_persistence::{
        SessionInspection, SessionPersistenceSnapshot, SessionRawArtifact, SessionSuffix,
    };
    use dsh_session::types::{EventKind, SessionHeader, SurfaceOp};
    use std::cell::RefCell;

    // ---- 测试用单元（state == view，测试语义与 checkpoint 的 val 是否投影无关） ----

    /// `count`：对每条事件 +1（stateVersion 1）。
    fn count_unit() -> ProjectionUnit {
        ProjectionUnit::new(
            "count",
            1,
            || Value::from(0),
            |state, _event| {
                let n = state.as_u64().unwrap_or(0) + 1;
                *state = Value::from(n);
            },
            |state| state.clone(),
        )
    }

    /// `title`：在 `session/title` 事件上 last-wins 标题（stateVersion 1）。
    fn title_unit() -> ProjectionUnit {
        ProjectionUnit::new(
            "title",
            1,
            || Value::Null,
            |state, event| {
                if event.kind == EventKind::SessionTitle {
                    if let Some(title) = event.data.get("title").and_then(Value::as_str) {
                        *state = Value::String(title.to_string());
                    }
                }
            },
            |state| state.clone(),
        )
    }

    fn title_event(seq: u64, id: &str, title: &str) -> SessionEvent {
        SessionEvent::new(
            seq,
            1000 + seq as i64,
            EventKind::SessionTitle,
            serde_json::json!({ "id": id, "title": title }),
        )
    }

    fn plain_event(seq: u64, kind: EventKind) -> SessionEvent {
        SessionEvent::new(seq, 1000 + seq as i64, kind, serde_json::json!({ "turn": 1 }))
            .with_surface_op(SurfaceOp::Append)
    }

    // ---- 注册表 ----

    #[test]
    fn register_refuses_duplicate_key_with_different_version() {
        let mut reg = ProjectionRegistry::new();
        reg.register(count_unit()).expect("first register ok");
        let conflicting = ProjectionUnit::new(
            "count",
            2,
            || Value::Null,
            |_s, _e| {},
            |s| s.clone(),
        );
        let err = reg.register(conflicting).expect_err("version conflict must be refused");
        assert_eq!(
            err,
            "session projection key count is already registered at stateVersion 1; \
             refusing to share it with stateVersion 2"
        );
        // 原单元未被动摇
        assert_eq!(reg.get("count").expect("still there").state_version(), 1);
    }

    #[test]
    fn register_same_key_same_version_is_noop() {
        let mut reg = ProjectionRegistry::new();
        reg.register(count_unit()).expect("first register ok");
        reg.register(count_unit()).expect("same-version re-register is a no-op");
        assert_eq!(reg.units().count(), 1, "no second unit stored");
    }

    #[test]
    fn register_adds_distinct_keys() {
        let mut reg = ProjectionRegistry::new();
        reg.register(count_unit()).unwrap();
        reg.register(title_unit()).unwrap();
        assert_eq!(reg.units().count(), 2);
    }

    // ---- observe / snapshot / checkpoint ----

    #[test]
    fn unobserved_session_has_zero_as_of_and_negative_checkpoint_seq() {
        let mut reg = ProjectionRegistry::new();
        reg.register(count_unit()).unwrap();
        reg.register(title_unit()).unwrap();

        let session = ProjectionSession::new(&reg);
        let snap = session.snapshot();
        assert_eq!(snap.as_of_seq, 0, "nothing observed → asOfSeq 0");
        assert_eq!(snap.values["count"], Value::from(0), "init state projected");
        assert_eq!(snap.values["title"], Value::Null, "init state projected");

        let cp = session.checkpoint();
        assert_eq!(cp["count"].ver, 1);
        assert_eq!(cp["count"].seq, -1, "nothing observed → checkpoint seq -1");
        assert_eq!(cp["title"].seq, -1);
    }

    #[test]
    fn observe_folds_every_event_into_every_unit_and_tracks_watermark() {
        let mut reg = ProjectionRegistry::new();
        reg.register(count_unit()).unwrap();
        reg.register(title_unit()).unwrap();
        let mut session = ProjectionSession::new(&reg);

        session.observe(&title_event(0, "s1", "Alpha"));
        session.observe(&plain_event(1, EventKind::TurnStart));
        session.observe(&title_event(2, "s1", "Beta"));

        let snap = session.snapshot();
        assert_eq!(snap.as_of_seq, 2, "last observed seq");
        assert_eq!(snap.values["count"], Value::from(3), "3 events folded");
        assert_eq!(snap.values["title"], Value::from("Beta"), "last-wins title");

        let cp = session.checkpoint();
        assert_eq!(cp["count"].ver, 1);
        assert_eq!(cp["count"].seq, 2, "checkpoint seq = observedSeq");
        assert_eq!(cp["count"].val, Value::from(3));
        assert_eq!(cp["title"].seq, 2);
        assert_eq!(cp["title"].val, Value::from("Beta"));
    }

    #[test]
    fn snapshot_values_and_checkpoint_rows_cover_all_registered_units() {
        let mut reg = ProjectionRegistry::new();
        reg.register(count_unit()).unwrap();
        reg.register(title_unit()).unwrap();
        let mut session = ProjectionSession::new(&reg);
        session.observe(&title_event(7, "s1", "Gamma"));

        let snap = session.snapshot();
        assert_eq!(snap.values.len(), 2);
        assert!(snap.values.contains_key("count"));
        assert!(snap.values.contains_key("title"));

        let cp = session.checkpoint();
        assert_eq!(cp.len(), 2);
        assert_eq!(cp["count"].seq, 7);
        assert_eq!(cp["title"].seq, 7);
    }

    // ---- 持久化冷折叠 ----

    /// 极小内存 mock：`read_from` 返回 `seq >= fromSeq` 的后缀；无会话 → NotFound。
    /// 其余方法走缝的默认形状（创建/追加成功即可，读取路径不会被测试调到）。
    struct MockPersistence {
        sessions: RefCell<HashMap<String, Vec<SessionEvent>>>,
        header: SessionHeader,
    }

    impl MockPersistence {
        fn new(id: &str, events: Vec<SessionEvent>) -> Self {
            let header = SessionHeader::new(SessionId::from_raw(id), 1000);
            Self {
                sessions: RefCell::new(HashMap::from([(id.to_string(), events)])),
                header,
            }
        }

        fn empty(id: &str) -> Self {
            Self {
                sessions: RefCell::new(HashMap::new()),
                header: SessionHeader::new(SessionId::from_raw(id), 1000),
            }
        }
    }

    impl SessionPersistence for MockPersistence {
        fn locate(&self, _meta: &SessionHeader) -> Option<dsh_persistence::SessionLocation> {
            None
        }
        fn supports_raw_artifacts(&self) -> bool {
            false
        }
        fn read_raw(&self, _id: &SessionId) -> Result<Option<SessionRawArtifact>, PersistenceError> {
            Err(PersistenceError::Invalid("mock does not read raw artifacts".into()))
        }
        fn create(&self, _meta: &SessionHeader) -> Result<(), PersistenceError> {
            Ok(())
        }
        fn append(&self, _id: &SessionId, _events: &[SessionEvent]) -> Result<(), PersistenceError> {
            Ok(())
        }
        fn prepare(
            &self,
            id: &SessionId,
        ) -> Result<dsh_persistence::SessionPreparation, PersistenceError> {
            Err(PersistenceError::Invalid(format!("mock cannot prepare {id}")))
        }
        fn load(&self, _id: &SessionId) -> Result<SessionInspection, PersistenceError> {
            Err(PersistenceError::Invalid("mock cannot load".into()))
        }
        fn inspect(&self, _id: &SessionId) -> Result<SessionInspection, PersistenceError> {
            Err(PersistenceError::Invalid("mock cannot inspect".into()))
        }
        fn read_from(&self, id: &SessionId, from_seq: u64) -> Result<SessionSuffix, PersistenceError> {
            let sessions = self.sessions.borrow();
            let events = sessions
                .get(id.raw())
                .ok_or_else(|| PersistenceError::NotFound(id.clone()))?;
            Ok(SessionSuffix {
                meta: self.header.clone(),
                events: events.iter().filter(|e| e.seq >= from_seq).cloned().collect(),
            })
        }
        fn list(&self) -> Result<Vec<SessionHeader>, PersistenceError> {
            Ok(vec![])
        }
        fn list_snapshots(&self) -> Result<Vec<SessionPersistenceSnapshot>, PersistenceError> {
            Ok(vec![])
        }
    }

    fn fixture_events() -> Vec<SessionEvent> {
        vec![
            title_event(0, "s1", "Amen"),
            plain_event(1, EventKind::TurnStart),
            title_event(2, "s1", "Blye"),
            plain_event(3, EventKind::TurnEnd),
        ]
    }

    #[test]
    fn projection_from_persistence_folds_exact_suffix_from_mock() {
        let mock = MockPersistence::new("s1", fixture_events());
        let mut reg = ProjectionRegistry::new();
        reg.register(count_unit()).unwrap();
        reg.register(title_unit()).unwrap();
        let id = SessionId::from_raw("s1");

        // read_from(1) → [seq1, seq2, seq3]
        let snap = projection_from_persistence(&mock, &id, &reg, 1).expect("fold ok");
        assert_eq!(snap.as_of_seq, 3);
        assert_eq!(snap.values["count"], Value::from(3));
        assert_eq!(snap.values["title"], Value::from("Blye"));

        // read_from(0) → 全部 4 条
        let snap = projection_from_persistence(&mock, &id, &reg, 0).expect("fold ok");
        assert_eq!(snap.as_of_seq, 3);
        assert_eq!(snap.values["count"], Value::from(4));
    }

    #[test]
    fn projection_from_persistence_on_empty_session_folds_nothing_ok() {
        // 会话存在但无事件（或全部低于 fromSeq）→ 空后缀不是错误；返回空折叠快照
        let mock = MockPersistence::new("s1", Vec::new());
        let reg = ProjectionRegistry::new();
        let id = SessionId::from_raw("s1");
        let snap = projection_from_persistence(&mock, &id, &reg, 0).expect("empty suffix is fine");
        assert_eq!(snap.as_of_seq, 0);
        assert!(snap.values.is_empty());
    }

    #[test]
    fn projection_from_persistence_propagates_not_found() {
        let mock = MockPersistence::empty("missing");
        let reg = ProjectionRegistry::new();
        let id = SessionId::from_raw("missing");
        let err = projection_from_persistence(&mock, &id, &reg, 0).expect_err("missing session");
        assert!(matches!(err, PersistenceError::NotFound(_)));
    }

    // ---- restoreFloor（one-below-anchor）----

    #[test]
    fn restore_from_floor_reads_one_below_the_anchor() {
        let mock = MockPersistence::new("s1", fixture_events());
        let mut reg = ProjectionRegistry::new();
        reg.register(count_unit()).unwrap();
        reg.register(title_unit()).unwrap();
        let id = SessionId::from_raw("s1");

        // floor = Some(2) → read_from(1) → [seq1..3]
        let snap = projection_restore_from_floor(&mock, &id, &reg, Some(2)).expect("ok");
        assert_eq!(snap.as_of_seq, 3);
        assert_eq!(snap.values["count"], Value::from(3));

        // floor = None → read_from(0)
        let snap = projection_restore_from_floor(&mock, &id, &reg, None).expect("ok");
        assert_eq!(snap.values["count"], Value::from(4));

        // floor = Some(0) → 饱和到 read_from(0)（无下溢）
        let snap = projection_restore_from_floor(&mock, &id, &reg, Some(0)).expect("ok");
        assert_eq!(snap.values["count"], Value::from(4));
    }
}

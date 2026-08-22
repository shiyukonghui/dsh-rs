//! `dsh-schedule` — 宿主侧计划提醒/时程能力（`@deepseek-ai/dsh-schedule` 等效迁移）。
//!
//! M4f 目标：durable `schedule/change` 事件域（decode/fold）、创建规则（after/at/every）、
//! every 锚定推进、scheduleView（overdue/scheduled, deliveryMode=session-local）。
//! 权威参考：`deepseek-harness/packages/schedule/schedule/src/domain.ts`。
//!
//! M4 范围限定：`time_zone` 仅接受 `UTC` 与数值偏移（+HH:MM / -HH:MM）；IANA 本地时区
//! （如 `Asia/Shanghai`）因 chrono-tz 离线不可用按 `invalid_time_zone` 处理，属 M4 显式
//! deferred（D-044/D-050：评价后引入），不伪装支持。

mod domain;
mod inject;

pub use domain::{
    allocate_id_from_seen, canonicalize_time_zone, create_after_record, create_at_record_from_offset,
    create_every_record, decode_schedule_change, fold_schedule_events, fold_schedule_events_seeded,
    resolve_every_occurrence, schedule_view, EveryOccurrence, FoldedSchedules, InputCode, LogError,
    ScheduleInputError, ScheduleRecordData, ScheduleViewData, SCHEDULE_CHANGE_VERSION,
};
pub use inject::{dispatch_schedule_change, due_records, framing_text};

/// `schedule/change` 事件投递标记。
pub const SCHEDULE_CHANGE_EVENT: &str = "schedule/change";

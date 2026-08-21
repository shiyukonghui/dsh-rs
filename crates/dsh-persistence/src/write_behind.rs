//! 有界每会话写批处理（M1d：`dsh-persistence:write_behind`）。
//!
//! 权威参考：`deepseek-harness/packages/session/session-persistence/src/write-behind.ts`
//! （规范 §C2；本文件逐行语义对齐）。在 M1d 单线程纪律（D-006）下把 TS 的 async/
//! timer 状态机移植为**显式推进**的状态机：`enqueue` 累积事件并记录 deadline，
//! `flush` 是唯一显式 quiescence 点（共享 barrier 语义：并发调用者加入同一 drain），
//! `tick` 代替代理计时器——服务层在每次批次窗口到期时调用它（或直接 `flush`）。
//!
//! 语义要点（与 TS 一致）：
//! - 失败保留：耐用写入失败时 batch 原序拼回队首，`automatic_paused = true`；
//! - 超预算：`tick` 发现 active 占用预算时置 `deadline_expired`，active 结束且
//!   队非空时由调用方继续 automatic；
//! - barrier：`flush` 期间再次调用 flush 加入同一 drain（M1d 同步下表现为幂等）。

use std::rc::Rc;

use dsh_session::types::SessionEvent;

use crate::seam::DEFAULT_WRITE_BATCH_MAX_DELAY_MS;

/// 同步 write-behind 的耐用批次 sink。
///
/// M1d 无 async：write 以同步 `Result` 表示耐用失败。
pub trait BatchSink {
    /// 持久化一批事件；失败时返回值被保留回队首。
    fn write(&mut self, batch: &[SessionEvent]) -> Result<(), String>;
}

/// 背景失败报告回调（写失败时的通知钩子）。
type FailureReporter = Rc<dyn Fn(&str)>;

/// 每会话写批控制器（对应 TS `SessionWriteBehind`）。
pub struct SessionWriteBehind {
    max_delay_ms: u64,
    pending: Vec<SessionEvent>,
    deadline_at: Option<u64>,
    /// 一次在飞持久化写入（同步下写入符号上为零时长，但保留语义位）。
    active: bool,
    /// 显式 quiescence barrier 在场（flush 正在进行）。
    barrier: bool,
    deadline_expired: bool,
    automatic_paused: bool,
    /// 灾难性失败保留后的报告钩子。
    report_failure: Option<FailureReporter>,
}

impl SessionWriteBehind {
    pub fn new(max_delay_ms: u64) -> Self {
        SessionWriteBehind {
            max_delay_ms,
            pending: Vec::new(),
            deadline_at: None,
            active: false,
            barrier: false,
            deadline_expired: false,
            automatic_paused: false,
            report_failure: None,
        }
    }

    /// 设置后台失败报告钩子（可选）。
    pub fn with_failure_reporter(mut self, reporter: FailureReporter) -> Self {
        self.report_failure = Some(reporter);
        self
    }

    /// 是否拥有排队事件或在飞写入。
    pub fn has_work(&self) -> bool {
        !self.pending.is_empty() || self.active
    }

    /// 复制一个事件进持久化队列；automatic 空闲时启动固定窗口。
    pub fn enqueue(&mut self, event: SessionEvent, now_ms: u64) {
        let was_empty = self.pending.is_empty();
        self.pending.push(event);
        if self.barrier {
            return;
        }
        if self.automatic_paused {
            self.automatic_paused = false;
            self.deadline_expired = false;
            self.arm_timer(now_ms);
        } else if was_empty {
            self.arm_timer(now_ms);
        }
    }

    fn arm_timer(&mut self, now_ms: u64) {
        self.deadline_at = Some(now_ms + self.max_delay_ms);
    }

    /// 取消 automatic 窗口而不丢失 retained 工作。
    pub fn cancel_automatic_wait(&mut self) {
        self.deadline_at = None;
        self.deadline_expired = false;
    }

    /// 代理计时器 tick：窗口到期时若 automatic 空闲则启动后台写入。
    /// 调用方在服务层的批次窗口到期时调用；返回是否开始了一次 automatic 写入。
    pub fn tick(&mut self, sink: &mut dyn BatchSink, now_ms: u64) -> bool {
        let Some(at) = self.deadline_at else {
            return false;
        };
        if now_ms < at {
            return false;
        }
        self.deadline_at = None;
        if self.active {
            self.deadline_expired = true;
            return false;
        }
        self.start_background(sink)
    }

    /// 显式 quiescence 点：取消等待并耐用 drain。M1d 同步下若已在 barrier，
    /// 幂等返回（并发 join 语义退化为 no-op——没有可并发等待的异步工作）。
    pub fn flush(&mut self, sink: &mut dyn BatchSink) -> Result<(), String> {
        if self.barrier {
            return Ok(());
        }
        self.cancel_automatic_wait();
        self.deadline_at = None;
        self.automatic_paused = false;
        self.barrier = true;
        let result = self.drain_barrier(sink);
        self.barrier = false;
        result
    }

    fn drain_barrier(&mut self, sink: &mut dyn BatchSink) -> Result<(), String> {
        // 与在飞 active 重叠（同步下无实际重叠，保留语义位）
        if self.active {
            self.automatic_paused = false;
            // 同步：等待符号上零时长；active 始终在单个调用内完成
            self.active = false;
        }
        while !self.pending.is_empty() {
            self.start_write(sink, false)?;
        }
        Ok(())
    }

    /// 启动一次 stable pending 前缀写入；失败保留其顺序。
    fn start_write(&mut self, sink: &mut dyn BatchSink, background: bool) -> Result<(), String> {
        let batch = std::mem::take(&mut self.pending);
        self.deadline_at = None;
        self.deadline_expired = false;
        self.active = true;
        let result = sink.write(&batch);
        self.active = false;
        match result {
            Ok(()) => Ok(()),
            Err(error) => {
                // 保留：原序拼回队首
                let mut retained = batch;
                retained.extend(std::mem::take(&mut self.pending));
                self.pending = retained;
                self.deadline_at = None;
                self.deadline_expired = false;
                self.automatic_paused = true;
                if background {
                    if let Some(report) = &self.report_failure {
                        report(&error);
                    }
                }
                Err(error)
            }
        }
    }

    fn start_background(&mut self, sink: &mut dyn BatchSink) -> bool {
        match self.start_write(sink, true) {
            Ok(()) => true,
            Err(_) => false,
        }
    }

    /// 过一次 automatic 继续（active 结束后由服务层调用）：超预算且队非空则再走 auto。
    pub fn continue_automatic(&mut self, sink: &mut dyn BatchSink) -> bool {
        if self.barrier || self.pending.is_empty() {
            return false;
        }
        if self.deadline_expired {
            self.deadline_expired = false;
            return self.start_background(sink);
        }
        false
    }

    /// 当前 pending 事件数。
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// 直接查看 pending 事件（调试/测试）。
    pub fn pending_events(&self) -> &[SessionEvent] {
        &self.pending
    }

    /// 未消费的失败保留队列（便于顺序校验）。仅在失败后非空。
    pub fn is_automatic_paused(&self) -> bool {
        self.automatic_paused
    }
}

impl Default for SessionWriteBehind {
    fn default() -> Self {
        SessionWriteBehind::new(DEFAULT_WRITE_BATCH_MAX_DELAY_MS)
    }
}

// ---- 测试只需的一组小工具 ----

/// 一个记录每次写入 batch 的测试 sink
pub struct RecordingSink {
    pub writes: Vec<Vec<SessionEvent>>,
    pub fail_after: Option<usize>,
}

impl RecordingSink {
    pub fn new() -> Self {
        RecordingSink { writes: Vec::new(), fail_after: None }
    }
}

impl Default for RecordingSink {
    fn default() -> Self {
        RecordingSink::new()
    }
}

impl BatchSink for RecordingSink {
    fn write(&mut self, batch: &[SessionEvent]) -> Result<(), String> {
        if let Some(n) = self.fail_after {
            if self.writes.len() >= n {
                return Err("durable write failed (injected)".into());
            }
        }
        self.writes.push(batch.to_vec());
        Ok(())
    }
}

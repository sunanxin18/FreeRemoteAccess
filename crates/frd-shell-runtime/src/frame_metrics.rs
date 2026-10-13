use std::time::{Duration, Instant};

pub fn checked_mailbox_age(
    apply_or_drain_started_at: Instant,
    enqueued_at: Instant,
) -> Option<Duration> {
    apply_or_drain_started_at.checked_duration_since(enqueued_at)
}

#[derive(Clone, Copy, Debug)]
pub struct BatchMetricContext {
    pub batch_started_at: Instant,
    pub source_update_count: usize,
    pub oldest_age: Option<Duration>,
    pub transaction_count: usize,
}

/// 已编译帧批次的平台无关时间与计数快照，不暴露后端指标状态。
#[derive(Clone, Copy, Debug)]
pub struct FrameBatchMetricsSnapshot {
    batch_started_at: Instant,
    source_update_count: usize,
    oldest_age: Option<Duration>,
    transaction_count: usize,
}

impl FrameBatchMetricsSnapshot {
    pub fn batch_started_at(&self) -> Instant {
        self.batch_started_at
    }
    pub fn source_update_count(&self) -> usize {
        self.source_update_count
    }
    pub fn oldest_age(&self) -> Option<Duration> {
        self.oldest_age
    }
    pub fn transaction_count(&self) -> usize {
        self.transaction_count
    }
}

impl BatchMetricContext {
    pub fn snapshot(&self) -> FrameBatchMetricsSnapshot {
        FrameBatchMetricsSnapshot {
            batch_started_at: self.batch_started_at,
            source_update_count: self.source_update_count,
            oldest_age: self.oldest_age,
            transaction_count: self.transaction_count,
        }
    }
}

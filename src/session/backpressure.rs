use std::collections::VecDeque;
use std::error::Error;
use std::fmt;

use crate::core::RenderUpdate;

#[derive(Debug)]
pub struct RenderUpdateQueue {
    entries: VecDeque<RenderUpdate>,
    max_entries: usize,
    max_bytes: usize,
    queued_bytes: usize,
}

impl RenderUpdateQueue {
    pub fn with_limits(max_entries: usize, max_bytes: usize) -> Result<Self, QueueError> {
        if max_entries == 0 {
            return Err(QueueError::new("render_queue_entry_limit_invalid"));
        }
        if max_bytes == 0 {
            return Err(QueueError::new("render_queue_byte_limit_invalid"));
        }
        Ok(Self {
            entries: VecDeque::with_capacity(max_entries),
            max_entries,
            max_bytes,
            queued_bytes: 0,
        })
    }

    pub fn push(&mut self, update: RenderUpdate) -> Result<QueuePushOutcome, QueueError> {
        let update_bytes = update_byte_len(&update);
        if update_bytes > self.max_bytes {
            return Err(QueueError::new("render_update_exceeds_budget"));
        }

        if matches!(update, RenderUpdate::Present { .. })
            && self.entries.iter().any(|queued| {
                matches!(queued, RenderUpdate::Present { .. })
                    && queued.generation() == update.generation()
            })
        {
            return Ok(QueuePushOutcome::Coalesced);
        }

        if matches!(update, RenderUpdate::Reset { .. }) {
            self.retain_generation_at_least(update.generation());
        } else {
            self.evict_stale_updates(update.generation());
        }

        if self.entries.len() >= self.max_entries
            || self.queued_bytes.saturating_add(update_bytes) > self.max_bytes
        {
            return Err(QueueError::new("render_queue_full"));
        }

        self.queued_bytes += update_bytes;
        self.entries.push_back(update);
        Ok(QueuePushOutcome::Queued)
    }

    pub fn pop_front(&mut self) -> Option<RenderUpdate> {
        let update = self.entries.pop_front()?;
        self.queued_bytes -= update_byte_len(&update);
        Some(update)
    }

    pub fn iter(&self) -> impl Iterator<Item = &RenderUpdate> {
        self.entries.iter()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn queued_bytes(&self) -> usize {
        self.queued_bytes
    }

    fn retain_generation_at_least(&mut self, generation: u64) {
        self.entries
            .retain(|queued| queued.generation() >= generation);
        self.recalculate_bytes();
    }

    fn evict_stale_updates(&mut self, generation: u64) {
        self.entries
            .retain(|queued| queued.generation() >= generation);
        self.recalculate_bytes();
    }

    fn recalculate_bytes(&mut self) {
        self.queued_bytes = self.entries.iter().map(update_byte_len).sum();
    }
}

fn update_byte_len(update: &RenderUpdate) -> usize {
    match update {
        RenderUpdate::DirtyRect { pixels, .. } => pixels.len(),
        RenderUpdate::Reset { .. } | RenderUpdate::Present { .. } => 0,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueuePushOutcome {
    Queued,
    Coalesced,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueError {
    code: &'static str,
}

impl QueueError {
    const fn new(code: &'static str) -> Self {
        Self { code }
    }

    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Display for QueueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "远程画面队列无效 ({})", self.code)
    }
}

impl Error for QueueError {}

use std::collections::VecDeque;
use std::error::Error;
use std::fmt;

use crate::core::RenderUpdate;
use crate::session::engine::SessionEvent;

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

        if matches!(update, RenderUpdate::Reset { .. }) {
            self.retain_generation_at_least(update.generation())?;
        } else {
            self.evict_stale_updates(update.generation())?;
        }

        if self.entries.iter().any(|queued| match (&update, queued) {
            (
                RenderUpdate::Reset { generation, .. },
                RenderUpdate::Reset {
                    generation: queued_generation,
                    ..
                },
            )
            | (
                RenderUpdate::Present { generation },
                RenderUpdate::Present {
                    generation: queued_generation,
                },
            ) => generation == queued_generation,
            _ => false,
        }) {
            return Ok(QueuePushOutcome::Coalesced);
        }

        let queued_after = checked_byte_add(
            self.queued_bytes,
            update_bytes,
            "render_queue_byte_accounting_overflow",
        )?;
        if self.entries.len() >= self.max_entries || queued_after > self.max_bytes {
            return Err(QueueError::new("render_queue_full"));
        }

        self.queued_bytes = queued_after;
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

    fn retain_generation_at_least(&mut self, generation: u64) -> Result<(), QueueError> {
        self.entries
            .retain(|queued| queued.generation() >= generation);
        self.recalculate_bytes()
    }

    fn evict_stale_updates(&mut self, generation: u64) -> Result<(), QueueError> {
        self.entries
            .retain(|queued| queued.generation() >= generation);
        self.recalculate_bytes()
    }

    fn recalculate_bytes(&mut self) -> Result<(), QueueError> {
        self.queued_bytes = self.entries.iter().try_fold(0usize, |total, update| {
            checked_byte_add(
                total,
                update_byte_len(update),
                "render_queue_byte_accounting_overflow",
            )
        })?;
        Ok(())
    }
}

fn checked_byte_add(
    total: usize,
    additional: usize,
    overflow_code: &'static str,
) -> Result<usize, QueueError> {
    total
        .checked_add(additional)
        .ok_or_else(|| QueueError::new(overflow_code))
}

#[derive(Debug)]
pub struct SessionEventMailbox {
    entries: VecDeque<SessionEvent>,
    max_entries: usize,
    max_bytes: usize,
    queued_bytes: usize,
    terminal: Option<SessionEvent>,
}

impl SessionEventMailbox {
    pub fn with_limits(max_entries: usize, max_bytes: usize) -> Result<Self, QueueError> {
        if max_entries == 0 {
            return Err(QueueError::new("session_event_entry_limit_invalid"));
        }
        if max_bytes == 0 {
            return Err(QueueError::new("session_event_byte_limit_invalid"));
        }
        Ok(Self {
            entries: VecDeque::with_capacity(max_entries),
            max_entries,
            max_bytes,
            queued_bytes: 0,
            terminal: None,
        })
    }

    pub fn push(&mut self, event: SessionEvent) -> Result<QueuePushOutcome, QueueError> {
        if is_terminal_event(&event) {
            if self.terminal.is_some() {
                return Err(QueueError::new("session_terminal_slot_full"));
            }
            self.terminal = Some(event);
            return Ok(QueuePushOutcome::Queued);
        }
        if self.terminal.is_some() {
            return Err(QueueError::new("session_terminal_pending"));
        }

        let event_bytes = event_byte_len(&event);
        if event_bytes > self.max_bytes {
            return Err(QueueError::new("render_update_exceeds_budget"));
        }
        let (stale_generation, coalesces) = match &event {
            SessionEvent::Render(update) => {
                let generation = update.generation();
                (
                    Some(generation),
                    self.entries
                        .iter()
                        .any(|queued| same_coalescible_render(queued, update)),
                )
            }
            _ => (None, false),
        };
        let retained_count = self
            .entries
            .iter()
            .filter(|queued| !is_stale_render(queued, stale_generation))
            .count();
        let retained_bytes = self
            .entries
            .iter()
            .filter(|queued| !is_stale_render(queued, stale_generation))
            .try_fold(0usize, |total, queued| {
                checked_byte_add(
                    total,
                    event_byte_len(queued),
                    "session_event_byte_accounting_overflow",
                )
            })?;

        if !coalesces && retained_count >= self.max_entries {
            return Err(QueueError::new("session_event_channel_full"));
        }
        let queued_after = checked_byte_add(
            retained_bytes,
            event_bytes,
            "session_event_byte_accounting_overflow",
        )?;
        if !coalesces && queued_after > self.max_bytes {
            return Err(QueueError::new("render_queue_full"));
        }

        if stale_generation.is_some() {
            self.entries
                .retain(|queued| !is_stale_render(queued, stale_generation));
            self.queued_bytes = retained_bytes;
        }
        if coalesces {
            return Ok(QueuePushOutcome::Coalesced);
        }

        self.queued_bytes = queued_after;
        self.entries.push_back(event);
        Ok(QueuePushOutcome::Queued)
    }

    pub fn pop_front(&mut self) -> Option<SessionEvent> {
        if let Some(event) = self.entries.pop_front() {
            self.queued_bytes -= event_byte_len(&event);
            return Some(event);
        }
        self.terminal.take()
    }
}

fn is_terminal_event(event: &SessionEvent) -> bool {
    matches!(
        event,
        SessionEvent::Disconnected | SessionEvent::Failed { .. }
    )
}

fn event_byte_len(event: &SessionEvent) -> usize {
    match event {
        SessionEvent::Render(update) => update_byte_len(update),
        _ => 0,
    }
}

fn is_stale_render(event: &SessionEvent, generation: Option<u64>) -> bool {
    matches!(
        (event, generation),
        (SessionEvent::Render(update), Some(current_generation)) if update.generation() < current_generation
    )
}

fn same_coalescible_render(event: &SessionEvent, incoming: &RenderUpdate) -> bool {
    matches!(
        (event, incoming),
        (
            SessionEvent::Render(RenderUpdate::Reset {
                generation: queued_generation,
                ..
            }),
            RenderUpdate::Reset { generation, .. }
        ) | (
            SessionEvent::Render(RenderUpdate::Present {
                generation: queued_generation
            }),
            RenderUpdate::Present { generation }
        ) if queued_generation == generation
    )
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

#[cfg(test)]
mod tests {
    use super::checked_byte_add;

    #[test]
    fn byte_budget_accounting_rejects_usize_overflow() {
        assert_eq!(
            checked_byte_add(usize::MAX, 1, "session_event_byte_accounting_overflow")
                .unwrap_err()
                .code(),
            "session_event_byte_accounting_overflow"
        );
    }
}

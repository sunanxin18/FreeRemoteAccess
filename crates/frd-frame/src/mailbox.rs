use std::collections::VecDeque;

use frd_core::{PixelSize, SessionId};

use crate::{PixelPatch, SurfaceUpdate};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PushOutcome {
    Queued,
    Rejected,
    NeedsFullSnapshot,
}

#[derive(Clone, Copy, Debug)]
struct SurfaceState {
    session_id: SessionId,
    generation: u64,
    size: PixelSize,
    bytes_per_pixel: u32,
}

/// 对单一会话世代的更新进行有界排队。
///
/// 世代和修订号均从一开始计数；零值不是已建立的生命周期的一部分。
pub struct FrameMailbox {
    entry_limit: usize,
    pixel_byte_limit: usize,
    queue: VecDeque<SurfaceUpdate>,
    queued_pixel_bytes: usize,
    current: Option<SurfaceState>,
}

impl FrameMailbox {
    pub fn new(entry_limit: usize, pixel_byte_limit: usize) -> Self {
        Self {
            // 一个 Reset 必须能保留，以便消费者知道之后的更新所属的表面。
            entry_limit: entry_limit.max(1),
            pixel_byte_limit,
            queue: VecDeque::new(),
            queued_pixel_bytes: 0,
            current: None,
        }
    }

    pub fn push(&mut self, update: SurfaceUpdate) -> PushOutcome {
        match update {
            SurfaceUpdate::Reset {
                session_id,
                generation,
                size,
                format,
            } => self.push_reset(
                session_id,
                generation,
                size,
                format.bytes_per_pixel(),
                update,
            ),
            SurfaceUpdate::Damage {
                session_id,
                generation,
                revision,
                ref patches,
            } => {
                let Some(current) = self.current else {
                    return PushOutcome::Rejected;
                };
                if !matches_current(current, session_id, generation) || revision == 0 {
                    return PushOutcome::Rejected;
                }
                let Some(pixel_bytes) = patches_pixel_bytes(patches, current) else {
                    return PushOutcome::Rejected;
                };
                self.push_current(update, pixel_bytes)
            }
            SurfaceUpdate::FrameBoundary {
                session_id,
                generation,
                revision,
                ..
            } => {
                let Some(current) = self.current else {
                    return PushOutcome::Rejected;
                };
                if !matches_current(current, session_id, generation) || revision == 0 {
                    return PushOutcome::Rejected;
                }
                self.push_current(update, 0)
            }
        }
    }

    pub fn pop(&mut self) -> Option<SurfaceUpdate> {
        let update = self.queue.pop_front()?;
        self.queued_pixel_bytes = self
            .queued_pixel_bytes
            .checked_sub(update_pixel_bytes(&update))
            .expect("帧邮箱像素字节记账不一致");
        Some(update)
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    pub fn queued_pixel_bytes(&self) -> usize {
        self.queued_pixel_bytes
    }

    fn push_reset(
        &mut self,
        session_id: SessionId,
        generation: u64,
        size: PixelSize,
        bytes_per_pixel: u32,
        update: SurfaceUpdate,
    ) -> PushOutcome {
        if generation == 0 || size.width == 0 || size.height == 0 {
            return PushOutcome::Rejected;
        }

        self.current = Some(SurfaceState {
            session_id,
            generation,
            size,
            bytes_per_pixel,
        });
        self.queue.clear();
        self.queued_pixel_bytes = 0;
        self.queue.push_back(update);
        PushOutcome::Queued
    }

    fn push_current(&mut self, update: SurfaceUpdate, pixel_bytes: usize) -> PushOutcome {
        let entry_overflow = self
            .queue
            .len()
            .checked_add(1)
            .is_none_or(|entries| entries > self.entry_limit);
        let byte_overflow = self
            .queued_pixel_bytes
            .checked_add(pixel_bytes)
            .is_none_or(|bytes| bytes > self.pixel_byte_limit);
        if entry_overflow || byte_overflow {
            self.clear_current_damage_and_boundaries();
            return PushOutcome::NeedsFullSnapshot;
        }

        self.queued_pixel_bytes = self
            .queued_pixel_bytes
            .checked_add(pixel_bytes)
            .expect("已检查帧邮箱像素字节溢出");
        self.queue.push_back(update);
        PushOutcome::Queued
    }

    fn clear_current_damage_and_boundaries(&mut self) {
        let Some(current) = self.current else {
            return;
        };
        self.queue.retain(|update| match update {
            SurfaceUpdate::Reset { .. } => true,
            SurfaceUpdate::Damage {
                session_id,
                generation,
                ..
            }
            | SurfaceUpdate::FrameBoundary {
                session_id,
                generation,
                ..
            } => !matches_current(current, *session_id, *generation),
        });
        self.queued_pixel_bytes = self
            .queue
            .iter()
            .try_fold(0usize, |total, update| {
                total.checked_add(update_pixel_bytes(update))
            })
            .expect("已入队帧像素字节溢出");
    }
}

fn matches_current(current: SurfaceState, session_id: SessionId, generation: u64) -> bool {
    current.session_id == session_id && current.generation == generation
}

fn patches_pixel_bytes(patches: &[PixelPatch], current: SurfaceState) -> Option<usize> {
    patches.iter().try_fold(0usize, |total, patch| {
        validate_patch(patch, current)?;
        total.checked_add(patch.pixels.len())
    })
}

fn validate_patch(patch: &PixelPatch, current: SurfaceState) -> Option<()> {
    let (_, end) = patch.rect.checked_bounds()?;
    if end.x > current.size.width || end.y > current.size.height {
        return None;
    }

    let minimum_stride = patch.rect.width.checked_mul(current.bytes_per_pixel)?;
    if patch.stride_bytes < minimum_stride {
        return None;
    }
    let expected_length = usize::try_from(patch.stride_bytes)
        .ok()?
        .checked_mul(usize::try_from(patch.rect.height).ok()?)?;
    (patch.pixels.len() == expected_length).then_some(())
}

fn update_pixel_bytes(update: &SurfaceUpdate) -> usize {
    match update {
        SurfaceUpdate::Damage { patches, .. } => patches
            .iter()
            .try_fold(0usize, |total, patch| total.checked_add(patch.pixels.len()))
            .expect("已验证补丁的像素字节溢出"),
        SurfaceUpdate::Reset { .. } | SurfaceUpdate::FrameBoundary { .. } => 0,
    }
}

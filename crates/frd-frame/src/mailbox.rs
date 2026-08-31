use std::collections::VecDeque;
use std::time::Instant;

use frd_core::{PixelSize, SessionId};

use crate::{PixelFormat, PixelPatch, SurfaceUpdate};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PushOutcome {
    Queued,
    Rejected,
    NeedsFullSnapshot,
}

pub struct EnqueuedSurfaceUpdate {
    pub enqueued_at: Instant,
    pub update: SurfaceUpdate,
}

#[derive(Clone, Copy, Debug)]
struct SurfaceState {
    session_id: SessionId,
    generation: u64,
    size: PixelSize,
    bytes_per_pixel: u32,
    last_damage_revision: u64,
    last_boundary_revision: u64,
    boundary_eligible_revision: Option<u64>,
}

/// 对单一会话世代的更新进行有界排队。
///
/// 世代和修订号均从一开始计数；零值不是已建立的生命周期的一部分。
pub struct FrameMailbox {
    entry_limit: usize,
    pixel_byte_limit: usize,
    queue: VecDeque<EnqueuedSurfaceUpdate>,
    queued_pixel_bytes: usize,
    current: Option<SurfaceState>,
}

impl FrameMailbox {
    /// 创建具有固定条目和像素字节预算的邮箱。
    ///
    /// # Panics
    ///
    /// 当 `entry_limit` 为零时恐慌；一个可用邮箱至少必须能够保留 Reset。
    pub fn new(entry_limit: usize, pixel_byte_limit: usize) -> Self {
        assert!(entry_limit > 0, "帧邮箱 entry_limit 必须大于零");
        Self {
            entry_limit,
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
                if !matches_current(current, session_id, generation)
                    || revision == 0
                    || revision <= current.last_damage_revision
                {
                    return PushOutcome::Rejected;
                }
                let Some(pixel_bytes) = patches_pixel_bytes(patches, current) else {
                    return PushOutcome::Rejected;
                };
                let outcome = self.push_current(update, pixel_bytes);
                if outcome == PushOutcome::Queued {
                    let current = self.current.as_mut().expect("当前表面已在损伤入队前验证");
                    current.last_damage_revision = revision;
                    current.boundary_eligible_revision = Some(revision);
                }
                outcome
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
                if !matches_current(current, session_id, generation)
                    || revision == 0
                    || revision != current.last_damage_revision
                    || current.boundary_eligible_revision != Some(revision)
                    || revision <= current.last_boundary_revision
                {
                    return PushOutcome::Rejected;
                }
                let outcome = self.push_current(update, 0);
                if outcome == PushOutcome::Queued {
                    let current = self.current.as_mut().expect("当前表面已在边界入队前验证");
                    current.last_boundary_revision = revision;
                    current.boundary_eligible_revision = None;
                }
                outcome
            }
        }
    }

    pub fn pop(&mut self) -> Option<SurfaceUpdate> {
        self.pop_enqueued().map(|entry| entry.update)
    }

    pub fn oldest_enqueued_at(&self) -> Option<Instant> {
        self.queue.front().map(|entry| entry.enqueued_at)
    }

    pub fn pop_enqueued(&mut self) -> Option<EnqueuedSurfaceUpdate> {
        let entry = self.queue.pop_front()?;
        self.queued_pixel_bytes = self
            .queued_pixel_bytes
            .checked_sub(update_pixel_bytes(&entry.update))
            .expect("帧邮箱像素字节记账不一致");
        Some(entry)
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

    /// 检查一个完整表面是否能在当前像素字节预算内一次发布。
    pub fn supports_complete_surface(&self, size: PixelSize, format: PixelFormat) -> bool {
        if size.width == 0 || size.height == 0 {
            return false;
        }
        usize::try_from(size.width)
            .ok()
            .and_then(|width| {
                usize::try_from(size.height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .and_then(|pixels| {
                usize::try_from(format.bytes_per_pixel())
                    .ok()
                    .and_then(|bytes_per_pixel| pixels.checked_mul(bytes_per_pixel))
            })
            .is_some_and(|bytes| bytes <= self.pixel_byte_limit)
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
        if let Some(current) = self.current {
            let advances_current_session =
                session_id == current.session_id && generation > current.generation;
            let starts_newer_session = session_id.get() > current.session_id.get();
            if !advances_current_session && !starts_newer_session {
                return PushOutcome::Rejected;
            }
        }

        self.current = Some(SurfaceState {
            session_id,
            generation,
            size,
            bytes_per_pixel,
            last_damage_revision: 0,
            last_boundary_revision: 0,
            boundary_eligible_revision: None,
        });
        self.queue.clear();
        self.queued_pixel_bytes = 0;
        self.queue.push_back(EnqueuedSurfaceUpdate {
            enqueued_at: Instant::now(),
            update,
        });
        PushOutcome::Queued
    }

    /// 溢出触发的更新不会推进生命周期修订号。
    /// 生产者必须重新发布更高修订的规范完整快照。
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
        self.queue.push_back(EnqueuedSurfaceUpdate {
            enqueued_at: Instant::now(),
            update,
        });
        PushOutcome::Queued
    }

    fn clear_current_damage_and_boundaries(&mut self) {
        let Some(current) = self.current else {
            return;
        };
        self.queue.retain(|entry| match &entry.update {
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
            .try_fold(0usize, |total, entry| {
                total.checked_add(update_pixel_bytes(&entry.update))
            })
            .expect("已入队帧像素字节溢出");
        self.current
            .as_mut()
            .expect("溢出清理要求当前表面存在")
            .boundary_eligible_revision = None;
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

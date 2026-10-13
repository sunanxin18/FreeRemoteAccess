use frd_core::{PixelSize, SessionId};
use frd_frame::{FrameCompleteness, PixelFormat, SurfaceUpdate};
use frd_protocol_api::{ProtocolError, ProtocolRuntime};
use ironrdp::pdu::geometry::InclusiveRectangle;
use ironrdp::session::image::DecodedImage;

use crate::surface::extract_bgrx_patch;

const FULL_SNAPSHOT_PATCH_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_DECODED_COVERAGE_ENTRIES: usize = 65_536;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BaselineError {
    InvalidSize,
    InvalidRegion,
    StaleGeneration,
    AllocationFailed,
}

pub(crate) struct CoverageTracker {
    session_id: SessionId,
    generation: u64,
    size: PixelSize,
    coverage_words: Box<[u64]>,
    covered_pixels: u64,
}

impl CoverageTracker {
    pub(crate) fn new(
        session_id: SessionId,
        generation: u64,
        size: PixelSize,
    ) -> Result<Self, BaselineError> {
        if generation == 0 || size.width == 0 || size.height == 0 {
            return Err(BaselineError::InvalidSize);
        }
        let total_pixels = u64::from(size.width)
            .checked_mul(u64::from(size.height))
            .ok_or(BaselineError::InvalidSize)?;
        let word_count = total_pixels
            .checked_add(63)
            .map(|rounded| rounded / 64)
            .and_then(|words| usize::try_from(words).ok())
            .ok_or(BaselineError::InvalidSize)?;
        let mut coverage_words = Vec::new();
        coverage_words
            .try_reserve_exact(word_count)
            .map_err(|_| BaselineError::AllocationFailed)?;
        coverage_words.resize(word_count, 0);
        Ok(Self {
            session_id,
            generation,
            size,
            coverage_words: coverage_words.into_boxed_slice(),
            covered_pixels: 0,
        })
    }

    #[cfg(test)]
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn size(&self) -> PixelSize {
        self.size
    }

    #[cfg(test)]
    pub(crate) fn covered_pixels(&self) -> u64 {
        self.covered_pixels
    }

    #[cfg(test)]
    pub(crate) fn storage_bytes(&self) -> usize {
        self.coverage_words.len() * std::mem::size_of::<u64>()
    }

    #[cfg(test)]
    pub(crate) fn reset(
        &mut self,
        session_id: SessionId,
        generation: u64,
        size: PixelSize,
    ) -> Result<(), BaselineError> {
        if session_id != self.session_id || generation <= self.generation {
            return Err(BaselineError::StaleGeneration);
        }
        *self = Self::new(session_id, generation, size)?;
        Ok(())
    }

    pub(crate) fn record(
        &mut self,
        session_id: SessionId,
        generation: u64,
        region: InclusiveRectangle,
    ) -> Result<bool, BaselineError> {
        self.validate_current_region(session_id, generation, &region)?;

        let start_x = u64::from(region.left);
        let end_x = u64::from(region.right) + 1;
        for y in region.top..=region.bottom {
            let row_start = u64::from(y)
                .checked_mul(u64::from(self.size.width))
                .ok_or(BaselineError::InvalidRegion)?;
            let added = self.mark_range(row_start + start_x, row_start + end_x)?;
            self.covered_pixels = self
                .covered_pixels
                .checked_add(added)
                .ok_or(BaselineError::InvalidRegion)?;
        }

        Ok(self.covered_pixels == self.total_pixels())
    }

    fn validate_current_region(
        &self,
        session_id: SessionId,
        generation: u64,
        region: &InclusiveRectangle,
    ) -> Result<(), BaselineError> {
        if session_id != self.session_id || generation != self.generation {
            return Err(BaselineError::StaleGeneration);
        }
        if region.left > region.right
            || region.top > region.bottom
            || u32::from(region.right) >= self.size.width
            || u32::from(region.bottom) >= self.size.height
        {
            return Err(BaselineError::InvalidRegion);
        }
        Ok(())
    }

    fn is_full(&self) -> bool {
        self.covered_pixels == self.total_pixels()
    }

    fn mark_range(&mut self, start: u64, end: u64) -> Result<u64, BaselineError> {
        let mut cursor = start;
        let mut added = 0_u64;
        while cursor < end {
            let word_index =
                usize::try_from(cursor / 64).map_err(|_| BaselineError::InvalidRegion)?;
            let bit_offset = (cursor % 64) as u32;
            let bit_count = u32::try_from((end - cursor).min(u64::from(64 - bit_offset)))
                .map_err(|_| BaselineError::InvalidRegion)?;
            let mask = if bit_count == 64 {
                u64::MAX
            } else {
                ((1_u64 << bit_count) - 1) << bit_offset
            };
            let word = self
                .coverage_words
                .get_mut(word_index)
                .ok_or(BaselineError::InvalidRegion)?;
            let new_bits = mask & !*word;
            *word |= mask;
            added += u64::from(new_bits.count_ones());
            cursor += u64::from(bit_count);
        }
        Ok(added)
    }

    fn total_pixels(&self) -> u64 {
        u64::from(self.size.width) * u64::from(self.size.height)
    }
}

pub(crate) struct RdpBaseline {
    coverage: CoverageTracker,
    revision: u64,
    baseline_established: bool,
}

impl RdpBaseline {
    pub(crate) fn begin(
        runtime: &mut ProtocolRuntime,
        session_id: SessionId,
        size: PixelSize,
    ) -> Result<Self, ProtocolError> {
        let coverage = CoverageTracker::new(session_id, 1, size)
            .map_err(|_| ProtocolError::InvalidGeneration)?;
        runtime.begin_generation(session_id, 1, size, PixelFormat::Bgrx8UnormSrgb)?;
        Ok(Self {
            coverage,
            revision: 0,
            baseline_established: false,
        })
    }

    pub(crate) fn begin_next_generation(
        &mut self,
        runtime: &mut ProtocolRuntime,
        generation: u64,
        size: PixelSize,
    ) -> Result<(), ProtocolError> {
        let session_id = self.coverage.session_id;
        let replacement = CoverageTracker::new(session_id, generation, size)
            .map_err(|_| ProtocolError::InvalidGeneration)?;
        if generation <= self.coverage.generation {
            return Err(ProtocolError::InvalidGeneration);
        }
        runtime.begin_generation(session_id, generation, size, PixelFormat::Bgrx8UnormSrgb)?;
        self.coverage = replacement;
        self.revision = 0;
        self.baseline_established = false;
        Ok(())
    }

    /// 外部 surface 成功发布后只接管呈现与修订号，不改变 legacy 内容知识。
    pub(crate) fn observe_external_revision(
        &mut self,
        generation: u64,
        revision: u64,
    ) -> Result<(), ProtocolError> {
        if generation != self.coverage.generation {
            return Err(ProtocolError::StaleSurface);
        }
        self.revision = self.revision.max(revision);
        self.baseline_established = false;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn presentation_revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn publish(
        &mut self,
        runtime: &mut ProtocolRuntime,
        image: &DecodedImage,
        generation: u64,
        damage: InclusiveRectangle,
        content: &[InclusiveRectangle],
    ) -> Result<(), ProtocolError> {
        self.publish_with_recovery_patch_limit(
            runtime,
            image,
            generation,
            damage,
            content,
            FULL_SNAPSHOT_PATCH_BYTES,
        )
    }

    /// 完整校验先于覆盖记账；损伤框内的空洞不得成为已收到的内容。
    pub(crate) fn validate_publication(
        &self,
        image: &DecodedImage,
        generation: u64,
        damage: &InclusiveRectangle,
        content: &[InclusiveRectangle],
    ) -> Result<(), ProtocolError> {
        self.validate_image_generation(image, generation)?;
        Self::validate_regions(image, damage, content)
    }

    pub(crate) fn validate_regions(
        image: &DecodedImage,
        damage: &InclusiveRectangle,
        content: &[InclusiveRectangle],
    ) -> Result<(), ProtocolError> {
        let fits = |r: &InclusiveRectangle| {
            r.left <= r.right
                && r.top <= r.bottom
                && r.right < image.width()
                && r.bottom < image.height()
        };
        if content.len() > MAX_DECODED_COVERAGE_ENTRIES || !fits(damage) {
            return Err(ProtocolError::FramePortRejected);
        }
        for r in content {
            if !fits(r)
                || r.left < damage.left
                || r.top < damage.top
                || r.right > damage.right
                || r.bottom > damage.bottom
            {
                return Err(ProtocolError::FramePortRejected);
            }
        }
        Ok(())
    }

    fn publish_with_recovery_patch_limit(
        &mut self,
        runtime: &mut ProtocolRuntime,
        image: &DecodedImage,
        generation: u64,
        damage: InclusiveRectangle,
        content: &[InclusiveRectangle],
        recovery_patch_bytes: usize,
    ) -> Result<(), ProtocolError> {
        self.validate_publication(image, generation, &damage, content)?;
        // 只有成功解码的实际内容贡献知识；光标和 no-op damage 不贡献。
        for region in content {
            self.coverage
                .record(self.coverage.session_id, generation, region.clone())
                .map_err(map_baseline_error)?;
        }
        if !self.baseline_established {
            if !self.coverage.is_full() {
                return Ok(());
            }
            return self.recover_full_snapshot(runtime, image, recovery_patch_bytes);
        }

        let patch =
            extract_bgrx_patch(image, damage).map_err(|_| ProtocolError::FramePortRejected)?;
        let revision = self.next_revision()?;
        match runtime.publish_surface(SurfaceUpdate::Damage {
            session_id: self.coverage.session_id,
            generation,
            revision,
            patches: vec![patch],
        }) {
            Ok(()) => {}
            Err(ProtocolError::NeedsFullSnapshot) => {
                self.baseline_established = false;
                return self.recover_full_snapshot(runtime, image, recovery_patch_bytes);
            }
            Err(error) => return Err(error),
        }
        match runtime.publish_surface(SurfaceUpdate::FrameBoundary {
            session_id: self.coverage.session_id,
            generation,
            revision,
            completeness: FrameCompleteness::Incremental,
        }) {
            Ok(()) => Ok(()),
            Err(ProtocolError::NeedsFullSnapshot) => {
                self.baseline_established = false;
                self.recover_full_snapshot(runtime, image, recovery_patch_bytes)
            }
            Err(error) => Err(error),
        }
    }

    #[cfg(test)]
    pub(crate) fn known_pixels(&self) -> u64 {
        self.coverage.covered_pixels()
    }

    #[cfg(test)]
    pub(crate) fn baseline_established(&self) -> bool {
        self.baseline_established
    }

    fn recover_full_snapshot(
        &mut self,
        runtime: &mut ProtocolRuntime,
        image: &DecodedImage,
        patch_byte_limit: usize,
    ) -> Result<(), ProtocolError> {
        self.validate_image_generation(image, self.coverage.generation)?;
        if !self.coverage.is_full() {
            return Err(ProtocolError::NeedsFullSnapshot);
        }
        let regions = full_snapshot_regions(self.coverage.size, patch_byte_limit)?;
        // 背压会清除整个待发布事务，完整快照必须共享一个修订号，
        // 只在全部有界 patch 入队后提交一次完整基线。恢复失败后也不得
        // 继续发布依赖已丢失基线的增量更新。
        self.baseline_established = false;
        for attempt in 0..2 {
            let patches = regions
                .iter()
                .cloned()
                .map(|region| {
                    extract_bgrx_patch(image, region).map_err(|_| ProtocolError::FramePortRejected)
                })
                .collect::<Result<Vec<_>, _>>()?;
            let revision = self.next_revision()?;
            let result = runtime
                .publish_surface(SurfaceUpdate::Damage {
                    session_id: self.coverage.session_id,
                    generation: self.coverage.generation,
                    revision,
                    patches,
                })
                .and_then(|()| {
                    runtime.publish_surface(SurfaceUpdate::FrameBoundary {
                        session_id: self.coverage.session_id,
                        generation: self.coverage.generation,
                        revision,
                        completeness: FrameCompleteness::FullBaseline,
                    })
                });
            match result {
                Ok(()) => break,
                Err(ProtocolError::NeedsFullSnapshot) if attempt == 0 => continue,
                Err(error) => return Err(error),
            }
        }
        self.baseline_established = true;
        Ok(())
    }

    fn validate_image_generation(
        &self,
        image: &DecodedImage,
        generation: u64,
    ) -> Result<(), ProtocolError> {
        if generation != self.coverage.generation {
            return Err(ProtocolError::StaleSurface);
        }
        let size = self.coverage.size();
        if u32::from(image.width()) != size.width || u32::from(image.height()) != size.height {
            return Err(ProtocolError::FramePortRejected);
        }
        Ok(())
    }

    fn next_revision(&mut self) -> Result<u64, ProtocolError> {
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or(ProtocolError::FramePortRejected)?;
        Ok(self.revision)
    }
}

fn full_snapshot_regions(
    size: PixelSize,
    patch_byte_limit: usize,
) -> Result<Vec<InclusiveRectangle>, ProtocolError> {
    let row_bytes = usize::try_from(size.width)
        .ok()
        .and_then(|width| width.checked_mul(4))
        .ok_or(ProtocolError::FramePortRejected)?;
    if row_bytes == 0 || patch_byte_limit < row_bytes || size.height == 0 {
        return Err(ProtocolError::FramePortRejected);
    }
    let rows_per_patch = patch_byte_limit / row_bytes;
    let mut regions = Vec::new();
    let patch_count = usize::try_from(size.height)
        .ok()
        .and_then(|height| height.checked_add(rows_per_patch - 1))
        .map(|rounded| rounded / rows_per_patch)
        .ok_or(ProtocolError::FramePortRejected)?;
    regions
        .try_reserve_exact(patch_count)
        .map_err(|_| ProtocolError::FramePortRejected)?;
    let right = u16::try_from(size.width - 1).map_err(|_| ProtocolError::FramePortRejected)?;
    let mut top = 0usize;
    let height = usize::try_from(size.height).map_err(|_| ProtocolError::FramePortRejected)?;
    while top < height {
        let bottom = top
            .checked_add(rows_per_patch)
            .map(|exclusive| exclusive.min(height))
            .and_then(|exclusive| exclusive.checked_sub(1))
            .ok_or(ProtocolError::FramePortRejected)?;
        regions.push(InclusiveRectangle {
            left: 0,
            top: u16::try_from(top).map_err(|_| ProtocolError::FramePortRejected)?,
            right,
            bottom: u16::try_from(bottom).map_err(|_| ProtocolError::FramePortRejected)?,
        });
        top = bottom + 1;
    }
    Ok(regions)
}

fn map_baseline_error(error: BaselineError) -> ProtocolError {
    match error {
        BaselineError::StaleGeneration => ProtocolError::StaleSurface,
        BaselineError::InvalidSize
        | BaselineError::InvalidRegion
        | BaselineError::AllocationFailed => ProtocolError::FramePortRejected,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{mpsc, Arc, Mutex};

    use frd_core::{PixelSize, SessionId};
    use frd_frame::{FrameCompleteness, SurfaceUpdate};
    use frd_protocol_api::{
        ProtocolError, ProtocolRuntime, RuntimeEventSink, RuntimeWake, SessionEvent,
        SurfacePublisher,
    };
    use ironrdp::graphics::image_processing::PixelFormat as IronPixelFormat;
    use ironrdp::pdu::geometry::InclusiveRectangle;
    use ironrdp::session::image::DecodedImage;

    use super::{BaselineError, CoverageTracker, RdpBaseline};

    fn region(left: u16, top: u16, right: u16, bottom: u16) -> InclusiveRectangle {
        InclusiveRectangle {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn baseline_incomplete_coverage_remains_incomplete() {
        let session_id = SessionId::allocate();
        let mut coverage = CoverageTracker::new(
            session_id,
            1,
            PixelSize {
                width: 4,
                height: 3,
            },
        )
        .expect("valid coverage");

        assert!(!coverage
            .record(session_id, 1, region(1, 1, 2, 2))
            .expect("current damage"));
        assert_eq!(coverage.covered_pixels(), 4);
    }

    #[test]
    fn baseline_overlaps_are_counted_once_before_exact_full_coverage() {
        let session_id = SessionId::allocate();
        let mut coverage = CoverageTracker::new(
            session_id,
            1,
            PixelSize {
                width: 4,
                height: 2,
            },
        )
        .expect("valid coverage");

        assert!(!coverage
            .record(session_id, 1, region(0, 0, 2, 1))
            .expect("first region"));
        assert_eq!(coverage.covered_pixels(), 6);
        assert!(coverage
            .record(session_id, 1, region(1, 0, 3, 1))
            .expect("overlapping completion"));
        assert_eq!(coverage.covered_pixels(), 8);
    }

    #[test]
    fn baseline_disjoint_regions_establish_exact_full_coverage() {
        let session_id = SessionId::allocate();
        let mut coverage = CoverageTracker::new(
            session_id,
            1,
            PixelSize {
                width: 3,
                height: 2,
            },
        )
        .expect("valid coverage");

        assert!(!coverage
            .record(session_id, 1, region(0, 0, 1, 1))
            .expect("left coverage"));
        assert!(coverage
            .record(session_id, 1, region(2, 0, 2, 1))
            .expect("right coverage"));
    }

    #[test]
    fn baseline_new_generation_resets_coverage() {
        let session_id = SessionId::allocate();
        let mut coverage = CoverageTracker::new(
            session_id,
            1,
            PixelSize {
                width: 2,
                height: 2,
            },
        )
        .expect("valid coverage");
        assert!(coverage
            .record(session_id, 1, region(0, 0, 1, 1))
            .expect("first generation full"));

        coverage
            .reset(
                session_id,
                2,
                PixelSize {
                    width: 3,
                    height: 2,
                },
            )
            .expect("new generation");

        assert_eq!(coverage.generation(), 2);
        assert_eq!(coverage.covered_pixels(), 0);
        assert!(!coverage
            .record(session_id, 2, region(0, 0, 1, 1))
            .expect("new generation partial"));
    }

    #[test]
    fn baseline_rejects_stale_generation_without_mutating_coverage() {
        let session_id = SessionId::allocate();
        let mut coverage = CoverageTracker::new(
            session_id,
            2,
            PixelSize {
                width: 2,
                height: 2,
            },
        )
        .expect("valid coverage");

        assert_eq!(
            coverage.record(session_id, 1, region(0, 0, 1, 1)),
            Err(BaselineError::StaleGeneration)
        );
        assert_eq!(coverage.covered_pixels(), 0);
    }

    #[test]
    fn baseline_fragmented_one_pixel_updates_keep_fixed_bounded_storage() {
        let session_id = SessionId::allocate();
        let mut coverage = CoverageTracker::new(
            session_id,
            1,
            PixelSize {
                width: 8192,
                height: 2048,
            },
        )
        .expect("maximum accepted surface has bounded coverage storage");
        let storage_bytes = coverage.storage_bytes();
        assert_eq!(storage_bytes, 2 * 1024 * 1024);

        for x in (0_u16..8192).step_by(2) {
            assert!(!coverage
                .record(session_id, 1, region(x, 0, x, 0))
                .expect("fragmented one-pixel update"));
        }

        assert_eq!(coverage.covered_pixels(), 4096);
        assert_eq!(coverage.storage_bytes(), storage_bytes);
    }

    #[test]
    fn baseline_mailbox_recovery_splits_patches_in_one_full_transaction() {
        let session_id = SessionId::allocate();
        let frames = Arc::new(Mutex::new(FailFirstDamageState::default()));
        let (_commands, command_rx) = mpsc::channel();
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(NoopEvents),
            Box::new(FailFirstDamageFrames(frames.clone())),
            None,
            Box::new(NoopWake),
        );
        let size = PixelSize {
            width: 4,
            height: 4,
        };
        let mut baseline =
            RdpBaseline::begin(&mut runtime, session_id, size).expect("generation begins");
        let image = DecodedImage::new(IronPixelFormat::RgbA32, 4, 4);

        baseline
            .publish_with_recovery_patch_limit(
                &mut runtime,
                &image,
                1,
                region(0, 0, 3, 3),
                &[region(0, 0, 3, 3)],
                16,
            )
            .expect("one recoverable mailbox request rebuilds a full snapshot");

        let mut state = frames.lock().expect("frame log");
        assert!(state.failed_damage);
        let updates = std::mem::take(&mut state.updates);
        drop(state);
        assert!(matches!(updates.first(), Some(SurfaceUpdate::Reset { .. })));
        let recovery = &updates[1..];
        assert_eq!(recovery.len(), 2);
        let SurfaceUpdate::Damage {
            patches, revision, ..
        } = &recovery[0]
        else {
            panic!("完整快照必须发布一个 Damage");
        };
        assert_eq!(*revision, 2);
        assert_eq!(patches.len(), 4);
        assert!(patches.iter().all(|patch| patch.pixels.len() == 16));
        assert!(matches!(
            &recovery[1],
            SurfaceUpdate::FrameBoundary {
                revision: 2,
                completeness: FrameCompleteness::FullBaseline,
                ..
            }
        ));
        assert_complete_snapshot(&updates, session_id, size);
        assert!(baseline.baseline_established());
    }

    #[test]
    fn baseline_waits_for_exact_coverage_before_publishing_full_baseline() {
        let session_id = SessionId::allocate();
        let frames = Arc::new(Mutex::new(FailFirstDamageState {
            failed_damage: true,
            updates: Vec::new(),
        }));
        let (_commands, command_rx) = mpsc::channel();
        let mut runtime = ProtocolRuntime::new(
            session_id,
            command_rx,
            Box::new(NoopEvents),
            Box::new(FailFirstDamageFrames(frames.clone())),
            None,
            Box::new(NoopWake),
        );
        let size = PixelSize {
            width: 4,
            height: 4,
        };
        let mut baseline =
            RdpBaseline::begin(&mut runtime, session_id, size).expect("generation begins");
        let image = DecodedImage::new(IronPixelFormat::RgbA32, 4, 4);

        baseline
            .publish_with_recovery_patch_limit(
                &mut runtime,
                &image,
                1,
                region(1, 1, 1, 1),
                &[region(1, 1, 1, 1)],
                16,
            )
            .expect("partial first update is buffered");

        {
            let state = frames.lock().expect("frame log");
            assert_eq!(
                state
                    .updates
                    .iter()
                    .filter(|update| matches!(update, SurfaceUpdate::FrameBoundary { .. }))
                    .count(),
                0
            );
        }

        baseline
            .publish_with_recovery_patch_limit(
                &mut runtime,
                &image,
                1,
                region(0, 0, 3, 3),
                &[region(0, 0, 3, 3)],
                16,
            )
            .expect("exact coverage publishes the complete baseline");

        let state = frames.lock().expect("frame log");
        let boundaries = state
            .updates
            .iter()
            .filter_map(|update| match update {
                SurfaceUpdate::FrameBoundary { completeness, .. } => Some(*completeness),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(boundaries, [FrameCompleteness::FullBaseline]);
        assert!(baseline.baseline_established());
    }

    #[test]
    fn baseline_snapshot_never_labels_a_partial_stripe_full() {
        let session_id = SessionId::allocate();
        let frames = Arc::new(Mutex::new(FailFirstDamageState {
            failed_damage: true,
            updates: Vec::new(),
        }));
        let (_, rx) = mpsc::channel();
        let mut runtime = ProtocolRuntime::new(
            session_id,
            rx,
            Box::new(NoopEvents),
            Box::new(FailFirstDamageFrames(frames.clone())),
            None,
            Box::new(NoopWake),
        );
        let size = PixelSize {
            width: 4,
            height: 4,
        };
        let mut baseline = RdpBaseline::begin(&mut runtime, session_id, size).unwrap();
        let image = DecodedImage::new(IronPixelFormat::RgbA32, 4, 4);
        baseline
            .publish_with_recovery_patch_limit(
                &mut runtime,
                &image,
                1,
                region(0, 0, 3, 3),
                &[region(0, 0, 3, 3)],
                16,
            )
            .unwrap();
        let state = frames.lock().unwrap();
        assert_complete_snapshot(&state.updates, session_id, size);
    }

    fn assert_complete_snapshot(updates: &[SurfaceUpdate], session_id: SessionId, size: PixelSize) {
        let mut coverage = CoverageTracker::new(session_id, 1, size).unwrap();
        let mut complete = false;
        let mut boundaries = 0;
        for update in updates {
            match update {
                SurfaceUpdate::Damage { patches, .. } => {
                    for patch in patches {
                        let r = patch.rect;
                        complete = coverage
                            .record(
                                session_id,
                                1,
                                region(
                                    r.x as u16,
                                    r.y as u16,
                                    (r.x + r.width - 1) as u16,
                                    (r.y + r.height - 1) as u16,
                                ),
                            )
                            .unwrap();
                    }
                }
                SurfaceUpdate::FrameBoundary {
                    completeness: FrameCompleteness::FullBaseline,
                    ..
                } => {
                    assert!(complete, "完整基线不得只包含快照前缀");
                    boundaries += 1;
                }
                _ => {}
            }
        }
        assert!(complete, "背压恢复后必须重新发布整个表面");
        assert_eq!(boundaries, 1);
    }

    #[test]
    fn baseline_snapshot_retries_after_boundary_discards_queued_prefix() {
        run_boundary_overflow_recovery(false, false);
    }

    #[test]
    fn baseline_established_snapshot_retries_after_boundary_overflow() {
        run_boundary_overflow_recovery(true, false);
    }

    #[test]
    fn baseline_failed_snapshot_does_not_remain_established() {
        run_boundary_overflow_recovery(true, true);
    }

    fn run_boundary_overflow_recovery(established: bool, always_fail: bool) {
        let session_id = SessionId::allocate();
        let frames = Arc::new(Mutex::new(BoundaryOverflowState::default()));
        let (_, rx) = mpsc::channel();
        let mut runtime = ProtocolRuntime::new(
            session_id,
            rx,
            Box::new(NoopEvents),
            Box::new(BoundaryOverflowFrames(frames.clone())),
            None,
            Box::new(NoopWake),
        );
        let size = PixelSize {
            width: 4,
            height: 4,
        };
        let mut baseline = RdpBaseline::begin(&mut runtime, session_id, size).unwrap();
        let image = DecodedImage::new(IronPixelFormat::RgbA32, 4, 4);
        if established {
            baseline
                .publish_with_recovery_patch_limit(
                    &mut runtime,
                    &image,
                    1,
                    region(0, 0, 3, 3),
                    &[region(0, 0, 3, 3)],
                    16,
                )
                .unwrap();
            frames.lock().unwrap().updates.clear();
        }
        {
            let mut state = frames.lock().unwrap();
            state.fail_boundaries = if always_fail { 2 } else { 1 };
        }
        baseline
            .coverage
            .record(session_id, 1, region(0, 0, 3, 3))
            .unwrap();
        let result = baseline.recover_full_snapshot(&mut runtime, &image, 16);
        let state = frames.lock().unwrap();
        if always_fail {
            assert_eq!(result, Err(ProtocolError::NeedsFullSnapshot));
            assert!(!baseline.baseline_established());
        } else {
            result.expect("一次背压必须恢复整个快照");
            assert_complete_snapshot(&state.updates, session_id, size);
            assert!(baseline.baseline_established());
            assert!(state.revisions.windows(2).all(|pair| pair[0] < pair[1]));
        }
    }

    #[derive(Default)]
    struct BoundaryOverflowState {
        updates: Vec<SurfaceUpdate>,
        revisions: Vec<u64>,
        fail_boundaries: usize,
    }
    struct BoundaryOverflowFrames(Arc<Mutex<BoundaryOverflowState>>);
    impl SurfacePublisher for BoundaryOverflowFrames {
        fn publish(&self, update: SurfaceUpdate) -> Result<(), ProtocolError> {
            let mut state = self.0.lock().unwrap();
            if let SurfaceUpdate::Damage { revision, .. } = &update {
                state.revisions.push(*revision);
            }
            if matches!(update, SurfaceUpdate::FrameBoundary { .. }) && state.fail_boundaries > 0 {
                state.fail_boundaries -= 1;
                state
                    .updates
                    .retain(|item| matches!(item, SurfaceUpdate::Reset { .. }));
                return Err(ProtocolError::NeedsFullSnapshot);
            }
            state.updates.push(update);
            Ok(())
        }
    }

    #[test]
    fn baseline_real_mailbox_compiles_complete_colored_startup() {
        real_mailbox_colored_snapshot(false);
    }

    #[test]
    fn baseline_real_mailbox_overflow_rebuilds_complete_colored_startup() {
        real_mailbox_colored_snapshot(true);
    }

    fn real_mailbox_colored_snapshot(prefill: bool) {
        use frd_frame::{FrameMailbox, FrameTransaction, FrameTransactionCompiler};
        use frd_protocol_api::MailboxSurfacePublisher;
        use ironrdp::pdu::bitmap::{BitmapData, BitmapUpdateData, Compression};
        use ironrdp::session::fast_path::ProcessorBuilder;

        let session_id = SessionId::allocate();
        let size = PixelSize {
            width: 4,
            height: 4,
        };
        let mailbox = Arc::new(Mutex::new(FrameMailbox::new(3, 64)));
        let (_, rx) = mpsc::channel();
        let mut runtime = ProtocolRuntime::new(
            session_id,
            rx,
            Box::new(NoopEvents),
            Box::new(MailboxSurfacePublisher::new(mailbox.clone())),
            None,
            Box::new(NoopWake),
        );
        let mut baseline = RdpBaseline::begin(&mut runtime, session_id, size).unwrap();
        let expected: Vec<u8> = (0..4u8)
            .flat_map(|y| (0..4u8).flat_map(move |x| [11 + x, 31 + y, 71 + x + 4 * y, 255]))
            .collect();
        // 原生 Bitmap 是自下而上的 BGRX；由真实 IronRDP 解码入口填充 image。
        let wire: Vec<u8> = expected.chunks_exact(16).rev().flatten().copied().collect();
        let mut image = DecodedImage::new(IronPixelFormat::RgbA32, 4, 4);
        ProcessorBuilder {
            io_channel_id: 1,
            user_channel_id: 2,
            share_id: 1,
            enable_server_pointer: false,
            pointer_software_rendering: false,
            bulk_decompressor: None,
        }
        .build()
        .process_bitmap_update(
            &mut image,
            BitmapUpdateData {
                rectangles: vec![BitmapData {
                    rectangle: region(0, 0, 3, 3),
                    width: 4,
                    height: 4,
                    bits_per_pixel: 32,
                    compression_flags: Compression::empty(),
                    compressed_data_header: None,
                    bitmap_data: &wire,
                }],
            },
        )
        .unwrap();
        if prefill {
            let revision = baseline.next_revision().unwrap();
            runtime
                .publish_surface(SurfaceUpdate::Damage {
                    session_id,
                    generation: 1,
                    revision,
                    patches: vec![
                        crate::surface::extract_bgrx_patch(&image, region(0, 0, 0, 0)).unwrap(),
                    ],
                })
                .unwrap();
            runtime
                .publish_surface(SurfaceUpdate::FrameBoundary {
                    session_id,
                    generation: 1,
                    revision,
                    completeness: FrameCompleteness::FullBaseline,
                })
                .unwrap();
            assert_eq!(mailbox.lock().unwrap().len(), 3);
        }
        baseline
            .publish_with_recovery_patch_limit(
                &mut runtime,
                &image,
                1,
                region(0, 0, 3, 3),
                &[region(0, 0, 3, 3)],
                16,
            )
            .unwrap();
        let mut mailbox = mailbox.lock().unwrap();
        assert_eq!(mailbox.len(), 3);
        assert_eq!(mailbox.queued_pixel_bytes(), 64);
        let mut compiler = FrameTransactionCompiler::new(session_id);
        let transactions = compiler
            .compile(std::iter::from_fn(|| mailbox.pop_enqueued()))
            .unwrap();
        assert_eq!(transactions.len(), 1);
        let FrameTransaction::Startup {
            reset, revision, ..
        } = &transactions[0]
        else {
            panic!("首次呈现必须是完整 Startup");
        };
        assert_eq!(reset.size, size);
        assert_eq!(revision.revision, if prefill { 3 } else { 1 });
        assert_eq!(revision.completeness, FrameCompleteness::FullBaseline);
        assert_eq!(revision.patches.len(), 4);
        let mut actual = vec![0u8; 64];
        for patch in &revision.patches {
            for row in 0..patch.rect.height as usize {
                let source = row * patch.stride_bytes as usize;
                let target = ((patch.rect.y as usize + row) * 4 + patch.rect.x as usize) * 4;
                let bytes = patch.rect.width as usize * 4;
                actual[target..target + bytes]
                    .copy_from_slice(&patch.pixels.as_bytes()[source..source + bytes]);
            }
        }
        assert_eq!(actual, expected, "原子快照必须保留每个彩色像素及行位置");
        assert!(baseline.baseline_established());
    }

    #[test]
    fn baseline_recovery_cannot_certify_unknown_image_pixels() {
        let session_id = SessionId::allocate();
        let frames = Arc::new(Mutex::new(FailFirstDamageState {
            failed_damage: true,
            updates: Vec::new(),
        }));
        let (_, rx) = mpsc::channel();
        let mut runtime = ProtocolRuntime::new(
            session_id,
            rx,
            Box::new(NoopEvents),
            Box::new(FailFirstDamageFrames(frames.clone())),
            None,
            Box::new(NoopWake),
        );
        let mut baseline = RdpBaseline::begin(
            &mut runtime,
            session_id,
            PixelSize {
                width: 3,
                height: 1,
            },
        )
        .unwrap();
        let image = DecodedImage::new(IronPixelFormat::RgbA32, 3, 1);
        assert_eq!(
            baseline.recover_full_snapshot(&mut runtime, &image, 12),
            Err(ProtocolError::NeedsFullSnapshot)
        );
        assert_eq!(baseline.coverage.covered_pixels(), 0);
        assert!(!baseline.baseline_established());
        assert_eq!(frames.lock().unwrap().updates.len(), 1);
    }

    #[test]
    fn baseline_explicit_content_validates_all_rectangles_before_recording_any_prefix() {
        let session_id = SessionId::allocate();
        let frames = Arc::new(Mutex::new(FailFirstDamageState {
            failed_damage: true,
            updates: Vec::new(),
        }));
        let (_, rx) = mpsc::channel();
        let mut runtime = ProtocolRuntime::new(
            session_id,
            rx,
            Box::new(NoopEvents),
            Box::new(FailFirstDamageFrames(frames.clone())),
            None,
            Box::new(NoopWake),
        );
        let mut baseline = RdpBaseline::begin(
            &mut runtime,
            session_id,
            PixelSize {
                width: 3,
                height: 1,
            },
        )
        .unwrap();
        let image = DecodedImage::new(IronPixelFormat::RgbA32, 3, 1);
        for content in [
            vec![region(0, 0, 0, 0), region(3, 0, 3, 0)],
            vec![region(0, 0, 0, 0), region(2, 0, 1, 0)],
        ] {
            assert_eq!(
                baseline.publish(&mut runtime, &image, 1, region(0, 0, 2, 0), &content),
                Err(ProtocolError::FramePortRejected)
            );
            assert_eq!(baseline.known_pixels(), 0);
            assert_eq!(baseline.revision, 0);
            assert_eq!(frames.lock().unwrap().updates.len(), 1);
        }
        baseline
            .publish(
                &mut runtime,
                &image,
                1,
                region(0, 0, 2, 0),
                &[region(0, 0, 0, 0), region(2, 0, 2, 0)],
            )
            .unwrap();
        assert_eq!(baseline.known_pixels(), 2);
        assert!(!baseline.baseline_established());
    }

    #[test]
    fn baseline_real_mailbox_overflow_cannot_recover_incomplete_content() {
        use frd_frame::{FrameMailbox, FrameTransactionCompiler};
        use frd_protocol_api::MailboxSurfacePublisher;
        let session_id = SessionId::allocate();
        let mailbox = Arc::new(Mutex::new(FrameMailbox::new(2, 12)));
        let (_, rx) = mpsc::channel();
        let mut runtime = ProtocolRuntime::new(
            session_id,
            rx,
            Box::new(NoopEvents),
            Box::new(MailboxSurfacePublisher::new(mailbox.clone())),
            None,
            Box::new(NoopWake),
        );
        let mut baseline = RdpBaseline::begin(
            &mut runtime,
            session_id,
            PixelSize {
                width: 3,
                height: 1,
            },
        )
        .unwrap();
        let mut image = DecodedImage::new(IronPixelFormat::RgbA32, 3, 1);
        let mut stage = ironrdp::session::ActiveStageBuilder {
            static_channels: ironrdp::svc::StaticChannelSet::new(),
            user_channel_id: 1001,
            io_channel_id: 1003,
            message_channel_id: None,
            share_id: 1,
            compression_type: None,
            enable_server_pointer: false,
            pointer_software_rendering: false,
        }
        .build();
        use ironrdp::core::encode_vec;
        use ironrdp::pdu::fast_path::{
            EncryptionFlags, FastPathHeader, FastPathUpdatePdu, Fragmentation, UpdateCode,
        };
        let data = encode_vec(&ironrdp::pdu::bitmap::BitmapUpdateData {
            rectangles: vec![ironrdp::pdu::bitmap::BitmapData {
                rectangle: region(0, 0, 0, 0),
                width: 1,
                height: 1,
                bits_per_pixel: 24,
                compression_flags: ironrdp::pdu::bitmap::Compression::empty(),
                compressed_data_header: None,
                bitmap_data: &[1, 2, 3, 0],
            }],
        })
        .unwrap();
        let payload = encode_vec(&FastPathUpdatePdu {
            fragmentation: Fragmentation::Single,
            update_code: UpdateCode::Bitmap,
            compression_flags: None,
            compression_type: None,
            data: &data,
        })
        .unwrap();
        let mut packet = encode_vec(&FastPathHeader::new(
            EncryptionFlags::empty(),
            payload.len(),
        ))
        .unwrap();
        packet.extend(payload);
        let result = stage
            .process_with_decoded_coverage(&mut image, ironrdp::pdu::Action::FastPath, &packet)
            .unwrap();
        for receipt in &result.decoded_coverage {
            let ironrdp::session::ActiveStageOutput::GraphicsUpdate(damage) =
                &result.outputs[receipt.graphics_output_index]
            else {
                panic!("真实内容必须对应 graphic output");
            };
            baseline
                .publish(&mut runtime, &image, 1, damage.clone(), &receipt.rectangles)
                .unwrap();
        }
        assert_eq!(baseline.known_pixels(), 1);
        // 通过真实邮箱制造队列背压；它不赋予 adapter 的 DecodedImage 新内容知识。
        runtime
            .publish_surface(SurfaceUpdate::Damage {
                session_id,
                generation: 1,
                revision: 1,
                patches: vec![
                    crate::surface::extract_bgrx_patch(&image, region(0, 0, 0, 0)).unwrap(),
                ],
            })
            .unwrap();
        assert_eq!(
            runtime.publish_surface(SurfaceUpdate::FrameBoundary {
                session_id,
                generation: 1,
                revision: 1,
                completeness: FrameCompleteness::Incremental
            }),
            Err(ProtocolError::NeedsFullSnapshot)
        );
        assert_eq!(
            baseline.recover_full_snapshot(&mut runtime, &image, 12),
            Err(ProtocolError::NeedsFullSnapshot)
        );
        assert_eq!(baseline.known_pixels(), 1);
        assert!(!baseline.baseline_established());
        let mut compiler = FrameTransactionCompiler::new(session_id);
        assert!(compiler
            .compile(mailbox.lock().unwrap().drain_committed_updates())
            .unwrap()
            .is_empty());
    }

    struct NoopEvents;

    impl RuntimeEventSink for NoopEvents {
        fn publish(&self, _: SessionEvent) -> Result<(), ProtocolError> {
            Ok(())
        }
    }

    struct NoopWake;

    impl RuntimeWake for NoopWake {
        fn wake(&self) -> Result<(), ProtocolError> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct FailFirstDamageState {
        failed_damage: bool,
        updates: Vec<SurfaceUpdate>,
    }

    struct FailFirstDamageFrames(Arc<Mutex<FailFirstDamageState>>);

    impl SurfacePublisher for FailFirstDamageFrames {
        fn publish(&self, update: SurfaceUpdate) -> Result<(), ProtocolError> {
            let mut state = self
                .0
                .lock()
                .map_err(|_| ProtocolError::FramePortRejected)?;
            if matches!(update, SurfaceUpdate::Damage { .. }) && !state.failed_damage {
                state.failed_damage = true;
                return Err(ProtocolError::NeedsFullSnapshot);
            }
            state.updates.push(update);
            Ok(())
        }
    }
}

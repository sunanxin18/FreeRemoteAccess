use frd_core::{PixelSize, SessionId};
use frd_frame::{FrameCompleteness, PixelFormat, SurfaceUpdate};
use frd_protocol_api::{ProtocolError, ProtocolRuntime};
use ironrdp::pdu::geometry::InclusiveRectangle;
use ironrdp::session::image::DecodedImage;

use crate::surface::extract_bgrx_patch;

const FULL_SNAPSHOT_PATCH_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BaselineError {
    InvalidSize,
    InvalidRegion,
    StaleGeneration,
    AllocationFailed,
}

#[derive(Clone, Copy, Debug)]
struct CoveredSpan {
    start: u32,
    end: u32,
}

pub(crate) struct CoverageTracker {
    session_id: SessionId,
    generation: u64,
    size: PixelSize,
    rows: Vec<Vec<CoveredSpan>>,
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
        let row_count = usize::try_from(size.height).map_err(|_| BaselineError::InvalidSize)?;
        let mut rows = Vec::new();
        rows.try_reserve_exact(row_count)
            .map_err(|_| BaselineError::AllocationFailed)?;
        rows.resize_with(row_count, Vec::new);
        Ok(Self {
            session_id,
            generation,
            size,
            rows,
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

        let start = u32::from(region.left);
        let end = u32::from(region.right) + 1;
        for y in region.top..=region.bottom {
            let spans = &mut self.rows[usize::from(y)];
            let mut insert_at = 0;
            while insert_at < spans.len() && spans[insert_at].end < start {
                insert_at += 1;
            }

            let mut merged_start = start;
            let mut merged_end = end;
            let mut removed_width = 0u64;
            while insert_at < spans.len() && spans[insert_at].start <= merged_end {
                let span = spans.remove(insert_at);
                merged_start = merged_start.min(span.start);
                merged_end = merged_end.max(span.end);
                removed_width = removed_width
                    .checked_add(u64::from(span.end - span.start))
                    .ok_or(BaselineError::InvalidRegion)?;
            }
            spans
                .try_reserve(1)
                .map_err(|_| BaselineError::AllocationFailed)?;
            spans.insert(
                insert_at,
                CoveredSpan {
                    start: merged_start,
                    end: merged_end,
                },
            );
            let merged_width = u64::from(merged_end - merged_start);
            self.covered_pixels = self
                .covered_pixels
                .checked_sub(removed_width)
                .and_then(|covered| covered.checked_add(merged_width))
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

    fn mark_full(&mut self) {
        for row in &mut self.rows {
            row.clear();
            row.push(CoveredSpan {
                start: 0,
                end: self.size.width,
            });
        }
        self.covered_pixels = self.total_pixels();
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

    pub(crate) fn publish(
        &mut self,
        runtime: &mut ProtocolRuntime,
        image: &DecodedImage,
        generation: u64,
        region: InclusiveRectangle,
    ) -> Result<(), ProtocolError> {
        self.publish_with_recovery_patch_limit(
            runtime,
            image,
            generation,
            region,
            FULL_SNAPSHOT_PATCH_BYTES,
        )
    }

    fn publish_with_recovery_patch_limit(
        &mut self,
        runtime: &mut ProtocolRuntime,
        image: &DecodedImage,
        generation: u64,
        region: InclusiveRectangle,
        recovery_patch_bytes: usize,
    ) -> Result<(), ProtocolError> {
        self.validate_image_generation(image, generation)?;
        self.coverage
            .validate_current_region(self.coverage.session_id, generation, &region)
            .map_err(map_baseline_error)?;
        let patch = extract_bgrx_patch(image, region.clone())
            .map_err(|_| ProtocolError::FramePortRejected)?;
        let revision = self.next_revision()?;
        match runtime.publish_surface(SurfaceUpdate::Damage {
            session_id: self.coverage.session_id,
            generation,
            revision,
            patches: vec![patch],
        }) {
            Ok(()) => {}
            Err(ProtocolError::NeedsFullSnapshot) => {
                return self.recover_full_snapshot(runtime, image, recovery_patch_bytes);
            }
            Err(error) => return Err(error),
        }

        let is_full = self
            .coverage
            .record(self.coverage.session_id, generation, region)
            .map_err(map_baseline_error)?;
        let completeness = if !self.baseline_established && is_full {
            FrameCompleteness::FullBaseline
        } else {
            FrameCompleteness::Incremental
        };
        match runtime.publish_surface(SurfaceUpdate::FrameBoundary {
            session_id: self.coverage.session_id,
            generation,
            revision,
            completeness,
        }) {
            Ok(()) => {}
            Err(ProtocolError::NeedsFullSnapshot) => {
                return self.recover_full_snapshot(runtime, image, recovery_patch_bytes);
            }
            Err(error) => return Err(error),
        }
        if completeness == FrameCompleteness::FullBaseline {
            self.baseline_established = true;
        }
        Ok(())
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
        let regions = full_snapshot_regions(self.coverage.size, patch_byte_limit)?;
        let last = regions
            .len()
            .checked_sub(1)
            .ok_or(ProtocolError::FramePortRejected)?;
        for (index, region) in regions.into_iter().enumerate() {
            let patch =
                extract_bgrx_patch(image, region).map_err(|_| ProtocolError::FramePortRejected)?;
            let revision = self.next_revision()?;
            runtime.publish_surface(SurfaceUpdate::Damage {
                session_id: self.coverage.session_id,
                generation: self.coverage.generation,
                revision,
                patches: vec![patch],
            })?;
            let completeness = if index == last {
                FrameCompleteness::FullBaseline
            } else {
                FrameCompleteness::Incremental
            };
            runtime.publish_surface(SurfaceUpdate::FrameBoundary {
                session_id: self.coverage.session_id,
                generation: self.coverage.generation,
                revision,
                completeness,
            })?;
        }
        self.coverage.mark_full();
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
    fn baseline_mailbox_recovery_splits_patches_and_marks_only_final_boundary_full() {
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
            .publish_with_recovery_patch_limit(&mut runtime, &image, 1, region(1, 1, 2, 2), 16)
            .expect("one recoverable mailbox request rebuilds a full snapshot");

        let mut state = frames.lock().expect("frame log");
        assert!(state.failed_damage);
        let updates = std::mem::take(&mut state.updates);
        drop(state);
        assert!(matches!(updates.first(), Some(SurfaceUpdate::Reset { .. })));
        let recovery = &updates[1..];
        assert_eq!(recovery.len(), 8);
        for (index, pair) in recovery.chunks_exact(2).enumerate() {
            let SurfaceUpdate::Damage {
                generation,
                revision,
                patches,
                ..
            } = &pair[0]
            else {
                panic!("recovery must alternate Damage and FrameBoundary");
            };
            assert_eq!(*generation, 1);
            assert_eq!(*revision, index as u64 + 2);
            assert_eq!(patches.len(), 1);
            assert_eq!(patches[0].pixels.len(), 16);

            let SurfaceUpdate::FrameBoundary {
                revision,
                completeness,
                ..
            } = &pair[1]
            else {
                panic!("recovery must alternate Damage and FrameBoundary");
            };
            assert_eq!(*revision, index as u64 + 2);
            let expected = if index == 3 {
                FrameCompleteness::FullBaseline
            } else {
                FrameCompleteness::Incremental
            };
            assert_eq!(*completeness, expected);
        }
        assert!(baseline.baseline_established());
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

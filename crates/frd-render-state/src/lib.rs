//! 平台无关的远端更新规划、上传校验与呈现回执状态。
//! GPU 执行和成功确认仍由后端负责；此模块只规划并暂存元数据。

use frd_core::{PixelRect, PixelSize, SessionId};
#[cfg(test)]
use frd_frame::SurfaceUpdate;
use frd_frame::{FrameCompleteness, FrameTransaction, PixelFormat, PixelPatch};

// 纯状态错误；设备和目标纹理错误由各后端独立映射。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransactionError {
    EmptyBatch,
    StaleUpdate,
    InvalidGeometry,
    TextureBudgetExceeded,
    UnsupportedPixelFormat,
    NonMonotonicRevision,
    BoundaryWithoutMatchingDamage,
    InvalidPatch,
    ResetRequired,
    StalePresentationReceipt,
}

const MAX_REMOTE_TEXTURE_BYTES: u64 = 256 * 1024 * 1024;
const BYTES_PER_PIXEL: u32 = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PresentationReceipt {
    pub session_id: SessionId,
    pub generation: u64,
    pub revision: u64,
    pub completeness: FrameCompleteness,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameBatchIdentity {
    pub session_id: SessionId,
    pub generation: u64,
    pub revision: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InstalledSurface {
    pub session_id: SessionId,
    pub generation: u64,
    pub size: PixelSize,
    pub format: PixelFormat,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BatchApplyOutcome {
    pub installed_surface: Option<InstalledSurface>,
    pub uploaded_rectangles: usize,
    pub had_texture_writes: bool,
    pub final_boundary: Option<PresentationReceipt>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UploadDescriptor {
    rect: PixelRect,
    stride_bytes: u32,
    byte_len: usize,
}

#[derive(Debug)]
pub enum PlannedUpdateData {
    StartupReset {
        session_id: SessionId,
        generation: u64,
        size: PixelSize,
        format: PixelFormat,
    },
    Damage {
        revision: u64,
        patches: Vec<PixelPatch>,
    },
    Boundary(PresentationReceipt),
}

#[derive(Debug)]
pub struct PlannedUpdate {
    uploads: Vec<UploadDescriptor>,
    data: PlannedUpdateData,
}

impl UploadDescriptor {
    pub fn rect(&self) -> PixelRect {
        self.rect
    }
    pub fn stride_bytes(&self) -> u32 {
        self.stride_bytes
    }
    pub fn byte_len(&self) -> usize {
        self.byte_len
    }
}

impl PlannedUpdate {
    pub fn data(&self) -> &PlannedUpdateData {
        &self.data
    }
    pub fn uploads(&self) -> &[UploadDescriptor] {
        &self.uploads
    }
}

#[derive(Clone, Copy, Debug)]
struct RemoteIdentity {
    session_id: SessionId,
    generation: u64,
    size: PixelSize,
    format: PixelFormat,
    last_damage_revision: u64,
    last_boundary_revision: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryRequirement {
    ResetAndFullSnapshot {
        session_id: SessionId,
        generation: u64,
    },
}

#[derive(Clone, Default)]
struct StateSnapshot {
    current: Option<RemoteIdentity>,
    pending_receipt: Option<PresentationReceipt>,
    unpresented_full_baseline: bool,
    baseline_presented: bool,
    recovery: Option<RecoveryRequirement>,
}

struct PlannedBatch {
    identity: FrameBatchIdentity,
    staged_state: StateSnapshot,
    operations: Vec<PlannedUpdate>,
    installed_surface: Option<InstalledSurface>,
    uploaded_rectangles: usize,
    had_texture_writes: bool,
    final_boundary: Option<PresentationReceipt>,
}

#[derive(Debug)]
pub struct BatchPlanningFailure {
    pub identity: Option<FrameBatchIdentity>,
    pub error: TransactionError,
}

impl StateSnapshot {
    fn clear(&mut self) {
        *self = Self::default();
    }
    #[cfg(test)]
    fn plan(&self, update: SurfaceUpdate) -> Result<PlannedUpdate, TransactionError> {
        match update {
            SurfaceUpdate::Reset {
                session_id,
                generation,
                size,
                format,
            } => self.plan_reset(session_id, generation, size, format),
            SurfaceUpdate::Damage {
                session_id,
                generation,
                revision,
                patches,
            } => self.plan_damage(session_id, generation, revision, patches),
            SurfaceUpdate::FrameBoundary {
                session_id,
                generation,
                revision,
                completeness,
            } => self.plan_boundary(session_id, generation, revision, completeness),
        }
    }

    fn plan_batch(
        &self,
        transactions: Vec<FrameTransaction>,
    ) -> Result<PlannedBatch, BatchPlanningFailure> {
        let mut staged_state = self.clone();
        let mut operations = Vec::new();
        let mut installed_surface = None;
        let mut uploaded_rectangles = 0_usize;
        let mut final_identity = None;

        for transaction in transactions {
            let identity = transaction_identity(&transaction);
            final_identity = Some(identity);
            let planned = (|| -> Result<Vec<PlannedUpdate>, TransactionError> {
                match transaction {
                    FrameTransaction::Startup {
                        reset, revision, ..
                    } => {
                        if revision.completeness != FrameCompleteness::FullBaseline {
                            return Err(TransactionError::BoundaryWithoutMatchingDamage);
                        }
                        let reset_plan = staged_state.plan_reset(
                            reset.session_id,
                            reset.generation,
                            reset.size,
                            reset.format,
                        )?;
                        staged_state.commit_metadata(&reset_plan);
                        installed_surface = Some(InstalledSurface {
                            session_id: reset.session_id,
                            generation: reset.generation,
                            size: reset.size,
                            format: reset.format,
                        });
                        let damage_plan = staged_state.plan_damage(
                            revision.session_id,
                            revision.generation,
                            revision.revision,
                            revision.patches,
                        )?;
                        let rectangles = damage_plan.uploads().len();
                        uploaded_rectangles = uploaded_rectangles
                            .checked_add(rectangles)
                            .ok_or(TransactionError::InvalidPatch)?;
                        staged_state.commit_metadata(&damage_plan);
                        let boundary_plan = staged_state.plan_boundary(
                            revision.session_id,
                            revision.generation,
                            revision.revision,
                            revision.completeness,
                        )?;
                        staged_state.commit_metadata(&boundary_plan);
                        Ok(vec![reset_plan, damage_plan, boundary_plan])
                    }
                    FrameTransaction::Revision { revision, .. } => {
                        let damage_plan = staged_state.plan_damage(
                            revision.session_id,
                            revision.generation,
                            revision.revision,
                            revision.patches,
                        )?;
                        let rectangles = damage_plan.uploads().len();
                        uploaded_rectangles = uploaded_rectangles
                            .checked_add(rectangles)
                            .ok_or(TransactionError::InvalidPatch)?;
                        staged_state.commit_metadata(&damage_plan);
                        let boundary_plan = staged_state.plan_boundary(
                            revision.session_id,
                            revision.generation,
                            revision.revision,
                            revision.completeness,
                        )?;
                        staged_state.commit_metadata(&boundary_plan);
                        Ok(vec![damage_plan, boundary_plan])
                    }
                }
            })()
            .map_err(|error| BatchPlanningFailure {
                identity: Some(identity),
                error,
            })?;
            operations.extend(planned);
        }

        let identity = final_identity.ok_or(BatchPlanningFailure {
            identity: None,
            error: TransactionError::EmptyBatch,
        })?;
        let final_boundary = staged_state.pending_receipt();
        Ok(PlannedBatch {
            identity,
            staged_state,
            operations,
            installed_surface,
            uploaded_rectangles,
            had_texture_writes: uploaded_rectangles != 0,
            final_boundary,
        })
    }

    #[cfg(test)]
    fn commit(&mut self, plan: PlannedUpdate) {
        self.commit_metadata(&plan);
    }

    fn commit_metadata(&mut self, plan: &PlannedUpdate) {
        match &plan.data {
            PlannedUpdateData::StartupReset {
                session_id,
                generation,
                size,
                format,
            } => {
                self.current = Some(RemoteIdentity {
                    session_id: *session_id,
                    generation: *generation,
                    size: *size,
                    format: *format,
                    last_damage_revision: 0,
                    last_boundary_revision: 0,
                });
                self.pending_receipt = None;
                self.unpresented_full_baseline = false;
                self.baseline_presented = false;
                self.recovery = None;
            }
            PlannedUpdateData::Damage { revision, .. } => {
                let current = self
                    .current
                    .as_mut()
                    .expect("damage plan requires reset state");
                current.last_damage_revision = *revision;
                self.pending_receipt = None;
            }
            PlannedUpdateData::Boundary(receipt) => {
                let current = self
                    .current
                    .as_mut()
                    .expect("boundary plan requires reset state");
                current.last_boundary_revision = receipt.revision;
                if receipt.completeness == FrameCompleteness::FullBaseline {
                    self.unpresented_full_baseline = true;
                }
                self.pending_receipt = Some(*receipt);
            }
        }
    }

    fn pending_receipt(&self) -> Option<PresentationReceipt> {
        self.pending_receipt
    }

    #[cfg(test)]
    fn last_damage_revision(&self) -> u64 {
        self.current
            .map_or(0, |current| current.last_damage_revision)
    }

    #[cfg(test)]
    fn baseline_presented(&self) -> bool {
        self.baseline_presented
    }

    fn confirm_presented(&mut self, receipt: PresentationReceipt) -> Result<(), TransactionError> {
        if self.pending_receipt != Some(receipt) {
            return Err(TransactionError::StalePresentationReceipt);
        }
        self.pending_receipt = None;
        if receipt.completeness == FrameCompleteness::FullBaseline {
            self.unpresented_full_baseline = false;
            self.baseline_presented = true;
        }
        Ok(())
    }

    fn invalidate_for_device_loss(&mut self) -> RecoveryRequirement {
        let current = self.current.take().expect("设备恢复要求当前远端纹理");
        let recovery = RecoveryRequirement::ResetAndFullSnapshot {
            session_id: current.session_id,
            generation: current.generation,
        };
        self.pending_receipt = None;
        self.unpresented_full_baseline = false;
        self.baseline_presented = false;
        self.recovery = Some(recovery);
        recovery
    }

    fn plan_reset(
        &self,
        session_id: SessionId,
        generation: u64,
        size: PixelSize,
        format: PixelFormat,
    ) -> Result<PlannedUpdate, TransactionError> {
        if format != PixelFormat::Bgrx8UnormSrgb {
            return Err(TransactionError::UnsupportedPixelFormat);
        }
        if generation == 0 || size.width == 0 || size.height == 0 {
            return Err(TransactionError::InvalidGeometry);
        }
        let texture_bytes = u64::from(size.width)
            .checked_mul(u64::from(size.height))
            .and_then(|pixels| pixels.checked_mul(u64::from(BYTES_PER_PIXEL)))
            .ok_or(TransactionError::InvalidGeometry)?;
        if texture_bytes > MAX_REMOTE_TEXTURE_BYTES {
            return Err(TransactionError::TextureBudgetExceeded);
        }

        if let Some(current) = self.current {
            let advances_current =
                session_id == current.session_id && generation > current.generation;
            let starts_newer_session = session_id.get() > current.session_id.get();
            if !advances_current && !starts_newer_session {
                return Err(TransactionError::StaleUpdate);
            }
        }
        if let Some(RecoveryRequirement::ResetAndFullSnapshot {
            session_id: required_session,
            generation: required_generation,
        }) = self.recovery
        {
            if session_id != required_session || generation != required_generation {
                return Err(TransactionError::StaleUpdate);
            }
        }

        Ok(PlannedUpdate {
            uploads: Vec::new(),
            data: PlannedUpdateData::StartupReset {
                session_id,
                generation,
                size,
                format,
            },
        })
    }

    fn plan_damage(
        &self,
        session_id: SessionId,
        generation: u64,
        revision: u64,
        patches: Vec<PixelPatch>,
    ) -> Result<PlannedUpdate, TransactionError> {
        let current = self.current.ok_or(TransactionError::ResetRequired)?;
        if current.format != PixelFormat::Bgrx8UnormSrgb {
            return Err(TransactionError::UnsupportedPixelFormat);
        }
        if session_id != current.session_id || generation != current.generation {
            return Err(TransactionError::StaleUpdate);
        }
        if revision == 0 || revision <= current.last_damage_revision {
            return Err(TransactionError::NonMonotonicRevision);
        }
        if patches.is_empty() {
            return Err(TransactionError::InvalidPatch);
        }

        let uploads = patches
            .iter()
            .map(|patch| validate_patch(patch, current.size))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(PlannedUpdate {
            uploads,
            data: PlannedUpdateData::Damage { revision, patches },
        })
    }

    fn plan_boundary(
        &self,
        session_id: SessionId,
        generation: u64,
        revision: u64,
        completeness: FrameCompleteness,
    ) -> Result<PlannedUpdate, TransactionError> {
        let current = self.current.ok_or(TransactionError::ResetRequired)?;
        if session_id != current.session_id || generation != current.generation {
            return Err(TransactionError::StaleUpdate);
        }
        if revision == 0
            || revision != current.last_damage_revision
            || revision <= current.last_boundary_revision
        {
            return Err(TransactionError::BoundaryWithoutMatchingDamage);
        }
        let completeness = if self.unpresented_full_baseline {
            FrameCompleteness::FullBaseline
        } else {
            completeness
        };
        Ok(PlannedUpdate {
            uploads: Vec::new(),
            data: PlannedUpdateData::Boundary(PresentationReceipt {
                session_id,
                generation,
                revision,
                completeness,
            }),
        })
    }
}

pub fn transaction_identity(transaction: &FrameTransaction) -> FrameBatchIdentity {
    match transaction {
        FrameTransaction::Startup {
            reset, revision, ..
        } => FrameBatchIdentity {
            session_id: reset.session_id,
            generation: reset.generation,
            revision: revision.revision,
        },
        FrameTransaction::Revision { revision, .. } => FrameBatchIdentity {
            session_id: revision.session_id,
            generation: revision.generation,
            revision: revision.revision,
        },
    }
}

fn validate_patch(
    patch: &PixelPatch,
    surface_size: PixelSize,
) -> Result<UploadDescriptor, TransactionError> {
    let (_, end) = patch
        .rect
        .checked_bounds()
        .ok_or(TransactionError::InvalidPatch)?;
    if end.x > surface_size.width || end.y > surface_size.height {
        return Err(TransactionError::InvalidPatch);
    }
    let minimum_stride = patch
        .rect
        .width
        .checked_mul(BYTES_PER_PIXEL)
        .ok_or(TransactionError::InvalidPatch)?;
    if patch.stride_bytes < minimum_stride {
        return Err(TransactionError::InvalidPatch);
    }
    let expected_length = usize::try_from(patch.stride_bytes)
        .ok()
        .and_then(|stride| {
            usize::try_from(patch.rect.height)
                .ok()
                .and_then(|height| stride.checked_mul(height))
        })
        .ok_or(TransactionError::InvalidPatch)?;
    if expected_length != patch.pixels.len() {
        return Err(TransactionError::InvalidPatch);
    }
    Ok(UploadDescriptor {
        rect: patch.rect,
        stride_bytes: patch.stride_bytes,
        byte_len: expected_length,
    })
}

/// 后端无关状态。候选事务独占借用它，避免旧规划覆盖新提交。
#[derive(Default)]
pub struct RemoteUpdateState {
    snapshot: StateSnapshot,
}

impl RemoteUpdateState {
    pub fn prepare_batch(
        &mut self,
        transactions: Vec<FrameTransaction>,
    ) -> Result<BatchCandidate<'_>, BatchPlanningFailure> {
        let plan = self.snapshot.plan_batch(transactions)?;
        Ok(BatchCandidate {
            destination: &mut self.snapshot,
            plan,
        })
    }
    pub fn clear(&mut self) {
        self.snapshot.clear();
    }
    pub fn pending_receipt(&self) -> Option<PresentationReceipt> {
        self.snapshot.pending_receipt()
    }
    pub fn confirm_presented(
        &mut self,
        receipt: PresentationReceipt,
    ) -> Result<(), TransactionError> {
        self.snapshot.confirm_presented(receipt)
    }
    pub fn invalidate_for_device_loss(&mut self) -> RecoveryRequirement {
        self.snapshot.invalidate_for_device_loss()
    }
    pub fn current_generation(&self) -> Option<u64> {
        self.snapshot.current.map(|current| current.generation)
    }
    pub fn last_damage_revision(&self) -> u64 {
        self.snapshot
            .current
            .map_or(0, |current| current.last_damage_revision)
    }
    pub fn baseline_presented(&self) -> bool {
        self.snapshot.baseline_presented
    }
}

/// 不可构造或复制的暂存事务。丢弃时不发布元数据。
/// 提交只保证状态身份和独占性；调用后端必须先完成自身 GPU 成功门控。
///
/// 候选不能复制后重复提交：
/// ```compile_fail
/// fn duplicate(candidate: frd_render_state::BatchCandidate<'_>) {
///     let duplicate = candidate.clone();
/// }
/// ```
/// 提交消耗候选：
/// ```compile_fail
/// fn replay(candidate: frd_render_state::BatchCandidate<'_>) {
///     candidate.commit();
///     candidate.commit();
/// }
/// ```
/// 暂存状态不向调用方开放：
/// ```compile_fail
/// fn forge(candidate: frd_render_state::BatchCandidate<'_>) {
///     let state = candidate.plan.staged_state;
/// }
/// ```
/// 经过校验的操作不可修改：
/// ```compile_fail
/// fn mutate(candidate: frd_render_state::BatchCandidate<'_>) {
///     candidate.operations()[0].data = unimplemented!();
/// }
/// ```
/// 候选存活期间不能清空或恢复原状态：
/// ```compile_fail
/// fn invalidate(state: &mut frd_render_state::RemoteUpdateState) {
///     let candidate = state.prepare_batch(vec![]).unwrap();
///     state.clear();
///     candidate.commit();
/// }
/// ```
/// 不能把候选提交给另一个状态：
/// ```compile_fail
/// fn cross_state(candidate: frd_render_state::BatchCandidate<'_>, other: &mut frd_render_state::RemoteUpdateState) {
///     candidate.commit(other);
/// }
/// ```
/// 不能伪造候选对象：
/// ```compile_fail
/// let candidate = frd_render_state::BatchCandidate { destination: unimplemented!(), plan: unimplemented!() };
/// ```
pub struct BatchCandidate<'state> {
    destination: &'state mut StateSnapshot,
    plan: PlannedBatch,
}
impl BatchCandidate<'_> {
    pub fn identity(&self) -> FrameBatchIdentity {
        self.plan.identity
    }
    pub fn operations(&self) -> &[PlannedUpdate] {
        &self.plan.operations
    }
    pub fn commit(self) -> BatchApplyOutcome {
        let outcome = BatchApplyOutcome {
            installed_surface: self.plan.installed_surface,
            uploaded_rectangles: self.plan.uploaded_rectangles,
            had_texture_writes: self.plan.had_texture_writes,
            final_boundary: self.plan.final_boundary,
        };
        *self.destination = self.plan.staged_state;
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::StateSnapshot as RemoteUpdateState;
    use super::TransactionError as RendererError;
    use super::*;
    use frd_frame::{PixelBuffer, SurfaceUpdate};
    fn reset(
        session_id: SessionId,
        generation: u64,
        size: PixelSize,
        format: PixelFormat,
    ) -> SurfaceUpdate {
        SurfaceUpdate::Reset {
            session_id,
            generation,
            size,
            format,
        }
    }

    fn damage(
        session_id: SessionId,
        generation: u64,
        revision: u64,
        rect: PixelRect,
        stride_bytes: u32,
        pixels: Vec<u8>,
    ) -> SurfaceUpdate {
        SurfaceUpdate::Damage {
            session_id,
            generation,
            revision,
            patches: vec![PixelPatch {
                rect,
                stride_bytes,
                pixels: PixelBuffer::new(pixels),
            }],
        }
    }

    fn boundary(
        session_id: SessionId,
        generation: u64,
        revision: u64,
        completeness: FrameCompleteness,
    ) -> SurfaceUpdate {
        SurfaceUpdate::FrameBoundary {
            session_id,
            generation,
            revision,
            completeness,
        }
    }

    fn pixel_rect(x: u32, y: u32, width: u32, height: u32) -> PixelRect {
        PixelRect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn state_rejects_stale_identity_and_non_monotonic_damage_or_boundary() {
        let session_id = SessionId::allocate();
        let other_session = SessionId::allocate();
        let size = PixelSize::new(4, 4).unwrap();
        let rect = pixel_rect(0, 0, 1, 1);
        let mut state = RemoteUpdateState::default();

        state.commit(
            state
                .plan(reset(session_id, 7, size, PixelFormat::Bgrx8UnormSrgb))
                .unwrap(),
        );
        state.commit(
            state
                .plan(damage(session_id, 7, 1, rect, 4, vec![0; 4]))
                .unwrap(),
        );

        assert_eq!(
            state
                .plan(damage(other_session, 7, 2, rect, 4, vec![0; 4]))
                .unwrap_err(),
            RendererError::StaleUpdate
        );
        assert_eq!(
            state
                .plan(damage(session_id, 6, 2, rect, 4, vec![0; 4]))
                .unwrap_err(),
            RendererError::StaleUpdate
        );
        assert_eq!(
            state
                .plan(damage(session_id, 7, 1, rect, 4, vec![0; 4]))
                .unwrap_err(),
            RendererError::NonMonotonicRevision
        );
        assert_eq!(
            state
                .plan(boundary(session_id, 7, 2, FrameCompleteness::Incremental))
                .unwrap_err(),
            RendererError::BoundaryWithoutMatchingDamage
        );
    }

    #[test]
    fn reset_clears_baseline_pending_receipt_and_presentation_eligibility() {
        let session_id = SessionId::allocate();
        let size = PixelSize::new(2, 2).unwrap();
        let rect = pixel_rect(0, 0, 2, 2);
        let mut state = RemoteUpdateState::default();

        for update in [
            reset(session_id, 1, size, PixelFormat::Bgrx8UnormSrgb),
            damage(session_id, 1, 1, rect, 8, vec![0; 16]),
            boundary(session_id, 1, 1, FrameCompleteness::FullBaseline),
        ] {
            let plan = state.plan(update).unwrap();
            state.commit(plan);
        }
        let receipt = state.pending_receipt().unwrap();
        state.confirm_presented(receipt).unwrap();
        assert!(state.baseline_presented());

        let plan = state
            .plan(reset(session_id, 2, size, PixelFormat::Bgrx8UnormSrgb))
            .unwrap();
        state.commit(plan);

        assert!(!state.baseline_presented());
        assert_eq!(state.pending_receipt(), None);
        assert_eq!(state.last_damage_revision(), 0);
    }

    #[test]
    fn receipts_preserve_completeness_and_confirm_only_after_present() {
        let session_id = SessionId::allocate();
        let size = PixelSize::new(2, 2).unwrap();
        let rect = pixel_rect(0, 0, 1, 1);
        let mut state = RemoteUpdateState::default();

        for update in [
            reset(session_id, 1, size, PixelFormat::Bgrx8UnormSrgb),
            damage(session_id, 1, 1, rect, 4, vec![0; 4]),
            boundary(session_id, 1, 1, FrameCompleteness::Incremental),
        ] {
            let plan = state.plan(update).unwrap();
            state.commit(plan);
        }

        let incremental = state.pending_receipt().unwrap();
        assert_eq!(incremental.completeness, FrameCompleteness::Incremental);
        assert!(!state.baseline_presented());
        state.confirm_presented(incremental).unwrap();
        assert!(!state.baseline_presented());

        for update in [
            damage(session_id, 1, 2, rect, 4, vec![0; 4]),
            boundary(session_id, 1, 2, FrameCompleteness::FullBaseline),
        ] {
            let plan = state.plan(update).unwrap();
            state.commit(plan);
        }

        let full = state.pending_receipt().unwrap();
        assert_eq!(full.completeness, FrameCompleteness::FullBaseline);
        assert!(!state.baseline_presented());
        state.confirm_presented(full).unwrap();
        assert!(state.baseline_presented());
    }

    #[test]
    fn first_present_keeps_unpresented_full_baseline_through_incremental_coalescing() {
        let session_id = SessionId::allocate();
        let size = PixelSize::new(2, 2).unwrap();
        let full_rect = pixel_rect(0, 0, 2, 2);
        let incremental_rect = pixel_rect(1, 1, 1, 1);
        let mut state = RemoteUpdateState::default();

        for update in [
            reset(session_id, 1, size, PixelFormat::Bgrx8UnormSrgb),
            damage(session_id, 1, 1, full_rect, 8, vec![0; 16]),
            boundary(session_id, 1, 1, FrameCompleteness::FullBaseline),
            damage(session_id, 1, 2, incremental_rect, 4, vec![1; 4]),
            boundary(session_id, 1, 2, FrameCompleteness::Incremental),
        ] {
            let plan = state.plan(update).unwrap();
            state.commit(plan);
        }

        let first_present = state.pending_receipt().unwrap();
        assert_eq!(first_present.revision, 2);
        assert_eq!(first_present.completeness, FrameCompleteness::FullBaseline);
        state.confirm_presented(first_present).unwrap();
        assert!(state.baseline_presented());
    }

    #[test]
    fn confirmed_baseline_does_not_promote_later_incremental_receipts() {
        let session_id = SessionId::allocate();
        let size = PixelSize::new(2, 2).unwrap();
        let rect = pixel_rect(0, 0, 2, 2);
        let mut state = RemoteUpdateState::default();

        for update in [
            reset(session_id, 1, size, PixelFormat::Bgrx8UnormSrgb),
            damage(session_id, 1, 1, rect, 8, vec![0; 16]),
            boundary(session_id, 1, 1, FrameCompleteness::FullBaseline),
        ] {
            let plan = state.plan(update).unwrap();
            state.commit(plan);
        }
        let baseline = state.pending_receipt().unwrap();
        state.confirm_presented(baseline).unwrap();

        for update in [
            damage(session_id, 1, 2, rect, 8, vec![1; 16]),
            boundary(session_id, 1, 2, FrameCompleteness::Incremental),
        ] {
            let plan = state.plan(update).unwrap();
            state.commit(plan);
        }

        let incremental = state.pending_receipt().unwrap();
        assert_eq!(incremental.revision, 2);
        assert_eq!(incremental.completeness, FrameCompleteness::Incremental);
    }

    #[test]
    fn damage_without_boundary_does_not_reuse_unpresented_baseline_receipt() {
        let session_id = SessionId::allocate();
        let size = PixelSize::new(2, 2).unwrap();
        let rect = pixel_rect(0, 0, 2, 2);
        let mut state = RemoteUpdateState::default();

        for update in [
            reset(session_id, 1, size, PixelFormat::Bgrx8UnormSrgb),
            damage(session_id, 1, 1, rect, 8, vec![0; 16]),
            boundary(session_id, 1, 1, FrameCompleteness::FullBaseline),
            damage(session_id, 1, 2, rect, 8, vec![1; 16]),
        ] {
            let plan = state.plan(update).unwrap();
            state.commit(plan);
        }

        assert_eq!(state.pending_receipt(), None);
    }

    #[test]
    fn reset_clears_unpresented_baseline_before_new_incremental_boundary() {
        let session_id = SessionId::allocate();
        let size = PixelSize::new(2, 2).unwrap();
        let rect = pixel_rect(0, 0, 2, 2);
        let mut state = RemoteUpdateState::default();

        for update in [
            reset(session_id, 1, size, PixelFormat::Bgrx8UnormSrgb),
            damage(session_id, 1, 1, rect, 8, vec![0; 16]),
            boundary(session_id, 1, 1, FrameCompleteness::FullBaseline),
            reset(session_id, 2, size, PixelFormat::Bgrx8UnormSrgb),
            damage(session_id, 2, 1, rect, 8, vec![1; 16]),
            boundary(session_id, 2, 1, FrameCompleteness::Incremental),
        ] {
            let plan = state.plan(update).unwrap();
            state.commit(plan);
        }

        let incremental = state.pending_receipt().unwrap();
        assert_eq!(incremental.generation, 2);
        assert_eq!(incremental.completeness, FrameCompleteness::Incremental);
    }

    #[test]
    fn device_loss_recovery_clears_unpresented_baseline_before_incremental_boundary() {
        let session_id = SessionId::allocate();
        let size = PixelSize::new(2, 2).unwrap();
        let rect = pixel_rect(0, 0, 2, 2);
        let mut state = RemoteUpdateState::default();

        for update in [
            reset(session_id, 1, size, PixelFormat::Bgrx8UnormSrgb),
            damage(session_id, 1, 1, rect, 8, vec![0; 16]),
            boundary(session_id, 1, 1, FrameCompleteness::FullBaseline),
        ] {
            let plan = state.plan(update).unwrap();
            state.commit(plan);
        }
        assert_eq!(
            state.invalidate_for_device_loss(),
            RecoveryRequirement::ResetAndFullSnapshot {
                session_id,
                generation: 1,
            }
        );

        for update in [
            reset(session_id, 1, size, PixelFormat::Bgrx8UnormSrgb),
            damage(session_id, 1, 1, rect, 8, vec![1; 16]),
            boundary(session_id, 1, 1, FrameCompleteness::Incremental),
        ] {
            let plan = state.plan(update).unwrap();
            state.commit(plan);
        }

        let incremental = state.pending_receipt().unwrap();
        assert_eq!(incremental.generation, 1);
        assert_eq!(incremental.completeness, FrameCompleteness::Incremental);
    }

    #[test]
    fn damage_plan_keeps_the_dirty_rectangle_and_rejects_invalid_payloads() {
        let session_id = SessionId::allocate();
        let size = PixelSize::new(4, 4).unwrap();
        let rect = pixel_rect(1, 2, 2, 1);
        let mut state = RemoteUpdateState::default();
        let plan = state
            .plan(reset(session_id, 1, size, PixelFormat::Bgrx8UnormSrgb))
            .unwrap();
        state.commit(plan);

        let plan = state
            .plan(damage(session_id, 1, 1, rect, 12, vec![0; 12]))
            .unwrap();
        assert_eq!(plan.uploads().len(), 1);
        assert_eq!(plan.uploads()[0].rect, rect);
        assert_eq!(plan.uploads()[0].stride_bytes, 12);
        assert_eq!(plan.uploads()[0].byte_len, 12);
        assert_ne!(plan.uploads()[0].rect, pixel_rect(0, 0, 4, 4));
        state.commit(plan);

        assert_eq!(
            state
                .plan(damage(session_id, 1, 2, rect, 4, vec![0; 4]))
                .unwrap_err(),
            RendererError::InvalidPatch
        );
        assert_eq!(
            state
                .plan(damage(session_id, 1, 2, rect, 12, vec![0; 8]))
                .unwrap_err(),
            RendererError::InvalidPatch
        );
        assert_eq!(
            state
                .plan(damage(
                    session_id,
                    1,
                    2,
                    pixel_rect(3, 3, 2, 1),
                    8,
                    vec![0; 8],
                ))
                .unwrap_err(),
            RendererError::InvalidPatch
        );
    }

    #[test]
    fn unsupported_format_and_device_recovery_require_a_fresh_reset_and_full_snapshot() {
        let session_id = SessionId::allocate();
        let size = PixelSize::new(2, 2).unwrap();
        let rect = pixel_rect(0, 0, 2, 2);
        let mut state = RemoteUpdateState::default();

        assert_eq!(
            state
                .plan(reset(session_id, 1, size, PixelFormat::Bgra8UnormSrgb,))
                .unwrap_err(),
            RendererError::UnsupportedPixelFormat
        );

        for update in [
            reset(session_id, 1, size, PixelFormat::Bgrx8UnormSrgb),
            damage(session_id, 1, 1, rect, 8, vec![0; 16]),
            boundary(session_id, 1, 1, FrameCompleteness::FullBaseline),
        ] {
            let plan = state.plan(update).unwrap();
            state.commit(plan);
        }

        assert_eq!(
            state.invalidate_for_device_loss(),
            RecoveryRequirement::ResetAndFullSnapshot {
                session_id,
                generation: 1,
            }
        );
        assert_eq!(state.pending_receipt(), None);
        assert_eq!(
            state
                .plan(damage(session_id, 1, 2, rect, 8, vec![0; 16]))
                .unwrap_err(),
            RendererError::ResetRequired
        );
    }
}

#[cfg(test)]
mod candidate_tests;

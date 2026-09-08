//! 平台无关的远端更新规划、上传校验与呈现回执状态。
//! GPU 执行和成功确认仍由后端负责；此模块只规划并暂存元数据。

use frd_core::{PixelRect, PixelSize, SessionId};
#[cfg(test)]
use frd_frame::SurfaceUpdate;
use frd_frame::{FrameCompleteness, FrameTransaction, PixelFormat, PixelPatch};

// 保留既有错误类型，避免本次模块提取改变公开 API。
use crate::remote_texture::RendererError;

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
pub(crate) struct UploadDescriptor {
    pub(crate) rect: PixelRect,
    pub(crate) stride_bytes: u32,
    pub(crate) byte_len: usize,
}

#[derive(Debug)]
pub(crate) enum PlannedUpdateData {
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
pub(crate) struct PlannedUpdate {
    uploads: Vec<UploadDescriptor>,
    pub(crate) data: PlannedUpdateData,
}

impl PlannedUpdate {
    pub(crate) fn uploads(&self) -> &[UploadDescriptor] {
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
pub(crate) struct RemoteUpdateState {
    current: Option<RemoteIdentity>,
    pending_receipt: Option<PresentationReceipt>,
    unpresented_full_baseline: bool,
    baseline_presented: bool,
    recovery: Option<RecoveryRequirement>,
}

pub(crate) struct PlannedBatch {
    pub(crate) identity: FrameBatchIdentity,
    pub(crate) staged_state: RemoteUpdateState,
    pub(crate) operations: Vec<PlannedUpdate>,
    pub(crate) installed_surface: Option<InstalledSurface>,
    pub(crate) uploaded_rectangles: usize,
    pub(crate) had_texture_writes: bool,
    pub(crate) final_boundary: Option<PresentationReceipt>,
}

#[derive(Debug)]
pub(crate) struct BatchPlanningFailure {
    pub(crate) identity: Option<FrameBatchIdentity>,
    pub(crate) error: RendererError,
}

impl RemoteUpdateState {
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }
    #[cfg(test)]
    pub(crate) fn plan(&self, update: SurfaceUpdate) -> Result<PlannedUpdate, RendererError> {
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

    pub(crate) fn plan_batch(
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
            let planned = (|| -> Result<Vec<PlannedUpdate>, RendererError> {
                match transaction {
                    FrameTransaction::Startup {
                        reset, revision, ..
                    } => {
                        if revision.completeness != FrameCompleteness::FullBaseline {
                            return Err(RendererError::BoundaryWithoutMatchingDamage);
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
                            .ok_or(RendererError::InvalidPatch)?;
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
                            .ok_or(RendererError::InvalidPatch)?;
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
            error: RendererError::EmptyBatch,
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
    pub(crate) fn commit(&mut self, plan: PlannedUpdate) {
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

    pub(crate) fn pending_receipt(&self) -> Option<PresentationReceipt> {
        self.pending_receipt
    }

    #[cfg(test)]
    pub(crate) fn last_damage_revision(&self) -> u64 {
        self.current
            .map_or(0, |current| current.last_damage_revision)
    }

    #[cfg(test)]
    pub(crate) fn current_generation(&self) -> Option<u64> {
        self.current.map(|current| current.generation)
    }

    #[cfg(test)]
    pub(crate) fn baseline_presented(&self) -> bool {
        self.baseline_presented
    }

    pub(crate) fn confirm_presented(
        &mut self,
        receipt: PresentationReceipt,
    ) -> Result<(), RendererError> {
        if self.pending_receipt != Some(receipt) {
            return Err(RendererError::StalePresentationReceipt);
        }
        self.pending_receipt = None;
        if receipt.completeness == FrameCompleteness::FullBaseline {
            self.unpresented_full_baseline = false;
            self.baseline_presented = true;
        }
        Ok(())
    }

    pub(crate) fn invalidate_for_device_loss(&mut self) -> RecoveryRequirement {
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
    ) -> Result<PlannedUpdate, RendererError> {
        if format != PixelFormat::Bgrx8UnormSrgb {
            return Err(RendererError::UnsupportedPixelFormat);
        }
        if generation == 0 || size.width == 0 || size.height == 0 {
            return Err(RendererError::InvalidGeometry);
        }
        let texture_bytes = u64::from(size.width)
            .checked_mul(u64::from(size.height))
            .and_then(|pixels| pixels.checked_mul(u64::from(BYTES_PER_PIXEL)))
            .ok_or(RendererError::InvalidGeometry)?;
        if texture_bytes > MAX_REMOTE_TEXTURE_BYTES {
            return Err(RendererError::TextureBudgetExceeded);
        }

        if let Some(current) = self.current {
            let advances_current =
                session_id == current.session_id && generation > current.generation;
            let starts_newer_session = session_id.get() > current.session_id.get();
            if !advances_current && !starts_newer_session {
                return Err(RendererError::StaleUpdate);
            }
        }
        if let Some(RecoveryRequirement::ResetAndFullSnapshot {
            session_id: required_session,
            generation: required_generation,
        }) = self.recovery
        {
            if session_id != required_session || generation != required_generation {
                return Err(RendererError::StaleUpdate);
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
    ) -> Result<PlannedUpdate, RendererError> {
        let current = self.current.ok_or(RendererError::ResetRequired)?;
        if current.format != PixelFormat::Bgrx8UnormSrgb {
            return Err(RendererError::UnsupportedPixelFormat);
        }
        if session_id != current.session_id || generation != current.generation {
            return Err(RendererError::StaleUpdate);
        }
        if revision == 0 || revision <= current.last_damage_revision {
            return Err(RendererError::NonMonotonicRevision);
        }
        if patches.is_empty() {
            return Err(RendererError::InvalidPatch);
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
    ) -> Result<PlannedUpdate, RendererError> {
        let current = self.current.ok_or(RendererError::ResetRequired)?;
        if session_id != current.session_id || generation != current.generation {
            return Err(RendererError::StaleUpdate);
        }
        if revision == 0
            || revision != current.last_damage_revision
            || revision <= current.last_boundary_revision
        {
            return Err(RendererError::BoundaryWithoutMatchingDamage);
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

pub(crate) fn transaction_identity(transaction: &FrameTransaction) -> FrameBatchIdentity {
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
) -> Result<UploadDescriptor, RendererError> {
    let (_, end) = patch
        .rect
        .checked_bounds()
        .ok_or(RendererError::InvalidPatch)?;
    if end.x > surface_size.width || end.y > surface_size.height {
        return Err(RendererError::InvalidPatch);
    }
    let minimum_stride = patch
        .rect
        .width
        .checked_mul(BYTES_PER_PIXEL)
        .ok_or(RendererError::InvalidPatch)?;
    if patch.stride_bytes < minimum_stride {
        return Err(RendererError::InvalidPatch);
    }
    let expected_length = usize::try_from(patch.stride_bytes)
        .ok()
        .and_then(|stride| {
            usize::try_from(patch.rect.height)
                .ok()
                .and_then(|height| stride.checked_mul(height))
        })
        .ok_or(RendererError::InvalidPatch)?;
    if expected_length != patch.pixels.len() {
        return Err(RendererError::InvalidPatch);
    }
    Ok(UploadDescriptor {
        rect: patch.rect,
        stride_bytes: patch.stride_bytes,
        byte_len: expected_length,
    })
}

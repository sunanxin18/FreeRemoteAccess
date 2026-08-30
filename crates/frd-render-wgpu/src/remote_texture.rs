use frd_core::{ContentViewport, PixelRect, PixelSize, SessionId};
use frd_frame::{FrameCompleteness, PixelFormat, PixelPatch, SurfaceUpdate};

use crate::{pass::RemotePass, GpuCleanToken, GpuContext, GpuContextId, GpuFaultClass};

const MAX_REMOTE_TEXTURE_BYTES: u64 = 256 * 1024 * 1024;
const BYTES_PER_PIXEL: u32 = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PresentationReceipt {
    pub session_id: SessionId,
    pub generation: u64,
    pub revision: u64,
    pub completeness: FrameCompleteness,
}

#[derive(Debug)]
pub struct ConfirmedPresentation(PresentationReceipt);

impl ConfirmedPresentation {
    pub fn into_receipt(self) -> PresentationReceipt {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct UploadDescriptor {
    rect: PixelRect,
    stride_bytes: u32,
    byte_len: usize,
}

#[derive(Debug)]
enum PlannedUpdateData {
    Reset {
        session_id: SessionId,
        generation: u64,
        size: PixelSize,
    },
    Damage {
        revision: u64,
        patches: Vec<PixelPatch>,
    },
    Boundary(PresentationReceipt),
}

#[derive(Debug)]
struct PlannedUpdate {
    uploads: Vec<UploadDescriptor>,
    data: PlannedUpdateData,
}

impl PlannedUpdate {
    fn uploads(&self) -> &[UploadDescriptor] {
        &self.uploads
    }
}

#[derive(Clone, Copy, Debug)]
struct RemoteIdentity {
    session_id: SessionId,
    generation: u64,
    size: PixelSize,
    last_damage_revision: u64,
    last_boundary_revision: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RendererError {
    StaleUpdate,
    InvalidGeometry,
    TextureBudgetExceeded,
    UnsupportedPixelFormat,
    NonMonotonicRevision,
    BoundaryWithoutMatchingDamage,
    InvalidPatch,
    ResetRequired,
    StalePresentationReceipt,
    TextureDimensionUnsupported,
    UnsupportedTargetFormat,
    GpuFault(GpuFaultClass),
}

impl From<GpuFaultClass> for RendererError {
    fn from(value: GpuFaultClass) -> Self {
        Self::GpuFault(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryRequirement {
    ResetAndFullSnapshot {
        session_id: SessionId,
        generation: u64,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplyOutcome {
    Reset,
    Damage { uploaded_rectangles: usize },
    BoundaryPending(PresentationReceipt),
}

struct RemoteTexture {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    size: PixelSize,
}

struct PreparedRendererRecovery {
    context: GpuContext,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

struct DetachedRendererResources {
    _context: GpuContext,
    _bind_group_layout: wgpu::BindGroupLayout,
    _sampler: wgpu::Sampler,
    _remote: Option<RemoteTexture>,
    _pass: Option<RemotePass>,
}

pub struct RemoteRenderer {
    context: GpuContext,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    remote: Option<RemoteTexture>,
    pass: Option<RemotePass>,
    state: RemoteUpdateState,
}

impl RemoteRenderer {
    pub fn new(context: GpuContext) -> Result<Self, RendererError> {
        let scope = context.begin_fault_scope()?;
        let (bind_group_layout, sampler) = create_sampling_resources(context.device());
        let token = scope.finish()?;
        let commit_context = context.clone();
        commit_context
            .commit_if_unchanged(token, || Self {
                context,
                bind_group_layout,
                sampler,
                remote: None,
                pass: None,
                state: RemoteUpdateState::default(),
            })
            .map_err(RendererError::from)
    }

    pub fn apply_update(&mut self, update: SurfaceUpdate) -> Result<ApplyOutcome, RendererError> {
        let plan = self.state.plan(update)?;
        match &plan.data {
            PlannedUpdateData::Reset { size, .. } => {
                let limits = self.context.device().limits();
                if size.width > limits.max_texture_dimension_2d
                    || size.height > limits.max_texture_dimension_2d
                {
                    return Err(RendererError::TextureDimensionUnsupported);
                }
                let scope = self.context.begin_fault_scope()?;
                let candidate = create_remote_texture(
                    self.context.device(),
                    &self.bind_group_layout,
                    &self.sampler,
                    *size,
                );
                let token = scope.finish()?;
                let context = self.context.clone();
                let committed = context
                    .commit_if_unchanged(token, || {
                        commit_reset_resource_after_gpu(
                            &mut self.state,
                            &mut self.remote,
                            plan,
                            Ok(candidate),
                        )
                    })
                    .map_err(RendererError::from)??;
                let (outcome, old_remote) = committed;
                drop(old_remote);
                Ok(outcome)
            }
            PlannedUpdateData::Damage { patches, .. } => {
                let remote = self.remote.as_ref().ok_or(RendererError::ResetRequired)?;
                let scope = self.context.begin_fault_scope()?;
                for (patch, upload) in patches.iter().zip(plan.uploads()) {
                    debug_assert_eq!(patch.pixels.len(), upload.byte_len);
                    self.context.queue().write_texture(
                        wgpu::TexelCopyTextureInfo {
                            texture: &remote.texture,
                            mip_level: 0,
                            origin: wgpu::Origin3d {
                                x: upload.rect.x,
                                y: upload.rect.y,
                                z: 0,
                            },
                            aspect: wgpu::TextureAspect::All,
                        },
                        patch.pixels.as_bytes(),
                        wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(upload.stride_bytes),
                            rows_per_image: Some(upload.rect.height),
                        },
                        wgpu::Extent3d {
                            width: upload.rect.width,
                            height: upload.rect.height,
                            depth_or_array_layers: 1,
                        },
                    );
                }
                let uploaded_rectangles = patches.len();
                let token = scope.finish()?;
                let context = self.context.clone();
                context
                    .commit_if_unchanged(token, || {
                        commit_planned_update_after_gpu(&mut self.state, plan, Ok(()))
                    })
                    .map_err(RendererError::from)??;
                Ok(ApplyOutcome::Damage {
                    uploaded_rectangles,
                })
            }
            PlannedUpdateData::Boundary(receipt) => {
                let receipt = *receipt;
                let scope = self.context.begin_fault_scope()?;
                let token = scope.finish()?;
                let context = self.context.clone();
                context
                    .commit_if_unchanged(token, || self.state.commit(plan))
                    .map_err(RendererError::from)?;
                Ok(ApplyOutcome::BoundaryPending(receipt))
            }
        }
    }

    pub fn record(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        drawable: PixelSize,
        target_format: wgpu::TextureFormat,
    ) -> Result<Option<PresentationReceipt>, RendererError> {
        let viewport = self
            .remote
            .as_ref()
            .map(|remote| ContentViewport::fit(remote.size, drawable));
        self.record_in(encoder, target, viewport, target_format)
    }

    pub fn record_in(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        viewport: Option<ContentViewport>,
        target_format: wgpu::TextureFormat,
    ) -> Result<Option<PresentationReceipt>, RendererError> {
        match (self.remote.as_ref(), viewport) {
            (Some(remote), Some(viewport)) if remote.size != viewport.remote => {
                return Err(RendererError::InvalidGeometry)
            }
            (None, Some(_)) => return Err(RendererError::InvalidGeometry),
            _ => {}
        }
        let scope = self.context.begin_fault_scope()?;
        let replacement_pass = if self
            .pass
            .as_ref()
            .is_none_or(|pass| !pass.matches(target_format))
        {
            Some(RemotePass::new(
                self.context.device(),
                &self.bind_group_layout,
                target_format,
            )?)
        } else {
            None
        };
        let remote = self.remote.as_ref();
        replacement_pass
            .as_ref()
            .or(self.pass.as_ref())
            .expect("远端 pass 已创建")
            .record(
                encoder,
                target,
                remote.map(|texture| &texture.bind_group),
                viewport.map(|viewport| viewport.content),
            );
        let token = scope.finish()?;
        let context = self.context.clone();
        let (old_pass, pending_receipt) = context
            .commit_if_unchanged(token, || {
                let old_pass = replacement_pass.and_then(|pass| self.pass.replace(pass));
                let receipt = viewport.and_then(|_| self.state.pending_receipt());
                (old_pass, receipt)
            })
            .map_err(RendererError::from)?;
        drop(old_pass);
        Ok(pending_receipt)
    }

    pub fn confirm_presented(
        &mut self,
        token: GpuCleanToken,
        receipt: PresentationReceipt,
    ) -> Result<ConfirmedPresentation, RendererError> {
        let context = self.context.clone();
        confirm_presented_with_commit(&mut self.state, receipt, |state, receipt| {
            context
                .commit_if_unchanged(token, || state.confirm_presented(receipt))
                .map_err(RendererError::from)?
        })
    }

    pub fn recover_device(
        &mut self,
        context: GpuContext,
    ) -> Result<Option<RecoveryRequirement>, RendererError> {
        let (requirement, ()) = self.recover_device_coordinated(context, || ())?;
        Ok(requirement)
    }

    pub fn recover_device_coordinated<R>(
        &mut self,
        context: GpuContext,
        install_peer: impl FnOnce() -> R,
    ) -> Result<(Option<RecoveryRequirement>, R), RendererError> {
        let scope = context.begin_fault_scope()?;
        let (bind_group_layout, sampler) = create_sampling_resources(context.device());
        let token = scope.finish()?;
        let commit_context = context.clone();
        let prepared = PreparedRendererRecovery {
            context,
            bind_group_layout,
            sampler,
        };
        let ((requirement, detached), peer) = commit_context
            .commit_if_unchanged(token, || {
                let installed = self.install_prepared_recovery(prepared);
                let peer = install_peer();
                (installed, peer)
            })
            .map_err(RendererError::from)?;
        drop(detached);
        Ok((requirement, peer))
    }

    fn install_prepared_recovery(
        &mut self,
        prepared: PreparedRendererRecovery,
    ) -> (Option<RecoveryRequirement>, DetachedRendererResources) {
        let requirement = self
            .remote
            .as_ref()
            .map(|_| self.state.invalidate_for_device_loss());
        let detached = DetachedRendererResources {
            _context: std::mem::replace(&mut self.context, prepared.context),
            _bind_group_layout: std::mem::replace(
                &mut self.bind_group_layout,
                prepared.bind_group_layout,
            ),
            _sampler: std::mem::replace(&mut self.sampler, prepared.sampler),
            _remote: self.remote.take(),
            _pass: self.pass.take(),
        };
        (requirement, detached)
    }

    pub fn uses_context(&self, context: &GpuContext) -> bool {
        self.context.is_same_context(context)
    }

    pub fn context_id(&self) -> GpuContextId {
        self.context.context_id()
    }

    pub fn detach(&mut self) {
        self.remote = None;
        self.pass = None;
        self.state.clear();
    }
}

fn commit_reset_resource_after_gpu<R>(
    state: &mut RemoteUpdateState,
    resource: &mut Option<R>,
    plan: PlannedUpdate,
    candidate: Result<R, RendererError>,
) -> Result<(ApplyOutcome, Option<R>), RendererError> {
    let candidate = candidate?;
    state.commit(plan);
    let old_resource = resource.replace(candidate);
    Ok((ApplyOutcome::Reset, old_resource))
}

fn commit_planned_update_after_gpu(
    state: &mut RemoteUpdateState,
    plan: PlannedUpdate,
    gpu_result: Result<(), RendererError>,
) -> Result<(), RendererError> {
    gpu_result?;
    state.commit(plan);
    Ok(())
}

fn confirm_presented_with_commit(
    state: &mut RemoteUpdateState,
    receipt: PresentationReceipt,
    commit: impl FnOnce(&mut RemoteUpdateState, PresentationReceipt) -> Result<(), RendererError>,
) -> Result<ConfirmedPresentation, RendererError> {
    commit(state, receipt)?;
    Ok(ConfirmedPresentation(receipt))
}

fn create_sampling_resources(device: &wgpu::Device) -> (wgpu::BindGroupLayout, wgpu::Sampler) {
    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("FreeRemoteDesk remote texture layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("FreeRemoteDesk remote texture sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    });
    (bind_group_layout, sampler)
}

fn create_remote_texture(
    device: &wgpu::Device,
    bind_group_layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    size: PixelSize,
) -> RemoteTexture {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("FreeRemoteDesk persistent remote texture"),
        size: wgpu::Extent3d {
            width: size.width,
            height: size.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: RemoteColorPolicy::texture_format(),
        usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("FreeRemoteDesk remote texture bind group"),
        layout: bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    });
    RemoteTexture {
        texture,
        bind_group,
        size,
    }
}

#[derive(Default)]
struct RemoteUpdateState {
    current: Option<RemoteIdentity>,
    pending_receipt: Option<PresentationReceipt>,
    unpresented_full_baseline: bool,
    baseline_presented: bool,
    recovery: Option<RecoveryRequirement>,
}

impl RemoteUpdateState {
    fn clear(&mut self) {
        *self = Self::default();
    }
    fn plan(&self, update: SurfaceUpdate) -> Result<PlannedUpdate, RendererError> {
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

    fn commit(&mut self, plan: PlannedUpdate) {
        match plan.data {
            PlannedUpdateData::Reset {
                session_id,
                generation,
                size,
            } => {
                self.current = Some(RemoteIdentity {
                    session_id,
                    generation,
                    size,
                    last_damage_revision: 0,
                    last_boundary_revision: 0,
                });
                self.pending_receipt = None;
                self.unpresented_full_baseline = false;
                self.baseline_presented = false;
                self.recovery = None;
            }
            PlannedUpdateData::Damage { revision, patches } => {
                let _pixels_are_consumed_after_upload = patches;
                let current = self
                    .current
                    .as_mut()
                    .expect("damage plan requires reset state");
                current.last_damage_revision = revision;
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
                self.pending_receipt = Some(receipt);
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
    fn current_generation(&self) -> Option<u64> {
        self.current.map(|current| current.generation)
    }

    #[cfg(test)]
    fn baseline_presented(&self) -> bool {
        self.baseline_presented
    }

    fn confirm_presented(&mut self, receipt: PresentationReceipt) -> Result<(), RendererError> {
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
            data: PlannedUpdateData::Reset {
                session_id,
                generation,
                size,
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

struct RemoteColorPolicy;

impl RemoteColorPolicy {
    #[cfg(test)]
    fn sample_bgrx([blue, green, red, _unused]: [u8; 4]) -> [u8; 4] {
        [red, green, blue, u8::MAX]
    }

    fn texture_format() -> wgpu::TextureFormat {
        wgpu::TextureFormat::Bgra8UnormSrgb
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};
    use std::thread;

    use frd_core::{PixelRect, PixelSize, SessionId};
    use frd_frame::{FrameCompleteness, PixelBuffer, PixelFormat, PixelPatch, SurfaceUpdate};

    use super::{
        commit_planned_update_after_gpu, commit_reset_resource_after_gpu,
        confirm_presented_with_commit, RecoveryRequirement, RemoteColorPolicy, RemoteUpdateState,
        RendererError,
    };
    use crate::{GpuContextId, GpuFaultClass, GpuFaultObserver};

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
    fn bgrx_policy_preserves_rgb_channels_and_shader_forces_alpha_without_manual_gamma() {
        let shader = include_str!("shaders/remote_surface.wgsl");

        assert_eq!(RemoteColorPolicy::sample_bgrx([1, 2, 3, 0]), [3, 2, 1, 255]);
        assert_eq!(
            RemoteColorPolicy::texture_format(),
            wgpu::TextureFormat::Bgra8UnormSrgb
        );
        assert!(shader.contains("vec4<f32>(remote.rgb, 1.0)"));
        assert!(!shader.contains("pow("));
        assert!(!shader.contains("gamma"));
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

    #[test]
    fn gpu_creation_failure_preserves_old_resource_identity_and_exact_renderer_state() {
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
        let pending = state.pending_receipt();
        let next_reset = state
            .plan(reset(session_id, 2, size, PixelFormat::Bgrx8UnormSrgb))
            .unwrap();
        let mut resource = Some("old-texture-owner");

        assert_eq!(
            commit_reset_resource_after_gpu(
                &mut state,
                &mut resource,
                next_reset,
                Err(RendererError::GpuFault(GpuFaultClass::OutOfMemory)),
            )
            .unwrap_err(),
            RendererError::GpuFault(GpuFaultClass::OutOfMemory)
        );
        assert_eq!(resource, Some("old-texture-owner"));
        assert_eq!(state.current_generation(), Some(1));
        assert_eq!(state.last_damage_revision(), 1);
        assert_eq!(state.pending_receipt(), pending);
    }

    #[test]
    fn gpu_write_failure_does_not_advance_damage_or_make_a_boundary_eligible() {
        let session_id = SessionId::allocate();
        let size = PixelSize::new(2, 2).unwrap();
        let rect = pixel_rect(0, 0, 1, 1);
        let mut state = RemoteUpdateState::default();
        for update in [
            reset(session_id, 1, size, PixelFormat::Bgrx8UnormSrgb),
            damage(session_id, 1, 1, rect, 4, vec![0; 4]),
        ] {
            let plan = state.plan(update).unwrap();
            state.commit(plan);
        }
        let failed_damage = state
            .plan(damage(session_id, 1, 2, rect, 4, vec![0; 4]))
            .unwrap();

        assert_eq!(
            commit_planned_update_after_gpu(
                &mut state,
                failed_damage,
                Err(RendererError::GpuFault(GpuFaultClass::Validation)),
            )
            .unwrap_err(),
            RendererError::GpuFault(GpuFaultClass::Validation)
        );
        assert_eq!(state.last_damage_revision(), 1);
        assert_eq!(
            state
                .plan(boundary(session_id, 1, 2, FrameCompleteness::Incremental,))
                .unwrap_err(),
            RendererError::BoundaryWithoutMatchingDamage
        );
    }

    #[test]
    fn fault_at_receipt_commit_boundary_preserves_pending_baseline_and_returns_no_confirmation() {
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
        let context_id = GpuContextId(73);
        let observer = Arc::new(GpuFaultObserver::new());
        let epoch = observer.begin_operation().unwrap();
        let token = observer.clean_token(context_id, epoch).unwrap();
        let published = Arc::new(Barrier::new(2));
        let confirmation_ready = Arc::new(Barrier::new(2));
        let release_publication = Arc::new(Barrier::new(2));

        let fault_thread = {
            let observer = observer.clone();
            let published = published.clone();
            let release_publication = release_publication.clone();
            thread::spawn(move || {
                observer.record_with_publication_paused(GpuFaultClass::DeviceLost, || {
                    published.wait();
                    release_publication.wait();
                });
            })
        };
        published.wait();

        let confirmation_thread = {
            let observer = observer.clone();
            let confirmation_ready = confirmation_ready.clone();
            thread::spawn(move || {
                confirmation_ready.wait();
                let confirmation =
                    confirm_presented_with_commit(&mut state, receipt, |state, receipt| {
                        observer
                            .commit_if_unchanged(context_id, token, || {
                                state.confirm_presented(receipt)
                            })
                            .map_err(RendererError::from)?
                    });
                (confirmation, state)
            })
        };

        confirmation_ready.wait();
        release_publication.wait();
        fault_thread.join().unwrap();
        let (confirmation, state) = confirmation_thread.join().unwrap();
        assert_eq!(
            confirmation.unwrap_err(),
            RendererError::GpuFault(GpuFaultClass::DeviceLost)
        );
        assert_eq!(state.pending_receipt(), Some(receipt));
        assert!(!state.baseline_presented());
    }
}

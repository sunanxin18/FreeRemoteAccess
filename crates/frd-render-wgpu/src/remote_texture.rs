use frd_core::{PixelRect, PixelSize, SessionId};
use frd_frame::{FrameCompleteness, PixelFormat, PixelPatch, SurfaceUpdate};

use crate::{pass::RemotePass, GpuContext};

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

pub struct RemoteRenderer {
    context: GpuContext,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    remote: Option<RemoteTexture>,
    pass: Option<RemotePass>,
    state: RemoteUpdateState,
}

impl RemoteRenderer {
    pub fn new(context: GpuContext) -> Self {
        let (bind_group_layout, sampler) = create_sampling_resources(context.device());
        Self {
            context,
            bind_group_layout,
            sampler,
            remote: None,
            pass: None,
            state: RemoteUpdateState::default(),
        }
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
                let remote = create_remote_texture(
                    self.context.device(),
                    &self.bind_group_layout,
                    &self.sampler,
                    *size,
                );
                self.state.commit(plan)?;
                self.remote = Some(remote);
                Ok(ApplyOutcome::Reset)
            }
            PlannedUpdateData::Damage { patches, .. } => {
                let remote = self.remote.as_ref().ok_or(RendererError::ResetRequired)?;
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
                self.state.commit(plan)?;
                Ok(ApplyOutcome::Damage {
                    uploaded_rectangles,
                })
            }
            PlannedUpdateData::Boundary(receipt) => {
                let receipt = *receipt;
                self.state.commit(plan)?;
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
        if self
            .pass
            .as_ref()
            .is_none_or(|pass| !pass.matches(target_format))
        {
            self.pass = Some(RemotePass::new(
                self.context.device(),
                &self.bind_group_layout,
                target_format,
            )?);
        }
        let remote = self.remote.as_ref();
        self.pass.as_ref().expect("远端 pass 已创建").record(
            encoder,
            target,
            remote.map(|texture| &texture.bind_group),
            remote.map(|texture| texture.size),
            drawable,
        );
        Ok(self.state.pending_receipt())
    }

    pub fn confirm_presented(&mut self, receipt: PresentationReceipt) -> Result<(), RendererError> {
        self.state.confirm_presented(receipt)
    }

    pub fn recover_device(&mut self, context: GpuContext) -> Option<RecoveryRequirement> {
        let requirement = self
            .remote
            .as_ref()
            .map(|_| self.state.invalidate_for_device_loss());
        self.context = context;
        let (bind_group_layout, sampler) = create_sampling_resources(self.context.device());
        self.bind_group_layout = bind_group_layout;
        self.sampler = sampler;
        self.remote = None;
        self.pass = None;
        requirement
    }

    pub fn detach(&mut self) {
        self.remote = None;
        self.pass = None;
        self.state.clear();
    }
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

    fn commit(&mut self, plan: PlannedUpdate) -> Result<(), RendererError> {
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
                self.baseline_presented = false;
                self.recovery = None;
            }
            PlannedUpdateData::Damage { revision, patches } => {
                let _pixels_are_consumed_after_upload = patches;
                let current = self.current.as_mut().ok_or(RendererError::ResetRequired)?;
                current.last_damage_revision = revision;
                self.pending_receipt = None;
            }
            PlannedUpdateData::Boundary(receipt) => {
                let current = self.current.as_mut().ok_or(RendererError::ResetRequired)?;
                current.last_boundary_revision = receipt.revision;
                self.pending_receipt = Some(receipt);
            }
        }
        Ok(())
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

    fn confirm_presented(&mut self, receipt: PresentationReceipt) -> Result<(), RendererError> {
        if self.pending_receipt != Some(receipt) {
            return Err(RendererError::StalePresentationReceipt);
        }
        self.pending_receipt = None;
        if receipt.completeness == FrameCompleteness::FullBaseline {
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
    use frd_core::{PixelRect, PixelSize, SessionId};
    use frd_frame::{FrameCompleteness, PixelBuffer, PixelFormat, PixelPatch, SurfaceUpdate};

    use super::{RecoveryRequirement, RemoteColorPolicy, RemoteUpdateState, RendererError};

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

        state
            .commit(
                state
                    .plan(reset(session_id, 7, size, PixelFormat::Bgrx8UnormSrgb))
                    .unwrap(),
            )
            .unwrap();
        state
            .commit(
                state
                    .plan(damage(session_id, 7, 1, rect, 4, vec![0; 4]))
                    .unwrap(),
            )
            .unwrap();

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
            state.commit(plan).unwrap();
        }
        let receipt = state.pending_receipt().unwrap();
        state.confirm_presented(receipt).unwrap();
        assert!(state.baseline_presented());

        let plan = state
            .plan(reset(session_id, 2, size, PixelFormat::Bgrx8UnormSrgb))
            .unwrap();
        state.commit(plan).unwrap();

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
            state.commit(plan).unwrap();
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
            state.commit(plan).unwrap();
        }

        let full = state.pending_receipt().unwrap();
        assert_eq!(full.completeness, FrameCompleteness::FullBaseline);
        assert!(!state.baseline_presented());
        state.confirm_presented(full).unwrap();
        assert!(state.baseline_presented());
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
        state.commit(plan).unwrap();

        let plan = state
            .plan(damage(session_id, 1, 1, rect, 12, vec![0; 12]))
            .unwrap();
        assert_eq!(plan.uploads().len(), 1);
        assert_eq!(plan.uploads()[0].rect, rect);
        assert_eq!(plan.uploads()[0].stride_bytes, 12);
        assert_eq!(plan.uploads()[0].byte_len, 12);
        assert_ne!(plan.uploads()[0].rect, pixel_rect(0, 0, 4, 4));
        state.commit(plan).unwrap();

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
            state.commit(plan).unwrap();
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

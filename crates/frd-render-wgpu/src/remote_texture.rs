use frd_core::{ContentViewport, PixelRect, PixelSize, SessionId};
#[cfg(test)]
use frd_frame::SurfaceUpdate;
use frd_frame::{FrameCompleteness, FrameReset, FrameTransaction, PixelFormat, PixelPatch};

use crate::{
    pass::RemotePass, GpuCleanToken, GpuContext, GpuContextId, GpuFaultClass, GpuFaultScope,
    GpuScopeObservation,
};

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
pub struct BatchScopeDiagnostics {
    pub observation: GpuScopeObservation,
    pub observed_fault: Option<GpuFaultClass>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BatchApplySuccess {
    pub outcome: BatchApplyOutcome,
    pub scope: BatchScopeDiagnostics,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BatchApplyFailure {
    pub identity: Option<FrameBatchIdentity>,
    pub primary: RendererError,
    pub secondary_execution: Option<RendererError>,
    pub scope: Option<BatchScopeDiagnostics>,
}

impl BatchApplyFailure {
    fn planning(identity: Option<FrameBatchIdentity>, error: RendererError) -> Self {
        Self {
            identity,
            primary: error,
            secondary_execution: None,
            scope: None,
        }
    }

    fn begin(identity: Option<FrameBatchIdentity>, fault: GpuFaultClass) -> Self {
        Self::planning(identity, RendererError::GpuFault(fault))
    }

    fn counter_regressed(identity: Option<FrameBatchIdentity>) -> Self {
        Self::planning(identity, RendererError::ScopeObservationInvalid)
    }
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
    format: PixelFormat,
    last_damage_revision: u64,
    last_boundary_revision: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RendererError {
    EmptyBatch,
    BatchExecutionPanicked,
    ScopeObservationInvalid,
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

trait RecordScopeBackend {
    type Scope;
    type CleanToken;

    fn begin(&self) -> Result<Self::Scope, GpuFaultClass>;
    fn finish(&self, scope: Self::Scope) -> Result<Self::CleanToken, GpuFaultClass>;
    fn commit_if_unchanged<R>(
        &self,
        token: Self::CleanToken,
        commit: impl FnOnce() -> R,
    ) -> Result<R, GpuFaultClass>;
}

trait PreparedRecordCommit {
    type Output;

    fn commit(self) -> Self::Output;
}

struct GpuContextRecordScopeBackend<'a> {
    context: &'a GpuContext,
}

impl<'a> GpuContextRecordScopeBackend<'a> {
    fn new(context: &'a GpuContext) -> Self {
        Self { context }
    }
}

impl RecordScopeBackend for GpuContextRecordScopeBackend<'_> {
    type Scope = GpuFaultScope;
    type CleanToken = GpuCleanToken;

    fn begin(&self) -> Result<Self::Scope, GpuFaultClass> {
        self.context.begin_fault_scope()
    }

    fn finish(&self, scope: Self::Scope) -> Result<Self::CleanToken, GpuFaultClass> {
        scope.finish()
    }

    fn commit_if_unchanged<R>(
        &self,
        token: Self::CleanToken,
        commit: impl FnOnce() -> R,
    ) -> Result<R, GpuFaultClass> {
        self.context.commit_if_unchanged(token, commit)
    }
}

fn execute_record_with_fault_scope<B, R>(
    backend: &B,
    record: impl FnOnce() -> Result<R, RendererError>,
) -> Result<R::Output, RendererError>
where
    B: RecordScopeBackend,
    R: PreparedRecordCommit,
{
    let scope = backend.begin()?;
    let recorded = record();
    let finish = backend.finish(scope);
    match (finish, recorded) {
        (Err(fault), _) => Err(RendererError::GpuFault(fault)),
        (Ok(_), Err(error)) => Err(error),
        (Ok(clean_token), Ok(recorded)) => backend
            .commit_if_unchanged(clean_token, || recorded.commit())
            .map_err(RendererError::from),
    }
}

struct WgpuPreparedRecord<'a> {
    replacement_pass: Option<RemotePass>,
    pass: &'a mut Option<RemotePass>,
    state: &'a RemoteUpdateState,
    viewport: Option<ContentViewport>,
}

impl PreparedRecordCommit for WgpuPreparedRecord<'_> {
    type Output = (Option<RemotePass>, Option<PresentationReceipt>);

    fn commit(self) -> Self::Output {
        let old_pass = self
            .replacement_pass
            .and_then(|pass| self.pass.replace(pass));
        let receipt = self.viewport.and_then(|_| self.state.pending_receipt());
        (old_pass, receipt)
    }
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

    pub fn apply_update_batch(
        &mut self,
        transactions: Vec<FrameTransaction>,
    ) -> Result<BatchApplySuccess, BatchApplyFailure> {
        if transactions.is_empty() {
            return Err(BatchApplyFailure::planning(None, RendererError::EmptyBatch));
        }
        let max_dimension = self.context.device().limits().max_texture_dimension_2d;
        for transaction in &transactions {
            if let FrameTransaction::Startup { reset, .. } = transaction {
                if reset.size.width > max_dimension || reset.size.height > max_dimension {
                    return Err(BatchApplyFailure::planning(
                        Some(transaction_identity(transaction)),
                        RendererError::TextureDimensionUnsupported,
                    ));
                }
            }
        }
        let planned = self
            .state
            .plan_batch(transactions)
            .map_err(|failure| BatchApplyFailure::planning(failure.identity, failure.error))?;
        self.run_planned_batch(planned)
    }

    fn run_planned_batch(
        &mut self,
        planned: PlannedBatch,
    ) -> Result<BatchApplySuccess, BatchApplyFailure> {
        let mut executor = WgpuPlannedExecutor::new(
            &self.context,
            &self.bind_group_layout,
            &self.sampler,
            self.remote.as_ref(),
        );
        let scoped = execute_with_observed_scope(
            &GpuContextBatchScopeBackend::new(&self.context),
            planned.identity,
            || {
                execute_planned_operations(&mut executor, &planned.operations)
                    .map(own_wgpu_prepared_resources)
            },
        )?;
        drop(executor);
        self.commit_clean_batch(
            scoped.clean_token,
            planned,
            scoped.prepared,
            scoped.scope.observation,
        )
    }

    fn commit_clean_batch(
        &mut self,
        clean_token: GpuCleanToken,
        planned: PlannedBatch,
        prepared: PreparedBatchResources<RemoteTexture>,
        observation: GpuScopeObservation,
    ) -> Result<BatchApplySuccess, BatchApplyFailure> {
        let identity = planned.identity;
        let context = self.context.clone();
        match context.commit_if_unchanged(clean_token, || {
            commit_planned_batch_after_gpu(&mut self.state, &mut self.remote, planned, prepared)
        }) {
            Ok((outcome, dropped)) => {
                drop(dropped);
                Ok(BatchApplySuccess {
                    outcome,
                    scope: BatchScopeDiagnostics {
                        observation,
                        observed_fault: None,
                    },
                })
            }
            Err(fault) => Err(BatchApplyFailure {
                identity: Some(identity),
                primary: RendererError::GpuFault(fault),
                secondary_execution: None,
                scope: Some(BatchScopeDiagnostics {
                    observation,
                    observed_fault: Some(fault),
                }),
            }),
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
        let Self {
            context,
            bind_group_layout,
            remote,
            pass,
            state,
            ..
        } = self;
        let scope_backend = GpuContextRecordScopeBackend::new(context);
        let (old_pass, pending_receipt) = execute_record_with_fault_scope(&scope_backend, || {
            let replacement_pass = if pass
                .as_ref()
                .is_none_or(|pass| !pass.matches(target_format))
            {
                Some(RemotePass::new(
                    context.device(),
                    bind_group_layout,
                    target_format,
                )?)
            } else {
                None
            };
            replacement_pass
                .as_ref()
                .or(pass.as_ref())
                .expect("远端 pass 已创建")
                .record(
                    encoder,
                    target,
                    remote.as_ref().map(|texture| &texture.bind_group),
                    viewport.map(|viewport| viewport.content),
                );
            Ok(WgpuPreparedRecord {
                replacement_pass,
                pass,
                state,
                viewport,
            })
        })?;
        drop(old_pass);
        Ok(pending_receipt)
    }

    pub fn confirm_presented(
        &mut self,
        token: GpuCleanToken,
        receipt: PresentationReceipt,
    ) -> Result<ConfirmedPresentation, RendererError> {
        let Self { context, state, .. } = self;
        confirm_presented_with_commit(state, receipt, |state, receipt| {
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

trait PlannedOperationExecutor {
    type Resource;

    fn allocate(&mut self, reset: &FrameReset) -> Result<Self::Resource, RendererError>;

    fn write_patch(
        &mut self,
        resource: &Self::Resource,
        revision: u64,
        patch_index: usize,
        patch: &PixelPatch,
        upload: UploadDescriptor,
    ) -> Result<(), RendererError>;
}

trait PlannedExecutionContext: PlannedOperationExecutor {
    fn take_existing_resource(&mut self) -> Option<Self::Resource>;
}

struct PreparedBatchResources<R> {
    final_startup: Option<R>,
    superseded: Vec<R>,
}

fn execute_planned_operations<E>(
    executor: &mut E,
    operations: &[PlannedUpdate],
) -> Result<PreparedBatchResources<E::Resource>, RendererError>
where
    E: PlannedExecutionContext,
{
    let mut active = executor.take_existing_resource();
    let mut active_is_startup = false;
    let mut superseded = Vec::new();

    for operation in operations {
        match &operation.data {
            PlannedUpdateData::StartupReset {
                session_id,
                generation,
                size,
                format,
            } => {
                let candidate = executor.allocate(&FrameReset {
                    session_id: *session_id,
                    generation: *generation,
                    size: *size,
                    format: *format,
                })?;
                if let Some(previous) = active.replace(candidate) {
                    if active_is_startup {
                        superseded.push(previous);
                    } else {
                        drop(previous);
                    }
                }
                active_is_startup = true;
            }
            PlannedUpdateData::Damage { revision, patches } => {
                let resource = active.as_ref().ok_or(RendererError::ResetRequired)?;
                for (patch_index, (patch, upload)) in
                    patches.iter().zip(operation.uploads()).enumerate()
                {
                    executor.write_patch(resource, *revision, patch_index, patch, *upload)?;
                }
            }
            PlannedUpdateData::Boundary(_) => {}
        }
    }

    let final_startup = if active_is_startup {
        active
    } else {
        drop(active);
        None
    };
    Ok(PreparedBatchResources {
        final_startup,
        superseded,
    })
}

struct WgpuPlannedExecutor<'a> {
    context: &'a GpuContext,
    bind_group_layout: &'a wgpu::BindGroupLayout,
    sampler: &'a wgpu::Sampler,
    existing: Option<&'a RemoteTexture>,
}

impl<'a> WgpuPlannedExecutor<'a> {
    fn new(
        context: &'a GpuContext,
        bind_group_layout: &'a wgpu::BindGroupLayout,
        sampler: &'a wgpu::Sampler,
        existing: Option<&'a RemoteTexture>,
    ) -> Self {
        Self {
            context,
            bind_group_layout,
            sampler,
            existing,
        }
    }
}

enum WgpuExecutionResource<'a> {
    Existing(&'a RemoteTexture),
    Startup(RemoteTexture),
}

impl WgpuExecutionResource<'_> {
    fn texture(&self) -> &wgpu::Texture {
        match self {
            Self::Existing(remote) => &remote.texture,
            Self::Startup(remote) => &remote.texture,
        }
    }
}

impl<'a> PlannedOperationExecutor for WgpuPlannedExecutor<'a> {
    type Resource = WgpuExecutionResource<'a>;

    fn allocate(&mut self, reset: &FrameReset) -> Result<Self::Resource, RendererError> {
        Ok(WgpuExecutionResource::Startup(create_remote_texture(
            self.context.device(),
            self.bind_group_layout,
            self.sampler,
            reset.size,
        )))
    }

    fn write_patch(
        &mut self,
        resource: &Self::Resource,
        _revision: u64,
        _patch_index: usize,
        patch: &PixelPatch,
        upload: UploadDescriptor,
    ) -> Result<(), RendererError> {
        debug_assert_eq!(patch.pixels.len(), upload.byte_len);
        self.context.queue().write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: resource.texture(),
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
        Ok(())
    }
}

impl<'a> PlannedExecutionContext for WgpuPlannedExecutor<'a> {
    fn take_existing_resource(&mut self) -> Option<Self::Resource> {
        self.existing.take().map(WgpuExecutionResource::Existing)
    }
}

fn own_wgpu_prepared_resources(
    prepared: PreparedBatchResources<WgpuExecutionResource<'_>>,
) -> PreparedBatchResources<RemoteTexture> {
    let own = |resource| match resource {
        WgpuExecutionResource::Startup(remote) => remote,
        WgpuExecutionResource::Existing(_) => {
            unreachable!("existing resource cannot become a startup candidate")
        }
    };
    PreparedBatchResources {
        final_startup: prepared.final_startup.map(own),
        superseded: prepared.superseded.into_iter().map(own).collect(),
    }
}

trait BatchScopeBackend {
    type Scope;
    type CleanToken;

    fn observation(&self) -> GpuScopeObservation;
    fn begin(&self) -> Result<Self::Scope, GpuFaultClass>;
    fn finish(&self, scope: Self::Scope) -> Result<Self::CleanToken, GpuFaultClass>;
}

struct GpuContextBatchScopeBackend<'a> {
    context: &'a GpuContext,
}

impl<'a> GpuContextBatchScopeBackend<'a> {
    fn new(context: &'a GpuContext) -> Self {
        Self { context }
    }
}

impl BatchScopeBackend for GpuContextBatchScopeBackend<'_> {
    type Scope = GpuFaultScope;
    type CleanToken = GpuCleanToken;

    fn observation(&self) -> GpuScopeObservation {
        self.context.scope_observation()
    }

    fn begin(&self) -> Result<Self::Scope, GpuFaultClass> {
        self.context.begin_fault_scope()
    }

    fn finish(&self, scope: Self::Scope) -> Result<Self::CleanToken, GpuFaultClass> {
        scope.finish()
    }
}

#[derive(Debug)]
struct ScopedExecution<T, R> {
    clean_token: T,
    prepared: R,
    scope: BatchScopeDiagnostics,
}

fn execute_with_observed_scope<B, R>(
    backend: &B,
    identity: FrameBatchIdentity,
    execute: impl FnOnce() -> Result<R, RendererError>,
) -> Result<ScopedExecution<B::CleanToken, R>, BatchApplyFailure>
where
    B: BatchScopeBackend,
{
    let before = backend.observation();
    let scope = match backend.begin() {
        Ok(scope) => scope,
        Err(fault) => return Err(BatchApplyFailure::begin(Some(identity), fault)),
    };
    let execution = std::panic::catch_unwind(std::panic::AssertUnwindSafe(execute))
        .unwrap_or(Err(RendererError::BatchExecutionPanicked));
    let finish = backend.finish(scope);
    let observation = backend.observation().checked_delta(before);

    match (finish, execution) {
        (Err(gpu), execution) => Err(BatchApplyFailure {
            identity: Some(identity),
            primary: RendererError::GpuFault(gpu),
            secondary_execution: execution.err(),
            scope: observation.map(|observation| BatchScopeDiagnostics {
                observation,
                observed_fault: Some(gpu),
            }),
        }),
        (Ok(_), _) if observation.is_none() => {
            Err(BatchApplyFailure::counter_regressed(Some(identity)))
        }
        (Ok(_), Err(execution)) => Err(BatchApplyFailure {
            identity: Some(identity),
            primary: execution,
            secondary_execution: None,
            scope: Some(BatchScopeDiagnostics {
                observation: observation.expect("checked above"),
                observed_fault: None,
            }),
        }),
        (Ok(clean_token), Ok(prepared)) => Ok(ScopedExecution {
            clean_token,
            prepared,
            scope: BatchScopeDiagnostics {
                observation: observation.expect("checked above"),
                observed_fault: None,
            },
        }),
    }
}

fn commit_planned_batch_after_gpu<R>(
    state: &mut RemoteUpdateState,
    resource: &mut Option<R>,
    planned: PlannedBatch,
    mut prepared: PreparedBatchResources<R>,
) -> (BatchApplyOutcome, Vec<R>) {
    let outcome = BatchApplyOutcome {
        installed_surface: planned.installed_surface,
        uploaded_rectangles: planned.uploaded_rectangles,
        had_texture_writes: planned.had_texture_writes,
        final_boundary: planned.final_boundary,
    };
    *state = planned.staged_state;
    if let Some(final_startup) = prepared.final_startup {
        if let Some(old_resource) = resource.replace(final_startup) {
            prepared.superseded.push(old_resource);
        }
    }
    (outcome, prepared.superseded)
}

#[cfg(test)]
fn commit_reset_resource_after_gpu<R>(
    state: &mut RemoteUpdateState,
    resource: &mut Option<R>,
    plan: PlannedUpdate,
    candidate: Result<R, RendererError>,
) -> Result<Option<R>, RendererError> {
    let candidate = candidate?;
    state.commit(plan);
    Ok(resource.replace(candidate))
}

#[cfg(test)]
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

#[derive(Clone, Default)]
struct RemoteUpdateState {
    current: Option<RemoteIdentity>,
    pending_receipt: Option<PresentationReceipt>,
    unpresented_full_baseline: bool,
    baseline_presented: bool,
    recovery: Option<RecoveryRequirement>,
}

struct PlannedBatch {
    identity: FrameBatchIdentity,
    staged_state: RemoteUpdateState,
    operations: Vec<PlannedUpdate>,
    installed_surface: Option<InstalledSurface>,
    uploaded_rectangles: usize,
    had_texture_writes: bool,
    final_boundary: Option<PresentationReceipt>,
}

#[derive(Debug)]
struct BatchPlanningFailure {
    identity: Option<FrameBatchIdentity>,
    error: RendererError,
}

impl RemoteUpdateState {
    fn clear(&mut self) {
        *self = Self::default();
    }
    #[cfg(test)]
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

fn transaction_identity(transaction: &FrameTransaction) -> FrameBatchIdentity {
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
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::Instant;

    use frd_core::{PixelRect, PixelSize, SessionId};
    use frd_frame::{
        FrameCompleteness, FrameReset, FrameRevision, FrameTransaction, PixelBuffer, PixelFormat,
        PixelPatch, SurfaceUpdate,
    };

    use super::{
        commit_planned_batch_after_gpu, commit_planned_update_after_gpu,
        commit_reset_resource_after_gpu, confirm_presented_with_commit, execute_planned_operations,
        execute_record_with_fault_scope, execute_with_observed_scope, BatchApplySuccess,
        BatchScopeDiagnostics, PlannedOperationExecutor, PreparedBatchResources,
        PreparedRecordCommit, RecordScopeBackend, RecoveryRequirement, RemoteColorPolicy,
        RemoteUpdateState, RendererError,
    };
    use crate::gpu_fault::{begin_observed_scope, ObservedScopeLifecycle, ScopeLifecycleEvent};
    use crate::{
        GpuContextId, GpuFaultClass, GpuFaultObserver, GpuScopeObservation, ScopeLifecycleObserver,
    };

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

    fn patch(rect: PixelRect, stride_bytes: u32, bytes: Vec<u8>) -> PixelPatch {
        PixelPatch {
            rect,
            stride_bytes,
            pixels: PixelBuffer::new(bytes),
        }
    }

    fn startup_transaction(
        session_id: SessionId,
        generation: u64,
        size: PixelSize,
        revision: u64,
        patches: Vec<PixelPatch>,
    ) -> FrameTransaction {
        FrameTransaction::Startup {
            earliest_constituent_enqueue_at: Instant::now(),
            reset: FrameReset {
                session_id,
                generation,
                size,
                format: PixelFormat::Bgrx8UnormSrgb,
            },
            revision: FrameRevision {
                session_id,
                generation,
                revision,
                patches,
                completeness: FrameCompleteness::FullBaseline,
            },
        }
    }

    fn revision_transaction(
        session_id: SessionId,
        generation: u64,
        revision: u64,
        patches: Vec<PixelPatch>,
    ) -> FrameTransaction {
        FrameTransaction::Revision {
            earliest_constituent_enqueue_at: Instant::now(),
            revision: FrameRevision {
                session_id,
                generation,
                revision,
                patches,
                completeness: FrameCompleteness::Incremental,
            },
        }
    }

    fn symbolic_row(ids: &[u8], padding: u8) -> Vec<u8> {
        let mut row = Vec::new();
        for id in ids {
            row.extend_from_slice(&[*id, *id, *id, 0]);
        }
        row.extend(std::iter::repeat_n(padding, 4));
        row
    }

    #[test]
    fn atomic_startup_plan_returns_all_four_product_facts() {
        let session_id = SessionId::allocate();
        let size = PixelSize::new(3, 2).unwrap();
        let full = patch(pixel_rect(0, 0, 3, 2), 12, vec![0x11; 24]);
        let steady = patch(pixel_rect(1, 0, 2, 2), 8, vec![0x22; 16]);
        let mut state = RemoteUpdateState::default();
        let planned = state
            .plan_batch(vec![
                startup_transaction(session_id, 7, size, 1, vec![full]),
                revision_transaction(session_id, 7, 2, vec![steady]),
            ])
            .unwrap();
        let mut resource = None;

        let (outcome, dropped) = commit_planned_batch_after_gpu(
            &mut state,
            &mut resource,
            planned,
            PreparedBatchResources {
                final_startup: Some("startup-texture"),
                superseded: Vec::new(),
            },
        );

        assert_eq!(
            outcome.installed_surface,
            Some(super::InstalledSurface {
                session_id,
                generation: 7,
                size,
                format: PixelFormat::Bgrx8UnormSrgb,
            })
        );
        assert_eq!(outcome.uploaded_rectangles, 2);
        assert!(outcome.had_texture_writes);
        assert_eq!(
            outcome.final_boundary,
            Some(super::PresentationReceipt {
                session_id,
                generation: 7,
                revision: 2,
                completeness: FrameCompleteness::FullBaseline,
            })
        );
        assert_eq!(resource, Some("startup-texture"));
        assert!(dropped.is_empty());
    }

    #[derive(Debug, Eq, PartialEq)]
    enum RecordedOperation {
        Reset,
        Patch { revision: u64, patch_index: usize },
    }

    struct RecordingResource {
        size: PixelSize,
        pixels: Rc<RefCell<Vec<u8>>>,
    }

    #[derive(Default)]
    struct RecordingExecutor {
        operations: Vec<RecordedOperation>,
        existing: Option<RecordingResource>,
    }

    impl PlannedOperationExecutor for RecordingExecutor {
        type Resource = RecordingResource;

        fn allocate(&mut self, reset: &FrameReset) -> Result<Self::Resource, RendererError> {
            self.operations.push(RecordedOperation::Reset);
            Ok(RecordingResource {
                size: reset.size,
                pixels: Rc::new(RefCell::new(vec![
                    0;
                    usize::try_from(
                        reset.size.width * reset.size.height * 4
                    )
                    .unwrap()
                ])),
            })
        }

        fn write_patch(
            &mut self,
            resource: &Self::Resource,
            revision: u64,
            patch_index: usize,
            patch: &PixelPatch,
            upload: super::UploadDescriptor,
        ) -> Result<(), RendererError> {
            self.operations.push(RecordedOperation::Patch {
                revision,
                patch_index,
            });
            let mut destination = resource.pixels.borrow_mut();
            let source = patch.pixels.as_bytes();
            let row_bytes = usize::try_from(upload.rect.width * 4).unwrap();
            let source_stride = usize::try_from(upload.stride_bytes).unwrap();
            let destination_stride = usize::try_from(resource.size.width * 4).unwrap();
            for row in 0..usize::try_from(upload.rect.height).unwrap() {
                let source_start = row * source_stride;
                let destination_start = (usize::try_from(upload.rect.y).unwrap() + row)
                    * destination_stride
                    + usize::try_from(upload.rect.x * 4).unwrap();
                destination[destination_start..destination_start + row_bytes]
                    .copy_from_slice(&source[source_start..source_start + row_bytes]);
            }
            Ok(())
        }
    }

    impl super::PlannedExecutionContext for RecordingExecutor {
        fn take_existing_resource(&mut self) -> Option<Self::Resource> {
            self.existing.take()
        }
    }

    #[test]
    fn recording_executor_preserves_fifo_patch_row_byte_and_overlap_order() {
        let session_id = SessionId::allocate();
        let size = PixelSize::new(3, 2).unwrap();
        let mut base = symbolic_row(b"ABC", 0xE0);
        base.extend(symbolic_row(b"DEF", 0xE0));
        let mut overlay = symbolic_row(b"GH", 0xE1);
        overlay.extend(symbolic_row(b"IJ", 0xE1));
        let mut final_patch = symbolic_row(b"KL", 0xE2);
        final_patch.extend(symbolic_row(b"MN", 0xE2));
        let planned = RemoteUpdateState::default()
            .plan_batch(vec![
                startup_transaction(
                    session_id,
                    1,
                    size,
                    1,
                    vec![
                        patch(pixel_rect(0, 0, 3, 2), 16, base),
                        patch(pixel_rect(1, 0, 2, 2), 12, overlay),
                    ],
                ),
                revision_transaction(
                    session_id,
                    1,
                    2,
                    vec![patch(pixel_rect(0, 0, 2, 2), 12, final_patch)],
                ),
            ])
            .unwrap();
        let mut executor = RecordingExecutor::default();

        let prepared = execute_planned_operations(&mut executor, &planned.operations).unwrap();

        assert_eq!(
            executor.operations,
            [
                RecordedOperation::Reset,
                RecordedOperation::Patch {
                    revision: 1,
                    patch_index: 0,
                },
                RecordedOperation::Patch {
                    revision: 1,
                    patch_index: 1,
                },
                RecordedOperation::Patch {
                    revision: 2,
                    patch_index: 0,
                },
            ]
        );
        let pixels = prepared.final_startup.unwrap().pixels.borrow().clone();
        let mut expected = Vec::new();
        for id in b"KLHMNJ" {
            expected.extend_from_slice(&[*id, *id, *id, 0]);
        }
        assert_eq!(pixels, expected);
        assert!(!pixels.iter().any(|byte| matches!(byte, 0xE0 | 0xE1 | 0xE2)));
    }

    #[test]
    fn recording_executor_steady_only_batch_uses_captured_existing_resource() {
        let session_id = SessionId::allocate();
        let size = PixelSize::new(2, 1).unwrap();
        let mut state = RemoteUpdateState::default();
        for update in [
            reset(session_id, 1, size, PixelFormat::Bgrx8UnormSrgb),
            damage(
                session_id,
                1,
                1,
                pixel_rect(0, 0, 2, 1),
                8,
                vec![b'A', b'A', b'A', 0, b'B', b'B', b'B', 0],
            ),
            boundary(session_id, 1, 1, FrameCompleteness::FullBaseline),
        ] {
            let plan = state.plan(update).unwrap();
            state.commit(plan);
        }
        let planned = state
            .plan_batch(vec![revision_transaction(
                session_id,
                1,
                2,
                vec![patch(pixel_rect(1, 0, 1, 1), 4, vec![b'Z', b'Z', b'Z', 0])],
            )])
            .unwrap();
        let pixels = Rc::new(RefCell::new(vec![b'A', b'A', b'A', 0, b'B', b'B', b'B', 0]));
        let mut executor = RecordingExecutor {
            operations: Vec::new(),
            existing: Some(RecordingResource {
                size,
                pixels: pixels.clone(),
            }),
        };

        let prepared = execute_planned_operations(&mut executor, &planned.operations).unwrap();

        assert!(prepared.final_startup.is_none());
        assert!(prepared.superseded.is_empty());
        assert_eq!(*pixels.borrow(), [b'A', b'A', b'A', 0, b'Z', b'Z', b'Z', 0]);
    }

    struct RecordingBatchScopeBackend {
        observer: Arc<dyn ScopeLifecycleObserver>,
        finish_result: Result<(), GpuFaultClass>,
    }

    impl super::BatchScopeBackend for RecordingBatchScopeBackend {
        type Scope = ObservedScopeLifecycle;
        type CleanToken = ();

        fn observation(&self) -> GpuScopeObservation {
            self.observer.snapshot()
        }

        fn begin(&self) -> Result<Self::Scope, GpuFaultClass> {
            begin_observed_scope(self.observer.clone(), || Ok::<_, GpuFaultClass>(()))
                .map(|(_, lifecycle)| lifecycle)
        }

        fn finish(&self, mut scope: Self::Scope) -> Result<Self::CleanToken, GpuFaultClass> {
            scope.record_finish().unwrap();
            scope.record_poll().unwrap();
            self.finish_result
        }
    }

    struct RecordingRecordScopeBackend {
        observer: Arc<dyn ScopeLifecycleObserver>,
        finish_result: Result<(), GpuFaultClass>,
        commit_calls: Rc<std::cell::Cell<u64>>,
    }

    impl RecordScopeBackend for RecordingRecordScopeBackend {
        type Scope = ObservedScopeLifecycle;
        type CleanToken = ();

        fn begin(&self) -> Result<Self::Scope, GpuFaultClass> {
            begin_observed_scope(self.observer.clone(), || Ok::<_, GpuFaultClass>(()))
                .map(|(_, lifecycle)| lifecycle)
        }

        fn finish(&self, mut scope: Self::Scope) -> Result<Self::CleanToken, GpuFaultClass> {
            scope.record_finish().unwrap();
            scope.record_poll().unwrap();
            self.finish_result
        }

        fn commit_if_unchanged<R>(
            &self,
            _token: Self::CleanToken,
            commit: impl FnOnce() -> R,
        ) -> Result<R, GpuFaultClass> {
            self.commit_calls.set(self.commit_calls.get() + 1);
            Ok(commit())
        }
    }

    #[derive(Default)]
    struct FakeRecordState {
        pass: Option<PreparedResource>,
        receipt: Option<u64>,
    }

    #[derive(Debug)]
    struct PreparedResource {
        id: u64,
        drops: Rc<std::cell::Cell<u64>>,
    }

    impl Drop for PreparedResource {
        fn drop(&mut self) {
            self.drops.set(self.drops.get() + 1);
        }
    }

    struct FakePreparedRecord {
        resource: PreparedResource,
        state: Rc<RefCell<FakeRecordState>>,
    }

    impl PreparedRecordCommit for FakePreparedRecord {
        type Output = Option<u64>;

        fn commit(self) -> Self::Output {
            let mut state = self.state.borrow_mut();
            state.pass = Some(self.resource);
            state.receipt = Some(17);
            state.receipt
        }
    }

    #[derive(Default)]
    struct TestScopeObserver {
        observation: std::sync::Mutex<GpuScopeObservation>,
    }

    impl ScopeLifecycleObserver for TestScopeObserver {
        fn record(&self, event: ScopeLifecycleEvent) {
            let mut observation = self.observation.lock().unwrap();
            match event {
                ScopeLifecycleEvent::Begin => observation.begins += 1,
                ScopeLifecycleEvent::Finish => observation.finishes += 1,
                ScopeLifecycleEvent::Poll => observation.polls += 1,
            }
        }

        fn snapshot(&self) -> GpuScopeObservation {
            *self.observation.lock().unwrap()
        }
    }

    #[test]
    fn record_operation_error_still_finishes_and_polls_exactly_once() {
        let observer = Arc::new(TestScopeObserver::default());
        let commit_calls = Rc::new(std::cell::Cell::new(0));
        let drops = Rc::new(std::cell::Cell::new(0));
        let state = Rc::new(RefCell::new(FakeRecordState::default()));
        let backend = RecordingRecordScopeBackend {
            observer: observer.clone(),
            finish_result: Ok(()),
            commit_calls: commit_calls.clone(),
        };

        let error = execute_record_with_fault_scope(&backend, || {
            let _prepared = PreparedResource {
                id: 1,
                drops: drops.clone(),
            };
            Err::<FakePreparedRecord, _>(RendererError::UnsupportedTargetFormat)
        })
        .map(|_| ())
        .unwrap_err();

        assert_eq!(error, RendererError::UnsupportedTargetFormat);
        assert_eq!(commit_calls.get(), 0);
        assert!(state.borrow().pass.is_none());
        assert_eq!(state.borrow().receipt, None);
        assert_eq!(drops.get(), 1);
        assert_eq!(
            observer.snapshot(),
            GpuScopeObservation {
                begins: 1,
                finishes: 1,
                polls: 1,
            }
        );
    }

    #[test]
    fn successful_record_with_finish_gpu_fault_drops_prepared_without_commit() {
        let observer = Arc::new(TestScopeObserver::default());
        let commit_calls = Rc::new(std::cell::Cell::new(0));
        let drops = Rc::new(std::cell::Cell::new(0));
        let state = Rc::new(RefCell::new(FakeRecordState::default()));
        let backend = RecordingRecordScopeBackend {
            observer: observer.clone(),
            finish_result: Err(GpuFaultClass::DeviceLost),
            commit_calls: commit_calls.clone(),
        };

        let result = execute_record_with_fault_scope(&backend, || {
            Ok(FakePreparedRecord {
                resource: PreparedResource {
                    id: 2,
                    drops: drops.clone(),
                },
                state: state.clone(),
            })
        });

        assert_eq!(
            result.map(|_| ()),
            Err(RendererError::GpuFault(GpuFaultClass::DeviceLost))
        );
        assert_eq!(commit_calls.get(), 0);
        assert!(state.borrow().pass.is_none());
        assert_eq!(state.borrow().receipt, None);
        assert_eq!(drops.get(), 1);
        assert_eq!(
            observer.snapshot(),
            GpuScopeObservation {
                begins: 1,
                finishes: 1,
                polls: 1,
            }
        );
    }

    #[test]
    fn successful_record_commits_prepared_pass_and_receipt_through_epoch_gate() {
        let observer = Arc::new(TestScopeObserver::default());
        let commit_calls = Rc::new(std::cell::Cell::new(0));
        let drops = Rc::new(std::cell::Cell::new(0));
        let state = Rc::new(RefCell::new(FakeRecordState::default()));
        let backend = RecordingRecordScopeBackend {
            observer: observer.clone(),
            finish_result: Ok(()),
            commit_calls: commit_calls.clone(),
        };

        let result = execute_record_with_fault_scope(&backend, || {
            Ok(FakePreparedRecord {
                resource: PreparedResource {
                    id: 3,
                    drops: drops.clone(),
                },
                state: state.clone(),
            })
        });

        assert!(result.is_ok());
        assert_eq!(commit_calls.get(), 1);
        assert_eq!(state.borrow().pass.as_ref().map(|pass| pass.id), Some(3));
        assert_eq!(state.borrow().receipt, Some(17));
        assert_eq!(drops.get(), 0);
        assert_eq!(
            observer.snapshot(),
            GpuScopeObservation {
                begins: 1,
                finishes: 1,
                polls: 1,
            }
        );
    }

    #[test]
    fn execution_error_still_finishes_and_returns_execution_primary() {
        let observer = Arc::new(TestScopeObserver::default());
        let backend = RecordingBatchScopeBackend {
            observer,
            finish_result: Ok(()),
        };
        let identity = super::FrameBatchIdentity {
            session_id: SessionId::allocate(),
            generation: 2,
            revision: 9,
        };
        let state = RemoteUpdateState::default();
        let resource: Option<&str> = None;

        let failure = execute_with_observed_scope(&backend, identity, || {
            Err::<(), _>(RendererError::InvalidPatch)
        })
        .unwrap_err();

        assert_eq!(failure.identity, Some(identity));
        assert_eq!(failure.primary, RendererError::InvalidPatch);
        assert_eq!(failure.secondary_execution, None);
        assert_eq!(
            failure.scope,
            Some(BatchScopeDiagnostics {
                observation: GpuScopeObservation {
                    begins: 1,
                    finishes: 1,
                    polls: 1,
                },
                observed_fault: None,
            })
        );
        assert_eq!(state.current_generation(), None);
        assert_eq!(resource, None);
    }

    #[test]
    fn gpu_fault_wins_when_execution_and_finish_both_fail() {
        let observer = Arc::new(TestScopeObserver::default());
        let backend = RecordingBatchScopeBackend {
            observer,
            finish_result: Err(GpuFaultClass::Validation),
        };
        let identity = super::FrameBatchIdentity {
            session_id: SessionId::allocate(),
            generation: 3,
            revision: 10,
        };
        let state = RemoteUpdateState::default();
        let resource: Option<&str> = None;

        let failure = execute_with_observed_scope(&backend, identity, || {
            Err::<(), _>(RendererError::InvalidPatch)
        })
        .unwrap_err();

        assert_eq!(failure.identity, Some(identity));
        assert_eq!(
            failure.primary,
            RendererError::GpuFault(GpuFaultClass::Validation)
        );
        assert_eq!(
            failure.secondary_execution,
            Some(RendererError::InvalidPatch)
        );
        assert_eq!(
            failure.scope,
            Some(BatchScopeDiagnostics {
                observation: GpuScopeObservation {
                    begins: 1,
                    finishes: 1,
                    polls: 1,
                },
                observed_fault: Some(GpuFaultClass::Validation),
            })
        );
        assert_eq!(state.current_generation(), None);
        assert_eq!(resource, None);
    }

    #[test]
    fn batch_receipt_is_not_presented_before_real_submit() {
        let session_id = SessionId::allocate();
        let size = PixelSize::new(2, 2).unwrap();
        let full = patch(pixel_rect(0, 0, 2, 2), 8, vec![0x44; 16]);
        let mut state = RemoteUpdateState::default();
        let planned = state
            .plan_batch(vec![startup_transaction(
                session_id,
                1,
                size,
                1,
                vec![full],
            )])
            .unwrap();
        let mut resource = None;
        let (outcome, _) = commit_planned_batch_after_gpu(
            &mut state,
            &mut resource,
            planned,
            PreparedBatchResources {
                final_startup: Some("startup-texture"),
                superseded: Vec::new(),
            },
        );
        let success = BatchApplySuccess {
            outcome,
            scope: BatchScopeDiagnostics {
                observation: GpuScopeObservation {
                    begins: 1,
                    finishes: 1,
                    polls: 1,
                },
                observed_fault: None,
            },
        };

        assert_eq!(state.pending_receipt(), success.outcome.final_boundary);
        assert!(!state.baseline_presented());
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

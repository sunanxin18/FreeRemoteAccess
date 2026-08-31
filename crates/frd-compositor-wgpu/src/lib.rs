mod state;
mod surface;

use frd_core::{ContentViewport, PixelSize};
use frd_protocol_api::PresentationEvent;
use frd_render_wgpu::{
    complete_scope_before_resuming_unwind, GpuCleanToken, GpuContext, GpuContextError,
    GpuFaultClass, RecoveryRequirement, RemoteRenderer, RendererError,
};

use state::{
    AcquiredFrame, AcquisitionAction, ContextPairState, SurfaceSizeAction, SurfaceSizeState,
};
pub use surface::{PresentationSurface, PresentationSurfaceLease};

pub trait PresentationHooks {
    fn before_submit(&self);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PresentError {
    SurfaceCreation,
    SurfaceUnsupported,
    SurfaceDetached,
    InvalidPhysicalSize,
    ContextMismatch,
    GpuUnavailable,
    GpuFault(GpuFaultClass),
    Renderer(RendererError),
}

impl From<RendererError> for PresentError {
    fn from(value: RendererError) -> Self {
        Self::Renderer(value)
    }
}

impl From<GpuFaultClass> for PresentError {
    fn from(value: GpuFaultClass) -> Self {
        Self::GpuFault(value)
    }
}

impl From<GpuContextError> for PresentError {
    fn from(_value: GpuContextError) -> Self {
        Self::GpuUnavailable
    }
}

fn require_context_match(matches: bool) -> Result<(), PresentError> {
    matches.then_some(()).ok_or(PresentError::ContextMismatch)
}

trait FrameScopeBackend {
    type Scope;
    type CleanToken;

    fn begin(&self) -> Result<Self::Scope, GpuFaultClass>;
    fn finish(&self, scope: Self::Scope) -> Result<Self::CleanToken, GpuFaultClass>;
}

trait RecordedFrameFlow<T> {
    type Output;

    fn overlay(&mut self);
    fn before_submit(&mut self);
    fn submit(&mut self);
    fn present(&mut self);
    fn reconfigure_after_finish(&mut self) -> Result<(), PresentError>;
    fn confirm_presented(&mut self, token: T) -> Result<(), PresentError>;
    fn emit_event(self) -> Self::Output;
}

struct GpuContextFrameScopeBackend<'a> {
    context: &'a GpuContext,
}

impl<'a> GpuContextFrameScopeBackend<'a> {
    fn new(context: &'a GpuContext) -> Self {
        Self { context }
    }
}

impl FrameScopeBackend for GpuContextFrameScopeBackend<'_> {
    type Scope = frd_render_wgpu::GpuFaultScope;
    type CleanToken = GpuCleanToken;

    fn begin(&self) -> Result<Self::Scope, GpuFaultClass> {
        self.context.begin_fault_scope()
    }

    fn finish(&self, scope: Self::Scope) -> Result<Self::CleanToken, GpuFaultClass> {
        scope.finish()
    }
}

fn execute_frame_with_fault_scope<B, R>(
    backend: &B,
    record_and_present: impl FnOnce() -> Result<R, PresentError>,
) -> Result<R::Output, PresentError>
where
    B: FrameScopeBackend,
    R: RecordedFrameFlow<B::CleanToken>,
{
    let scope = backend.begin()?;
    let (finish, recorded) = complete_scope_before_resuming_unwind(
        scope,
        || {
            record_and_present().map(|mut recorded| {
                recorded.overlay();
                recorded.before_submit();
                recorded.submit();
                recorded.present();
                recorded
            })
        },
        |scope| backend.finish(scope),
    );
    match (finish, recorded) {
        (Err(fault), _) => Err(PresentError::GpuFault(fault)),
        (Ok(_), Err(error)) => Err(error),
        (Ok(clean_token), Ok(mut recorded)) => {
            recorded.reconfigure_after_finish()?;
            recorded.confirm_presented(clean_token)?;
            Ok(recorded.emit_event())
        }
    }
}

struct WgpuRecordedFrame<'a, O> {
    context: &'a GpuContext,
    presentation: &'a PresentationSurface,
    configuration: &'a Option<wgpu::SurfaceConfiguration>,
    action: AcquisitionAction,
    remote: &'a mut RemoteRenderer,
    texture: Option<wgpu::SurfaceTexture>,
    view: wgpu::TextureView,
    encoder: Option<wgpu::CommandEncoder>,
    receipt: Option<frd_render_wgpu::PresentationReceipt>,
    overlay: Option<O>,
    hooks: &'a dyn PresentationHooks,
    event: Option<PresentationEvent>,
}

impl<'a, O> WgpuRecordedFrame<'a, O>
where
    O: FnOnce(&mut wgpu::CommandEncoder, &wgpu::TextureView),
{
    #[allow(clippy::too_many_arguments)]
    fn record(
        context: &'a GpuContext,
        presentation: &'a PresentationSurface,
        configuration: &'a Option<wgpu::SurfaceConfiguration>,
        action: AcquisitionAction,
        remote: &'a mut RemoteRenderer,
        texture: wgpu::SurfaceTexture,
        viewport: Option<ContentViewport>,
        explicit_viewport: bool,
        physical_size: PixelSize,
        target_format: wgpu::TextureFormat,
        overlay: O,
        hooks: &'a dyn PresentationHooks,
    ) -> Result<Self, PresentError> {
        let view = texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder =
            context
                .device()
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("FreeRemoteDesk presentation encoder"),
                });
        let receipt = if explicit_viewport {
            remote.record_in(&mut encoder, &view, viewport, target_format)?
        } else {
            remote.record(&mut encoder, &view, physical_size, target_format)?
        };
        Ok(Self {
            context,
            presentation,
            configuration,
            action,
            remote,
            texture: Some(texture),
            view,
            encoder: Some(encoder),
            receipt,
            overlay: Some(overlay),
            hooks,
            event: None,
        })
    }
}

impl<O> RecordedFrameFlow<GpuCleanToken> for WgpuRecordedFrame<'_, O>
where
    O: FnOnce(&mut wgpu::CommandEncoder, &wgpu::TextureView),
{
    type Output = Option<PresentationEvent>;

    fn overlay(&mut self) {
        self.overlay.take().expect("叠加层回调只调用一次")(
            self.encoder.as_mut().expect("呈现编码器尚未提交"),
            &self.view,
        );
    }

    fn before_submit(&mut self) {
        self.hooks.before_submit();
    }

    fn submit(&mut self) {
        let encoder = self.encoder.take().expect("呈现编码器只提交一次");
        self.context.queue().submit([encoder.finish()]);
    }

    fn present(&mut self) {
        let texture = self.texture.take().expect("呈现纹理只提交一次");
        self.context.queue().present(texture);
    }

    fn reconfigure_after_finish(&mut self) -> Result<(), PresentError> {
        if self.action != AcquisitionAction::RenderThenReconfigure {
            return Ok(());
        }
        let configuration = self
            .configuration
            .as_ref()
            .ok_or(PresentError::SurfaceDetached)?;
        let surface = self
            .presentation
            .surface()
            .ok_or(PresentError::SurfaceDetached)?;
        let token = configure_surface_with(surface, self.context, configuration)?;
        self.context.commit_if_unchanged(token, || ())?;
        Ok(())
    }

    fn confirm_presented(&mut self, token: GpuCleanToken) -> Result<(), PresentError> {
        if let Some(receipt) = self.receipt {
            let receipt = self
                .remote
                .confirm_presented(token, receipt)?
                .into_receipt();
            self.event = Some(PresentationEvent::FramePresented {
                session_id: receipt.session_id,
                generation: receipt.generation,
                revision: receipt.revision,
                completeness: receipt.completeness,
            });
        } else {
            self.context.commit_if_unchanged(token, || ())?;
        }
        Ok(())
    }

    fn emit_event(self) -> Self::Output {
        self.event
    }
}

pub struct PresentationCompositor {
    context: GpuContext,
    presentation: PresentationSurface,
    configuration: Option<wgpu::SurfaceConfiguration>,
    size_state: SurfaceSizeState,
}

impl PresentationCompositor {
    pub fn new(
        presentation: PresentationSurface,
        context: GpuContext,
        physical_size: PixelSize,
    ) -> Result<Self, PresentError> {
        let size_state =
            SurfaceSizeState::new(physical_size).ok_or(PresentError::InvalidPhysicalSize)?;
        let mut compositor = Self {
            context,
            presentation,
            configuration: None,
            size_state,
        };
        compositor.configure_surface()?;
        Ok(compositor)
    }

    pub fn target_format(&self) -> Option<wgpu::TextureFormat> {
        self.configuration.as_ref().map(|config| config.format)
    }

    pub fn resize(&mut self, size: PixelSize) -> Result<(), PresentError> {
        let previous = self.size_state;
        let result = (|| {
            match self.size_state.resize(size) {
                SurfaceSizeAction::Pause => {}
                SurfaceSizeAction::Configure(size) => {
                    if let Some(mut configuration) = self.configuration.clone() {
                        configuration.width = size.width;
                        configuration.height = size.height;
                        let token = self.configure_existing_with(&configuration)?;
                        let context = self.context.clone();
                        context.commit_if_unchanged(token, || {
                            self.configuration = Some(configuration);
                        })?;
                    } else {
                        self.configure_surface()?;
                    }
                }
            }
            Ok(())
        })();
        if result.is_err() {
            self.size_state = previous;
        }
        result
    }

    pub fn pause_presenting(&mut self) {
        self.size_state.pause();
    }

    pub fn render(
        &mut self,
        remote: &mut RemoteRenderer,
        overlay: impl FnOnce(&mut wgpu::CommandEncoder, &wgpu::TextureView),
        hooks: &dyn PresentationHooks,
    ) -> Result<Option<PresentationEvent>, PresentError> {
        self.render_with_viewport(remote, None, false, overlay, hooks)
    }

    pub fn render_in(
        &mut self,
        remote: &mut RemoteRenderer,
        viewport: Option<ContentViewport>,
        overlay: impl FnOnce(&mut wgpu::CommandEncoder, &wgpu::TextureView),
        hooks: &dyn PresentationHooks,
    ) -> Result<Option<PresentationEvent>, PresentError> {
        self.render_with_viewport(remote, viewport, true, overlay, hooks)
    }

    fn render_with_viewport(
        &mut self,
        remote: &mut RemoteRenderer,
        viewport: Option<ContentViewport>,
        explicit_viewport: bool,
        overlay: impl FnOnce(&mut wgpu::CommandEncoder, &wgpu::TextureView),
        hooks: &dyn PresentationHooks,
    ) -> Result<Option<PresentationEvent>, PresentError> {
        let context_pair = ContextPairState::new(self.context.context_id(), remote.context_id());
        require_context_match(context_pair.matches() && remote.uses_context(&self.context))?;
        if let Some(fault) = self.context.observed_fault() {
            return Err(PresentError::GpuFault(fault));
        }
        let Some(physical_size) = self.size_state.active() else {
            return Ok(None);
        };
        let target_format = self
            .configuration
            .as_ref()
            .ok_or(PresentError::SurfaceDetached)?
            .format;
        let surface = self
            .presentation
            .surface()
            .ok_or(PresentError::SurfaceDetached)?;
        let acquisition_scope = self.context.begin_fault_scope()?;
        let (finish, acquired) = complete_scope_before_resuming_unwind(
            acquisition_scope,
            || surface.get_current_texture(),
            |scope| scope.finish(),
        );
        let acquisition_token = finish?;
        self.context.commit_if_unchanged(acquisition_token, || ())?;
        let acquired = AcquiredFrame::from(acquired);
        let action = acquired.action();

        match action {
            AcquisitionAction::Render | AcquisitionAction::RenderThenReconfigure => {
                let texture = match acquired {
                    AcquiredFrame::Success(texture) | AcquiredFrame::Suboptimal(texture) => texture,
                    _ => unreachable!("渲染动作必须包含 SurfaceTexture"),
                };
                let scope_backend = GpuContextFrameScopeBackend::new(&self.context);
                // submit/present 在 wgpu 30 中不返回逐帧 Result；同一错误作用域和共享
                // observer 负责在确认回执前收集同步验证错误与设备丢失。finish 使用
                // 非阻塞 Poll，不把呈现路径变成等待 GPU 完成的 fence。
                execute_frame_with_fault_scope(&scope_backend, || {
                    WgpuRecordedFrame::record(
                        &self.context,
                        &self.presentation,
                        &self.configuration,
                        action,
                        remote,
                        texture,
                        viewport,
                        explicit_viewport,
                        physical_size,
                        target_format,
                        overlay,
                        hooks,
                    )
                })
            }
            AcquisitionAction::Reconfigure => {
                self.reconfigure_existing()?;
                Ok(None)
            }
            AcquisitionAction::RecreateSurface => {
                self.presentation.recreate(self.context.instance())?;
                self.configure_surface()?;
                Ok(None)
            }
            AcquisitionAction::Skip => Ok(None),
            AcquisitionAction::ValidationError => {
                self.context.observe_fault(GpuFaultClass::Validation);
                Err(PresentError::GpuFault(GpuFaultClass::Validation))
            }
        }
    }

    pub fn recover_gpu(
        &mut self,
        remote: &mut RemoteRenderer,
        context: GpuContext,
    ) -> Result<Option<RecoveryRequirement>, PresentError> {
        let mut context_pair =
            ContextPairState::new(self.context.context_id(), remote.context_id());
        let new_context_id = context.context_id();
        let candidate = self.presentation.create_candidate(context.instance())?;
        let configuration = if let Some(size) = self.size_state.active() {
            let configuration = surface_configuration(&candidate, &context, size)?;
            let token = configure_surface_with(&candidate, &context, &configuration)?;
            context.commit_if_unchanged(token, || ())?;
            Some(configuration)
        } else {
            None
        };
        let renderer_context = context.clone();
        let (requirement, detached_presentation) =
            remote.recover_device_coordinated(renderer_context, || {
                let old_surface = self.presentation.replace_surface(candidate);
                let old_context = std::mem::replace(&mut self.context, context);
                let old_configuration = std::mem::replace(&mut self.configuration, configuration);
                (old_surface, old_context, old_configuration)
            })?;
        drop(detached_presentation);
        context_pair.install_recovery(new_context_id);
        debug_assert!(context_pair.matches());
        debug_assert_eq!(self.context.context_id(), remote.context_id());
        Ok(requirement)
    }

    pub async fn recover_gpu_with_new_instance(
        &mut self,
        remote: &mut RemoteRenderer,
        instance: wgpu::Instance,
    ) -> Result<(Option<RecoveryRequirement>, GpuContext), PresentError> {
        let candidate = self.presentation.create_candidate(&instance)?;
        let context = GpuContext::request(instance, Some(&candidate)).await?;
        drop(candidate);
        let shell_context = context.clone();
        let requirement = self.recover_gpu(remote, context)?;
        Ok((requirement, shell_context))
    }

    pub fn detach(&mut self) {
        self.configuration = None;
        self.size_state.pause();
        self.presentation.detach();
    }

    fn reconfigure_existing(&mut self) -> Result<(), PresentError> {
        let configuration = self
            .configuration
            .as_ref()
            .ok_or(PresentError::SurfaceDetached)?;
        let token = self.configure_existing_with(configuration)?;
        self.context.commit_if_unchanged(token, || ())?;
        Ok(())
    }

    fn configure_existing_with(
        &self,
        configuration: &wgpu::SurfaceConfiguration,
    ) -> Result<GpuCleanToken, PresentError> {
        let surface = self
            .presentation
            .surface()
            .ok_or(PresentError::SurfaceDetached)?;
        configure_surface_with(surface, &self.context, configuration)
    }

    fn configure_surface(&mut self) -> Result<(), PresentError> {
        let size = self
            .size_state
            .active()
            .ok_or(PresentError::InvalidPhysicalSize)?;
        let surface = self
            .presentation
            .surface()
            .ok_or(PresentError::SurfaceDetached)?;
        let configuration = surface_configuration(surface, &self.context, size)?;
        let token = configure_surface_with(surface, &self.context, &configuration)?;
        let context = self.context.clone();
        context.commit_if_unchanged(token, || {
            self.configuration = Some(configuration);
        })?;
        Ok(())
    }
}

fn surface_configuration(
    surface: &wgpu::Surface<'_>,
    context: &GpuContext,
    size: PixelSize,
) -> Result<wgpu::SurfaceConfiguration, PresentError> {
    if size.width == 0 || size.height == 0 {
        return Err(PresentError::InvalidPhysicalSize);
    }
    let capabilities = surface.get_capabilities(context.adapter());
    let format = capabilities
        .formats
        .iter()
        .copied()
        .find(|format| *format == wgpu::TextureFormat::Bgra8UnormSrgb)
        .or_else(|| {
            capabilities
                .formats
                .iter()
                .copied()
                .find(wgpu::TextureFormat::is_srgb)
        })
        .ok_or(PresentError::SurfaceUnsupported)?;
    let present_mode = capabilities
        .present_modes
        .first()
        .copied()
        .ok_or(PresentError::SurfaceUnsupported)?;
    let alpha_mode = capabilities
        .alpha_modes
        .first()
        .copied()
        .ok_or(PresentError::SurfaceUnsupported)?;
    Ok(wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        color_space: wgpu::SurfaceColorSpace::Auto,
        width: size.width,
        height: size.height,
        desired_maximum_frame_latency: 2,
        present_mode,
        alpha_mode,
        view_formats: Vec::new(),
    })
}

fn configure_surface_with(
    surface: &wgpu::Surface<'_>,
    context: &GpuContext,
    configuration: &wgpu::SurfaceConfiguration,
) -> Result<GpuCleanToken, PresentError> {
    let scope = context.begin_fault_scope()?;
    let (finish, ()) = complete_scope_before_resuming_unwind(
        scope,
        || surface.configure(context.device(), configuration),
        |scope| scope.finish(),
    );
    finish.map_err(PresentError::from)
}

impl Drop for PresentationCompositor {
    fn drop(&mut self) {
        self.detach();
    }
}

#[cfg(test)]
mod api_tests {
    use frd_core::PixelSize;
    use frd_protocol_api::PresentationEvent;
    use frd_render_wgpu::{GpuContext, GpuContextError, RecoveryRequirement, RemoteRenderer};

    use super::{
        PresentError, PresentationCompositor, PresentationHooks, PresentationSurface,
        PresentationSurfaceLease,
    };

    fn create_surface_contract(
        instance: &wgpu::Instance,
        lease: PresentationSurfaceLease,
    ) -> Result<PresentationSurface, PresentError> {
        PresentationSurface::create(instance, lease)
    }

    fn render_contract(
        compositor: &mut PresentationCompositor,
        remote: &mut RemoteRenderer,
        hooks: &dyn PresentationHooks,
    ) -> Result<Option<PresentationEvent>, PresentError> {
        compositor.render(remote, |_encoder, _target| {}, hooks)
    }

    async fn request_context_contract(
        presentation: &PresentationSurface,
        instance: wgpu::Instance,
    ) -> Result<GpuContext, GpuContextError> {
        presentation.request_gpu_context(instance).await
    }

    fn resize_contract(compositor: &mut PresentationCompositor, size: PixelSize) {
        compositor.resize(size).unwrap();
    }

    fn coordinated_recovery_contract(
        compositor: &mut PresentationCompositor,
        renderer: &mut RemoteRenderer,
        context: GpuContext,
    ) -> Result<Option<RecoveryRequirement>, PresentError> {
        compositor.recover_gpu(renderer, context)
    }

    async fn new_instance_recovery_contract(
        compositor: &mut PresentationCompositor,
        renderer: &mut RemoteRenderer,
        instance: wgpu::Instance,
    ) -> Result<(Option<RecoveryRequirement>, GpuContext), PresentError> {
        compositor
            .recover_gpu_with_new_instance(renderer, instance)
            .await
    }

    #[test]
    fn public_compositor_boundary_keeps_surface_ownership_and_overlay_recording_local() {
        let _ = create_surface_contract;
        let _ = request_context_contract;
        let _ = render_contract;
        let _ = resize_contract;
        let _ = coordinated_recovery_contract;
        let _ = new_instance_recovery_contract;
    }

    #[test]
    fn context_mismatch_is_a_stable_fail_closed_error() {
        assert_eq!(
            super::require_context_match(false),
            Err(PresentError::ContextMismatch)
        );
        assert_eq!(super::require_context_match(true), Ok(()));
    }
}

#[cfg(test)]
mod scope_tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use frd_render_wgpu::{GpuFaultClass, RendererError};

    use super::{
        execute_frame_with_fault_scope, FrameScopeBackend, PresentError, RecordedFrameFlow,
    };

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum ScopeName {
        Inner,
        Outer,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum LifecycleEvent {
        Begin(ScopeName),
        Record,
        Finish(ScopeName),
        Poll(ScopeName),
        Overlay,
        BeforeSubmit,
        Submit,
        Present,
        Reconfigure,
        ConfirmReceipt,
        FramePresented,
    }

    struct RecordingFrameScopeBackend {
        name: ScopeName,
        events: Rc<RefCell<Vec<LifecycleEvent>>>,
        finish_result: Result<(), GpuFaultClass>,
    }

    impl FrameScopeBackend for RecordingFrameScopeBackend {
        type Scope = ();
        type CleanToken = ();

        fn begin(&self) -> Result<Self::Scope, GpuFaultClass> {
            self.events
                .borrow_mut()
                .push(LifecycleEvent::Begin(self.name));
            Ok(())
        }

        fn finish(&self, _scope: Self::Scope) -> Result<Self::CleanToken, GpuFaultClass> {
            let mut events = self.events.borrow_mut();
            events.push(LifecycleEvent::Finish(self.name));
            events.push(LifecycleEvent::Poll(self.name));
            self.finish_result
        }
    }

    struct RecordingPresentedFrame {
        events: Rc<RefCell<Vec<LifecycleEvent>>>,
    }

    impl RecordedFrameFlow<()> for RecordingPresentedFrame {
        type Output = ();

        fn overlay(&mut self) {
            self.events.borrow_mut().push(LifecycleEvent::Overlay);
        }

        fn before_submit(&mut self) {
            self.events.borrow_mut().push(LifecycleEvent::BeforeSubmit);
        }

        fn submit(&mut self) {
            self.events.borrow_mut().push(LifecycleEvent::Submit);
        }

        fn present(&mut self) {
            self.events.borrow_mut().push(LifecycleEvent::Present);
        }

        fn reconfigure_after_finish(&mut self) -> Result<(), PresentError> {
            self.events.borrow_mut().push(LifecycleEvent::Reconfigure);
            Ok(())
        }

        fn confirm_presented(&mut self, _token: ()) -> Result<(), PresentError> {
            self.events
                .borrow_mut()
                .push(LifecycleEvent::ConfirmReceipt);
            Ok(())
        }

        fn emit_event(self) -> Self::Output {
            self.events
                .borrow_mut()
                .push(LifecycleEvent::FramePresented);
        }
    }

    struct SilentRecordedFrame;

    impl RecordedFrameFlow<()> for SilentRecordedFrame {
        type Output = ();

        fn overlay(&mut self) {}

        fn before_submit(&mut self) {}

        fn submit(&mut self) {}

        fn present(&mut self) {}

        fn reconfigure_after_finish(&mut self) -> Result<(), PresentError> {
            Ok(())
        }

        fn confirm_presented(&mut self, _token: ()) -> Result<(), PresentError> {
            Ok(())
        }

        fn emit_event(self) -> Self::Output {}
    }

    #[derive(Clone, Copy)]
    enum PanicStage {
        Overlay,
        BeforeSubmit,
    }

    struct PanickingRecordedFrame {
        events: Rc<RefCell<Vec<LifecycleEvent>>>,
        drops: Rc<std::cell::Cell<u64>>,
        stage: PanicStage,
    }

    impl Drop for PanickingRecordedFrame {
        fn drop(&mut self) {
            self.drops.set(self.drops.get() + 1);
        }
    }

    impl RecordedFrameFlow<()> for PanickingRecordedFrame {
        type Output = ();

        fn overlay(&mut self) {
            self.events.borrow_mut().push(LifecycleEvent::Overlay);
            if matches!(self.stage, PanicStage::Overlay) {
                std::panic::panic_any("presentation overlay panic");
            }
        }

        fn before_submit(&mut self) {
            self.events.borrow_mut().push(LifecycleEvent::BeforeSubmit);
            if matches!(self.stage, PanicStage::BeforeSubmit) {
                std::panic::panic_any("presentation hook panic");
            }
        }

        fn submit(&mut self) {
            self.events.borrow_mut().push(LifecycleEvent::Submit);
        }

        fn present(&mut self) {
            self.events.borrow_mut().push(LifecycleEvent::Present);
        }

        fn reconfigure_after_finish(&mut self) -> Result<(), PresentError> {
            self.events.borrow_mut().push(LifecycleEvent::Reconfigure);
            Ok(())
        }

        fn confirm_presented(&mut self, _token: ()) -> Result<(), PresentError> {
            self.events
                .borrow_mut()
                .push(LifecycleEvent::ConfirmReceipt);
            Ok(())
        }

        fn emit_event(self) -> Self::Output {
            self.events
                .borrow_mut()
                .push(LifecycleEvent::FramePresented);
        }
    }

    fn assert_presentation_panic_closes_scope(
        stage: PanicStage,
        expected_payload: &'static str,
        expected_events: &[LifecycleEvent],
    ) {
        let events = Rc::new(RefCell::new(Vec::new()));
        let drops = Rc::new(std::cell::Cell::new(0));
        let backend = RecordingFrameScopeBackend {
            name: ScopeName::Outer,
            events: events.clone(),
            finish_result: Ok(()),
        };

        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            execute_frame_with_fault_scope(&backend, || {
                events.borrow_mut().push(LifecycleEvent::Record);
                Ok(PanickingRecordedFrame {
                    events: events.clone(),
                    drops: drops.clone(),
                    stage,
                })
            })
        }))
        .unwrap_err();

        assert_eq!(panic.downcast_ref::<&str>(), Some(&expected_payload));
        assert_eq!(&*events.borrow(), expected_events);
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn overlay_panic_closes_scope_before_resuming_without_presentation_callbacks() {
        assert_presentation_panic_closes_scope(
            PanicStage::Overlay,
            "presentation overlay panic",
            &[
                LifecycleEvent::Begin(ScopeName::Outer),
                LifecycleEvent::Record,
                LifecycleEvent::Overlay,
                LifecycleEvent::Finish(ScopeName::Outer),
                LifecycleEvent::Poll(ScopeName::Outer),
            ],
        );
    }

    #[test]
    fn hook_panic_closes_scope_before_resuming_without_submit_or_confirmation() {
        assert_presentation_panic_closes_scope(
            PanicStage::BeforeSubmit,
            "presentation hook panic",
            &[
                LifecycleEvent::Begin(ScopeName::Outer),
                LifecycleEvent::Record,
                LifecycleEvent::Overlay,
                LifecycleEvent::BeforeSubmit,
                LifecycleEvent::Finish(ScopeName::Outer),
                LifecycleEvent::Poll(ScopeName::Outer),
            ],
        );
    }

    #[test]
    fn outer_frame_scope_closes_when_record_fails_without_presentation_callbacks() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let outer = RecordingFrameScopeBackend {
            name: ScopeName::Outer,
            events: events.clone(),
            finish_result: Ok(()),
        };

        let error = execute_frame_with_fault_scope(&outer, || {
            events.borrow_mut().push(LifecycleEvent::Record);
            Err::<RecordingPresentedFrame, _>(PresentError::Renderer(
                RendererError::UnsupportedTargetFormat,
            ))
        })
        .map(|_| ())
        .unwrap_err();

        assert_eq!(
            error,
            PresentError::Renderer(RendererError::UnsupportedTargetFormat)
        );
        assert_eq!(
            *events.borrow(),
            [
                LifecycleEvent::Begin(ScopeName::Outer),
                LifecycleEvent::Record,
                LifecycleEvent::Finish(ScopeName::Outer),
                LifecycleEvent::Poll(ScopeName::Outer),
            ]
        );
    }

    #[test]
    fn outer_gpu_fault_wins_after_inner_gpu_fault_and_both_scopes_close() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let outer = RecordingFrameScopeBackend {
            name: ScopeName::Outer,
            events: events.clone(),
            finish_result: Err(GpuFaultClass::DeviceLost),
        };
        let inner = RecordingFrameScopeBackend {
            name: ScopeName::Inner,
            events: events.clone(),
            finish_result: Err(GpuFaultClass::Validation),
        };

        let result = execute_frame_with_fault_scope(&outer, || {
            let inner_result = execute_frame_with_fault_scope(&inner, || {
                events.borrow_mut().push(LifecycleEvent::Record);
                Ok(SilentRecordedFrame)
            });
            inner_result?;
            Ok(RecordingPresentedFrame {
                events: events.clone(),
            })
        });

        assert_eq!(
            result.map(|_| ()),
            Err(PresentError::GpuFault(GpuFaultClass::DeviceLost))
        );
        assert_eq!(
            *events.borrow(),
            [
                LifecycleEvent::Begin(ScopeName::Outer),
                LifecycleEvent::Begin(ScopeName::Inner),
                LifecycleEvent::Record,
                LifecycleEvent::Finish(ScopeName::Inner),
                LifecycleEvent::Poll(ScopeName::Inner),
                LifecycleEvent::Finish(ScopeName::Outer),
                LifecycleEvent::Poll(ScopeName::Outer),
            ]
        );
    }

    #[test]
    fn successful_nested_scopes_preserve_presentation_and_confirmation_order() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let outer = RecordingFrameScopeBackend {
            name: ScopeName::Outer,
            events: events.clone(),
            finish_result: Ok(()),
        };
        let inner = RecordingFrameScopeBackend {
            name: ScopeName::Inner,
            events: events.clone(),
            finish_result: Ok(()),
        };

        let result = execute_frame_with_fault_scope(&outer, || {
            let inner_result = execute_frame_with_fault_scope(&inner, || {
                events.borrow_mut().push(LifecycleEvent::Record);
                Ok(SilentRecordedFrame)
            });
            assert!(inner_result.is_ok());
            Ok(RecordingPresentedFrame {
                events: events.clone(),
            })
        });
        assert!(result.is_ok());

        assert_eq!(
            *events.borrow(),
            [
                LifecycleEvent::Begin(ScopeName::Outer),
                LifecycleEvent::Begin(ScopeName::Inner),
                LifecycleEvent::Record,
                LifecycleEvent::Finish(ScopeName::Inner),
                LifecycleEvent::Poll(ScopeName::Inner),
                LifecycleEvent::Overlay,
                LifecycleEvent::BeforeSubmit,
                LifecycleEvent::Submit,
                LifecycleEvent::Present,
                LifecycleEvent::Finish(ScopeName::Outer),
                LifecycleEvent::Poll(ScopeName::Outer),
                LifecycleEvent::Reconfigure,
                LifecycleEvent::ConfirmReceipt,
                LifecycleEvent::FramePresented,
            ]
        );
    }
}

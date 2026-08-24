use std::error::Error;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytemuck::{Pod, Zeroable};
use egui_wgpu::{Renderer as EguiRenderer, RendererOptions, ScreenDescriptor};
use wgpu::util::DeviceExt;
use winit::dpi::PhysicalSize;
use winit::window::Window;

use crate::core::{RemotePixelFormat, RenderUpdate};

use super::{
    RemoteTextureAction, RendererRuntimePolicy, ResetDisposition, SurfaceAcquireOutcome,
    SurfaceRecoveryController, SurfaceRecoveryDecision, SurfaceRecoveryExecutor,
    SurfaceRecoveryPort, TextureUpdateDisposition,
};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ViewportUniform {
    host_size: [f32; 2],
    remote_size: [f32; 2],
}

struct RemoteTexture {
    _texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    format: RemotePixelFormat,
}

/// `egui::Context::run_ui` transfers its texture delta to the integration. A
/// recoverable surface acquisition failure must therefore retain that delta;
/// egui will not submit the same font/image upload again on its own.
#[derive(Default)]
struct PendingEguiTextureDeltas {
    delta: egui::TexturesDelta,
}

impl PendingEguiTextureDeltas {
    fn append(&mut self, newer: egui::TexturesDelta) {
        self.delta.append(newer);
    }

    fn take_sets(&mut self) -> egui::TexturesDelta {
        let mut sets = std::mem::take(&mut self.delta);
        self.delta.free.extend(std::mem::take(&mut sets.free));
        sets
    }

    fn free_after_present(&mut self, mut free: impl FnMut(egui::TextureId)) {
        for texture_id in std::mem::take(&mut self.delta.free) {
            free(texture_id);
        }
    }

    #[cfg(test)]
    fn pending_set_count(&self) -> usize {
        self.delta.set.len()
    }

    #[cfg(test)]
    fn pending_free_is_empty(&self) -> bool {
        self.delta.free.is_empty()
    }
}

pub struct Renderer {
    instance: wgpu::Instance,
    window: Arc<Window>,
    adapter: wgpu::Adapter,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    surface_format: wgpu::TextureFormat,
    size: PhysicalSize<u32>,
    remote_pipeline: wgpu::RenderPipeline,
    remote_bind_group_layout: wgpu::BindGroupLayout,
    remote_sampler: wgpu::Sampler,
    viewport_uniform: wgpu::Buffer,
    remote_texture: Option<RemoteTexture>,
    runtime_policy: RendererRuntimePolicy,
    recovery_controller: SurfaceRecoveryController,
    recovery_executor: SurfaceRecoveryExecutor,
    pending_egui_deltas: PendingEguiTextureDeltas,
    egui_renderer: EguiRenderer,
}

impl Renderer {
    pub async fn new(window: Arc<Window>) -> Result<Self, RenderError> {
        let size = window.inner_size();
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle(
            Box::new(window.clone()),
        ));
        let surface = instance
            .create_surface(window.clone())
            .map_err(|_| RenderError::new("surface_create_failed"))?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
                apply_limit_buckets: false,
            })
            .await
            .map_err(|_| RenderError::new("gpu_adapter_unavailable"))?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("FreeRemoteAccess GPU"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
            })
            .await
            .map_err(|_| RenderError::new("gpu_device_create_failed"))?;
        let initial_format = surface
            .get_capabilities(&adapter)
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .or_else(|| surface.get_capabilities(&adapter).formats.first().copied())
            .ok_or_else(|| RenderError::new("surface_format_unavailable"))?;
        let (config, surface_format) =
            Self::surface_configuration(&surface, &adapter, size, initial_format)?;
        let width = config.width;
        let height = config.height;
        surface.configure(&device, &config);

        let remote_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("remote texture bind group layout"),
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
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });
        let remote_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("remote texture sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let viewport_uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("remote viewport uniform"),
            contents: bytemuck::bytes_of(&ViewportUniform {
                host_size: [width as f32, height as f32],
                remote_size: [1.0, 1.0],
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let remote_pipeline =
            Self::create_remote_pipeline(&device, &remote_bind_group_layout, surface_format);
        let egui_renderer = EguiRenderer::new(&device, surface_format, RendererOptions::default());

        Ok(Self {
            instance,
            window,
            adapter,
            surface,
            device,
            queue,
            config,
            surface_format,
            size,
            remote_pipeline,
            remote_bind_group_layout,
            remote_sampler,
            viewport_uniform,
            remote_texture: None,
            runtime_policy: RendererRuntimePolicy::new(),
            recovery_controller: SurfaceRecoveryController::default(),
            recovery_executor: SurfaceRecoveryExecutor,
            pending_egui_deltas: PendingEguiTextureDeltas::default(),
            egui_renderer,
        })
    }

    pub fn resize(&mut self, size: PhysicalSize<u32>) {
        self.size = size;
        if size.width == 0 || size.height == 0 {
            self.runtime_policy.mark_surface_unavailable();
            return;
        }
        self.config.width = size.width;
        self.config.height = size.height;
        self.surface.configure(&self.device, &self.config);
        self.runtime_policy.mark_surface_available();
        self.update_viewport_uniform();
    }

    pub fn begin_authenticated_session(&mut self) {
        let action = self.runtime_policy.begin_authenticated_session();
        self.apply_remote_texture_action(action);
        self.update_viewport_uniform();
    }

    pub fn finish_disconnected_session(&mut self) {
        let action = self.runtime_policy.finish_disconnected_session();
        self.apply_remote_texture_action(action);
        self.update_viewport_uniform();
    }

    pub fn finish_failed_session(&mut self) {
        let action = self.runtime_policy.finish_failed_session();
        self.apply_remote_texture_action(action);
        self.update_viewport_uniform();
    }

    pub fn apply_update(&mut self, update: RenderUpdate) -> Result<bool, RenderError> {
        match update {
            RenderUpdate::Reset {
                generation,
                width,
                height,
                format,
            } => match self
                .runtime_policy
                .apply_reset(generation, width, height)
                .map_err(|_| RenderError::new("texture_reset_invalid"))?
            {
                ResetDisposition::Stale => Ok(false),
                ResetDisposition::Unchanged => {
                    if self
                        .remote_texture
                        .as_ref()
                        .is_some_and(|texture| texture.format != format)
                    {
                        return Err(RenderError::new("texture_format_changed_in_generation"));
                    }
                    Ok(false)
                }
                ResetDisposition::Created | ResetDisposition::Recreated => {
                    self.create_remote_texture(width, height, format);
                    self.update_viewport_uniform();
                    Ok(true)
                }
            },
            update @ RenderUpdate::DirtyRect { .. } => {
                if self
                    .runtime_policy
                    .classify(&update)
                    .map_err(|_| RenderError::new("texture_update_invalid"))?
                    == TextureUpdateDisposition::Stale
                {
                    return Ok(false);
                }
                let RenderUpdate::DirtyRect {
                    rect,
                    format,
                    bytes_per_row,
                    pixels,
                    ..
                } = update
                else {
                    unreachable!();
                };
                let texture = self
                    .remote_texture
                    .as_ref()
                    .ok_or_else(|| RenderError::new("texture_update_without_gpu_texture"))?;
                if texture.format != format {
                    return Err(RenderError::new("texture_update_format_mismatch"));
                }
                self.queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &texture._texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d {
                            x: rect.x(),
                            y: rect.y(),
                            z: 0,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    &pixels,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(bytes_per_row),
                        rows_per_image: Some(rect.height()),
                    },
                    wgpu::Extent3d {
                        width: rect.width(),
                        height: rect.height(),
                        depth_or_array_layers: 1,
                    },
                );
                Ok(false)
            }
            update @ RenderUpdate::Present { .. } => Ok(self
                .runtime_policy
                .classify(&update)
                .map_err(|_| RenderError::new("texture_present_invalid"))?
                == TextureUpdateDisposition::Current),
        }
    }

    pub fn render_egui(
        &mut self,
        context: &egui::Context,
        mut output: egui::FullOutput,
    ) -> Result<RenderOutcome, RenderError> {
        self.pending_egui_deltas
            .append(std::mem::take(&mut output.textures_delta));
        if self.size.width == 0 || self.size.height == 0 {
            return Ok(RenderOutcome::WaitForVisibility);
        }
        // The egui renderer remains valid across recoverable surface work, so
        // uploads are safe before acquisition. This ensures a Timeout/Lost does
        // not drop the already-taken delta from `run_ui`.
        let mut deltas_to_apply = self.pending_egui_deltas.take_sets();
        for (texture_id, deltas) in std::mem::take(&mut deltas_to_apply.set) {
            for delta in deltas {
                self.egui_renderer
                    .update_texture(&self.device, &self.queue, texture_id, &delta);
            }
        }
        let (frame, post_present_recovery) = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => {
                self.acquire_frame(SurfaceAcquireOutcome::Success, frame)?
            }
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
                self.acquire_frame(SurfaceAcquireOutcome::Suboptimal, frame)?
            }
            wgpu::CurrentSurfaceTexture::Timeout => {
                return self.handle_acquire_without_frame(SurfaceAcquireOutcome::Timeout);
            }
            wgpu::CurrentSurfaceTexture::Occluded => {
                return self.handle_acquire_without_frame(SurfaceAcquireOutcome::Occluded);
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                return self.handle_acquire_without_frame(SurfaceAcquireOutcome::Outdated);
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                return self.handle_acquire_without_frame(SurfaceAcquireOutcome::Lost);
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return self.handle_acquire_without_frame(SurfaceAcquireOutcome::Validation);
            }
        };
        let paint_jobs = context.tessellate(output.shapes, output.pixels_per_point);
        let screen = ScreenDescriptor {
            size_in_pixels: [self.size.width, self.size.height],
            pixels_per_point: output.pixels_per_point,
        };
        let view = frame.texture.create_view(&Default::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("FreeRemoteAccess frame encoder"),
            });
        let callback_buffers = self.egui_renderer.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            &paint_jobs,
            &screen,
        );
        {
            let color_attachment = wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.015,
                        g: 0.020,
                        b: 0.030,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            };
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("FreeRemoteAccess render pass"),
                color_attachments: &[Some(color_attachment)],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if let Some(texture) = &self.remote_texture {
                pass.set_pipeline(&self.remote_pipeline);
                pass.set_bind_group(0, &texture.bind_group, &[]);
                pass.draw(0..3, 0..1);
            }
            self.egui_renderer
                .render(&mut pass.forget_lifetime(), &paint_jobs, &screen);
        }
        self.queue.submit(
            callback_buffers
                .into_iter()
                .chain(std::iter::once(encoder.finish())),
        );
        let recovery_result = match post_present_recovery {
            Some(decision) => {
                let queue = self.queue.clone();
                let executor = self.recovery_executor;
                executor.execute_post_present(decision, move || queue.present(frame), self)
            }
            None => {
                self.queue.present(frame);
                Ok(super::RecoveryExecution::RetryAfter(Duration::ZERO))
            }
        };
        self.pending_egui_deltas
            .free_after_present(|texture_id| self.egui_renderer.free_texture(&texture_id));
        let execution = recovery_result.map_err(|error: RenderError| error)?;
        if post_present_recovery.is_some() {
            Ok(Self::outcome_from_recovery(execution))
        } else {
            Ok(RenderOutcome::Rendered)
        }
    }

    fn create_remote_texture(&mut self, width: u32, height: u32, format: RemotePixelFormat) {
        let gpu_format = match format {
            RemotePixelFormat::Bgra8Srgb => wgpu::TextureFormat::Bgra8UnormSrgb,
            RemotePixelFormat::Rgba8Srgb => wgpu::TextureFormat::Rgba8UnormSrgb,
        };
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("remote desktop texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: gpu_format,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("remote texture bind group"),
            layout: &self.remote_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.remote_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.viewport_uniform.as_entire_binding(),
                },
            ],
        });
        self.remote_texture = Some(RemoteTexture {
            _texture: texture,
            bind_group,
            format,
        });
    }

    fn apply_remote_texture_action(&mut self, action: RemoteTextureAction) {
        match action {
            RemoteTextureAction::Clear => self.remote_texture = None,
        }
    }

    fn acquire_frame(
        &mut self,
        outcome: SurfaceAcquireOutcome,
        frame: wgpu::SurfaceTexture,
    ) -> Result<(wgpu::SurfaceTexture, Option<SurfaceRecoveryDecision>), RenderError> {
        match self.recovery_controller.on_acquire(outcome) {
            SurfaceRecoveryDecision::Render => Ok((frame, None)),
            decision @ SurfaceRecoveryDecision::RenderThenReconfigure(_) => {
                Ok((frame, Some(decision)))
            }
            SurfaceRecoveryDecision::Terminal(code) => Err(RenderError::new(code)),
            _ => Err(RenderError::new("surface_acquire_decision_without_frame")),
        }
    }

    fn handle_acquire_without_frame(
        &mut self,
        outcome: SurfaceAcquireOutcome,
    ) -> Result<RenderOutcome, RenderError> {
        let decision = self.recovery_controller.on_acquire(outcome);
        if let SurfaceRecoveryDecision::Terminal(code) = decision {
            return Err(RenderError::new(code));
        }
        self.runtime_policy.mark_surface_unavailable();
        let executor = self.recovery_executor;
        let execution = executor
            .execute_without_frame(decision, self)
            .map_err(|error: RenderError| error)?;
        Ok(Self::outcome_from_recovery(execution))
    }

    fn outcome_from_recovery(execution: super::RecoveryExecution) -> RenderOutcome {
        match execution {
            super::RecoveryExecution::RetryAfter(delay) => {
                RenderOutcome::RetryAt(Instant::now() + delay)
            }
            super::RecoveryExecution::WaitForVisibility => RenderOutcome::WaitForVisibility,
        }
    }

    fn recreate_surface_local(&mut self) -> Result<(), RenderError> {
        let surface = self
            .instance
            .create_surface(self.window.clone())
            .map_err(|_| RenderError::new("surface_recreate_failed"))?;
        let (config, _) =
            Self::surface_configuration(&surface, &self.adapter, self.size, self.surface_format)?;
        self.surface = surface;
        self.config = config;
        Ok(())
    }

    fn configure_surface_local(&mut self) -> Result<(), RenderError> {
        if self.config.width == 0 || self.config.height == 0 {
            return Err(RenderError::new("surface_config_dimensions_invalid"));
        }
        let capabilities = self.surface.get_capabilities(&self.adapter);
        self.config = Self::refreshed_surface_configuration(&self.config, &capabilities)?;
        self.surface.configure(&self.device, &self.config);
        self.runtime_policy.mark_surface_available();
        self.update_viewport_uniform();
        Ok(())
    }

    fn surface_configuration(
        surface: &wgpu::Surface<'_>,
        adapter: &wgpu::Adapter,
        size: PhysicalSize<u32>,
        preferred_format: wgpu::TextureFormat,
    ) -> Result<(wgpu::SurfaceConfiguration, wgpu::TextureFormat), RenderError> {
        let capabilities = surface.get_capabilities(adapter);
        let surface_format = if capabilities.formats.contains(&preferred_format) {
            preferred_format
        } else {
            return Err(RenderError::new("surface_format_changed"));
        };
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 1,
            alpha_mode: capabilities
                .alpha_modes
                .first()
                .copied()
                .unwrap_or(wgpu::CompositeAlphaMode::Auto),
            view_formats: vec![],
        };
        let config = Self::refreshed_surface_configuration(&config, &capabilities)?;
        Ok((config, surface_format))
    }

    fn refreshed_surface_configuration(
        current: &wgpu::SurfaceConfiguration,
        capabilities: &wgpu::SurfaceCapabilities,
    ) -> Result<wgpu::SurfaceConfiguration, RenderError> {
        if !capabilities.formats.contains(&current.format) {
            return Err(RenderError::new("surface_format_changed"));
        }
        if !capabilities.usages.contains(current.usage) {
            return Err(RenderError::new("surface_usage_unavailable"));
        }
        if !capabilities.present_modes.contains(&current.present_mode) {
            return Err(RenderError::new("surface_present_mode_unavailable"));
        }
        let mut config = current.clone();
        if !capabilities.alpha_modes.contains(&config.alpha_mode) {
            config.alpha_mode = capabilities
                .alpha_modes
                .first()
                .copied()
                .unwrap_or(wgpu::CompositeAlphaMode::Auto);
        }
        Ok(config)
    }

    fn create_remote_pipeline(
        device: &wgpu::Device,
        remote_bind_group_layout: &wgpu::BindGroupLayout,
        surface_format: wgpu::TextureFormat,
    ) -> wgpu::RenderPipeline {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("remote surface shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/remote_surface.wgsl").into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("remote surface pipeline layout"),
            bind_group_layouts: &[Some(remote_bind_group_layout)],
            immediate_size: 0,
        });
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("remote surface pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fragment_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        })
    }

    fn update_viewport_uniform(&self) {
        let remote_size = self
            .runtime_policy
            .dimensions()
            .map(|(width, height)| [width as f32, height as f32])
            .unwrap_or([1.0, 1.0]);
        self.queue.write_buffer(
            &self.viewport_uniform,
            0,
            bytemuck::bytes_of(&ViewportUniform {
                host_size: [self.config.width as f32, self.config.height as f32],
                remote_size,
            }),
        );
    }
}

impl SurfaceRecoveryPort for Renderer {
    type Error = RenderError;

    fn recreate_surface(&mut self) -> Result<(), Self::Error> {
        self.recreate_surface_local()
    }

    fn configure_surface(&mut self) -> Result<(), Self::Error> {
        self.configure_surface_local()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderOutcome {
    Rendered,
    RetryAt(Instant),
    WaitForVisibility,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderError {
    code: &'static str,
}

impl RenderError {
    const fn new(code: &'static str) -> Self {
        Self { code }
    }

    pub const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Display for RenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "GPU 渲染失败 ({})", self.code)
    }
}

impl Error for RenderError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_egui_deltas_survive_timeout_and_release_only_after_present() {
        let mut pending = PendingEguiTextureDeltas::default();
        let context = egui::Context::default();
        let first = context.run_ui(Default::default(), |ui| {
            ui.label("font texture must be uploaded once");
        });
        let texture_id = *first
            .textures_delta
            .set
            .keys()
            .next()
            .expect("first egui frame publishes a font texture delta");
        pending.append(first.textures_delta);

        // `run_ui` has already transferred the font upload to the integration.
        // The production renderer applies it before acquire, so a subsequent
        // Timeout/Lost cannot make the next successful frame miss the font.
        assert_eq!(pending.pending_set_count(), 1);
        assert!(pending.pending_free_is_empty());

        let mut operations = Vec::new();
        let mut sets = pending.take_sets();
        for (id, deltas) in std::mem::take(&mut sets.set) {
            for _ in deltas {
                operations.push(("set", id));
            }
        }
        assert_eq!(operations, [("set", texture_id)]);
        assert!(pending.pending_free_is_empty());

        operations.push(("timeout", texture_id));
        operations.push(("lost", texture_id));
        operations.push(("present", texture_id));
        let mut second = egui::TexturesDelta::default();
        second.free.insert(texture_id);
        pending.append(second);
        let second_sets = pending.take_sets();
        assert!(second_sets.set.is_empty());
        operations.push(("present", texture_id));
        assert!(!pending.pending_free_is_empty());
        pending.free_after_present(|id| operations.push(("free", id)));
        assert_eq!(
            operations,
            [
                ("set", texture_id),
                ("timeout", texture_id),
                ("lost", texture_id),
                ("present", texture_id),
                ("present", texture_id),
                ("free", texture_id)
            ]
        );
        assert!(pending.pending_free_is_empty());
    }

    #[test]
    fn refreshed_capabilities_preserve_format_and_refresh_alpha() {
        let mut config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: wgpu::TextureFormat::Bgra8UnormSrgb,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: 16,
            height: 16,
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 1,
            alpha_mode: wgpu::CompositeAlphaMode::Opaque,
            view_formats: vec![],
        };
        let capabilities = wgpu::SurfaceCapabilities {
            formats: vec![wgpu::TextureFormat::Bgra8UnormSrgb],
            format_capabilities: vec![],
            present_modes: vec![wgpu::PresentMode::Fifo],
            alpha_modes: vec![wgpu::CompositeAlphaMode::PreMultiplied],
            usages: wgpu::TextureUsages::RENDER_ATTACHMENT,
        };
        config = Renderer::refreshed_surface_configuration(&config, &capabilities).unwrap();
        assert_eq!(config.format, wgpu::TextureFormat::Bgra8UnormSrgb);
        assert_eq!(config.alpha_mode, wgpu::CompositeAlphaMode::PreMultiplied);

        let changed_format = wgpu::SurfaceCapabilities {
            formats: vec![wgpu::TextureFormat::Rgba8UnormSrgb],
            ..capabilities
        };
        assert_eq!(
            Renderer::refreshed_surface_configuration(&config, &changed_format)
                .unwrap_err()
                .code(),
            "surface_format_changed"
        );
    }
}

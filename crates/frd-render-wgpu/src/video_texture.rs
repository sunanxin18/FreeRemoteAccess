use frd_core::{ContentViewport, PixelRect, PixelSize};
use frd_media_api::{
    DecodedVideoFrame, VideoColorimetry, VideoPixelFormat, VideoRange, VideoStreamIdentity,
    VideoTimestamp,
};

use crate::{
    complete_scope_before_resuming_unwind, GpuCleanToken, GpuContext, GpuContextId, GpuFaultClass,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VideoStreamEpoch {
    pub identity: VideoStreamIdentity,
    pub generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VideoFrameLayout {
    pub coded_size: PixelSize,
    pub visible_rect: PixelRect,
    pub plane_strides: [u32; 3],
}

impl VideoFrameLayout {
    pub fn try_from_frame(frame: &DecodedVideoFrame) -> Result<Self, VideoRendererError> {
        let frame = frame.as_input();
        if frame.format != VideoPixelFormat::Yuv444P8 || frame.planes.len() != 3 {
            return Err(VideoRendererError::UnsupportedPixelFormat);
        }
        Ok(Self {
            coded_size: frame.coded_size,
            visible_rect: frame.visible_rect,
            plane_strides: [
                frame.planes[0].stride_bytes(),
                frame.planes[1].stride_bytes(),
                frame.planes[2].stride_bytes(),
            ],
        })
    }

    pub fn visible_size(self) -> PixelSize {
        PixelSize {
            width: self.visible_rect.width,
            height: self.visible_rect.height,
        }
    }

    pub fn viewport(self, drawable: PixelSize) -> ContentViewport {
        ContentViewport::fit(self.visible_size(), drawable)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VideoColorSelection {
    pub y_offset: f32,
    pub y_scale: f32,
    /// Column-major matrix matching WGSL's `mat3x3 * vec3` convention.
    pub yuv_to_rgb: [[f32; 3]; 3],
    pub used_product_default: bool,
}

impl VideoColorSelection {
    pub fn for_metadata(
        colorimetry: VideoColorimetry,
        range: VideoRange,
    ) -> Result<Self, VideoRendererError> {
        let used_product_default = colorimetry == VideoColorimetry::Unspecified;
        if !matches!(
            colorimetry,
            VideoColorimetry::Bt709 | VideoColorimetry::Unspecified
        ) {
            return Err(VideoRendererError::UnsupportedColorimetry);
        }
        let (y_offset, y_scale, chroma_scale) = match range {
            VideoRange::Limited => (16.0 / 255.0, 255.0 / 219.0, 255.0 / 224.0),
            VideoRange::Full => (0.0, 1.0, 1.0),
        };
        Ok(Self {
            y_offset,
            y_scale,
            yuv_to_rgb: [
                [1.0, 1.0, 1.0],
                [0.0, -0.187_324 * chroma_scale, 1.855_6 * chroma_scale],
                [1.574_8 * chroma_scale, -0.468_124 * chroma_scale, 0.0],
            ],
            used_product_default,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VideoRendererError {
    UnsupportedPixelFormat,
    UnsupportedColorimetry,
    StaleStreamOrGeneration,
    StalePresentationReceipt,
    InvalidGeometry,
    TextureDimensionUnsupported,
    UnsupportedTargetFormat,
    GpuFault(GpuFaultClass),
}

impl From<GpuFaultClass> for VideoRendererError {
    fn from(value: GpuFaultClass) -> Self {
        Self::GpuFault(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VideoPresentationReceipt {
    pub identity: VideoStreamIdentity,
    pub generation: u64,
    pub timestamp: VideoTimestamp,
    upload_id: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConfirmedVideoPresentation(VideoPresentationReceipt);

impl ConfirmedVideoPresentation {
    pub fn into_receipt(self) -> VideoPresentationReceipt {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VideoUploadOutcome {
    pub receipt: VideoPresentationReceipt,
    pub layout: VideoFrameLayout,
    pub color: VideoColorSelection,
    pub rebuilt_textures: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct VideoUploadPlan {
    layout: VideoFrameLayout,
    color: VideoColorSelection,
    receipt: VideoPresentationReceipt,
    rebuild_textures: bool,
}

#[derive(Clone, Debug)]
struct VideoUploadState {
    epoch: VideoStreamEpoch,
    layout: Option<VideoFrameLayout>,
    pending_receipt: Option<VideoPresentationReceipt>,
    upload_count: u64,
    texture_rebuild_count: u64,
}

impl VideoUploadState {
    fn new(epoch: VideoStreamEpoch) -> Self {
        Self {
            epoch,
            layout: None,
            pending_receipt: None,
            upload_count: 0,
            texture_rebuild_count: 0,
        }
    }

    fn plan(&self, frame: &DecodedVideoFrame) -> Result<VideoUploadPlan, VideoRendererError> {
        let input = frame.as_input();
        if input.identity != self.epoch.identity || input.generation != self.epoch.generation {
            return Err(VideoRendererError::StaleStreamOrGeneration);
        }
        let layout = VideoFrameLayout::try_from_frame(frame)?;
        let color = VideoColorSelection::for_metadata(input.colorimetry, input.range)?;
        let rebuild_textures = self
            .layout
            .is_none_or(|current| current.coded_size != layout.coded_size);
        Ok(VideoUploadPlan {
            layout,
            color,
            receipt: VideoPresentationReceipt {
                identity: input.identity,
                generation: input.generation,
                timestamp: input.timestamp,
                upload_id: self.upload_count + 1,
            },
            rebuild_textures,
        })
    }

    fn commit(&mut self, plan: VideoUploadPlan) {
        self.layout = Some(plan.layout);
        self.pending_receipt = Some(plan.receipt);
        self.upload_count += 1;
        if plan.rebuild_textures {
            self.texture_rebuild_count += 1;
        }
    }

    fn confirm_presented(
        &mut self,
        receipt: VideoPresentationReceipt,
    ) -> Result<(), VideoRendererError> {
        if self.pending_receipt != Some(receipt) {
            return Err(VideoRendererError::StalePresentationReceipt);
        }
        self.pending_receipt = None;
        Ok(())
    }

    fn invalidate_resources(&mut self) {
        self.layout = None;
        self.pending_receipt = None;
    }

    #[cfg(test)]
    fn upload_count(&self) -> u64 {
        self.upload_count
    }

    #[cfg(test)]
    fn texture_rebuild_count(&self) -> u64 {
        self.texture_rebuild_count
    }
}

struct VideoTextures {
    planes: [wgpu::Texture; 3],
    bind_group: wgpu::BindGroup,
    uniform: wgpu::Buffer,
    coded_size: PixelSize,
}

struct VideoPass {
    target_format: wgpu::TextureFormat,
    pipeline: wgpu::RenderPipeline,
}

impl VideoPass {
    fn new(
        device: &wgpu::Device,
        bind_group_layout: &wgpu::BindGroupLayout,
        target_format: wgpu::TextureFormat,
    ) -> Result<Self, VideoRendererError> {
        if !target_format.is_srgb() {
            return Err(VideoRendererError::UnsupportedTargetFormat);
        }
        let shader = device.create_shader_module(wgpu::include_wgsl!("shaders/video_yuv444.wgsl"));
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("FreeRemoteDesk video pipeline layout"),
            bind_group_layouts: &[Some(bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("FreeRemoteDesk YUV444 video pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        Ok(Self {
            target_format,
            pipeline,
        })
    }

    fn record(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        bind_group: Option<&wgpu::BindGroup>,
        content: Option<PixelRect>,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("FreeRemoteDesk YUV444 video pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        if let (Some(bind_group), Some(content)) = (bind_group, content) {
            pass.set_viewport(
                content.x as f32,
                content.y as f32,
                content.width as f32,
                content.height as f32,
                0.0,
                1.0,
            );
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
    }
}

pub struct VideoRenderer {
    context: GpuContext,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    textures: Option<VideoTextures>,
    pass: Option<VideoPass>,
    state: Option<VideoUploadState>,
}

impl VideoRenderer {
    pub fn new(context: GpuContext) -> Result<Self, VideoRendererError> {
        let scope = context.begin_fault_scope()?;
        let (finish, (bind_group_layout, sampler)) = complete_scope_before_resuming_unwind(
            scope,
            || create_sampling_resources(context.device()),
            |scope| scope.finish(),
        );
        let token = finish?;
        let commit_context = context.clone();
        commit_context
            .commit_if_unchanged(token, || Self {
                context,
                bind_group_layout,
                sampler,
                textures: None,
                pass: None,
                state: None,
            })
            .map_err(VideoRendererError::from)
    }

    pub fn configure_stream(&mut self, epoch: VideoStreamEpoch) -> Result<(), VideoRendererError> {
        if self.state.as_ref().is_some_and(|state| {
            state.epoch.identity == epoch.identity && state.epoch.generation > epoch.generation
        }) {
            return Err(VideoRendererError::StaleStreamOrGeneration);
        }
        if self
            .state
            .as_ref()
            .is_some_and(|state| state.epoch == epoch)
        {
            return Ok(());
        }
        self.textures = None;
        self.pass = None;
        self.state = Some(VideoUploadState::new(epoch));
        Ok(())
    }

    pub fn upload_frame(
        &mut self,
        frame: DecodedVideoFrame,
    ) -> Result<VideoUploadOutcome, VideoRendererError> {
        let plan = self
            .state
            .as_ref()
            .ok_or(VideoRendererError::StaleStreamOrGeneration)?
            .plan(&frame)?;
        let max_dimension = self.context.device().limits().max_texture_dimension_2d;
        if plan.layout.coded_size.width > max_dimension
            || plan.layout.coded_size.height > max_dimension
        {
            return Err(VideoRendererError::TextureDimensionUnsupported);
        }
        let scope = self.context.begin_fault_scope()?;
        let (finish, mut candidate) = complete_scope_before_resuming_unwind(
            scope,
            || {
                let candidate = plan.rebuild_textures.then(|| {
                    create_video_textures(
                        self.context.device(),
                        &self.bind_group_layout,
                        &self.sampler,
                        plan.layout.coded_size,
                    )
                });
                let textures = candidate
                    .as_ref()
                    .or(self.textures.as_ref())
                    .expect("first video frame rebuilds textures");
                debug_assert_eq!(textures.coded_size, plan.layout.coded_size);
                for (texture, plane) in textures.planes.iter().zip(frame.as_input().planes.iter()) {
                    self.context.queue().write_texture(
                        wgpu::TexelCopyTextureInfo {
                            texture,
                            mip_level: 0,
                            origin: wgpu::Origin3d::ZERO,
                            aspect: wgpu::TextureAspect::All,
                        },
                        plane.bytes(),
                        wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(plane.stride_bytes()),
                            rows_per_image: Some(plane.height()),
                        },
                        wgpu::Extent3d {
                            width: plane.width(),
                            height: plane.height(),
                            depth_or_array_layers: 1,
                        },
                    );
                }
                self.context.queue().write_buffer(
                    &textures.uniform,
                    0,
                    &uniform_bytes(plan.layout, plan.color),
                );
                candidate
            },
            |scope| scope.finish(),
        );
        let token = finish?;
        let context = self.context.clone();
        let state = self
            .state
            .as_mut()
            .expect("configured video stream remains present");
        let textures = &mut self.textures;
        let old = context
            .commit_if_unchanged(token, || {
                let old = candidate
                    .take()
                    .and_then(|candidate| textures.replace(candidate));
                state.commit(plan);
                old
            })
            .map_err(VideoRendererError::from)?;
        drop(old);
        Ok(VideoUploadOutcome {
            receipt: plan.receipt,
            layout: plan.layout,
            color: plan.color,
            rebuilt_textures: plan.rebuild_textures,
        })
    }

    pub fn record(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        drawable: PixelSize,
        target_format: wgpu::TextureFormat,
    ) -> Result<Option<VideoPresentationReceipt>, VideoRendererError> {
        let viewport = self
            .state
            .as_ref()
            .and_then(|state| state.layout)
            .map(|layout| layout.viewport(drawable));
        self.record_in(encoder, target, viewport, target_format)
    }

    pub fn record_in(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        viewport: Option<ContentViewport>,
        target_format: wgpu::TextureFormat,
    ) -> Result<Option<VideoPresentationReceipt>, VideoRendererError> {
        let layout = self.state.as_ref().and_then(|state| state.layout);
        match (layout, viewport) {
            (Some(layout), Some(viewport)) if layout.visible_size() != viewport.remote => {
                return Err(VideoRendererError::InvalidGeometry)
            }
            (None, Some(_)) => return Err(VideoRendererError::InvalidGeometry),
            _ => {}
        }
        let scope = self.context.begin_fault_scope()?;
        let (finish, prepared) = complete_scope_before_resuming_unwind(
            scope,
            || {
                let replacement = if self
                    .pass
                    .as_ref()
                    .is_none_or(|pass| pass.target_format != target_format)
                {
                    Some(VideoPass::new(
                        self.context.device(),
                        &self.bind_group_layout,
                        target_format,
                    )?)
                } else {
                    None
                };
                replacement
                    .as_ref()
                    .or(self.pass.as_ref())
                    .expect("video pass has been created")
                    .record(
                        encoder,
                        target,
                        self.textures.as_ref().map(|textures| &textures.bind_group),
                        viewport.map(|viewport| viewport.content),
                    );
                Ok::<_, VideoRendererError>(replacement)
            },
            |scope| scope.finish(),
        );
        let token = finish?;
        let replacement = prepared?;
        let context = self.context.clone();
        let installed_pass = &mut self.pass;
        let old = context
            .commit_if_unchanged(token, || {
                replacement.and_then(|replacement| installed_pass.replace(replacement))
            })
            .map_err(VideoRendererError::from)?;
        drop(old);
        Ok(viewport.and_then(|_| self.state.as_ref()?.pending_receipt))
    }

    pub fn confirm_presented(
        &mut self,
        token: GpuCleanToken,
        receipt: VideoPresentationReceipt,
    ) -> Result<ConfirmedVideoPresentation, VideoRendererError> {
        let state = self
            .state
            .as_mut()
            .ok_or(VideoRendererError::StalePresentationReceipt)?;
        self.context
            .commit_if_unchanged(token, || state.confirm_presented(receipt))
            .map_err(VideoRendererError::from)??;
        Ok(ConfirmedVideoPresentation(receipt))
    }

    pub fn recover_device(&mut self, context: GpuContext) -> Result<(), VideoRendererError> {
        let scope = context.begin_fault_scope()?;
        let (finish, (bind_group_layout, sampler)) = complete_scope_before_resuming_unwind(
            scope,
            || create_sampling_resources(context.device()),
            |scope| scope.finish(),
        );
        let token = finish?;
        let commit_context = context.clone();
        let old = commit_context
            .commit_if_unchanged(token, || {
                let old = (
                    std::mem::replace(&mut self.context, context),
                    std::mem::replace(&mut self.bind_group_layout, bind_group_layout),
                    std::mem::replace(&mut self.sampler, sampler),
                    self.textures.take(),
                    self.pass.take(),
                );
                if let Some(state) = &mut self.state {
                    state.invalidate_resources();
                }
                old
            })
            .map_err(VideoRendererError::from)?;
        drop(old);
        Ok(())
    }

    pub fn uses_context(&self, context: &GpuContext) -> bool {
        self.context.is_same_context(context)
    }

    pub fn context_id(&self) -> GpuContextId {
        self.context.context_id()
    }

    pub fn frame_layout(&self) -> Option<VideoFrameLayout> {
        self.state.as_ref()?.layout
    }

    pub fn pending_receipt(&self) -> Option<VideoPresentationReceipt> {
        self.state.as_ref()?.pending_receipt
    }

    pub fn detach(&mut self) {
        self.textures = None;
        self.pass = None;
        self.state = None;
    }
}

fn create_sampling_resources(device: &wgpu::Device) -> (wgpu::BindGroupLayout, wgpu::Sampler) {
    let texture_entry = |binding| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    };
    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("FreeRemoteDesk YUV444 bind group layout"),
        entries: &[
            texture_entry(0),
            texture_entry(1),
            texture_entry(2),
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 4,
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
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("FreeRemoteDesk YUV444 sampler"),
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

fn create_video_textures(
    device: &wgpu::Device,
    bind_group_layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    coded_size: PixelSize,
) -> VideoTextures {
    let planes = std::array::from_fn(|index| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(match index {
                0 => "FreeRemoteDesk Y plane",
                1 => "FreeRemoteDesk U plane",
                _ => "FreeRemoteDesk V plane",
            }),
            size: wgpu::Extent3d {
                width: coded_size.width,
                height: coded_size.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
    });
    let views: [wgpu::TextureView; 3] = std::array::from_fn(|index| {
        planes[index].create_view(&wgpu::TextureViewDescriptor::default())
    });
    let uniform = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("FreeRemoteDesk YUV444 color uniform"),
        size: 80,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("FreeRemoteDesk YUV444 bind group"),
        layout: bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&views[0]),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&views[1]),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&views[2]),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: uniform.as_entire_binding(),
            },
        ],
    });
    VideoTextures {
        planes,
        bind_group,
        uniform,
        coded_size,
    }
}

fn uniform_bytes(layout: VideoFrameLayout, color: VideoColorSelection) -> Vec<u8> {
    let coded_width = layout.coded_size.width as f32;
    let coded_height = layout.coded_size.height as f32;
    let values = [
        layout.visible_rect.x as f32 / coded_width,
        layout.visible_rect.y as f32 / coded_height,
        layout.visible_rect.width as f32 / coded_width,
        layout.visible_rect.height as f32 / coded_height,
        color.y_offset,
        color.y_scale,
        0.0,
        0.0,
        color.yuv_to_rgb[0][0],
        color.yuv_to_rgb[0][1],
        color.yuv_to_rgb[0][2],
        0.0,
        color.yuv_to_rgb[1][0],
        color.yuv_to_rgb[1][1],
        color.yuv_to_rgb[1][2],
        0.0,
        color.yuv_to_rgb[2][0],
        color.yuv_to_rgb[2][1],
        color.yuv_to_rgb[2][2],
        0.0,
    ];
    values
        .into_iter()
        .flat_map(f32::to_ne_bytes)
        .collect::<Vec<_>>()
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use frd_core::{ContentViewport, PixelRect, PixelSize, SessionId};
    use frd_media_api::{
        DecodedVideoFrame, DecodedVideoFrameInput, VideoColorimetry, VideoPixelFormat, VideoPlane,
        VideoRange, VideoStreamIdentity, VideoTimestamp,
    };

    use super::{
        VideoColorSelection, VideoFrameLayout, VideoRendererError, VideoStreamEpoch,
        VideoUploadState,
    };

    fn identity(stream_id: u32) -> VideoStreamIdentity {
        VideoStreamIdentity {
            session_id: SessionId::allocate(),
            stream_id,
        }
    }

    fn frame(
        identity: VideoStreamIdentity,
        generation: u64,
        coded_size: PixelSize,
        visible_rect: PixelRect,
        strides: [u32; 3],
        colorimetry: VideoColorimetry,
        range: VideoRange,
    ) -> DecodedVideoFrame {
        let planes = strides.map(|stride| {
            VideoPlane::try_new(
                coded_size.width,
                coded_size.height,
                stride,
                vec![0x80; stride as usize * coded_size.height as usize].into_boxed_slice(),
            )
            .unwrap()
        });
        DecodedVideoFrame::try_new(DecodedVideoFrameInput {
            identity,
            generation,
            timestamp: VideoTimestamp {
                ticks: 7,
                timescale: NonZeroU32::new(90_000).unwrap(),
            },
            coded_size,
            visible_rect,
            format: VideoPixelFormat::Yuv444P8,
            colorimetry,
            range,
            planes: Box::new(planes),
        })
        .unwrap()
    }

    #[test]
    fn video_texture_layout_preserves_visible_crop_and_each_plane_stride() {
        let stream = identity(3);
        let decoded = frame(
            stream,
            9,
            PixelSize::new(8, 6).unwrap(),
            PixelRect {
                x: 2,
                y: 1,
                width: 4,
                height: 3,
            },
            [16, 20, 24],
            VideoColorimetry::Bt709,
            VideoRange::Limited,
        );

        let layout = VideoFrameLayout::try_from_frame(&decoded).unwrap();

        assert_eq!(layout.coded_size, PixelSize::new(8, 6).unwrap());
        assert_eq!(
            layout.visible_rect,
            PixelRect {
                x: 2,
                y: 1,
                width: 4,
                height: 3,
            }
        );
        assert_eq!(layout.plane_strides, [16, 20, 24]);
        assert_eq!(
            layout.viewport(PixelSize::new(12, 12).unwrap()),
            ContentViewport::fit(
                PixelSize::new(4, 3).unwrap(),
                PixelSize::new(12, 12).unwrap()
            )
        );
    }

    #[test]
    fn video_texture_color_selection_resets_for_limited_full_and_unspecified_metadata() {
        let limited =
            VideoColorSelection::for_metadata(VideoColorimetry::Bt709, VideoRange::Limited)
                .unwrap();
        let full =
            VideoColorSelection::for_metadata(VideoColorimetry::Bt709, VideoRange::Full).unwrap();
        let defaulted =
            VideoColorSelection::for_metadata(VideoColorimetry::Unspecified, VideoRange::Limited)
                .unwrap();

        assert_eq!(limited.y_offset, 16.0 / 255.0);
        assert_eq!(limited.y_scale, 255.0 / 219.0);
        assert_eq!(full.y_offset, 0.0);
        assert_eq!(full.y_scale, 1.0);
        assert!(!limited.used_product_default);
        assert!(!full.used_product_default);
        assert!(defaulted.used_product_default);
        assert_eq!(defaulted.yuv_to_rgb, limited.yuv_to_rgb);
        assert_ne!(limited.yuv_to_rgb, full.yuv_to_rgb);
    }

    #[test]
    fn video_texture_planner_rejects_stale_epoch_before_upload() {
        let current = identity(4);
        let stale = identity(5);
        let state = VideoUploadState::new(VideoStreamEpoch {
            identity: current,
            generation: 12,
        });
        let stale_generation = frame(
            current,
            11,
            PixelSize::new(4, 4).unwrap(),
            PixelRect {
                x: 0,
                y: 0,
                width: 4,
                height: 4,
            },
            [4; 3],
            VideoColorimetry::Bt709,
            VideoRange::Limited,
        );
        let stale_identity = frame(
            stale,
            12,
            PixelSize::new(4, 4).unwrap(),
            PixelRect {
                x: 0,
                y: 0,
                width: 4,
                height: 4,
            },
            [4; 3],
            VideoColorimetry::Bt709,
            VideoRange::Limited,
        );

        assert_eq!(
            state.plan(&stale_generation),
            Err(VideoRendererError::StaleStreamOrGeneration)
        );
        assert_eq!(
            state.plan(&stale_identity),
            Err(VideoRendererError::StaleStreamOrGeneration)
        );
        assert_eq!(state.upload_count(), 0);
    }

    #[test]
    fn video_texture_planner_rebuilds_only_when_coded_size_changes() {
        let stream = identity(6);
        let epoch = VideoStreamEpoch {
            identity: stream,
            generation: 13,
        };
        let mut state = VideoUploadState::new(epoch);
        let first = frame(
            stream,
            13,
            PixelSize::new(4, 4).unwrap(),
            PixelRect {
                x: 0,
                y: 0,
                width: 4,
                height: 4,
            },
            [8; 3],
            VideoColorimetry::Bt709,
            VideoRange::Limited,
        );
        let same_size = frame(
            stream,
            13,
            PixelSize::new(4, 4).unwrap(),
            PixelRect {
                x: 1,
                y: 1,
                width: 2,
                height: 2,
            },
            [12; 3],
            VideoColorimetry::Bt709,
            VideoRange::Full,
        );
        let resized = frame(
            stream,
            13,
            PixelSize::new(8, 6).unwrap(),
            PixelRect {
                x: 2,
                y: 1,
                width: 4,
                height: 3,
            },
            [16; 3],
            VideoColorimetry::Bt709,
            VideoRange::Limited,
        );

        let first_plan = state.plan(&first).unwrap();
        assert!(first_plan.rebuild_textures);
        state.commit(first_plan);
        let same_plan = state.plan(&same_size).unwrap();
        assert!(!same_plan.rebuild_textures);
        state.commit(same_plan);
        let resized_plan = state.plan(&resized).unwrap();
        assert!(resized_plan.rebuild_textures);
        state.commit(resized_plan);
        assert_eq!(state.upload_count(), 3);
        assert_eq!(state.texture_rebuild_count(), 2);
    }

    #[test]
    fn video_texture_gpu_readback_converts_red_green_blue_gray_and_applies_visible_crop() {
        pollster::block_on(async {
            let instance =
                wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
            let context = match crate::GpuContext::request(instance, None).await {
                Ok(context) => context,
                Err(crate::GpuContextError::AdapterUnavailable) => {
                    println!("SKIP adapter_unavailable: no headless adapter supports the required render path");
                    return;
                }
                Err(crate::GpuContextError::DeviceUnavailable) => panic!(
                    "device_unavailable: headless adapter was found but device creation failed"
                ),
            };
            let stream = identity(7);
            let epoch = VideoStreamEpoch {
                identity: stream,
                generation: 14,
            };
            let mut renderer = super::VideoRenderer::new(context.clone()).unwrap();
            renderer.configure_stream(epoch).unwrap();

            // Four visible pixels at coded coordinates (1,1)..(2,2):
            // red, green / blue, gray. Padding and the outer crop use unrelated bytes.
            let mut y = vec![16_u8; 8 * 4];
            let mut u = vec![128_u8; 8 * 4];
            let mut v = vec![128_u8; 8 * 4];
            for (x, y_pos, yy, uu, vv) in [
                (1, 1, 58, 97, 243),
                (2, 1, 170, 37, 21),
                (1, 2, 27, 242, 113),
                (2, 2, 125, 128, 128),
            ] {
                let offset = y_pos * 8 + x;
                y[offset] = yy;
                u[offset] = uu;
                v[offset] = vv;
            }
            let decoded = DecodedVideoFrame::try_new(DecodedVideoFrameInput {
                identity: stream,
                generation: 14,
                timestamp: VideoTimestamp {
                    ticks: 8,
                    timescale: NonZeroU32::new(90_000).unwrap(),
                },
                coded_size: PixelSize::new(4, 4).unwrap(),
                visible_rect: PixelRect {
                    x: 1,
                    y: 1,
                    width: 2,
                    height: 2,
                },
                format: VideoPixelFormat::Yuv444P8,
                colorimetry: VideoColorimetry::Bt709,
                range: VideoRange::Limited,
                planes: vec![
                    VideoPlane::try_new(4, 4, 8, y.into_boxed_slice()).unwrap(),
                    VideoPlane::try_new(4, 4, 8, u.into_boxed_slice()).unwrap(),
                    VideoPlane::try_new(4, 4, 8, v.into_boxed_slice()).unwrap(),
                ]
                .into_boxed_slice(),
            })
            .unwrap();
            let upload = renderer.upload_frame(decoded).unwrap();
            assert_eq!(upload.layout.plane_strides, [8; 3]);

            let target = context.device().create_texture(&wgpu::TextureDescriptor {
                label: Some("FreeRemoteDesk 4x4 video color gate target"),
                size: wgpu::Extent3d {
                    width: 2,
                    height: 2,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = target.create_view(&wgpu::TextureViewDescriptor::default());
            let readback = context.device().create_buffer(&wgpu::BufferDescriptor {
                label: Some("FreeRemoteDesk 4x4 video color gate readback"),
                size: 512,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut encoder =
                context
                    .device()
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("FreeRemoteDesk 4x4 video color gate encoder"),
                    });
            let receipt = renderer
                .record(
                    &mut encoder,
                    &view,
                    PixelSize::new(2, 2).unwrap(),
                    wgpu::TextureFormat::Rgba8UnormSrgb,
                )
                .unwrap();
            assert_eq!(receipt, Some(upload.receipt));
            encoder.copy_texture_to_buffer(
                target.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &readback,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(256),
                        rows_per_image: Some(2),
                    },
                },
                wgpu::Extent3d {
                    width: 2,
                    height: 2,
                    depth_or_array_layers: 1,
                },
            );
            context.queue().submit([encoder.finish()]);

            let (mapped_tx, mapped_rx) = std::sync::mpsc::sync_channel(1);
            readback
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |result| {
                    mapped_tx.send(result).unwrap()
                });
            context
                .device()
                .poll(wgpu::PollType::wait_indefinitely())
                .unwrap();
            mapped_rx.recv().unwrap().unwrap();
            let bytes = readback.slice(..).get_mapped_range().unwrap();
            let pixels = [
                [bytes[0], bytes[1], bytes[2], bytes[3]],
                [bytes[4], bytes[5], bytes[6], bytes[7]],
                [bytes[256], bytes[257], bytes[258], bytes[259]],
                [bytes[260], bytes[261], bytes[262], bytes[263]],
            ];
            println!(
                "READBACK adapter_backend={:?} pixels={pixels:?}",
                context.adapter().get_info().backend
            );
            // The gray sample's matrix result is approximately
            // [0.501232, 0.496254, 0.501859]. Inverse BT.709 followed by the
            // attachment's sRGB encoding quantizes to [140, 138, 140].
            let expected: [[u8; 3]; 4] = [[255, 0, 0], [0, 255, 0], [0, 0, 255], [140, 138, 140]];
            for (pixel, expected) in pixels.into_iter().zip(expected) {
                for channel in 0..3 {
                    assert!(
                        i16::from(pixel[channel]).abs_diff(i16::from(expected[channel])) <= 1,
                        "pixel={pixel:?}, expected={expected:?}, channel={channel}"
                    );
                }
                assert_eq!(pixel[3], 255);
            }
            drop(bytes);
            readback.unmap();
        });
    }
}

use frd_core::{ContentViewport, PixelSize};

use crate::RendererError;

pub(crate) struct RemotePass {
    target_format: wgpu::TextureFormat,
    pipeline: wgpu::RenderPipeline,
}

impl RemotePass {
    pub(crate) fn new(
        device: &wgpu::Device,
        bind_group_layout: &wgpu::BindGroupLayout,
        target_format: wgpu::TextureFormat,
    ) -> Result<Self, RendererError> {
        if !target_format.is_srgb() {
            return Err(RendererError::UnsupportedTargetFormat);
        }
        let shader =
            device.create_shader_module(wgpu::include_wgsl!("shaders/remote_surface.wgsl"));
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("FreeRemoteDesk remote pipeline layout"),
            bind_group_layouts: &[Some(bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("FreeRemoteDesk remote surface pipeline"),
            layout: Some(&pipeline_layout),
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

    pub(crate) fn matches(&self, target_format: wgpu::TextureFormat) -> bool {
        self.target_format == target_format
    }

    pub(crate) fn record(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        bind_group: Option<&wgpu::BindGroup>,
        remote_size: Option<PixelSize>,
        drawable: PixelSize,
    ) {
        let color_attachment = Some(wgpu::RenderPassColorAttachment {
            view: target,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                store: wgpu::StoreOp::Store,
            },
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("FreeRemoteDesk remote surface pass"),
            color_attachments: &[color_attachment],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });

        if let (Some(bind_group), Some(remote_size)) = (bind_group, remote_size) {
            let viewport = ContentViewport::fit(remote_size, drawable);
            pass.set_viewport(
                viewport.content.x as f32,
                viewport.content.y as f32,
                viewport.content.width as f32,
                viewport.content.height as f32,
                0.0,
                1.0,
            );
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
    }
}

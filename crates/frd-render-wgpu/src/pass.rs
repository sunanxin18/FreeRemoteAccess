use frd_core::TransformedViewport;
use wgpu::util::DeviceExt;

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
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: 16,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2],
                })],
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
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        bind_group: Option<&wgpu::BindGroup>,
        content: Option<TransformedViewport>,
    ) {
        let vertices = content.map(|v| v.vertices());
        let buffer = vertices.map(|vertices| {
            let bytes: Vec<u8> = vertices
                .iter()
                .flatten()
                .flat_map(|v| v.to_ne_bytes())
                .collect();
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("远端视口顶点"),
                contents: &bytes,
                usage: wgpu::BufferUsages::VERTEX,
            })
        });
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

        if let (Some(bind_group), Some(content)) = (bind_group, content) {
            let clip = content.clip();
            pass.set_scissor_rect(clip.x, clip.y, clip.width, clip.height);
            pass.set_vertex_buffer(0, buffer.as_ref().unwrap().slice(..));
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, bind_group, &[]);
            pass.draw(0..6, 0..1);
        }
    }
}

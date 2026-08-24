struct ViewportUniform {
    host_size: vec2<f32>,
    remote_size: vec2<f32>,
};

@group(0) @binding(0) var remote_texture: texture_2d<f32>;
@group(0) @binding(1) var remote_sampler: sampler;
@group(0) @binding(2) var<uniform> viewport: ViewportUniform;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vertex_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -3.0),
        vec2<f32>(3.0, 1.0),
        vec2<f32>(-1.0, 1.0),
    );
    var uvs = array<vec2<f32>, 3>(
        vec2<f32>(0.0, 2.0),
        vec2<f32>(2.0, 0.0),
        vec2<f32>(0.0, 0.0),
    );
    var output: VertexOutput;
    output.position = vec4<f32>(positions[vertex_index], 0.0, 1.0);
    output.uv = uvs[vertex_index];
    return output;
}

@fragment
fn fragment_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let host_aspect = viewport.host_size.x / viewport.host_size.y;
    let remote_aspect = viewport.remote_size.x / viewport.remote_size.y;
    var content_uv = input.uv;

    if (host_aspect > remote_aspect) {
        let visible_width = remote_aspect / host_aspect;
        let left = (1.0 - visible_width) * 0.5;
        if (input.uv.x < left || input.uv.x > left + visible_width) {
            return vec4<f32>(0.015, 0.020, 0.030, 1.0);
        }
        content_uv.x = (input.uv.x - left) / visible_width;
    } else {
        let visible_height = host_aspect / remote_aspect;
        let top = (1.0 - visible_height) * 0.5;
        if (input.uv.y < top || input.uv.y > top + visible_height) {
            return vec4<f32>(0.015, 0.020, 0.030, 1.0);
        }
        content_uv.y = (input.uv.y - top) / visible_height;
    }

    return textureSample(remote_texture, remote_sampler, content_uv);
}

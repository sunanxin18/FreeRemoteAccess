struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) tex_coord: vec2<f32>,
}

struct VideoColor {
    crop_origin: vec2<f32>,
    crop_size: vec2<f32>,
    y_offset: f32,
    y_scale: f32,
    _padding: vec2<f32>,
    yuv_to_rgb: mat3x3<f32>,
}

@group(0) @binding(0)
var y_tex: texture_2d<f32>;
@group(0) @binding(1)
var u_tex: texture_2d<f32>;
@group(0) @binding(2)
var v_tex: texture_2d<f32>;
@group(0) @binding(3)
var linear_sampler: sampler;
@group(0) @binding(4)
var<uniform> color: VideoColor;

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    let positions = array(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    let tex_coords = array(
        vec2<f32>(0.0, 1.0),
        vec2<f32>(2.0, 1.0),
        vec2<f32>(0.0, -1.0),
    );
    var output: VertexOutput;
    output.position = vec4<f32>(positions[vertex_index], 0.0, 1.0);
    output.tex_coord = tex_coords[vertex_index];
    return output;
}

fn bt709_to_linear_channel(encoded: f32) -> f32 {
    if encoded < 0.081 {
        return encoded / 4.5;
    }
    return pow((encoded + 0.099) / 1.099, 1.0 / 0.45);
}

fn bt709_to_linear(encoded: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        bt709_to_linear_channel(encoded.r),
        bt709_to_linear_channel(encoded.g),
        bt709_to_linear_channel(encoded.b),
    );
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let uv = color.crop_origin + input.tex_coord * color.crop_size;
    let y = (textureSample(y_tex, linear_sampler, uv).r - color.y_offset) * color.y_scale;
    let u = textureSample(u_tex, linear_sampler, uv).r - 0.5;
    let v = textureSample(v_tex, linear_sampler, uv).r - 0.5;
    let encoded_rgb = clamp(color.yuv_to_rgb * vec3<f32>(y, u, v), vec3(0.0), vec3(1.0));
    return vec4<f32>(bt709_to_linear(encoded_rgb), 1.0);
}

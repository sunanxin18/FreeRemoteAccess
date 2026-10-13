struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) tex_coord: vec2<f32>,
}

@group(0) @binding(0)
var remote_texture: texture_2d<f32>;

@group(0) @binding(1)
var remote_sampler: sampler;

@vertex
fn vs_main(@location(0) position: vec2<f32>, @location(1) tex_coord: vec2<f32>) -> VertexOutput {
    var output: VertexOutput;
    output.position = vec4<f32>(position, 0.0, 1.0);
    output.tex_coord = tex_coord;
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let remote = textureSample(remote_texture, remote_sampler, input.tex_coord);
    return vec4<f32>(remote.rgb, 1.0);
}

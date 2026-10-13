// 仅用于 native 生命周期与 GPU 的离线验收；不代表远程画面。
struct Vertex { @builtin(position) position: vec4f, @location(0) uv: vec2f };
@vertex fn vs(@builtin(vertex_index) index: u32) -> Vertex {
    var points = array<vec2f, 3>(vec2f(-1.0,-1.0), vec2f(3.0,-1.0), vec2f(-1.0,3.0));
    var result: Vertex;
    result.position = vec4f(points[index], 0.0, 1.0);
    result.uv = (points[index] + 1.0) * 0.5;
    return result;
}
@fragment fn fs(vertex: Vertex) -> @location(0) vec4f {
    if abs(vertex.uv.x - 0.5) < 0.008 || abs(vertex.uv.y - 0.5) < 0.008 { return vec4f(1.0); }
    if vertex.uv.x < 0.5 {
        if vertex.uv.y < 0.5 { return vec4f(0.05, 0.30, 0.75, 1.0); }
        return vec4f(0.90, 0.35, 0.08, 1.0);
    }
    if vertex.uv.y < 0.5 { return vec4f(0.08, 0.65, 0.40, 1.0); }
    return vec4f(0.65, 0.15, 0.60, 1.0);
}

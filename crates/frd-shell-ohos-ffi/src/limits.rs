/// 鸿蒙 GLES3 的现有纹理/片元 renderer 设备策略，不请求 compute。
pub(crate) fn renderer_limits(available: wgpu::Limits) -> wgpu::Limits {
    let variables = available.max_inter_stage_shader_variables;
    let mut required = wgpu::Limits::downlevel_webgl2_defaults()
        .using_resolution(available.clone())
        .using_alignment(available);
    // wgpu30 的 WebGL2 默认值为15，但当前 GLES3 guest 是7；现有 remote/POC
    // shader 只用1个 varying。真实 shader 仍由 wgpu 验证，超限拒绝呈现。
    required.max_inter_stage_shader_variables =
        required.max_inter_stage_shader_variables.min(variables);
    required
}
#[cfg(test)]
mod tests {
    #[test]
    fn gles3_renderer_does_not_request_more_varyings_or_compute_than_device() {
        let available = wgpu::Limits {
            max_compute_workgroups_per_dimension: 0,
            max_compute_workgroup_size_x: 0,
            max_compute_workgroup_size_y: 0,
            max_compute_workgroup_size_z: 0,
            max_compute_invocations_per_workgroup: 0,
            max_compute_workgroup_storage_size: 0,
            max_inter_stage_shader_variables: 7,
            ..wgpu::Limits::default()
        };
        let limits = super::renderer_limits(available.clone());
        assert!(limits.check_limits(&available));
        assert_eq!(
            limits.max_texture_dimension_2d,
            available.max_texture_dimension_2d
        );
        assert_eq!(limits.max_compute_workgroups_per_dimension, 0);
    }
}

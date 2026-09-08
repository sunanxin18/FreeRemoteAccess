#![cfg(all(
    target_os = "linux",
    feature = "linux-gl",
    any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")
))]
use frd_core::{ContentViewport, PixelRect, PixelSize, SessionId};
use frd_frame::{
    FrameCompleteness, FrameReset, FrameRevision, FrameTransaction, PixelBuffer, PixelFormat,
    PixelPatch,
};
use frd_render_gl::{ExternalContext, GlOutputContract, GlRenderTarget, RemoteGlRenderer};
use glow::HasContext;
use khronos_egl as egl;
use std::{ffi::c_void, rc::Rc};

fn frame(session: SessionId, generation: u64) -> FrameTransaction {
    // 顶行 red/gray，底行非端点 RGB/blue；每行额外3字节 padding，X均不同。
    let pixels = vec![
        0, 0, 255, 7, 128, 128, 128, 13, 0xee, 0xee, 0xee, 192, 96, 32, 29, 255, 0, 0, 201, 0xdd,
        0xdd, 0xdd,
    ];
    FrameTransaction::Startup {
        earliest_constituent_enqueue_at: std::time::Instant::now(),
        reset: FrameReset {
            session_id: session,
            generation,
            size: PixelSize::new(2, 2).unwrap(),
            format: PixelFormat::Bgrx8UnormSrgb,
        },
        revision: FrameRevision {
            session_id: session,
            generation,
            revision: 1,
            patches: vec![PixelPatch {
                rect: PixelRect {
                    x: 0,
                    y: 0,
                    width: 2,
                    height: 2,
                },
                stride_bytes: 11,
                pixels: PixelBuffer::new(pixels),
            }],
            completeness: FrameCompleteness::FullBaseline,
        },
    }
}

// CPU传递函数仅作测试oracle；生产采样、过滤和编码全部在GPU执行。
fn srgb_decode(value: u8) -> f64 {
    let encoded = f64::from(value) / 255.0;
    if encoded <= 0.04045 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

fn srgb_encode(linear: f64) -> u8 {
    let encoded = if linear <= 0.0031308 {
        12.92 * linear
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0).round() as u8
}

fn expected_filtered_pixel(source: &[[u8; 3]; 4], x: usize, y: usize, side: usize) -> [u8; 4] {
    let u = ((x as f64 + 0.5) * 2.0 / side as f64 - 0.5).clamp(0.0, 1.0);
    // 测试读回底部原点；source按顶部行序排列。
    let v = (((side - 1 - y) as f64 + 0.5) * 2.0 / side as f64 - 0.5).clamp(0.0, 1.0);
    let mut result = [255; 4];
    for channel in 0..3 {
        let top = srgb_decode(source[0][channel]) * (1.0 - u) + srgb_decode(source[1][channel]) * u;
        let bottom =
            srgb_decode(source[2][channel]) * (1.0 - u) + srgb_decode(source[3][channel]) * u;
        result[channel] = srgb_encode(top * (1.0 - v) + bottom * v);
    }
    result
}

unsafe fn verify_output_contracts(context: &ExternalContext, gl: &glow::Context) {
    let original_srgb = gl.is_enabled(glow::FRAMEBUFFER_SRGB);
    for source in [
        [[255, 0, 0], [128, 128, 128], [32, 96, 192], [0, 0, 255]],
        [[8, 8, 8], [10, 10, 10], [11, 11, 11], [12, 12, 12]],
    ] {
        let mut renderer = RemoteGlRenderer::create(context).unwrap();
        let mut transaction = frame(SessionId::allocate(), 1);
        if let FrameTransaction::Startup { revision, .. } = &mut transaction {
            revision.patches[0].stride_bytes = 8;
            revision.patches[0].pixels = PixelBuffer::new(
                source
                    .into_iter()
                    .enumerate()
                    .flat_map(|(index, [r, g, b])| [b, g, r, (index * 47) as u8])
                    .collect(),
            );
        }
        renderer.apply_batch(vec![transaction]).unwrap();
        for side in [2, 4] {
            let mut previous_pixels: Option<Vec<u8>> = None;
            for (contract, internal_format) in [
                (GlOutputContract::SrgbFramebuffer, glow::SRGB8_ALPHA8),
                (GlOutputContract::SrgbEncodedRgba8, glow::RGBA8),
            ] {
                let texture = gl.create_texture().unwrap();
                gl.bind_texture(glow::TEXTURE_2D, Some(texture));
                gl.tex_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    internal_format as i32,
                    side,
                    side,
                    0,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(None),
                );
                let fbo = gl.create_framebuffer().unwrap();
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
                gl.framebuffer_texture_2d(
                    glow::FRAMEBUFFER,
                    glow::COLOR_ATTACHMENT0,
                    glow::TEXTURE_2D,
                    Some(texture),
                    0,
                );
                gl.viewport(0, 0, side, side);
                let size = PixelSize::new(side as u32, side as u32).unwrap();
                let target =
                    GlRenderTarget::capture_with_output_contract(context, size, contract).unwrap();
                let other = if contract == GlOutputContract::SrgbFramebuffer {
                    GlOutputContract::SrgbEncodedRgba8
                } else {
                    assert!(GlRenderTarget::capture(context, size).is_err());
                    GlOutputContract::SrgbFramebuffer
                };
                assert!(
                    GlRenderTarget::capture_with_output_contract(context, size, other).is_err()
                );
                let viewport = ContentViewport::fit_in(
                    PixelSize::new(2, 2).unwrap(),
                    size,
                    PixelRect {
                        x: 0,
                        y: 0,
                        width: size.width,
                        height: size.height,
                    },
                )
                .unwrap();
                // 两种入口状态都必须恢复，输出均不能依赖宿主遗留的开关。
                for enabled in [false, true] {
                    if enabled {
                        gl.enable(glow::FRAMEBUFFER_SRGB);
                    } else {
                        gl.disable(glow::FRAMEBUFFER_SRGB);
                    }
                    let receipt = renderer.draw(&target, viewport).unwrap().unwrap();
                    assert!(receipt.is_valid());
                    assert_eq!(gl.is_enabled(glow::FRAMEBUFFER_SRGB), enabled);
                    let mut pixels = vec![0; (side * side * 4) as usize];
                    gl.read_pixels(
                        0,
                        0,
                        side,
                        side,
                        glow::RGBA,
                        glow::UNSIGNED_BYTE,
                        glow::PixelPackData::Slice(Some(&mut pixels)),
                    );
                    for y in 0..side as usize {
                        for x in 0..side as usize {
                            let expected = expected_filtered_pixel(&source, x, y, side as usize);
                            let offset = (y * side as usize + x) * 4;
                            for channel in 0..4 {
                                assert!(pixels[offset + channel].abs_diff(expected[channel]) <= 1,
                                    "contract={contract:?} side={side} ({x},{y}) channel={channel} actual={} expected={}", pixels[offset + channel], expected[channel]);
                            }
                            assert_eq!(pixels[offset + 3], 255);
                        }
                    }
                    if let Some(previous) = &previous_pixels {
                        for (actual, previous) in pixels.iter().zip(previous) {
                            assert!(
                                actual.abs_diff(*previous) <= 1,
                                "两个输出契约的编码字节必须一致"
                            );
                        }
                    }
                    previous_pixels = Some(pixels);
                }
                if contract == GlOutputContract::SrgbEncodedRgba8 {
                    gl.bind_texture(glow::TEXTURE_2D, Some(texture));
                    let previous_base =
                        gl.get_tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_BASE_LEVEL);
                    let previous_max =
                        gl.get_tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAX_LEVEL);
                    // 将level1设为唯一有效mip；不让level0的同尺寸图像破坏附件完整性。
                    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_BASE_LEVEL, 1);
                    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAX_LEVEL, 1);
                    gl.tex_image_2d(
                        glow::TEXTURE_2D,
                        1,
                        glow::RGBA8 as i32,
                        side,
                        side,
                        0,
                        glow::RGBA,
                        glow::UNSIGNED_BYTE,
                        glow::PixelUnpackData::Slice(None),
                    );
                    gl.framebuffer_texture_2d(
                        glow::FRAMEBUFFER,
                        glow::COLOR_ATTACHMENT0,
                        glow::TEXTURE_2D,
                        Some(texture),
                        1,
                    );
                    assert_eq!(
                        gl.check_framebuffer_status(glow::FRAMEBUFFER),
                        glow::FRAMEBUFFER_COMPLETE
                    );
                    assert!(
                        GlRenderTarget::capture_with_output_contract(context, size, contract)
                            .is_err(),
                        "编码RGBA8契约只能接受level0"
                    );
                    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_BASE_LEVEL, previous_base);
                    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAX_LEVEL, previous_max);
                    gl.framebuffer_texture_2d(
                        glow::FRAMEBUFFER,
                        glow::COLOR_ATTACHMENT0,
                        glow::TEXTURE_2D,
                        Some(texture),
                        0,
                    );
                    gl.tex_image_2d(
                        glow::TEXTURE_2D,
                        0,
                        glow::RGBA16 as i32,
                        side,
                        side,
                        0,
                        glow::RGBA,
                        glow::UNSIGNED_BYTE,
                        glow::PixelUnpackData::Slice(None),
                    );
                    assert!(
                        GlRenderTarget::capture_with_output_contract(context, size, contract)
                            .is_err(),
                        "LINEAR编码不能代替精确RGBA8格式"
                    );
                    let multisample = gl.create_texture().unwrap();
                    gl.bind_texture(glow::TEXTURE_2D_MULTISAMPLE, Some(multisample));
                    gl.tex_image_2d_multisample(
                        glow::TEXTURE_2D_MULTISAMPLE,
                        1,
                        glow::RGBA8 as i32,
                        side,
                        side,
                        true,
                    );
                    gl.framebuffer_texture_2d(
                        glow::FRAMEBUFFER,
                        glow::COLOR_ATTACHMENT0,
                        glow::TEXTURE_2D_MULTISAMPLE,
                        Some(multisample),
                        0,
                    );
                    assert_eq!(
                        gl.check_framebuffer_status(glow::FRAMEBUFFER),
                        glow::FRAMEBUFFER_COMPLETE
                    );
                    assert!(gl.get_parameter_i32(glow::SAMPLES) > 0);
                    assert!(
                        GlRenderTarget::capture_with_output_contract(context, size, contract)
                            .is_err(),
                        "编码RGBA8契约不接受多采样附件"
                    );
                    gl.delete_texture(multisample);
                }
                gl.delete_framebuffer(fbo);
                gl.delete_texture(texture);
            }
        }
        renderer.detach().unwrap();
    }
    if original_srgb {
        gl.enable(glow::FRAMEBUFFER_SRGB);
    } else {
        gl.disable(glow::FRAMEBUFFER_SRGB);
    }
    assert_eq!(gl.get_error(), glow::NO_ERROR);
}

/// 显式运行；只证明 EGL/GL 实际上传绘制，不证明 GTK 或窗口呈现/ACK。
#[test]
#[ignore = "需要原生 Linux EGL desktop GL 3.3；显式执行并保留1 test结果"]
fn native_egl_renderer_roundtrip() {
    unsafe {
        let egl =
            Rc::new(egl::DynamicInstance::<egl::EGL1_5>::load_required().expect("EGL loader"));
        let display = egl.get_display(egl::DEFAULT_DISPLAY).expect("EGL display");
        egl.initialize(display).expect("EGL initialize");
        egl.bind_api(egl::OPENGL_API).expect("desktop GL API");
        let config = egl
            .choose_first_config(
                display,
                &[
                    egl::SURFACE_TYPE,
                    egl::PBUFFER_BIT,
                    egl::RENDERABLE_TYPE,
                    egl::OPENGL_BIT,
                    egl::RED_SIZE,
                    8,
                    egl::GREEN_SIZE,
                    8,
                    egl::BLUE_SIZE,
                    8,
                    egl::NONE,
                ],
            )
            .unwrap()
            .expect("EGL pbuffer config");
        let native = egl
            .create_context(
                display,
                config,
                None,
                &[
                    egl::CONTEXT_MAJOR_VERSION,
                    3,
                    egl::CONTEXT_MINOR_VERSION,
                    3,
                    egl::CONTEXT_OPENGL_PROFILE_MASK,
                    egl::CONTEXT_OPENGL_CORE_PROFILE_BIT,
                    egl::NONE,
                ],
            )
            .expect("desktop core context");
        let surface = egl
            .create_pbuffer_surface(display, config, &[egl::WIDTH, 2, egl::HEIGHT, 2, egl::NONE])
            .unwrap();
        egl.make_current(display, Some(surface), Some(surface), Some(native))
            .unwrap();
        let checker = egl.clone();
        let context = ExternalContext::new(
            |name| {
                egl.get_proc_address(name)
                    .map_or(std::ptr::null(), |f| f as *const c_void)
            },
            move || checker.get_current_context() == Some(native),
        )
        .unwrap();
        let gl = glow::Context::from_loader_function(|name| {
            egl.get_proc_address(name)
                .map_or(std::ptr::null(), |f| f as *const c_void)
        });
        println!("native EGL desktop GL fixture; hardware_verified=0; no GTK presentation claim");
        verify_output_contracts(&context, &gl);
        let color = gl.create_texture().unwrap();
        gl.bind_texture(glow::TEXTURE_2D, Some(color));
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::SRGB8_ALPHA8 as i32,
            2,
            2,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(None),
        );
        let fbo = gl.create_framebuffer().unwrap();
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
        gl.framebuffer_texture_2d(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(color),
            0,
        );
        gl.viewport(0, 0, 2, 2);
        let target = GlRenderTarget::capture(&context, PixelSize::new(2, 2).unwrap()).unwrap();
        assert!(GlRenderTarget::capture(&context, PixelSize::new(3, 2).unwrap()).is_err());
        // 非二维sRGB附件必须拒绝，恢复绑定且不污染后续有效capture。
        let cube = gl.create_texture().unwrap();
        gl.bind_texture(glow::TEXTURE_CUBE_MAP, Some(cube));
        for face in 0..6 {
            gl.tex_image_2d(
                glow::TEXTURE_CUBE_MAP_POSITIVE_X + face,
                0,
                glow::SRGB8_ALPHA8 as i32,
                2,
                2,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(None),
            );
        }
        gl.framebuffer_texture_2d(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_CUBE_MAP_POSITIVE_X,
            Some(cube),
            0,
        );
        assert_eq!(
            gl.check_framebuffer_status(glow::FRAMEBUFFER),
            glow::FRAMEBUFFER_COMPLETE
        );
        // 保留另一张尺寸不匹配的二维宿主纹理，覆盖旧实现提前返回的错误路径。
        let unrelated = gl.create_texture().unwrap();
        gl.bind_texture(glow::TEXTURE_2D, Some(unrelated));
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA8 as i32,
            1,
            1,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(None),
        );
        assert!(GlRenderTarget::capture(&context, PixelSize::new(2, 2).unwrap()).is_err());
        assert_eq!(
            gl.get_parameter_texture(glow::TEXTURE_BINDING_2D),
            Some(unrelated)
        );
        gl.framebuffer_texture_2d(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(color),
            0,
        );
        // 此前不能调用get_error，否则会掩盖错误泄漏。
        GlRenderTarget::capture(&context, PixelSize::new(2, 2).unwrap()).unwrap();
        assert_eq!(gl.get_error(), glow::NO_ERROR);
        gl.bind_texture(glow::TEXTURE_2D, Some(color));
        gl.delete_texture(unrelated);
        gl.delete_texture(cube);
        // 这里只实测执行器消费契约，不提供/伪造窗口提交或生产 ACK。
        {
            let identity = SessionId::allocate();
            let mut first = RemoteGlRenderer::create(&context).unwrap();
            let mut second = RemoteGlRenderer::create(&context).unwrap();
            first.apply_batch(vec![frame(identity, 1)]).unwrap();
            second.apply_batch(vec![frame(identity, 1)]).unwrap();
            let view =
                ContentViewport::fit(PixelSize::new(2, 2).unwrap(), PixelSize::new(2, 2).unwrap());
            let foreign = second.draw(&target, view).unwrap().unwrap();
            assert!(
                first.confirm_submitted_draw(foreign).is_err(),
                "相同frame身份不能跨renderer确认"
            );
            let stale = first.draw(&target, view).unwrap().unwrap();
            let current = first.draw(&target, view).unwrap().unwrap();
            assert!(
                first.confirm_submitted_draw(stale).is_err(),
                "较新draw撤销旧serial"
            );
            let expected = *current.frame();
            // 故意留一个 GL 错误，并解绑 current；元数据确认不能观察/清除它。
            gl.enable(u32::MAX);
            egl.make_current(display, None, None, None).unwrap();
            let confirmed = first
                .confirm_submitted_draw(current)
                .unwrap()
                .into_receipt();
            assert_eq!(confirmed, expected);
            assert!(
                egl.get_current_context().is_none(),
                "确认不能切换宿主上下文"
            );
            egl.make_current(display, Some(surface), Some(surface), Some(native))
                .unwrap();
            assert_eq!(gl.get_error(), glow::INVALID_ENUM, "确认不能读取GL错误");
            assert!(
                first.draw(&target, view).unwrap().is_none(),
                "同revision重绘不重复发布已消费receipt"
            );
            // 外来确认失败不能消费真正所有者的pending状态。
            let own = second.draw(&target, view).unwrap().unwrap();
            assert_eq!(
                second.confirm_submitted_draw(own).unwrap().into_receipt(),
                expected
            );
            assert!(second.draw(&target, view).unwrap().is_none());
            first.detach().unwrap();
            second.detach().unwrap();
            context.drain_deletions().unwrap();
        }
        let mut renderer = RemoteGlRenderer::create(&context).unwrap();
        let session = SessionId::allocate();
        renderer.apply_batch(vec![frame(session, 1)]).unwrap();
        let viewport = ContentViewport::fit_in(
            PixelSize::new(2, 2).unwrap(),
            PixelSize::new(2, 2).unwrap(),
            PixelRect {
                x: 0,
                y: 0,
                width: 2,
                height: 2,
            },
        )
        .unwrap();
        let get_boolean_indexed: unsafe extern "system" fn(u32, u32, *mut u8) =
            std::mem::transmute(egl.get_proc_address("glGetBooleani_v").unwrap());
        let is_enabled_indexed: unsafe extern "system" fn(u32, u32) -> u8 =
            std::mem::transmute(egl.get_proc_address("glIsEnabledi").unwrap());
        gl.enable_draw_buffer(glow::BLEND, 1);
        gl.color_mask_draw_buffer(1, false, true, false, true);
        let indexed_viewports = (gl.version().major, gl.version().minor) >= (4, 1)
            || gl.supported_extensions().contains("GL_ARB_viewport_array");
        let host_viewport = [0.25, 0.5, 1.25, 1.5];
        if indexed_viewports {
            gl.viewport_f32_slice(1, 1, &[host_viewport]);
        }
        gl.enable(glow::CLIP_DISTANCE0);
        let receipt = renderer.draw(&target, viewport).unwrap().unwrap();
        assert!(gl.is_enabled(glow::CLIP_DISTANCE0), "宿主裁剪状态必须恢复");
        for index in 1..gl.get_parameter_i32(glow::MAX_CLIP_DISTANCES) as u32 {
            assert!(!gl.is_enabled(glow::CLIP_DISTANCE0 + index));
        }
        if indexed_viewports {
            let get_float_indexed: unsafe extern "system" fn(u32, u32, *mut f32) =
                std::mem::transmute(egl.get_proc_address("glGetFloati_v").unwrap());
            let mut preserved_viewport = [0.0; 4];
            get_float_indexed(glow::VIEWPORT, 1, preserved_viewport.as_mut_ptr());
            assert_eq!(
                preserved_viewport, host_viewport,
                "宿主viewport1必须保持原值"
            );
        }
        let mut mask = [0u8; 4];
        get_boolean_indexed(glow::COLOR_WRITEMASK, 1, mask.as_mut_ptr());
        assert_eq!(mask, [0, 1, 0, 1]);
        assert_ne!(is_enabled_indexed(glow::BLEND, 1), 0);

        assert!(receipt.is_valid());
        // 读回只在测试中；生产后端不含 read_pixels。
        let mut pixels = [0u8; 16];
        gl.read_pixels(
            0,
            0,
            2,
            2,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelPackData::Slice(Some(&mut pixels)),
        );
        let expected = [
            32u8, 96, 192, 255, 0, 0, 255, 255, 255, 0, 0, 255, 128, 128, 128, 255,
        ];
        for (actual, expected) in pixels.into_iter().zip(expected) {
            assert!(
                actual.abs_diff(expected) <= 1,
                "channel actual={actual} expected={expected}"
            );
        }
        assert_eq!(gl.get_error(), glow::NO_ERROR);
        renderer.apply_batch(vec![frame(session, 2)]).unwrap();
        assert!(!receipt.is_valid(), "写入新代必须撤销旧绘制记录");
        assert!(renderer.confirm_submitted_draw(receipt).is_err());
        let current = renderer.draw(&target, viewport).unwrap().unwrap();
        assert!(current.is_current());
        // 真正解绑 current；安全入口必须拒绝，且隔离旧纹理/回执。
        egl.make_current(display, None, None, None).unwrap();
        assert!(!current.is_current());
        assert!(current.is_valid(), "只读current查询不能撤销绘制记录");
        egl.make_current(display, Some(surface), Some(surface), Some(native))
            .unwrap();
        assert!(current.is_current());
        egl.make_current(display, None, None, None).unwrap();
        assert!(renderer.draw(&target, viewport).is_err());
        assert!(!current.is_valid());
        assert!(!current.is_current());
        assert!(renderer.confirm_submitted_draw(current).is_err());
        egl.make_current(display, Some(surface), Some(surface), Some(native))
            .unwrap();
        assert!(
            renderer.apply_batch(vec![frame(session, 3)]).is_err(),
            "恢复要求精确generation=2"
        );
        renderer.apply_batch(vec![frame(session, 2)]).unwrap();
        renderer.draw(&target, viewport).unwrap();
        let mut wrong = viewport;
        wrong.remote = PixelSize::new(1, 1).unwrap();
        assert!(renderer.draw(&target, wrong).is_err());
        renderer.apply_batch(vec![frame(session, 2)]).unwrap();
        wrong = viewport;
        wrong.drawable = PixelSize::new(3, 2).unwrap();
        assert!(renderer.draw(&target, wrong).is_err());
        renderer.apply_batch(vec![frame(session, 2)]).unwrap();
        // 同时覆盖像素整数倍 stride 快路径：每行12字节，仅最后4字节padding。
        let FrameTransaction::Startup { revision, .. } = frame(session, 2) else {
            unreachable!()
        };
        let input = revision.patches[0].pixels.as_bytes();
        let mut padded = Vec::new();
        padded.extend_from_slice(&input[..8]);
        padded.extend_from_slice(&[0xcc; 4]);
        padded.extend_from_slice(&input[11..19]);
        padded.extend_from_slice(&[0xcc; 4]);
        renderer
            .apply_batch(vec![FrameTransaction::Revision {
                earliest_constituent_enqueue_at: std::time::Instant::now(),
                revision: FrameRevision {
                    session_id: session,
                    generation: 2,
                    revision: 2,
                    patches: vec![PixelPatch {
                        rect: PixelRect {
                            x: 0,
                            y: 0,
                            width: 2,
                            height: 2,
                        },
                        stride_bytes: 12,
                        pixels: PixelBuffer::new(padded),
                    }],
                    completeness: FrameCompleteness::Incremental,
                },
            }])
            .unwrap();
        gl.bind_texture(glow::TEXTURE_2D, Some(color));
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::SRGB8_ALPHA8 as i32,
            4,
            4,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(None),
        );
        gl.viewport(0, 0, 4, 4);
        gl.clear_color(1.0, 0.0, 1.0, 0.5);
        gl.clear(glow::COLOR_BUFFER_BIT);
        let large = GlRenderTarget::capture(&context, PixelSize::new(4, 4).unwrap()).unwrap();
        let inset = ContentViewport::fit_in(
            PixelSize::new(2, 2).unwrap(),
            PixelSize::new(4, 4).unwrap(),
            PixelRect {
                x: 1,
                y: 1,
                width: 2,
                height: 2,
            },
        )
        .unwrap();
        let final_receipt = renderer.draw(&large, inset).unwrap().unwrap();
        let mut clear = [0.0; 4];
        gl.get_parameter_f32_slice(glow::COLOR_CLEAR_VALUE, &mut clear);
        assert_eq!(clear, [1.0, 0.0, 1.0, 0.5]);
        let mut boxed = [0u8; 64];
        gl.read_pixels(
            0,
            0,
            4,
            4,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelPackData::Slice(Some(&mut boxed)),
        );
        for y in 0..4 {
            for x in 0..4 {
                let pixel = &boxed[(y * 4 + x) * 4..(y * 4 + x + 1) * 4];
                if x == 0 || x == 3 || y == 0 || y == 3 {
                    assert_eq!(pixel, [0, 0, 0, 255]);
                } else {
                    for (actual, want) in pixel
                        .iter()
                        .zip(expected[((y - 1) * 2 + x - 1) * 4..][..4].iter())
                    {
                        assert!(actual.abs_diff(*want) <= 1);
                    }
                }
            }
        }
        egl.make_current(display, None, None, None).unwrap();
        assert!(renderer.detach().is_err());
        assert!(
            !final_receipt.is_valid(),
            "非current detach也必须撤销旧绘制记录"
        );
        assert!(renderer.confirm_submitted_draw(final_receipt).is_err());
        egl.make_current(display, Some(surface), Some(surface), Some(native))
            .unwrap();
        renderer.detach().unwrap();
        assert!(renderer.apply_batch(vec![frame(session, 2)]).is_err());
        drop(renderer);
        context.drain_deletions().unwrap();
        gl.delete_framebuffer(fbo);
        gl.delete_texture(color);
        assert_eq!(gl.get_error(), glow::NO_ERROR);
        let abandoned = RemoteGlRenderer::create(&context).unwrap();
        egl.make_current(display, None, None, None).unwrap();
        drop(abandoned); // Drop 不能在无 current 时调用 GL。
        assert!(context.drain_deletions().is_err());
        egl.make_current(display, Some(surface), Some(surface), Some(native))
            .unwrap();
        context.drain_deletions().unwrap();
        context.mark_lost();
        assert!(context.drain_deletions().is_err());
        egl.make_current(display, None, None, None).unwrap();
        egl.destroy_surface(display, surface).unwrap();
        egl.destroy_context(display, native).unwrap();
        egl.terminate(display).unwrap();
    }
}

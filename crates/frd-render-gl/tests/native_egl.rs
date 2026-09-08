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
use frd_render_gl::{ExternalContext, GlRenderTarget, RemoteGlRenderer};
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
        let receipt = renderer.draw(&target, viewport).unwrap().unwrap();
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
        let current = renderer.draw(&target, viewport).unwrap().unwrap();
        // 真正解绑 current；安全入口必须拒绝，且隔离旧纹理/回执。
        egl.make_current(display, None, None, None).unwrap();
        assert!(renderer.draw(&target, viewport).is_err());
        assert!(!current.is_valid());
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

#![cfg(all(
    target_os = "linux",
    feature = "gtk-shell",
    any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")
))]
use frd_core::{ContentViewport, PixelRect, PixelSize, SessionId};
use frd_frame::{
    FrameCompleteness, FrameReset, FrameRevision, FrameTransaction, PixelBuffer, PixelFormat,
    PixelPatch,
};
use frd_render_gl::DrawReceipt;
use frd_shell_gtk::{AdapterEvent, GtkFrameArea, SubmitError};
use glow::HasContext;
use gtk4::{gdk, glib, prelude::*};
use std::{
    ffi::c_void,
    time::{Duration, Instant},
};

fn frame(session_id: SessionId, generation: u64) -> FrameTransaction {
    FrameTransaction::Startup {
        earliest_constituent_enqueue_at: Instant::now(),
        reset: FrameReset {
            session_id,
            generation,
            size: PixelSize::new(2, 2).unwrap(),
            format: PixelFormat::Bgrx8UnormSrgb,
        },
        revision: FrameRevision {
            session_id,
            generation,
            revision: 1,
            completeness: FrameCompleteness::FullBaseline,
            patches: vec![PixelPatch {
                rect: PixelRect {
                    x: 0,
                    y: 0,
                    width: 2,
                    height: 2,
                },
                stride_bytes: 8,
                pixels: PixelBuffer::new(vec![
                    0, 0, 255, 7, 128, 128, 128, 13, 192, 96, 32, 29, 255, 0, 0, 201,
                ]),
            }],
        },
    }
}

#[test]
#[ignore = "需要独立原生 Linux GTK 显示与 Mesa；无显示不能证明通过"]
fn native_gtk_frame_adapter_roundtrip() {
    let expected_backend = std::env::var("FRD_GTK_TEST_BACKEND").unwrap_or_else(|_| "x11".into());
    assert!(matches!(expected_backend.as_str(), "x11" | "wayland"));
    let expected_scale: i32 = std::env::var("FRD_GTK_TEST_SCALE")
        .unwrap_or_else(|_| "1".into())
        .parse()
        .unwrap();
    assert!(matches!(expected_scale, 1 | 2));
    gtk4::init().expect("原生 GTK 初始化必须成功");
    let display = gdk::Display::default().unwrap();
    let expected_type = if expected_backend == "x11" {
        "GdkX11Display"
    } else {
        "GdkWaylandDisplay"
    };
    assert_eq!(display.type_().name(), expected_type);
    let never_realized = GtkFrameArea::new();
    never_realized.detach();
    assert_eq!(
        never_realized
            .submit_batch(vec![frame(SessionId::allocate(), 1)])
            .unwrap_err()
            .reason,
        SubmitError::Closed
    );
    let adapter = GtkFrameArea::new();
    let window = gtk4::Window::builder()
        .title("FreeRemoteDesk GTK 帧适配器测试")
        .default_width(128)
        .default_height(128)
        .child(adapter.widget())
        .build();
    let session_id = SessionId::allocate();
    adapter.submit_batch(vec![frame(session_id, 1)]).unwrap();
    window.present();
    let (receipt, viewport) = wait_draw(&adapter);
    assert!(receipt.is_valid());
    verify_pixels(&adapter, viewport, expected_scale, [255, 0, 0, 255]);
    adapter.widget().set_error(Some(&glib::Error::new(
        gdk::GLError::NotAvailable,
        "测试注入上下文错误",
    )));
    let rejected = adapter.submit_batch(vec![partial(session_id)]).unwrap_err();
    assert_eq!(rejected.reason, SubmitError::ContextUnavailable);
    assert_eq!(rejected.transactions.len(), 1);
    adapter.widget().set_error(None);
    // 清除宿主错误后重试同一所有权；若先前错误占了pending，此处会Busy。
    adapter.submit_batch(rejected.transactions).unwrap();
    let (second, viewport) = wait_draw(&adapter);
    assert!(!receipt.is_valid());
    assert!(second.is_valid());
    verify_pixels(&adapter, viewport, expected_scale, [64, 160, 224, 255]);
    window.set_child(gtk4::Widget::NONE);
    assert!(!second.is_valid(), "unrealize 必须撤销旧 draw proof");
    assert!(adapter
        .drain_events()
        .iter()
        .any(|event| matches!(event, AdapterEvent::Invalidated { .. })));
    adapter.submit_batch(vec![frame(session_id, 2)]).unwrap();
    window.set_child(Some(adapter.widget()));
    let (third, viewport) = wait_draw(&adapter);
    assert!(third.is_valid());
    assert!(!second.is_valid());
    verify_pixels(&adapter, viewport, expected_scale, [255, 0, 0, 255]);
    window.set_child(gtk4::Widget::NONE);
    assert!(!third.is_valid());
    assert!(!adapter.widget().is_realized());
    adapter.detach();
    assert_eq!(
        adapter
            .submit_batch(vec![frame(session_id, 3)])
            .unwrap_err()
            .reason,
        SubmitError::Closed
    );
    window.close();
    adapter.detach();
    println!("native GTK adapter backend={expected_backend} scale={expected_scale} transaction_draw=3 partial_update=1 context_rebuild=1");
}

fn wait_draw(adapter: &GtkFrameArea) -> (DrawReceipt, ContentViewport) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let context = glib::MainContext::default();
        for _ in 0..64 {
            if !context.pending() {
                break;
            }
            context.iteration(false);
        }
        let mut drawn = None;
        for event in adapter.drain_events() {
            match event {
                AdapterEvent::Drawn { receipt, viewport } => {
                    assert!(viewport.drawable.width > 0);
                    drawn = Some((receipt, viewport));
                }
                AdapterEvent::Failed(error) => panic!("实际 GTK 渲染失败: {error:?}"),
                AdapterEvent::Invalidated { .. } => panic!("首次绘制不应失效"),
            }
        }
        if let Some(receipt) = drawn {
            break receipt;
        }
        assert!(Instant::now() < deadline, "10 秒内没有真实帧事务 draw");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn verify_pixels(
    adapter: &GtkFrameArea,
    viewport: ContentViewport,
    expected_scale: i32,
    expected_top_left: [u8; 4],
) {
    assert_eq!(adapter.widget().scale_factor(), expected_scale);
    adapter.widget().make_current();
    assert_eq!(gdk::GLContext::current(), adapter.widget().context());
    // 只在测试中读取最后一次 render 的 FBO；不调用 attach_buffers，它会分配下一纹理。
    unsafe {
        let library = libloading::Library::new("libepoxy.so.0").unwrap();
        let gl = glow::Context::from_loader_function(|name| {
            library
                .get::<*const *const c_void>(format!("epoxy_{name}\0").as_bytes())
                .map_or(std::ptr::null(), |symbol| **symbol)
        });
        assert_eq!(gl.get_error(), glow::NO_ERROR);
        let fbo = gl.get_parameter_i32(glow::DRAW_FRAMEBUFFER_BINDING);
        assert_ne!(fbo, 0);
        let old_read = gl.get_parameter_i32(glow::READ_FRAMEBUFFER_BINDING);
        gl.bind_framebuffer(
            glow::READ_FRAMEBUFFER,
            std::num::NonZeroU32::new(fbo as u32).map(glow::NativeFramebuffer),
        );
        let width = adapter.widget().width() * expected_scale;
        let height = adapter.widget().height() * expected_scale;
        assert_eq!(
            viewport.drawable,
            PixelSize::new(width as u32, height as u32).unwrap()
        );
        let left = viewport.content.x as i32;
        let right = left + viewport.content.width as i32 - 1;
        let bottom = height - (viewport.content.y + viewport.content.height) as i32;
        let top = height - viewport.content.y as i32 - 1;
        gl.bind_buffer(glow::PIXEL_PACK_BUFFER, None);
        gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
        gl.pixel_store_i32(glow::PACK_ROW_LENGTH, 0);
        gl.pixel_store_i32(glow::PACK_SKIP_PIXELS, 0);
        gl.pixel_store_i32(glow::PACK_SKIP_ROWS, 0);
        let mut pixel = [0u8; 4];
        for (x, y, expected) in [
            (left, top, expected_top_left),
            (right, top, [128, 128, 128, 255]),
            (left, bottom, [32, 96, 192, 255]),
            (right, bottom, [0, 0, 255, 255]),
        ] {
            gl.read_pixels(
                x,
                y,
                1,
                1,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut pixel)),
            );
            for channel in 0..4 {
                assert!(
                    pixel[channel].abs_diff(expected[channel]) <= 1,
                    "RGBA/origin/alpha/gamma: actual={pixel:?} expected={expected:?}"
                );
            }
        }
        assert_eq!(gl.get_error(), glow::NO_ERROR);
        gl.bind_framebuffer(
            glow::READ_FRAMEBUFFER,
            std::num::NonZeroU32::new(old_read as u32).map(glow::NativeFramebuffer),
        );
        println!("native GTK adapter scale={expected_scale} width={width} height={height} color_corners=4");
    }
}

fn partial(session_id: SessionId) -> FrameTransaction {
    FrameTransaction::Revision {
        earliest_constituent_enqueue_at: Instant::now(),
        revision: FrameRevision {
            session_id,
            generation: 1,
            revision: 2,
            completeness: FrameCompleteness::Incremental,
            patches: vec![PixelPatch {
                rect: PixelRect {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                },
                stride_bytes: 4,
                pixels: PixelBuffer::new(vec![224, 160, 64, 99]),
            }],
        },
    }
}

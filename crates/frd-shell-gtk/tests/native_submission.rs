#![cfg(all(
    target_os = "linux",
    feature = "gtk-shell",
    any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")
))]
use frd_core::{PixelRect, PixelSize, SessionId};
use frd_frame::{
    FrameCompleteness, FrameReset, FrameRevision, FrameTransaction, PixelBuffer, PixelFormat,
    PixelPatch,
};
use frd_shell_gtk::{AdapterEvent, GtkFrameArea, SubmissionError, WindowSubmission};
use gtk4::{gdk, glib, prelude::*};
use std::{
    cell::{Cell, RefCell},
    ffi::c_void,
    rc::Rc,
    time::{Duration, Instant},
};

fn startup(session_id: SessionId, generation: u64) -> FrameTransaction {
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
                    0, 0, 255, 0, 128, 128, 128, 0, 192, 96, 32, 0, 255, 0, 0, 0,
                ]),
            }],
        },
    }
}

struct EglProbe {
    _library: libloading::Library,
    context: unsafe extern "C" fn() -> *mut c_void,
    drawable: unsafe extern "C" fn(u32) -> *mut c_void,
    display: unsafe extern "C" fn() -> *mut c_void,
    swap: unsafe extern "C" fn(*mut c_void, *mut c_void) -> u32,
}
impl EglProbe {
    fn load() -> Self {
        unsafe {
            let library = libloading::Library::new("libEGL.so.1").expect("fixture 必须有真实 EGL");
            Self {
                context: *library.get(b"eglGetCurrentContext\0").unwrap(),
                display: *library.get(b"eglGetCurrentDisplay\0").unwrap(),
                swap: *library.get(b"eglSwapBuffers\0").unwrap(),
                drawable: *library.get(b"eglGetCurrentSurface\0").unwrap(),
                _library: library,
            }
        }
    }
    fn actual_window(&self) -> bool {
        unsafe { !(self.context)().is_null() && !(self.drawable)(0x3059).is_null() }
    }
}

#[test]
#[ignore = "需要原生 Linux GTK 4.14、旧 GSK gl 和实际 EGL 窗口；GLX 不计通过"]
fn native_gtk_window_submission_roundtrip() {
    let backend = std::env::var("FRD_GTK_TEST_BACKEND").expect("显式选择 x11 或 wayland");
    assert!(matches!(backend.as_str(), "x11" | "wayland"));
    let scale: i32 = std::env::var("FRD_GTK_TEST_SCALE")
        .expect("显式比例")
        .parse()
        .unwrap();
    assert!(matches!(scale, 1 | 2));
    gtk4::init().expect("必须原生初始化 GTK");
    let display = gdk::Display::default().unwrap();
    assert_eq!(
        display.type_().name(),
        if backend == "x11" {
            "GdkX11Display"
        } else {
            "GdkWaylandDisplay"
        }
    );
    let adapter = Rc::new(GtkFrameArea::new());
    let window = gtk4::Window::builder()
        .title("FreeRemoteDesk EGL 窗口提交测试")
        .default_width(128)
        .default_height(128)
        .child(adapter.widget())
        .build();
    let session_id = SessionId::allocate();
    adapter.submit_batch(vec![startup(session_id, 1)]).unwrap();
    window.present();
    assert!(adapter.widget().is_realized());
    let surface = window.surface().unwrap();
    let clock = surface.frame_clock();
    let first_window_frame = Rc::new(Cell::new(None));
    let inject = Rc::new(Cell::new(false));
    let injected_frame = Rc::new(Cell::new(None));
    let inject_egl = Rc::new(Cell::new(false));
    let egl_injected_frame = Rc::new(Cell::new(None));
    let egl = EglProbe::load();
    let epoxy = unsafe { libloading::Library::new("libepoxy.so.0").unwrap() };
    // 先连接：此回调在 GSK paint 返回后、observer 的 after-paint 检查之前运行。
    // 故障命中本轮 before/after 作用域，不能在这里调用 glGetError 消费故障。
    let hook = {
        let first = first_window_frame.clone();
        let inject = inject.clone();
        let injected = injected_frame.clone();
        let inject_egl = inject_egl.clone();
        let egl_injected = egl_injected_frame.clone();
        let surface = surface.clone();
        clock.connect_after_paint(move |clock| {
            let current = gdk::GLContext::current();
            let window_current = current
                .as_ref()
                .is_some_and(|c| DrawContextExt::surface(c).as_ref() == Some(&surface));
            if window_current && egl.actual_window() {
                if first.get().is_none() {
                    first.set(Some(clock.frame_counter()));
                }
                if inject.replace(false) {
                    unsafe {
                        let enable = **epoxy
                            .get::<*const unsafe extern "C" fn(u32)>(b"epoxy_glEnable\0")
                            .unwrap();
                        enable(u32::MAX); // 确定性 GL_INVALID_ENUM，保持错误给 observer。
                    }
                    injected.set(Some(clock.frame_counter()));
                }
                if inject_egl.replace(false) {
                    unsafe {
                        let display = (egl.display)();
                        assert!(!display.is_null());
                        // 有效display与EGL_NO_SURFACE：真实EGL_BAD_SURFACE，留给observer消费。
                        assert_eq!((egl.swap)(display, std::ptr::null_mut()), 0);
                    }
                    egl_injected.set(Some(clock.frame_counter()));
                }
            }
        })
    };
    adapter
        .enable_window_submission(&window)
        .expect("只接受固定旧 GL renderer");
    // 真实 GtkWidget tick 在下一帧 UPDATE 才消费前一 AFTER_PAINT 的证明。
    // 不依赖协议新消息或在两帧之间主动轮询 take。
    let tick_result = Rc::new(RefCell::new(None));
    let pure_tick_checked = Rc::new(Cell::new(false));
    let callback_checked = pure_tick_checked.clone();
    let weak_adapter = Rc::downgrade(&adapter);
    let callback_result = tick_result.clone();
    adapter.widget().add_tick_callback(move |_, clock| {
        let Some(adapter) = weak_adapter.upgrade() else {
            return glib::ControlFlow::Break;
        };
        if callback_result.borrow().is_some() {
            assert!(
                adapter.take_submission_error().is_none(),
                "消费后的纯tick不能成为呈现错误"
            );
            assert!(
                adapter.take_window_submission().is_none(),
                "纯tick不能产生新证明"
            );
            callback_checked.set(true);
            return glib::ControlFlow::Break;
        }
        if let Some(submission) = adapter.take_window_submission() {
            let produced = submission.frame_counter();
            assert!(clock.frame_counter() > produced, "必须跨到下一 UPDATE 消费");
            let draw = submission.consume().expect("下一UPDATE时证明仍须有效");
            *callback_result.borrow_mut() =
                Some((produced, draw, adapter.submission_diagnostics().unwrap()));
            return glib::ControlFlow::Continue;
        }
        if let Some(error) = adapter.take_submission_error() {
            assert_eq!(
                error,
                SubmissionError::Association,
                "bootstrap之外的原生错误"
            );
        }
        glib::ControlFlow::Continue
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while !pure_tick_checked.get() {
        pump();
        assert!(
            Instant::now() < deadline,
            "下一UPDATE未得到提交: {:?}",
            adapter.submission_diagnostics()
        );
    }
    let (first_counter, drawn, diagnostics) = tick_result.borrow_mut().take().unwrap();
    assert!(adapter.take_submission_error().is_none());
    assert!(diagnostics.bootstrap_count >= 1 && diagnostics.confirmed_count >= 1);
    assert!(diagnostics.draw_surfaceless && diagnostics.window_context_transition,
        "必须观察本次 GLArea surfaceless → GSK 窗口 context 转换，不能只看 expose 是否非空: {diagnostics:?}");
    assert!(
        first_counter
            > first_window_frame
                .get()
                .expect("必须实际观察 EGL window drawable"),
        "bootstrap 首帧不得签发提交"
    );
    assert!(
        adapter.take_window_submission().is_none(),
        "take 必须消费槽位一次"
    );
    assert!(drawn.is_valid());
    assert_eq!(drawn.frame().session_id, session_id);
    assert_eq!(drawn.frame().generation, 1);
    assert_eq!(drawn.frame().revision, 1);
    assert_eq!(adapter.widget().scale_factor(), scale);
    assert!(
        adapter
            .drain_events()
            .iter()
            .all(|e| !matches!(e, AdapterEvent::Drawn { .. })),
        "enabled 时不能复制 draw receipt 到事件通道"
    );

    // 留一份ready不取，再用新draw替代；旧serial失效不得误报为消费错误。
    let mut confirmed_count = adapter.submission_diagnostics().unwrap().confirmed_count;
    let mut replacement_frames = Vec::new();
    for _ in 0..2 {
        adapter.widget().queue_render();
        let deadline = Instant::now() + Duration::from_secs(10);
        while adapter.submission_diagnostics().unwrap().confirmed_count == confirmed_count {
            pump();
            assert!(
                adapter.take_submission_error().is_none(),
                "新draw替代未消费ready不能报错"
            );
            assert!(Instant::now() < deadline, "替代ready重绘超时");
        }
        let observed = adapter.submission_diagnostics().unwrap();
        confirmed_count = observed.confirmed_count;
        replacement_frames.push(observed.frame_counter);
    }
    let replacement = adapter.take_window_submission().unwrap();
    assert_eq!(replacement.frame_counter(), replacement_frames[1]);
    assert!(replacement_frames[1] > replacement_frames[0]);
    assert!(replacement.consume().is_ok());

    // 在已完成 bootstrap 的下一帧，真实窗口 GL context 上注入错误。
    while adapter.take_submission_error().is_some() {}
    inject.set(true);
    adapter.widget().queue_render();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        pump();
        assert!(
            adapter.take_window_submission().is_none(),
            "含 GL 错误的窗口帧不得确认"
        );
        if let Some(error) = adapter.take_submission_error() {
            assert_eq!(error, SubmissionError::GlFault);
            assert!(
                injected_frame.get().is_some(),
                "必须确实执行原生 GL 故障注入"
            );
            break;
        }
        assert!(Instant::now() < deadline, "没有观察到作用域内 GL 错误");
    }
    assert!(!drawn.is_valid(), "新 draw 必须撤销旧 renderer serial");
    // observer 已消费自身故障；下一次显式重绘可重新 bootstrap 并得到干净提交。
    adapter.widget().queue_render();
    let recovered = wait_submission(&adapter);
    assert!(recovered.frame_counter() > injected_frame.get().unwrap());
    assert!(recovered.consume().is_ok());

    // 同一作用域观察真实 EGL 失败；测试不能先调用 eglGetError。
    while adapter.take_submission_error().is_some() {}
    inject_egl.set(true);
    adapter.widget().queue_render();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        pump();
        let unexpected = adapter.take_window_submission();
        assert!(
            unexpected.is_none(),
            "EGL swap 失败的帧不得确认: injected_frame={:?} received_frame={:?} diagnostics={:?}",
            egl_injected_frame.get(),
            unexpected.as_ref().map(WindowSubmission::frame_counter),
            adapter.submission_diagnostics()
        );
        if let Some(error) = adapter.take_submission_error() {
            assert_eq!(error, SubmissionError::EglFault);
            assert_eq!(
                adapter.submission_diagnostics().unwrap().last_egl_error,
                0x300d,
                "必须观察到EGL_BAD_SURFACE"
            );
            assert!(
                egl_injected_frame.get().is_some(),
                "必须实际执行失败的 EGL swap"
            );
            break;
        }
        assert!(Instant::now() < deadline, "未观察到真实 EGL 错误");
    }
    adapter.widget().queue_render();
    let recovered = wait_submission(&adapter);
    assert!(recovered.frame_counter() > egl_injected_frame.get().unwrap());
    assert!(recovered.consume().is_ok());

    // 另一个真正 realize 的窗口不能替换现有 observer 的绑定。
    let other = gtk4::Window::builder()
        .title("错误窗口绑定负例")
        .default_width(64)
        .default_height(64)
        .build();
    other.present();
    assert!(other.surface().is_some());
    assert_eq!(
        adapter.enable_window_submission(&other),
        Err(SubmissionError::Association)
    );
    other.close();
    window.present();
    adapter.widget().queue_render();
    let before_resize = wait_submission(&adapter);
    let old_size = (adapter.widget().width(), adapter.widget().height());
    adapter
        .widget()
        .set_size_request(old_size.0 + 64, old_size.1 + 48);
    window.set_default_size(old_size.0 + 64, old_size.1 + 48);
    let deadline = Instant::now() + Duration::from_secs(10);
    while (adapter.widget().width(), adapter.widget().height()) == old_size {
        pump();
        assert!(
            Instant::now() < deadline,
            "必须发生真实 GTK allocation 改变"
        );
    }
    assert!(
        before_resize.consume().is_err(),
        "resize 必须撤销已取出的窗口证明"
    );
    adapter.widget().queue_render();
    let after_resize = wait_submission(&adapter);
    assert!(
        after_resize.consume().is_ok(),
        "错误窗口绑定和resize不能永久破坏原observer"
    );
    adapter.widget().queue_render();
    let before_unrealize = wait_submission(&adapter);
    window.set_child(gtk4::Widget::NONE);
    assert!(
        before_unrealize.consume().is_err(),
        "unrealize 必须撤销尚未消费的窗口证明"
    );
    assert!(adapter.take_window_submission().is_none());

    adapter.submit_batch(vec![startup(session_id, 2)]).unwrap();
    window.set_child(Some(adapter.widget()));
    assert!(adapter.widget().is_realized());
    adapter
        .enable_window_submission(&window)
        .expect("新生命周期必须显式重启 observer");
    let deadline = Instant::now() + Duration::from_secs(10);
    let confirmed = loop {
        pump();
        if let Some(confirmed) = adapter.take_confirmed_presentation().unwrap() {
            break confirmed;
        }
        assert!(
            Instant::now() < deadline,
            "消费式确认超时: {:?}",
            adapter.submission_diagnostics()
        );
    }
    .into_receipt();
    assert_eq!(confirmed.session_id, session_id);
    assert_eq!(confirmed.generation, 2);
    assert_eq!(confirmed.revision, 1);
    assert!(adapter.take_confirmed_presentation().unwrap().is_none());
    let previous_frame = clock.frame_counter();
    adapter.widget().queue_render();
    let deadline = Instant::now() + Duration::from_secs(5);
    while clock.frame_counter() <= previous_frame {
        pump();
        assert!(
            adapter.take_confirmed_presentation().unwrap().is_none(),
            "已确认revision不能被重绘重复确认"
        );
        assert!(Instant::now() < deadline, "确认后重绘必须实际发生");
    }
    assert!(adapter.take_confirmed_presentation().unwrap().is_none());
    adapter.detach();
    clock.disconnect(hook);
    window.close();
    println!("native GTK window submission backend={backend} scale={scale} actual_egl=1 bootstrap_rejected=1 consume_once=1 next_update_consumed=1 gl_fault_rejected=1 egl_fault_rejected=1 wrong_window_rejected=1 resize_revoked=1 recovery=1 unrealize_revoked=1 reenabled=1 confirmed_once=1");
}

fn pump() {
    let context = glib::MainContext::default();
    if context.pending() {
        context.iteration(false);
    } else {
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn wait_submission(adapter: &GtkFrameArea) -> WindowSubmission {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        pump();
        if let Some(submission) = adapter.take_window_submission() {
            return submission;
        }
        if let Some(error) = adapter.take_submission_error() {
            // layout 在 before-paint 之后发生时，本帧关联撤销，observer 会补一次重绘。
            assert_eq!(
                error,
                SubmissionError::Association,
                "真实窗口 observer 错误"
            );
        }
        for event in adapter.drain_events() {
            if let AdapterEvent::Failed(error) = event {
                panic!("GTK draw 失败: {error:?}");
            }
        }
        assert!(
            Instant::now() < deadline,
            "10 秒内没有经实际 EGL 窗口提交的证明: {:?}",
            adapter.submission_diagnostics()
        );
    }
}
